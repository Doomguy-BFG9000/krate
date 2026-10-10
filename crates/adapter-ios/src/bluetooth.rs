//! Bluetooth LE on the iPhone, for `krate:ui/bluetooth` (IC-917).
//!
//! An app never scans. `choose` looks for devices offering one service,
//! shows them in Krate's own list (a system action sheet Krate draws -- an
//! app cannot draw it or fake it), and connects to the one the person taps.
//! Only devices picked this way get a handle, so an app reaches nothing
//! else. Everything CoreBluetooth does happens on the main thread, where
//! its delegate is called; the app's thread asks and waits on a channel.
//!
//! CoreBluetooth is reached through the Objective-C runtime (no binding
//! crate): its classes by name, its delegate methods by selector.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{channel, Sender};
use std::sync::Mutex;
use std::time::Duration;

use dispatch2::DispatchQueue;
use krate_adapter_common::ui::{BtDevice, BtError, BtNotification};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObjectProtocol};
use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};
use objc2_foundation::NSString;

#[link(name = "CoreBluetooth", kind = "framework")]
unsafe extern "C" {}

/// How long `choose` looks before it shows what it found.
const SCAN_FOR: Duration = Duration::from_millis(3500);
/// How long one exchange with a device may take.
const EXCHANGE: Duration = Duration::from_secs(15);

/// The standard short form of a Bluetooth UUID, upper case: the full form of
/// a standard one ("0000180D-0000-1000-8000-00805F9B34FB") is "180D".
pub(crate) fn short_uuid(uuid: &str) -> String {
    let upper = uuid.trim().to_ascii_uppercase();
    match upper.strip_suffix("-0000-1000-8000-00805F9B34FB") {
        Some(head) if head.len() == 8 && head.starts_with("0000") => head[4..].to_string(),
        _ => upper,
    }
}

/// A key for one characteristic of one device.
type CharKey = (u64, String, String);

/// Who waits for a call that answers only done or not.
type Done = Sender<Result<(), BtError>>;

/// What the main thread keeps: the manager, the devices handed out, their
/// characteristics, and who is waiting for which answer.
#[derive(Default)]
struct Central {
    manager: Option<Retained<AnyObject>>,
    delegate: Option<Retained<KrateBtDelegate>>,
    /// CBManagerState once known: 5 is powered on.
    state: Option<i64>,
    waiting_for_state: Vec<Sender<i64>>,
    /// The service the current scan is for, short form.
    scan_for: Option<String>,
    /// Devices seen during the current scan, by identifier, with names.
    seen: Vec<(String, String, Retained<AnyObject>)>,
    /// Devices handed out, by handle.
    devices: HashMap<u64, Retained<AnyObject>>,
    handle_of: HashMap<String, u64>,
    next_handle: u64,
    characteristics: HashMap<CharKey, Retained<AnyObject>>,
    connecting: HashMap<String, Sender<Result<(), BtError>>>,
    /// Services still to have their characteristics found, per device.
    discovering: HashMap<u64, (usize, Option<Done>)>,
    reading: HashMap<CharKey, Sender<Result<Vec<u8>, BtError>>>,
    writing: HashMap<CharKey, Sender<Result<(), BtError>>>,
    subscribing: HashMap<CharKey, Sender<Result<(), BtError>>>,
}

thread_local! {
    static CENTRAL: RefCell<Central> = RefCell::new(Central::default());
}

/// Values devices sent on their own, by handle; read from the app's thread.
static NOTIFIED: Mutex<Option<HashMap<u64, Vec<BtNotification>>>> = Mutex::new(None);
/// Handles connected now.
static CONNECTED: Mutex<Option<HashSet<u64>>> = Mutex::new(None);

fn with_central<R>(f: impl FnOnce(&mut Central) -> R) -> R {
    CENTRAL.with(|central| f(&mut central.borrow_mut()))
}

fn ns(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

/// A CBUUID for a UUID as the app wrote it.
fn cbuuid(uuid: &str) -> Option<Retained<AnyObject>> {
    let class = AnyClass::get(c"CBUUID")?;
    // SAFETY: +[CBUUID UUIDWithString:] takes a 16-bit, 32-bit or 128-bit
    // UUID string; anything else raises, so the string is checked first.
    let valid = {
        let hex: String = uuid.chars().filter(|c| *c != '-').collect();
        matches!(hex.len(), 4 | 8 | 32) && hex.chars().all(|c| c.is_ascii_hexdigit())
    };
    if !valid {
        return None;
    }
    unsafe { msg_send![class, UUIDWithString: &*ns(uuid)] }
}

/// The short form of an object's `UUID` (a CBService's or CBCharacteristic's).
fn uuid_of(object: &AnyObject) -> String {
    // SAFETY: CBAttribute's UUID and CBUUID's UUIDString.
    unsafe {
        let uuid: Option<Retained<AnyObject>> = msg_send![object, UUID];
        let text: Option<Retained<NSString>> = uuid.and_then(|u| msg_send![&*u, UUIDString]);
        text.map(|t| short_uuid(&t.to_string())).unwrap_or_default()
    }
}

/// A peripheral's identifier, as text.
fn identifier_of(peripheral: &AnyObject) -> String {
    // SAFETY: CBPeer's identifier (an NSUUID) and its UUIDString.
    unsafe {
        let id: Option<Retained<AnyObject>> = msg_send![peripheral, identifier];
        let text: Option<Retained<NSString>> = id.and_then(|i| msg_send![&*i, UUIDString]);
        text.map(|t| t.to_string()).unwrap_or_default()
    }
}

/// Whether an advertisement lists `wanted` among its services, in the main
/// list or the overflow area (where iOS and macOS put a background
/// advertiser's services).
fn advertises(ad: &AnyObject, wanted: &str) -> bool {
    ["kCBAdvDataServiceUUIDs", "kCBAdvDataOverflowServiceUUIDs"]
        .iter()
        .any(|key| {
            // SAFETY: an NSDictionary's objectForKey:; the values under
            // these keys are NSArrays of CBUUIDs.
            let list: Option<Retained<AnyObject>> =
                unsafe { msg_send![ad, objectForKey: &*ns(key)] };
            objects(list).iter().any(|uuid| {
                let text: Option<Retained<NSString>> = unsafe { msg_send![&**uuid, UUIDString] };
                text.is_some_and(|t| short_uuid(&t.to_string()) == wanted)
            })
        })
}

fn objects(array: Option<Retained<AnyObject>>) -> Vec<Retained<AnyObject>> {
    let Some(array) = array else {
        return Vec::new();
    };
    // SAFETY: an NSArray's count and objectAtIndex:.
    unsafe {
        let count: usize = msg_send![&*array, count];
        (0..count)
            .filter_map(|i| {
                let item: Option<Retained<AnyObject>> = msg_send![&*array, objectAtIndex: i];
                item
            })
            .collect()
    }
}

fn bytes_of(data: Option<Retained<AnyObject>>) -> Vec<u8> {
    let Some(data) = data else {
        return Vec::new();
    };
    // SAFETY: an NSData's length and bytes, copied out at once.
    unsafe {
        let len: usize = msg_send![&*data, length];
        let ptr: *const u8 = msg_send![&*data, bytes];
        if ptr.is_null() || len == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(ptr, len).to_vec()
        }
    }
}

fn error_text(error: Option<&AnyObject>) -> Option<String> {
    let error = error?;
    // SAFETY: NSError's localizedDescription.
    let text: Option<Retained<NSString>> = unsafe { msg_send![error, localizedDescription] };
    Some(
        text.map(|t| t.to_string())
            .unwrap_or_else(|| "the device refused".to_string()),
    )
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements for a delegate.
    // - The central manager is made with the main queue, so CoreBluetooth
    //   calls these on the main thread.
    #[unsafe(super = objc2_foundation::NSObject)]
    #[thread_kind = MainThreadOnly]
    struct KrateBtDelegate;

    // SAFETY: NSObjectProtocol has no additional requirements.
    unsafe impl NSObjectProtocol for KrateBtDelegate {}

    // CBCentralManagerDelegate and CBPeripheralDelegate, by selector:
    // CoreBluetooth asks respondsToSelector: before each call.
    impl KrateBtDelegate {
        #[unsafe(method(centralManagerDidUpdateState:))]
        fn did_update_state(&self, central: &AnyObject) {
            // SAFETY: CBManager's state.
            let state: i64 = unsafe { msg_send![central, state] };
            with_central(|c| {
                c.state = Some(state);
                for tx in c.waiting_for_state.drain(..) {
                    let _ = tx.send(state);
                }
            });
        }

        #[unsafe(method(centralManager:didDiscoverPeripheral:advertisementData:RSSI:))]
        fn did_discover(&self, _central: &AnyObject, peripheral: &AnyObject, ad: &AnyObject, _rssi: &AnyObject) {
            let id = identifier_of(peripheral);
            // Only a device that says it offers the service asked for: the
            // scan is unfiltered (iOS's own filter missed devices that do
            // advertise it, measured against a Mac advertising 180D), so the
            // advertisement is checked here, overflow area included.
            let wanted = with_central(|c| c.scan_for.clone());
            let Some(wanted) = wanted else {
                return;
            };
            if !advertises(ad, &wanted) {
                return;
            }
            // SAFETY: CBPeripheral's name, nil when it gives none.
            let name: Option<Retained<NSString>> = unsafe { msg_send![peripheral, name] };
            let name = name.map(|n| n.to_string()).filter(|n| !n.trim().is_empty());
            // SAFETY: CoreBluetooth hands a live peripheral; keep it.
            let retained = unsafe { Retained::retain(peripheral as *const AnyObject as *mut AnyObject) };
            with_central(|c| {
                if let Some(retained) = retained {
                    if !c.seen.iter().any(|(seen, _, _)| *seen == id) {
                        c.seen.push((id, name.unwrap_or_else(|| "Unnamed device".to_string()), retained));
                    }
                }
            });
        }

        #[unsafe(method(centralManager:didConnectPeripheral:))]
        fn did_connect(&self, _central: &AnyObject, peripheral: &AnyObject) {
            let id = identifier_of(peripheral);
            with_central(|c| {
                if let Some(tx) = c.connecting.remove(&id) {
                    let _ = tx.send(Ok(()));
                }
            });
        }

        #[unsafe(method(centralManager:didFailToConnectPeripheral:error:))]
        fn did_fail(&self, _central: &AnyObject, peripheral: &AnyObject, error: Option<&AnyObject>) {
            let id = identifier_of(peripheral);
            let why = error_text(error).unwrap_or_else(|| "it would not connect".to_string());
            with_central(|c| {
                if let Some(tx) = c.connecting.remove(&id) {
                    let _ = tx.send(Err(BtError::Failed(why)));
                }
            });
        }

        #[unsafe(method(centralManager:didDisconnectPeripheral:error:))]
        fn did_disconnect(&self, _central: &AnyObject, peripheral: &AnyObject, _error: Option<&AnyObject>) {
            let id = identifier_of(peripheral);
            with_central(|c| {
                if let Some(handle) = c.handle_of.get(&id).copied() {
                    if let Some(set) = CONNECTED.lock().expect("bt lock").as_mut() {
                        set.remove(&handle);
                    }
                    // Everyone waiting on this device hears it went.
                    let gone = |key: &CharKey| key.0 == handle;
                    for (_, tx) in c.reading.extract_if(|k, _| gone(k)) {
                        let _ = tx.send(Err(BtError::Disconnected));
                    }
                    for (_, tx) in c.writing.extract_if(|k, _| gone(k)) {
                        let _ = tx.send(Err(BtError::Disconnected));
                    }
                    for (_, tx) in c.subscribing.extract_if(|k, _| gone(k)) {
                        let _ = tx.send(Err(BtError::Disconnected));
                    }
                }
                if let Some(tx) = c.connecting.remove(&id) {
                    let _ = tx.send(Err(BtError::Disconnected));
                }
            });
        }

        #[unsafe(method(peripheral:didDiscoverServices:))]
        fn did_discover_services(&self, peripheral: &AnyObject, error: Option<&AnyObject>) {
            let id = identifier_of(peripheral);
            // SAFETY: CBPeripheral's services.
            let services = objects(unsafe { msg_send![peripheral, services] });
            with_central(|c| {
                let Some(handle) = c.handle_of.get(&id).copied() else {
                    return;
                };
                if let Some(why) = error_text(error) {
                    if let Some((_, Some(tx))) = c.discovering.remove(&handle) {
                        let _ = tx.send(Err(BtError::Failed(why)));
                    }
                    return;
                }
                if services.is_empty() {
                    if let Some((_, Some(tx))) = c.discovering.remove(&handle) {
                        let _ = tx.send(Ok(()));
                    }
                    return;
                }
                if let Some(entry) = c.discovering.get_mut(&handle) {
                    entry.0 = services.len();
                }
                for service in &services {
                    // SAFETY: discover every characteristic of a service of
                    // this peripheral.
                    let _: () = unsafe {
                        msg_send![peripheral, discoverCharacteristics: Option::<&AnyObject>::None, forService: &**service]
                    };
                }
            });
        }

        #[unsafe(method(peripheral:didDiscoverCharacteristicsForService:error:))]
        fn did_discover_characteristics(&self, peripheral: &AnyObject, service: &AnyObject, _error: Option<&AnyObject>) {
            let id = identifier_of(peripheral);
            let service_uuid = uuid_of(service);
            // SAFETY: CBService's characteristics.
            let found = objects(unsafe { msg_send![service, characteristics] });
            with_central(|c| {
                let Some(handle) = c.handle_of.get(&id).copied() else {
                    return;
                };
                for characteristic in found {
                    let key = (handle, service_uuid.clone(), uuid_of(&characteristic));
                    c.characteristics.insert(key, characteristic);
                }
                let done = match c.discovering.get_mut(&handle) {
                    Some(entry) => {
                        entry.0 = entry.0.saturating_sub(1);
                        entry.0 == 0
                    }
                    None => false,
                };
                if done {
                    if let Some((_, Some(tx))) = c.discovering.remove(&handle) {
                        let _ = tx.send(Ok(()));
                    }
                }
            });
        }

        #[unsafe(method(peripheral:didUpdateValueForCharacteristic:error:))]
        fn did_update_value(&self, peripheral: &AnyObject, characteristic: &AnyObject, error: Option<&AnyObject>) {
            let id = identifier_of(peripheral);
            // SAFETY: CBCharacteristic's service and value.
            let service: Option<Retained<AnyObject>> = unsafe { msg_send![characteristic, service] };
            let service_uuid = service.map(|s| uuid_of(&s)).unwrap_or_default();
            let data = bytes_of(unsafe { msg_send![characteristic, value] });
            let char_uuid = uuid_of(characteristic);
            with_central(|c| {
                let Some(handle) = c.handle_of.get(&id).copied() else {
                    return;
                };
                let key = (handle, service_uuid.clone(), char_uuid.clone());
                if let Some(tx) = c.reading.remove(&key) {
                    let _ = tx.send(match error_text(error) {
                        Some(why) => Err(BtError::Failed(why)),
                        None => Ok(data),
                    });
                    return;
                }
                if error.is_none() {
                    let mut notified = NOTIFIED.lock().expect("bt lock");
                    let queue = notified.get_or_insert_with(HashMap::new).entry(handle).or_default();
                    // Bounded: an app that never collects keeps the newest.
                    if queue.len() >= 256 {
                        queue.remove(0);
                    }
                    queue.push((service_uuid, char_uuid, data));
                }
            });
        }

        #[unsafe(method(peripheral:didWriteValueForCharacteristic:error:))]
        fn did_write(&self, peripheral: &AnyObject, characteristic: &AnyObject, error: Option<&AnyObject>) {
            self.answer(peripheral, characteristic, error, |c, key| c.writing.remove(key));
        }

        #[unsafe(method(peripheral:didUpdateNotificationStateForCharacteristic:error:))]
        fn did_update_notification_state(&self, peripheral: &AnyObject, characteristic: &AnyObject, error: Option<&AnyObject>) {
            self.answer(peripheral, characteristic, error, |c, key| c.subscribing.remove(key));
        }
    }
);

impl KrateBtDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's init.
        unsafe { msg_send![super(this), init] }
    }

    /// Answer whoever waits on a write or a subscription to `characteristic`.
    fn answer(
        &self,
        peripheral: &AnyObject,
        characteristic: &AnyObject,
        error: Option<&AnyObject>,
        take: impl FnOnce(&mut Central, &CharKey) -> Option<Sender<Result<(), BtError>>>,
    ) {
        let id = identifier_of(peripheral);
        // SAFETY: CBCharacteristic's service.
        let service: Option<Retained<AnyObject>> = unsafe { msg_send![characteristic, service] };
        let service_uuid = service.map(|s| uuid_of(&s)).unwrap_or_default();
        let char_uuid = uuid_of(characteristic);
        with_central(|c| {
            let Some(handle) = c.handle_of.get(&id).copied() else {
                return;
            };
            if let Some(tx) = take(c, &(handle, service_uuid, char_uuid)) {
                let _ = tx.send(match error_text(error) {
                    Some(why) => Err(BtError::Failed(why)),
                    None => Ok(()),
                });
            }
        });
    }
}

/// Run `f` on the main thread with the central, and wait for what it sends.
fn on_main<T: Send + 'static>(
    wait: Duration,
    f: impl FnOnce(&mut Central, MainThreadMarker, Sender<T>) + Send + 'static,
) -> Option<T> {
    let (tx, rx) = channel();
    DispatchQueue::main().exec_async(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        with_central(|c| f(c, mtm, tx));
    });
    rx.recv_timeout(wait).ok()
}

/// Make the central manager if there is none, and wait for Bluetooth to
/// say whether it is on (the phone's own prompt shows the first time).
fn powered() -> Result<(), BtError> {
    let state = on_main(Duration::from_secs(60), |c, mtm, tx| {
        if let Some(state) = c.state {
            let _ = tx.send(state);
            return;
        }
        c.waiting_for_state.push(tx);
        if c.manager.is_none() {
            let Some(class) = AnyClass::get(c"CBCentralManager") else {
                return;
            };
            let delegate = KrateBtDelegate::new(mtm);
            // SAFETY: CBCentralManager on the main queue (a nil queue),
            // its delegate held by Krate (CoreBluetooth holds it weakly).
            let manager: Option<Retained<AnyObject>> = unsafe {
                let made: objc2::rc::Allocated<AnyObject> = msg_send![class, alloc];
                msg_send![made, initWithDelegate: &*delegate, queue: Option::<&AnyObject>::None]
            };
            c.manager = manager;
            c.delegate = Some(delegate);
        }
    });
    match state {
        Some(5) => Ok(()),
        // Unauthorized.
        Some(3) => Err(BtError::NotAllowed),
        _ => Err(BtError::Unavailable),
    }
}

pub fn available() -> bool {
    AnyClass::get(c"CBCentralManager").is_some()
}

/// Look, show Krate's list, connect to the pick, and find its services.
pub fn choose(service: &str, title: &str) -> Result<Option<BtDevice>, BtError> {
    let Some(_) = cbuuid(service) else {
        return Err(BtError::NotFound);
    };
    powered()?;
    let service_text = service.to_string();
    let title_text = title.to_string();
    CANCELLED.store(false, std::sync::atomic::Ordering::Relaxed);
    // Look, while a sheet says so and offers Cancel.
    let looking = on_main(Duration::from_secs(2), move |c, mtm, tx: Sender<()>| {
        c.seen.clear();
        c.scan_for = Some(short_uuid(&service_text));
        let Some(manager) = c.manager.clone() else {
            return;
        };
        // Every report of every device, not just the first: a device's
        // first report can lack its service list (a scan response with only
        // its name), and without repeats it was dropped and never seen
        // again -- found once, missed the next time.
        let options: Option<Retained<AnyObject>> = (|| {
            let number = AnyClass::get(c"NSNumber")?;
            let dictionary = AnyClass::get(c"NSDictionary")?;
            // SAFETY: an NSNumber YES under CoreBluetooth's
            // allow-duplicates key, in a one-entry NSDictionary.
            unsafe {
                let yes: Option<Retained<AnyObject>> = msg_send![number, numberWithBool: true];
                let yes = yes?;
                msg_send![dictionary, dictionaryWithObject: &*yes, forKey: &*ns("kCBScanOptionAllowDuplicates")]
            }
        })();
        // SAFETY: scan everything nearby; did_discover keeps only devices
        // advertising the service.
        let _: () = unsafe {
            msg_send![&*manager, scanForPeripheralsWithServices: Option::<&AnyObject>::None, options: options.as_deref()]
        };
        present_sheet(
            mtm,
            &title_text,
            "Looking for devices nearby\u{2026}",
            &[],
            move |_| {
                CANCELLED.store(true, std::sync::atomic::Ordering::Relaxed);
            },
        );
        let _ = tx.send(());
    });
    if looking.is_none() {
        return Err(BtError::Unavailable);
    }
    let started = std::time::Instant::now();
    while started.elapsed() < SCAN_FOR {
        if CANCELLED.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let cancelled = CANCELLED.load(std::sync::atomic::Ordering::Relaxed);
    let title_text = title.to_string();
    let service_wanted = service.to_string();
    // Stop looking, then show what was found; the tap is the answer.
    let picked = on_main(
        Duration::from_secs(600),
        move |c, mtm, tx: Sender<Option<usize>>| {
            c.scan_for = None;
            if let Some(manager) = c.manager.as_ref() {
                // SAFETY: stop the scan this started.
                let _: () = unsafe { msg_send![&**manager, stopScan] };
            }
            dismiss(mtm);
            if cancelled {
                let _ = tx.send(None);
                return;
            }
            // Devices already connected to the phone (headphones in use, a strap
            // paired with Health) do not advertise, so a scan never sees them:
            // ask iOS which of them offer the service, and list them too.
            if let (Some(manager), Some(wanted)) = (c.manager.clone(), cbuuid(&service_wanted)) {
                let services = objc2_foundation::NSArray::from_retained_slice(&[wanted]);
                // SAFETY: CBCentralManager's retrieveConnectedPeripheralsWithServices:.
                let already: Option<Retained<AnyObject>> = unsafe {
                    msg_send![&*manager, retrieveConnectedPeripheralsWithServices: &*services]
                };
                for peripheral in objects(already) {
                    let id = identifier_of(&peripheral);
                    if c.seen.iter().any(|(seen, _, _)| *seen == id) {
                        continue;
                    }
                    // SAFETY: CBPeripheral's name.
                    let name: Option<Retained<NSString>> = unsafe { msg_send![&*peripheral, name] };
                    let name = name.map(|n| n.to_string()).filter(|n| !n.trim().is_empty());
                    c.seen.push((
                        id,
                        name.unwrap_or_else(|| "Unnamed device".to_string()),
                        peripheral,
                    ));
                }
            }
            let names: Vec<String> = c.seen.iter().map(|(_, name, _)| name.clone()).collect();
            let message = if names.is_empty() {
                "No device offering this was found nearby. Check it is on and close by."
            } else {
                "Only the device you pick is shared with the app, and only while it is open."
            };
            let tx = std::sync::Arc::new(Mutex::new(Some(tx)));
            present_sheet(mtm, &title_text, message, &names, move |pick| {
                if let Some(tx) = tx.lock().expect("pick lock").take() {
                    let _ = tx.send(pick);
                }
            });
        },
    )
    .flatten();
    let Some(index) = picked else {
        return Ok(None);
    };
    // Connect, then find every service and characteristic.
    let connected = on_main(EXCHANGE, move |c, _mtm, tx| {
        let Some((id, _, peripheral)) = c.seen.get(index).cloned() else {
            let _ = tx.send(Err(BtError::NotFound));
            return;
        };
        let handle = match c.handle_of.get(&id) {
            Some(handle) => *handle,
            None => {
                c.next_handle += 1;
                let handle = c.next_handle;
                c.handle_of.insert(id.clone(), handle);
                handle
            }
        };
        c.devices.insert(handle, peripheral.clone());
        c.connecting.insert(id, tx);
        if let Some(manager) = c.manager.as_ref() {
            // SAFETY: connect to a peripheral this manager found.
            let _: () = unsafe {
                msg_send![&**manager, connectPeripheral: &*peripheral, options: Option::<&AnyObject>::None]
            };
        }
    })
    .unwrap_or_else(|| {
        // A connect left pending keeps trying for ever: take it back.
        on_main(Duration::from_secs(2), move |c, _mtm, tx: Sender<()>| {
            if let (Some(manager), Some((id, _, peripheral))) = (c.manager.as_ref(), c.seen.get(index)) {
                c.connecting.remove(id);
                // SAFETY: cancel the connect this asked for.
                let _: () = unsafe { msg_send![&**manager, cancelPeripheralConnection: &**peripheral] };
            }
            let _ = tx.send(());
        });
        Err(BtError::Failed("it did not answer".to_string()))
    });
    connected?;
    let device = on_main(Duration::from_secs(1), move |c, _mtm, tx| {
        let found = c.seen.get(index).and_then(|(id, name, _)| {
            c.handle_of.get(id).map(|handle| BtDevice {
                handle: *handle,
                name: name.clone(),
            })
        });
        let _ = tx.send(found);
    })
    .flatten()
    .ok_or(BtError::Disconnected)?;
    let handle = device.handle;
    let discovered = on_main(EXCHANGE, move |c, _mtm, tx| {
        let (Some(peripheral), Some(delegate)) =
            (c.devices.get(&handle).cloned(), c.delegate.clone())
        else {
            let _ = tx.send(Err(BtError::Disconnected));
            return;
        };
        c.discovering.insert(handle, (0, Some(tx)));
        // SAFETY: be the peripheral's delegate, then find all its services.
        unsafe {
            let _: () = msg_send![&*peripheral, setDelegate: &*delegate];
            let _: () = msg_send![&*peripheral, discoverServices: Option::<&AnyObject>::None];
        }
    })
    .unwrap_or(Err(BtError::Failed(
        "it did not list its services".to_string(),
    )));
    discovered?;
    CONNECTED
        .lock()
        .expect("bt lock")
        .get_or_insert_with(HashSet::new)
        .insert(handle);
    Ok(Some(device))
}

static CANCELLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

thread_local! {
    /// The sheet on screen, to take down before the next one.
    static SHEET: RefCell<Option<Retained<objc2_ui_kit::UIAlertController>>> = const { RefCell::new(None) };
}

fn dismiss(_mtm: MainThreadMarker) {
    if let Some(sheet) = SHEET.with(|s| s.borrow_mut().take()) {
        sheet.dismissViewControllerAnimated_completion(false, None);
    }
}

/// Krate's own list: `choices` as buttons, then Cancel. `on_pick` hears the
/// index tapped, or None for Cancel.
fn present_sheet(
    mtm: MainThreadMarker,
    title: &str,
    message: &str,
    choices: &[String],
    on_pick: impl Fn(Option<usize>) + 'static,
) {
    use objc2_ui_kit::{
        UIAlertAction, UIAlertActionStyle, UIAlertController, UIAlertControllerStyle,
    };
    let Some(main_side) = crate::uikit_native::main_controller() else {
        on_pick(None);
        return;
    };
    let sheet = UIAlertController::alertControllerWithTitle_message_preferredStyle(
        Some(&ns(title)),
        Some(&ns(message)),
        UIAlertControllerStyle::ActionSheet,
        mtm,
    );
    let on_pick = std::rc::Rc::new(on_pick);
    for (index, name) in choices.iter().enumerate() {
        let pick = on_pick.clone();
        let block = block2::RcBlock::new(move |_: std::ptr::NonNull<UIAlertAction>| {
            SHEET.with(|s| s.borrow_mut().take());
            pick(Some(index));
        });
        let action = UIAlertAction::actionWithTitle_style_handler(
            Some(&ns(name)),
            UIAlertActionStyle::Default,
            Some(&block),
            mtm,
        );
        sheet.addAction(&action);
    }
    let cancel = on_pick.clone();
    let block = block2::RcBlock::new(move |_: std::ptr::NonNull<UIAlertAction>| {
        SHEET.with(|s| s.borrow_mut().take());
        cancel(None);
    });
    let action = UIAlertAction::actionWithTitle_style_handler(
        Some(&ns("Cancel")),
        UIAlertActionStyle::Cancel,
        Some(&block),
        mtm,
    );
    sheet.addAction(&action);
    main_side.presentViewController_animated_completion(&sheet, true, None);
    SHEET.with(|s| *s.borrow_mut() = Some(sheet));
}

/// Find a characteristic of a handed-out device and act on it there.
fn with_characteristic<T: Send + 'static>(
    device: u64,
    service: &str,
    characteristic: &str,
    act: impl FnOnce(
            &mut Central,
            &Retained<AnyObject>,
            &Retained<AnyObject>,
            CharKey,
            Sender<Result<T, BtError>>,
        ) + Send
        + 'static,
) -> Result<T, BtError> {
    let key = (device, short_uuid(service), short_uuid(characteristic));
    on_main(EXCHANGE, move |c, _mtm, tx| {
        let Some(peripheral) = c.devices.get(&device).cloned() else {
            let _ = tx.send(Err(BtError::Disconnected));
            return;
        };
        if !CONNECTED
            .lock()
            .expect("bt lock")
            .as_ref()
            .is_some_and(|set| set.contains(&device))
        {
            let _ = tx.send(Err(BtError::Disconnected));
            return;
        }
        let Some(found) = c.characteristics.get(&key).cloned() else {
            let _ = tx.send(Err(BtError::NotFound));
            return;
        };
        act(c, &peripheral, &found, key, tx);
    })
    .unwrap_or(Err(BtError::Failed(
        "it did not answer in time".to_string(),
    )))
}

pub fn read(device: u64, service: &str, characteristic: &str) -> Result<Vec<u8>, BtError> {
    with_characteristic(
        device,
        service,
        characteristic,
        |c, peripheral, found, key, tx| {
            c.reading.insert(key, tx);
            // SAFETY: read a characteristic of this peripheral.
            let _: () = unsafe { msg_send![&**peripheral, readValueForCharacteristic: &**found] };
        },
    )
}

pub fn write(
    device: u64,
    service: &str,
    characteristic: &str,
    data: &[u8],
    with_response: bool,
) -> Result<(), BtError> {
    let bytes = data.to_vec();
    with_characteristic(
        device,
        service,
        characteristic,
        move |c, peripheral, found, key, tx| {
            let Some(class) = AnyClass::get(c"NSData") else {
                let _ = tx.send(Err(BtError::Unavailable));
                return;
            };
            // SAFETY: +[NSData dataWithBytes:length:] copies the bytes; then
            // writeValue:forCharacteristic:type: (0 with a response, 1 without).
            unsafe {
                let value: Option<Retained<AnyObject>> = msg_send![class, dataWithBytes: bytes.as_ptr() as *const std::ffi::c_void, length: bytes.len()];
                let Some(value) = value else {
                    let _ = tx.send(Err(BtError::Failed("could not hold the value".to_string())));
                    return;
                };
                let kind: isize = if with_response { 0 } else { 1 };
                if with_response {
                    c.writing.insert(key, tx);
                } else {
                    let _ = tx.send(Ok(()));
                }
                let _: () = msg_send![&**peripheral, writeValue: &*value, forCharacteristic: &**found, type: kind];
            }
        },
    )
}

pub fn subscribe(device: u64, service: &str, characteristic: &str) -> Result<(), BtError> {
    with_characteristic(
        device,
        service,
        characteristic,
        |c, peripheral, found, key, tx| {
            c.subscribing.insert(key, tx);
            // SAFETY: ask to be told of this characteristic's changes.
            let _: () = unsafe {
                msg_send![&**peripheral, setNotifyValue: true, forCharacteristic: &**found]
            };
        },
    )
}

pub fn notifications(device: u64) -> Vec<BtNotification> {
    NOTIFIED
        .lock()
        .expect("bt lock")
        .as_mut()
        .and_then(|all| all.get_mut(&device).map(std::mem::take))
        .unwrap_or_default()
}

pub fn connected(device: u64) -> bool {
    CONNECTED
        .lock()
        .expect("bt lock")
        .as_ref()
        .is_some_and(|set| set.contains(&device))
}

pub fn disconnect(device: u64) {
    if let Some(set) = CONNECTED.lock().expect("bt lock").as_mut() {
        set.remove(&device);
    }
    DispatchQueue::main().exec_async(move || {
        with_central(|c| {
            if let (Some(manager), Some(peripheral)) = (c.manager.as_ref(), c.devices.get(&device))
            {
                // SAFETY: let go of a peripheral this manager connected.
                let _: () =
                    unsafe { msg_send![&**manager, cancelPeripheralConnection: &**peripheral] };
            }
        });
    });
}

/// The app closed: let every device go, so nothing stays connected after it.
pub fn disconnect_all() {
    let handles: Vec<u64> = CONNECTED
        .lock()
        .expect("bt lock")
        .as_ref()
        .map(|set| set.iter().copied().collect())
        .unwrap_or_default();
    for handle in handles {
        disconnect(handle);
    }
    NOTIFIED.lock().expect("bt lock").take();
}

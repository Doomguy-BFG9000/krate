//! A Bluetooth LE device the person picks (krate:ui/bluetooth, IC-917):
//! BluetoothLeScanner and BluetoothGatt through the player activity's
//! Ble.java, with the iOS host's meaning -- Krate's own list, filtered by
//! the advertised service, connect to the pick, exchanges that time out.
//! The runtime has already checked `bluetooth.connect`.

use krate_adapter_common::ui::{BtDevice, BtError, BtNotification};

/// One Ble.java answer: a status byte, then data.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn status_of(answer: &[u8]) -> Result<&[u8], Option<BtError>> {
    let Some((&status, rest)) = answer.split_first() else {
        return Err(Some(BtError::Failed("no answer".to_string())));
    };
    match status {
        0 => Ok(rest),
        1 => Err(Some(BtError::NotAllowed)),
        2 => Err(Some(BtError::Unavailable)),
        3 => Err(Some(BtError::NotFound)),
        4 => Err(Some(BtError::Disconnected)),
        6 => Err(None),
        _ => Err(Some(BtError::Failed(
            String::from_utf8_lossy(rest).into_owned(),
        ))),
    }
}

/// The device a finished choose answered: its handle (8 bytes, big end
/// first) and its name; None when the person cancelled.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn chosen_from(answer: &[u8]) -> Result<Option<BtDevice>, BtError> {
    match status_of(answer) {
        Ok(rest) if rest.len() >= 8 => {
            let (handle, name) = rest.split_at(8);
            Ok(Some(BtDevice {
                handle: u64::from_be_bytes(handle.try_into().expect("eight bytes")),
                name: String::from_utf8_lossy(name).into_owned(),
            }))
        }
        Ok(_) => Err(BtError::Failed("no device in the answer".to_string())),
        Err(None) => Ok(None),
        Err(Some(error)) => Err(error),
    }
}

/// One notification as Ble.java packs it: the service's length and text,
/// the characteristic's length and text, then the value.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn notification_from(entry: &[u8]) -> Option<BtNotification> {
    let (&service_len, rest) = entry.split_first()?;
    let service = rest.get(..service_len as usize)?;
    let rest = &rest[service_len as usize..];
    let (&char_len, rest) = rest.split_first()?;
    let characteristic = rest.get(..char_len as usize)?;
    let data = rest[char_len as usize..].to_vec();
    Some((
        String::from_utf8_lossy(service).into_owned(),
        String::from_utf8_lossy(characteristic).into_owned(),
        data,
    ))
}

#[cfg(target_os = "android")]
mod real {
    use super::*;
    use crate::java::{wait_for, with_activity};
    use jni::objects::{JByteArray, JObject, JObjectArray, JValue};
    use jni::JNIEnv;
    use std::time::Duration;

    /// The phone's own prompt, the list, and the connect, at most.
    const CHOOSE_LIMIT: Duration = Duration::from_secs(10 * 60);
    /// One exchange; Ble.java gives up at 15 s itself.
    const EXCHANGE_LIMIT: Duration = Duration::from_secs(20);

    fn bytes(env: &mut JNIEnv, object: JObject) -> jni::errors::Result<Option<Vec<u8>>> {
        if object.is_null() {
            return Ok(None);
        }
        let array = JByteArray::from(object);
        Ok(Some(env.convert_byte_array(&array)?))
    }

    pub fn available() -> bool {
        with_activity("asking for Bluetooth", |env, activity| {
            env.call_method(activity, "krateBtAvailable", "()Z", &[])?
                .z()
        })
        .unwrap_or(false)
    }

    pub fn choose(service: &str, title: &str) -> Result<Option<BtDevice>, BtError> {
        // Start: while the phone's own prompt is up, ask again; then the
        // list is up (no answer yet), or it could not start (an answer).
        let started = wait_for(CHOOSE_LIMIT, || {
            let answer = with_activity("starting the Bluetooth list", |env, activity| {
                let service = env.new_string(service)?;
                let title = env.new_string(title)?;
                let made = env
                    .call_method(
                        activity,
                        "krateBtChoose",
                        "(Ljava/lang/String;Ljava/lang/String;)[B",
                        &[JValue::Object(&service), JValue::Object(&title)],
                    )?
                    .l()?;
                bytes(env, made)
            });
            match answer {
                None => Some(Some(vec![5])),
                Some(Some(asking)) if asking == [0xff] => None,
                Some(other) => Some(other),
            }
        })
        .ok_or(BtError::NotAllowed)?;
        if let Some(answer) = started {
            return chosen_from(&answer);
        }
        let answer = wait_for(CHOOSE_LIMIT, || {
            with_activity("reading the Bluetooth list", |env, activity| {
                let answer = env
                    .call_method(activity, "krateBtChooseAnswer", "()[B", &[])?
                    .l()?;
                bytes(env, answer)
            })
            .unwrap_or(Some(vec![6]))
        })
        .unwrap_or_else(|| vec![6]);
        chosen_from(&answer)
    }

    /// Start an exchange (0 read, 1 write, 2 subscribe) and wait for it.
    fn exchange(
        device: u64,
        kind: i32,
        service: &str,
        characteristic: &str,
        data: &[u8],
        with_response: bool,
    ) -> Result<Vec<u8>, BtError> {
        with_activity("starting a Bluetooth exchange", |env, activity| {
            let service = env.new_string(service)?;
            let characteristic = env.new_string(characteristic)?;
            let data = env.byte_array_from_slice(data)?;
            env.call_method(
                activity,
                "krateBtOp",
                "(JILjava/lang/String;Ljava/lang/String;[BZ)V",
                &[
                    JValue::Long(device as i64),
                    JValue::Int(kind),
                    JValue::Object(&service),
                    JValue::Object(&characteristic),
                    JValue::Object(&data),
                    JValue::Bool(u8::from(with_response)),
                ],
            )?
            .v()
        })
        .ok_or(BtError::Unavailable)?;
        let answer = wait_for(EXCHANGE_LIMIT, || {
            with_activity("reading a Bluetooth exchange", |env, activity| {
                let answer = env
                    .call_method(activity, "krateBtOpAnswer", "()[B", &[])?
                    .l()?;
                bytes(env, answer)
            })
            .unwrap_or(Some(vec![2]))
        })
        .ok_or_else(|| BtError::Failed("it did not answer in time".to_string()))?;
        match status_of(&answer) {
            Ok(rest) => Ok(rest.to_vec()),
            Err(error) => Err(error.unwrap_or(BtError::Disconnected)),
        }
    }

    pub fn read(device: u64, service: &str, characteristic: &str) -> Result<Vec<u8>, BtError> {
        exchange(device, 0, service, characteristic, &[], true)
    }

    pub fn write(
        device: u64,
        service: &str,
        characteristic: &str,
        data: &[u8],
        with_response: bool,
    ) -> Result<(), BtError> {
        exchange(device, 1, service, characteristic, data, with_response).map(|_| ())
    }

    pub fn subscribe(device: u64, service: &str, characteristic: &str) -> Result<(), BtError> {
        exchange(device, 2, service, characteristic, &[], true).map(|_| ())
    }

    pub fn notifications(device: u64) -> Vec<BtNotification> {
        with_activity("collecting Bluetooth values", |env, activity| {
            let all = env
                .call_method(
                    activity,
                    "krateBtNotifications",
                    "(J)[[B",
                    &[JValue::Long(device as i64)],
                )?
                .l()?;
            let all = JObjectArray::from(all);
            let n = env.get_array_length(&all)?;
            let mut out = Vec::with_capacity(n as usize);
            for i in 0..n {
                let entry = env.get_object_array_element(&all, i)?;
                if let Some(entry) = bytes(env, entry)? {
                    out.extend(notification_from(&entry));
                }
            }
            Ok(out)
        })
        .unwrap_or_default()
    }

    pub fn connected(device: u64) -> bool {
        with_activity("asking if a device is connected", |env, activity| {
            env.call_method(
                activity,
                "krateBtConnected",
                "(J)Z",
                &[JValue::Long(device as i64)],
            )?
            .z()
        })
        .unwrap_or(false)
    }

    pub fn disconnect(device: u64) {
        let _ = with_activity("letting a device go", |env, activity| {
            env.call_method(
                activity,
                "krateBtDisconnect",
                "(J)V",
                &[JValue::Long(device as i64)],
            )?
            .v()
        });
    }

    /// The app closed: every device it was given goes.
    pub fn disconnect_all() {
        let _ = with_activity("letting every device go", |env, activity| {
            env.call_method(activity, "krateBtDisconnectAll", "()V", &[])?
                .v()
        });
    }
}

#[cfg(target_os = "android")]
pub use real::*;

#[cfg(not(target_os = "android"))]
mod stub {
    pub fn disconnect_all() {}
}

#[cfg(not(target_os = "android"))]
pub use stub::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pick_carries_its_handle_and_name_and_a_cancel_is_none() {
        let mut answer = vec![0];
        answer.extend_from_slice(&7u64.to_be_bytes());
        answer.extend_from_slice("Krate Test Heart".as_bytes());
        assert_eq!(
            chosen_from(&answer),
            Ok(Some(BtDevice {
                handle: 7,
                name: "Krate Test Heart".to_string()
            }))
        );
        assert_eq!(chosen_from(&[6]), Ok(None));
        assert_eq!(chosen_from(&[1]), Err(BtError::NotAllowed));
        assert_eq!(
            chosen_from(b"\x05it did not answer"),
            Err(BtError::Failed("it did not answer".to_string()))
        );
        assert!(
            chosen_from(&[0, 1, 2]).is_err(),
            "a short handle is no pick"
        );
    }

    #[test]
    fn a_notification_unpacks_to_service_characteristic_value() {
        let entry = b"\x04180D\x042A37\x00\x48";
        assert_eq!(
            notification_from(entry),
            Some(("180D".to_string(), "2A37".to_string(), vec![0, 72]))
        );
        assert_eq!(
            notification_from(b"\x09180D"),
            None,
            "a cut entry is dropped"
        );
    }
}

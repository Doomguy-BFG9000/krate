//! Krate's calls into the player's own activity (KrateActivity, Java).
//!
//! Some things on Android are only reachable from Java: the answer of the
//! system's file picker, a permission prompt's answer, a sensor or Bluetooth
//! callback. The player's activity carries a few small methods for them, and
//! this is how the adapter calls them from Krate's native thread. None of
//! those methods waits on the UI thread; where an answer takes the person's
//! time, the adapter asks again while it keeps the event loop running
//! ([`wait_for`]), because Android stops an activity whose native thread
//! stops answering its lifecycle.

use jni::objects::{JObject, JString};
use jni::JNIEnv;
use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::time::{Duration, Instant};

static VM: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static ACTIVITY: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Keep the VM and the activity android-activity hands `android_main`; both
/// live as long as the process.
pub(crate) fn set_handles(vm: *mut c_void, activity: *mut c_void) {
    VM.store(vm, Ordering::Release);
    ACTIVITY.store(activity, Ordering::Release);
}

/// Run `f` with a JNI environment and the activity, in a local frame so the
/// references it makes are let go of. A Java exception is written to logcat
/// and cleared, and answers None.
pub(crate) fn with_activity<R>(
    what: &str,
    f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<R>,
) -> Option<R> {
    let vm = VM.load(Ordering::Acquire);
    let activity = ACTIVITY.load(Ordering::Acquire);
    if vm.is_null() || activity.is_null() {
        return None;
    }
    // SAFETY: the JavaVM pointer android-activity gave android_main.
    let vm = unsafe { jni::JavaVM::from_raw(vm.cast()) }.ok()?;
    // Krate's thread calls in many times a second (motion at sixty): attach
    // once for good rather than attach and detach every call.
    let mut env = vm.attach_current_thread_permanently().ok()?;
    // SAFETY: the activity's global reference, held by android-activity for
    // the life of the process; JObject does not release it on drop.
    let activity = unsafe { JObject::from_raw(activity.cast()) };
    call(&mut env, &activity, what, f)
}

/// Run `f` on a thread of its own while this one keeps the event loop
/// running: for a call that takes a while (copying a picked folder in,
/// writing a save out), during which Android must still see the app alive
/// -- it holds the activity's resume until Krate's thread answers.
pub(crate) fn off_thread<R: Send + 'static>(
    what: &'static str,
    f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<R> + Send + 'static,
) -> Option<R> {
    let vm = VM.load(Ordering::Acquire) as usize;
    let activity = ACTIVITY.load(Ordering::Acquire) as usize;
    if vm == 0 || activity == 0 {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let answer = (|| {
            // SAFETY: as in with_activity; this thread attaches for the call
            // and detaches when the guard drops.
            let vm = unsafe { jni::JavaVM::from_raw(vm as *mut _) }.ok()?;
            let mut env = vm.attach_current_thread().ok()?;
            let activity = unsafe { JObject::from_raw(activity as *mut _) };
            call(&mut env, &activity, what, f)
        })();
        let _ = tx.send(answer);
    });
    wait_for(Duration::from_secs(60 * 60), || match rx.try_recv() {
        Ok(answer) => Some(answer),
        Err(std::sync::mpsc::TryRecvError::Empty) => None,
        Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(None),
    })
    .flatten()
}

fn call<R>(
    env: &mut JNIEnv,
    activity: &JObject,
    what: &str,
    f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<R>,
) -> Option<R> {
    let result = env.with_local_frame(32, |env| f(env, activity));
    match result {
        Ok(value) => Some(value),
        Err(err) => {
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_describe();
                let _ = env.exception_clear();
            }
            log::warn!("{what}: {err}");
            None
        }
    }
}

/// A Java string as a Rust one; None for null.
pub(crate) fn string(env: &mut JNIEnv, object: JObject) -> jni::errors::Result<Option<String>> {
    if object.is_null() {
        return Ok(None);
    }
    let text = JString::from(object);
    let owned: String = env.get_string(&text)?.into();
    Ok(Some(owned))
}

/// Ask `poll` until it answers, keeping the event loop running between asks
/// so Android sees the app alive while the person answers a sheet. None if
/// `limit` passes first.
pub(crate) fn wait_for<T>(limit: Duration, mut poll: impl FnMut() -> Option<T>) -> Option<T> {
    let started = Instant::now();
    loop {
        if let Some(answer) = poll() {
            return Some(answer);
        }
        if started.elapsed() > limit {
            return None;
        }
        crate::winit_native::keep_alive();
        std::thread::sleep(Duration::from_millis(16));
    }
}

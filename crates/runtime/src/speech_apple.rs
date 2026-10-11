//! Speech to text on a Mac (IC-918): Apple's SFSpeechRecognizer, the same
//! recognizer the iPhone uses (crates/adapter-ios `transcribe`), over the
//! app's mono 16-bit PCM, on the device when it can, asking the person
//! once. Used when the app carries no whisper model of its own.

use objc2::msg_send;
use objc2::rc::Retained;

#[link(name = "Speech", kind = "framework")]
unsafe extern "C" {}

#[link(name = "AVFAudio", kind = "framework")]
unsafe extern "C" {}

/// macOS kills a process that asks for speech recognition without this
/// Info.plist key, with no error the app can catch. Say so instead.
fn usage_declared() -> Result<(), String> {
    let key = objc2_foundation::NSString::from_str("NSSpeechRecognitionUsageDescription");
    // SAFETY: the main bundle's info dictionary lookup; nil when absent.
    let value: Option<Retained<objc2::runtime::AnyObject>> = unsafe {
        let bundle: Retained<objc2::runtime::AnyObject> =
            msg_send![objc2::class!(NSBundle), mainBundle];
        msg_send![&*bundle, objectForInfoDictionaryKey: &*key]
    };
    value.map(|_| ()).ok_or_else(|| {
        "this Krate is not an app bundle that declares speech recognition, which macOS \
         requires: open the app from Krate.app"
            .to_string()
    })
}

pub(crate) fn transcribe(pcm: &[u8], rate: u32, language: Option<&str>) -> Result<String, String> {
    usage_declared()?;
    let get = |name: &std::ffi::CStr| {
        objc2::runtime::AnyClass::get(name).ok_or_else(|| format!("no {}", name.to_string_lossy()))
    };
    let recognizer_class = get(c"SFSpeechRecognizer")?;
    // Asked once; the answer is waited for.
    let (tx, rx) = std::sync::mpsc::channel::<isize>();
    let asked = block2::RcBlock::new(move |status: isize| {
        let _ = tx.send(status);
    });
    // SAFETY: +[SFSpeechRecognizer requestAuthorization:] with a block
    // that reports the status.
    unsafe {
        let _: () = msg_send![recognizer_class, requestAuthorization: &*asked];
    }
    match rx.recv_timeout(std::time::Duration::from_secs(120)) {
        // Authorized.
        Ok(3) => {}
        Ok(_) => {
            return Err("speech recognition is not allowed for Krate: Settings > Krate".to_string())
        }
        Err(_) => return Err("the phone did not answer".to_string()),
    }
    let samples: Vec<i16> = pcm
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    let (format_class, buffer_class, request_class, locale_class) = (
        get(c"AVAudioFormat")?,
        get(c"AVAudioPCMBuffer")?,
        get(c"SFSpeechAudioBufferRecognitionRequest")?,
        get(c"NSLocale")?,
    );
    let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
    // SAFETY: a recognizer for the language, one PCM buffer of the
    // app's samples (int16, mono, interleaved), a buffer request ended
    // at once, and a task whose handler answers on the final result.
    unsafe {
        let recognizer: Option<Retained<objc2::runtime::AnyObject>> = match language {
            Some(tag) => {
                let locale: Retained<objc2::runtime::AnyObject> = msg_send![locale_class, localeWithLocaleIdentifier: &*objc2_foundation::NSString::from_str(tag)];
                let made: objc2::rc::Allocated<objc2::runtime::AnyObject> =
                    msg_send![recognizer_class, alloc];
                msg_send![made, initWithLocale: &*locale]
            }
            None => msg_send![recognizer_class, new],
        };
        let recognizer = recognizer.ok_or("no recognizer for that language")?;
        let available: bool = msg_send![&*recognizer, isAvailable];
        if !available {
            return Err("speech recognition is not available right now".to_string());
        }
        let made: objc2::rc::Allocated<objc2::runtime::AnyObject> = msg_send![format_class, alloc];
        // AVAudioPCMFormatInt16 = 3.
        let format: Option<Retained<objc2::runtime::AnyObject>> = msg_send![made, initWithCommonFormat: 3usize, sampleRate: rate as f64, channels: 1u32, interleaved: true];
        let format = format.ok_or("that sample rate cannot be read")?;
        let made: objc2::rc::Allocated<objc2::runtime::AnyObject> = msg_send![buffer_class, alloc];
        let buffer: Option<Retained<objc2::runtime::AnyObject>> =
            msg_send![made, initWithPCMFormat: &*format, frameCapacity: samples.len() as u32];
        let buffer = buffer.ok_or("the audio would not fit")?;
        let channels: *const *mut i16 = msg_send![&*buffer, int16ChannelData];
        if channels.is_null() || (*channels).is_null() {
            return Err("the audio buffer has no room".to_string());
        }
        std::ptr::copy_nonoverlapping(samples.as_ptr(), *channels, samples.len());
        let _: () = msg_send![&*buffer, setFrameLength: samples.len() as u32];
        let request: Retained<objc2::runtime::AnyObject> = msg_send![request_class, new];
        let _: () = msg_send![&*request, setShouldReportPartialResults: false];
        let on_device: bool = msg_send![&*recognizer, supportsOnDeviceRecognition];
        if on_device {
            let _: () = msg_send![&*request, setRequiresOnDeviceRecognition: true];
        }
        let _: () = msg_send![&*request, appendAudioPCMBuffer: &*buffer];
        let _: () = msg_send![&*request, endAudio];
        let tx = std::sync::Mutex::new(Some(tx));
        let heard = block2::RcBlock::new(
            move |result: *mut objc2::runtime::AnyObject, error: *mut objc2::runtime::AnyObject| {
                let answer = if !result.is_null() {
                    let done: bool = msg_send![&*result, isFinal];
                    if !done {
                        return;
                    }
                    let best: Retained<objc2::runtime::AnyObject> =
                        msg_send![&*result, bestTranscription];
                    let text: Retained<objc2_foundation::NSString> =
                        msg_send![&*best, formattedString];
                    Ok(text.to_string())
                } else if !error.is_null() {
                    let why: Option<Retained<objc2_foundation::NSString>> =
                        msg_send![&*error, localizedDescription];
                    let why = why.map(|w| w.to_string()).unwrap_or_default();
                    // No speech in the audio is an empty transcript, not a fault.
                    if why.to_ascii_lowercase().contains("no speech") {
                        Ok(String::new())
                    } else {
                        Err(why)
                    }
                } else {
                    return;
                };
                if let Some(tx) = tx.lock().expect("speech lock").take() {
                    let _ = tx.send(answer);
                }
            },
        );
        let task: Option<Retained<objc2::runtime::AnyObject>> =
            msg_send![&*recognizer, recognitionTaskWithRequest: &*request, resultHandler: &*heard];
        let _keep = (task, recognizer);
        rx.recv_timeout(std::time::Duration::from_secs(60))
            .unwrap_or_else(|_| Err("speech recognition took too long".to_string()))
    }
}

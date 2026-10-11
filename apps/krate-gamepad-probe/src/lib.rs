//! gilrs' `ev` example (print every event) inside a Krate app, through
//! gilrs-core's Krate backend over krate:ui's gamepad calls (IC-936).
//!
//! It opens a window (a controller belongs to the app in front) and, every
//! frame, prints whatever gilrs reports: a pad connecting, buttons, sticks.
//! It ends after `seconds` (argument, default 8).
//!
//! ```text
//! python3 crates/gilrs-krate/make-gilrs.py ~/Projects/krate-bevy/patches
//! ~/Projects/krate-bevy/build-app.sh $PWD/apps/krate-gamepad-probe
//! ```
//!
//! The result is crates/cli/tests/fixtures/gamepad-probe.wasm.

use std::time::{Duration, Instant};

use gilrs::{Event, Gilrs};
use krate::bindings::krate::ui::{events, types, window};

struct App;

impl krate::Guest for App {
    fn run() -> i32 {
        let seconds: u64 = std::env::args()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(8);
        let win = window::create(
            "Gamepad probe",
            types::WindowSize {
                width: 320,
                height: 200,
            },
        )
        .expect("a window");
        let mut gilrs = Gilrs::new().expect("gilrs");
        println!("gilrs: started; {} pad(s) known", gilrs.gamepads().count());
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(seconds) {
            while let Some(e) = events::wait(Some(16)) {
                if let types::Event::CloseRequested(_) = e {
                    krate_c_runtime::finish();
                    return 0;
                }
            }
            while let Some(Event { id, event, .. }) = gilrs.next_event() {
                println!(
                    "gilrs: {:?} New event from {}: {:?}",
                    Instant::now() - start,
                    id,
                    event
                );
            }
        }
        let _ = win;
        krate_c_runtime::finish();
        0
    }
}

krate::export!(App);

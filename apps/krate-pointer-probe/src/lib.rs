// The smallest app that asks about the mouse (krate:ui/pointer, IC-909), so
// the interface is proved through the binary a person runs.
//
//   krate run pointer-probe.krate              one report, then exit
//   krate run pointer-probe.krate -- watch     ten seconds: click to hold the
//                                              pointer, move the mouse, press
//                                              Escape; one line per second
//
// A report line: where the pointer is, which buttons are down, the motion
// since the last report, whether it is held, and what `capture` answered.
#![no_std]
extern crate alloc;

use alloc::format;
use alloc::string::String;
use krate::{
    io::{
        args, stdio,
        streams::OutputStream,
    },
    time::clock,
    ui::{
        events,
        pointer::{self, PointerButton},
        tree,
        types::{self, Event},
        window,
    },
    Guest,
};
use krate::gfx::{canvas2d, canvas_images, types as gfx};

/// Keep a picture for a canvas, draw it both ways, and check the refusals:
/// a picture shorter than its size, an unknown canvas, a forgotten handle
/// (krate:gfx/canvas-images).
fn kept_pictures(win: u64) -> String {
    let node = |id, parent, kind| types::WidgetNode {
        id,
        parent,
        kind,
        label: None,
        role: None,
        style: types::Style {
            width: None,
            height: None,
            grow: 1.0,
            padding: 0.0,
            text: None,
            place: None,
            box_: None,
        },
        checked: None,
        value: None,
        selected: None,
        text_cursor: None,
    };
    let _ = tree::set_root(win, &node(1, None, types::WidgetKind::Stack));
    let _ = tree::upsert_node(win, &node(2, Some(1), types::WidgetKind::Canvas));
    let Ok(canvas) = canvas2d::bind(win, 2) else {
        return String::from("kept=no-canvas");
    };
    let area = gfx::Rect { x: 0.0, y: 0.0, width: 20.0, height: 20.0 };
    let radii = gfx::CornerRadii { top_left: 4.0, top_right: 4.0, bottom_right: 4.0, bottom_left: 4.0 };
    let word = |ok: bool| if ok { "ok" } else { "refused" };
    let kept = canvas_images::keep(canvas, 2, 2, &[255; 16]);
    let short = canvas_images::keep(canvas, 2, 2, &[255; 15]).is_err();
    let unknown = canvas_images::keep(canvas + 999, 2, 2, &[255; 16]).is_err();
    let Ok(handle) = kept else {
        return String::from("kept=refused");
    };
    let draw = canvas_images::draw(canvas, handle, area).is_ok();
    let round = canvas_images::draw_round(canvas, handle, area, radii).is_ok();
    canvas_images::forget(canvas, handle);
    let forgotten = canvas_images::draw(canvas, handle, area).is_err();
    format!(
        "kept=ok draw={} round={} short={} unknown={} forgotten={}",
        word(draw),
        word(round),
        word(!short),
        word(!unknown),
        word(!forgotten),
    )
}

struct Component;

fn report(win: u64, capture: &str) -> String {
    let at = match pointer::position(win) {
        Some((x, y)) => format!("{x:.0},{y:.0}"),
        None => String::from("none"),
    };
    let held = |b| if pointer::button_held(b) { "down" } else { "up" };
    let (dx, dy) = pointer::take_motion(win);
    format!(
        "position={at} primary={} secondary={} middle={} motion={dx:.0},{dy:.0} captured={} touches={} keyboard={} capture={capture}",
        held(PointerButton::Primary),
        held(PointerButton::Secondary),
        held(PointerButton::Middle),
        pointer::captured(win),
        krate::ui::touch::touches(win).len(),
        {
            // A host with no on-screen keyboard says so; stop is always safe.
            let shown = krate::ui::text_entry::start(win);
            krate::ui::text_entry::stop(win);
            shown
        },
    )
}

fn answer(result: Result<(), types::UiError>) -> String {
    match result {
        Ok(()) => String::from("ok"),
        Err(types::UiError::Unsupported(why)) => format!("refused: {why}"),
        Err(other) => format!("error: {other:?}"),
    }
}

impl Guest for Component {
    fn run() -> i32 {
        let out = stdio::stdout();
        let Ok(win) = window::create(
            "Pointer probe",
            types::WindowSize {
                width: 480,
                height: 320,
            },
        ) else {
            let _ = write_line(&out, "window=refused");
            return 1;
        };
        let watch = args::raw().split('\n').any(|w| w == "watch");
        if !watch {
            // A window is not focused the instant it is made; give the system
            // a moment, as a game would before its first click.
            let start = clock::monotonic_nanos();
            let mut focused = false;
            while !focused && clock::monotonic_nanos() - start < 1_500_000_000 {
                if let Some(Event::FocusChanged(_)) | Some(Event::Resized(_)) = events::wait(Some(50)) {}
                focused = pointer::capture(win).is_ok();
            }
            let capture = if focused {
                String::from("ok")
            } else {
                answer(pointer::capture(win))
            };
            let _ = write_line(&out, &report(win, &capture));
            pointer::release(win);
            let _ = write_line(&out, &format!("released captured={}", pointer::captured(win)));
            let _ = write_line(&out, &kept_pictures(win));
            let _ = window::close(win);
            return 0;
        }
        let start = clock::monotonic_nanos();
        let mut last = start;
        let mut capture = String::from("-");
        while clock::monotonic_nanos() - start < 10_000_000_000 {
            match events::wait(Some(16)) {
                Some(Event::CloseRequested(_)) => break,
                Some(Event::Pointer(p)) if p.pressed => capture = answer(pointer::capture(win)),
                _ => {}
            }
            if clock::monotonic_nanos() - last >= 1_000_000_000 {
                last = clock::monotonic_nanos();
                let _ = write_line(&out, &report(win, &capture));
            }
        }
        let _ = window::close(win);
        0
    }
}

fn write_line(out: &OutputStream, line: &str) -> bool {
    out.write(format!("{line}\n").as_bytes()).is_ok()
}

krate::export!(Component);

// Where Am I: the phone's position from krate:ui/location (IC-917).
//
//   krate run location-probe.krate -- quick   one line, then exit:
//                                             `location:<lat>,<lon> within <m> m`
//                                             or `location:<why there is none>`
#![no_std]
extern crate alloc;

use alloc::format;
use alloc::string::String;
use krate::gfx::{canvas2d, types as gfx};
use krate::layout::Screen;
use krate::ui::location::{self, LocationError, Position};
use krate::{
    io::{args, stdio},
    ui::{
        events, tree,
        types::{self, Event},
        window,
    },
    Guest,
};

fn rgb(r: f32, g: f32, b: f32) -> gfx::Color {
    gfx::Color { r, g, b, a: 1.0 }
}

fn style(weight: u16) -> gfx::TextStyle {
    gfx::TextStyle { weight, italic: false, letter_spacing: 0.0, family: gfx::FontFamily::Sans }
}

fn say(line: &str) {
    let out = stdio::stdout();
    let _ = out.write(line.as_bytes());
    let _ = out.write(b"\n");
}

fn node(id: u64, parent: Option<u64>, kind: types::WidgetKind) -> types::WidgetNode {
    types::WidgetNode {
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
    }
}

/// Degrees as the phone's own apps write them: 1.29027 N, 103.85196 E.
fn coordinate(value: f64, positive: &str, negative: &str) -> String {
    let side = if value < 0.0 { negative } else { positive };
    format!("{:.5}\u{00b0} {side}", if value < 0.0 { -value } else { value })
}

fn why(error: LocationError) -> &'static str {
    match error {
        LocationError::NotAllowed => "Not allowed. Krate asks before an app can see where you are.",
        LocationError::Unavailable => "This device cannot tell where it is, or location is off.",
        LocationError::Finding => "Finding where you are\u{2026}",
    }
}

fn draw(canvas: u64, screen: &Screen, answer: &Result<Position, LocationError>) -> Result<(), gfx::GfxError> {
    let ink = rgb(0.07, 0.07, 0.09);
    let muted = rgb(0.43, 0.44, 0.48);
    let blue = rgb(0.0, 0.48, 1.0);
    canvas2d::clear(canvas, rgb(0.96, 0.96, 0.97))?;
    let area = screen.safe_area().inset(16.0, 20.0, 16.0, 20.0);
    let x = area.x;
    canvas2d::draw_text_styled(canvas, "Where Am I", gfx::Point { x, y: area.y + 34.0 }, 32.0, ink, style(800))?;
    let (cx, cy) = (area.x + area.w / 2.0, area.y + area.h * 0.36);
    match answer {
        Ok(p) => {
            // The dot and its circle of doubt, as a map draws them.
            let doubt = (p.accuracy / 2.0).clamp(18.0, area.w * 0.4);
            canvas2d::fill_circle(canvas, gfx::Point { x: cx, y: cy }, doubt, gfx::Color { r: 0.0, g: 0.48, b: 1.0, a: 0.14 })?;
            canvas2d::fill_circle(canvas, gfx::Point { x: cx, y: cy }, 13.0, rgb(1.0, 1.0, 1.0))?;
            canvas2d::fill_circle(canvas, gfx::Point { x: cx, y: cy }, 9.0, blue)?;
            let mut y = cy + doubt.max(40.0) + 64.0;
            for (text, size, colour, weight) in [
                (coordinate(p.latitude, "N", "S"), 30.0, ink, 700),
                (coordinate(p.longitude, "E", "W"), 30.0, ink, 700),
                (format!("within {:.0} m", p.accuracy), 17.0, muted, 500),
            ] {
                let m = canvas2d::measure_text_styled(canvas, &text, size, style(weight))?;
                canvas2d::draw_text_styled(canvas, &text, gfx::Point { x: cx - m.width / 2.0, y }, size, colour, style(weight))?;
                y += size + 12.0;
            }
            let mut extra = String::new();
            if let Some(alt) = p.altitude {
                extra.push_str(&format!("{alt:.0} m above sea level"));
            }
            if let Some(speed) = p.speed {
                if speed > 0.3 {
                    if !extra.is_empty() {
                        extra.push_str("  \u{00b7}  ");
                    }
                    extra.push_str(&format!("{:.1} km/h", speed * 3.6));
                }
            }
            if !extra.is_empty() {
                let m = canvas2d::measure_text_styled(canvas, &extra, 15.0, style(500))?;
                canvas2d::draw_text_styled(canvas, &extra, gfx::Point { x: cx - m.width / 2.0, y }, 15.0, muted, style(500))?;
            }
        }
        Err(error) => {
            let text = why(*error);
            canvas2d::draw_text_styled(canvas, text, gfx::Point { x, y: area.y + 70.0 }, 15.0, muted, style(500))?;
        }
    }
    canvas2d::present(canvas)
}

struct Component;

impl Guest for Component {
    fn run() -> i32 {
        if args::raw().split('\n').any(|a| a == "quick") {
            match location::current() {
                Ok(p) => say(&format!("location:{:.5},{:.5} within {:.0} m", p.latitude, p.longitude, p.accuracy)),
                Err(LocationError::NotAllowed) => say("location:not-allowed"),
                Err(LocationError::Unavailable) => say("location:unavailable"),
                Err(LocationError::Finding) => say("location:finding"),
            }
            return 0;
        }
        let Ok(win) = window::create("Where Am I", types::WindowSize { width: 420, height: 720 }) else {
            return 1;
        };
        if tree::set_root(win, &node(1, None, types::WidgetKind::Stack)).is_err()
            || tree::upsert_node(win, &node(2, Some(1), types::WidgetKind::Canvas)).is_err()
        {
            return 1;
        }
        let Ok(canvas) = canvas2d::bind(win, 2) else {
            return 1;
        };
        loop {
            let answer = location::current();
            if draw(canvas, &Screen::of(win), &answer).is_err() {
                return 1;
            }
            // Ask twice a second: a fix changes no faster than a person walks.
            if let Some(Event::CloseRequested(_)) = events::wait(Some(500)) {
                return 0;
            }
            while let Some(event) = events::poll() {
                if let Event::CloseRequested(_) = event {
                    return 0;
                }
            }
        }
    }
}

krate::export!(Component);

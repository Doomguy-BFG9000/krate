// Level: how the phone is held, from krate:ui/motion (IC-917).
//
// Lying flat it is a bubble level; held up it shows how far it leans; the
// compass heading of the screen's top is under both. Exactly level, it taps
// back once.
//
//   krate run motion-probe.krate -- quick   one line, then exit:
//                                           `motion:available=no` or
//                                           `motion:gravity=x,y,z heading=h`
#![no_std]
extern crate alloc;

use alloc::format;
use alloc::string::String;
use krate::gfx::{canvas2d, types as gfx};
use krate::layout::Screen;
use krate::ui::haptics::{self, Feel};
use krate::ui::motion;
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

/// Degrees from radians, without std's float helpers.
fn degrees(radians: f32) -> f32 {
    radians * 180.0 / core::f32::consts::PI
}

// A small atan2: no_std has no float trig. Accurate to about 0.3 degrees,
// plenty for a level.
fn atan2(y: f32, x: f32) -> f32 {
    use core::f32::consts::{FRAC_PI_2, PI};
    if x == 0.0 && y == 0.0 {
        return 0.0;
    }
    let (ax, ay) = (if x < 0.0 { -x } else { x }, if y < 0.0 { -y } else { y });
    let (a, swap) = if ax >= ay { (ay / ax, false) } else { (ax / ay, true) };
    let s = a * a;
    let mut r = ((-0.046_496_47 * s + 0.159_314_22) * s - 0.327_622_76) * s * a + a;
    if swap {
        r = FRAC_PI_2 - r;
    }
    if x < 0.0 {
        r = PI - r;
    }
    if y < 0.0 {
        -r
    } else {
        r
    }
}

fn sqrt(v: f32) -> f32 {
    if v <= 0.0 {
        return 0.0;
    }
    let mut x = v;
    for _ in 0..8 {
        x = 0.5 * (x + v / x);
    }
    x
}

fn draw(canvas: u64, screen: &Screen, reading: Option<&motion::Reading>, level: bool) -> Result<(), gfx::GfxError> {
    let ink = rgb(0.07, 0.07, 0.09);
    let muted = rgb(0.43, 0.44, 0.48);
    let green = rgb(0.20, 0.78, 0.35);
    canvas2d::clear(canvas, rgb(0.96, 0.96, 0.97))?;
    let area = screen.safe_area().inset(16.0, 20.0, 16.0, 20.0);
    canvas2d::draw_text_styled(canvas, "Level", gfx::Point { x: area.x, y: area.y + 34.0 }, 32.0, ink, style(800))?;
    let Some(m) = reading else {
        let line = if motion::available() { "Waiting for the sensors" } else { "This device cannot tell how it is held." };
        canvas2d::draw_text_styled(canvas, line, gfx::Point { x: area.x, y: area.y + 62.0 }, 15.0, muted, style(450))?;
        return canvas2d::present(canvas);
    };
    let g = m.gravity;
    let (cx, cy) = (area.x + area.w / 2.0, area.y + area.h * 0.48);
    let r = (area.w.min(area.h) * 0.36).min(160.0);
    let flat = g.z < -0.6 || g.z > 0.6;
    let tint = if level { green } else { ink };
    if flat {
        // A bubble level: the bubble floats to the high side, away from
        // where gravity pulls along the screen.
        canvas2d::fill_circle(canvas, gfx::Point { x: cx, y: cy }, r, rgb(1.0, 1.0, 1.0))?;
        canvas2d::stroke_circle(canvas, gfx::Point { x: cx, y: cy }, r, 2.0, rgb(0.85, 0.86, 0.89))?;
        canvas2d::stroke_circle(canvas, gfx::Point { x: cx, y: cy }, 30.0, 2.0, rgb(0.80, 0.81, 0.85))?;
        let reach = r - 26.0;
        let (mut bx, mut by) = (-g.x * reach * 3.0, -g.y * reach * 3.0);
        let d = sqrt(bx * bx + by * by);
        if d > reach {
            bx *= reach / d;
            by *= reach / d;
        }
        canvas2d::fill_circle(canvas, gfx::Point { x: cx + bx, y: cy + by }, 24.0, if level { green } else { rgb(0.30, 0.38, 0.96) })?;
        let tilt = degrees(atan2(sqrt(g.x * g.x + g.y * g.y), if g.z < 0.0 { -g.z } else { g.z }));
        let text = format!("{:.1}\u{00b0}", tilt);
        let tm = canvas2d::measure_text_styled(canvas, &text, 44.0, style(700))?;
        canvas2d::draw_text_styled(canvas, &text, gfx::Point { x: cx - tm.width / 2.0, y: cy + r + 62.0 }, 44.0, tint, style(700))?;
    } else {
        // Held up: how far the screen leans from straight up and down.
        let lean = degrees(atan2(g.x, g.y));
        let text = format!("{:.1}\u{00b0}", if lean < 0.0 { -lean } else { lean });
        let tm = canvas2d::measure_text_styled(canvas, &text, 72.0, style(700))?;
        canvas2d::draw_text_styled(canvas, &text, gfx::Point { x: cx - tm.width / 2.0, y: cy + 24.0 }, 72.0, tint, style(700))?;
        let side = if lean > 0.5 { "leaning right" } else if lean < -0.5 { "leaning left" } else { "straight" };
        let sm = canvas2d::measure_text_styled(canvas, side, 17.0, style(500))?;
        canvas2d::draw_text_styled(canvas, side, gfx::Point { x: cx - sm.width / 2.0, y: cy + 60.0 }, 17.0, muted, style(500))?;
    }
    let heading = match m.heading {
        Some(h) => {
            let names = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
            let name = names[(((h + 22.5) / 45.0) as usize) % 8];
            format!("Top of the screen faces {name}, {:.0}\u{00b0}", h)
        }
        None => String::from("No compass heading yet"),
    };
    canvas2d::draw_text_styled(canvas, &heading, gfx::Point { x: area.x, y: area.y + area.h - 8.0 }, 15.0, muted, style(500))?;
    canvas2d::present(canvas)
}

struct Component;

impl Guest for Component {
    fn run() -> i32 {
        if args::raw().split('\n').any(|a| a == "quick") {
            if !motion::available() {
                say("motion:available=no");
                return 0;
            }
            let reading = motion::read();
            match reading {
                Some(m) => say(&format!(
                    "motion:gravity={:.2},{:.2},{:.2} heading={}",
                    m.gravity.x,
                    m.gravity.y,
                    m.gravity.z,
                    m.heading.map(|h| format!("{h:.0}")).unwrap_or_else(|| String::from("none"))
                )),
                None => say("motion:waiting"),
            }
            return 0;
        }
        let Ok(win) = window::create("Level", types::WindowSize { width: 420, height: 720 }) else {
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
        let mut was_level = false;
        loop {
            while let Some(event) = events::poll() {
                if let Event::CloseRequested(_) = event {
                    return 0;
                }
            }
            let reading = motion::read();
            let level = reading.as_ref().is_some_and(|m| {
                let flat = m.gravity.z < -0.6 || m.gravity.z > 0.6;
                let off = if flat { sqrt(m.gravity.x * m.gravity.x + m.gravity.y * m.gravity.y) } else { m.gravity.x };
                (if off < 0.0 { -off } else { off }) < 0.008
            });
            if level && !was_level {
                haptics::play(Feel::Success);
            }
            was_level = level;
            if draw(canvas, &Screen::of(win), reading.as_ref(), level).is_err() {
                return 1;
            }
            // About thirty readings a second is plenty for a level.
            if let Some(Event::CloseRequested(_)) = events::wait(Some(33)) {
                return 0;
            }
        }
    }
}

krate::export!(Component);

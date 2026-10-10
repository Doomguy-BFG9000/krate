// Tap Back: every feel krate:ui/haptics offers, one button each, and an
// impact bar for a game's hits (IC-917).
//
//   krate run haptics-probe.krate -- quick   one line, then exit:
//                                            `haptics:available=<yes|no>`,
//                                            after playing every feel once
//   krate run haptics-probe.krate            the buttons; on a phone each
//                                            taps back, on a computer the
//                                            page says it cannot
#![no_std]
extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use krate::gfx::{canvas2d, types as gfx};
use krate::layout::{Rect, Screen};
use krate::ui::haptics::{self, Feel};
use krate::{
    io::{args, stdio},
    ui::{
        events, tree,
        types::{self, Event},
        window,
    },
    Guest,
};

const FEELS: [(Feel, &str, &str); 7] = [
    (Feel::Light, "Light", "A button pressed"),
    (Feel::Medium, "Medium", "A switch flipped"),
    (Feel::Heavy, "Heavy", "A landing"),
    (Feel::Selection, "Selection", "A picker's tick"),
    (Feel::Success, "Success", "Saved, solved"),
    (Feel::Warning, "Warning", "Needs a look"),
    (Feel::Error, "Error", "Something failed"),
];

fn rgb(r: f32, g: f32, b: f32) -> gfx::Color {
    gfx::Color { r, g, b, a: 1.0 }
}

fn rect(r: Rect) -> gfx::Rect {
    gfx::Rect { x: r.x, y: r.y, width: r.w, height: r.h }
}

fn radii(all: f32) -> gfx::CornerRadii {
    gfx::CornerRadii { top_left: all, top_right: all, bottom_right: all, bottom_left: all }
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

struct Page {
    buttons: Vec<Rect>,
    bar: Rect,
    /// Which control a finger is on, and how hard the last impact was.
    pressed: Option<usize>,
    strength: Option<f32>,
}

fn draw(canvas: u64, screen: &Screen, available: bool, page: &mut Page) -> Result<(), gfx::GfxError> {
    let ink = rgb(0.07, 0.07, 0.09);
    let muted = rgb(0.43, 0.44, 0.48);
    let accent = rgb(0.23, 0.30, 0.95);
    canvas2d::clear(canvas, rgb(0.96, 0.96, 0.97))?;
    let area = screen.safe_area().inset(16.0, 20.0, 16.0, 20.0);
    let col = area.w.min(560.0);
    let x = area.x + (area.w - col) / 2.0;
    let mut y = area.y + 34.0;
    canvas2d::draw_text_styled(canvas, "Tap Back", gfx::Point { x, y }, 32.0, ink, style(800))?;
    y += 26.0;
    let note = if available {
        "Each one taps back, as the phone's own apps do."
    } else {
        "No haptics here; on a phone each one taps."
    };
    canvas2d::draw_text_styled(canvas, note, gfx::Point { x, y }, 15.0, muted, style(450))?;
    y += 24.0;

    // Two columns of feels on a phone, more where there is room.
    let cols = if col >= 520.0 { 3 } else { 2 };
    let gap = 12.0;
    let bw = (col - gap * (cols as f32 - 1.0)) / cols as f32;
    let bh = 76.0;
    page.buttons.clear();
    for (i, (_, name, what)) in FEELS.iter().enumerate() {
        let (c, r) = (i % cols, i / cols);
        let b = Rect::new(x + c as f32 * (bw + gap), y + r as f32 * (bh + gap), bw, bh);
        page.buttons.push(b);
        let on = page.pressed == Some(i);
        canvas2d::drop_shadow_round_rect(canvas, rect(b), radii(18.0), 12.0, gfx::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.06 })?;
        canvas2d::fill_round_rect(canvas, rect(b), radii(18.0), if on { rgb(0.90, 0.91, 0.99) } else { rgb(1.0, 1.0, 1.0) })?;
        canvas2d::draw_text_styled(canvas, name, gfx::Point { x: b.x + 16.0, y: b.y + 32.0 }, 18.0, if on { accent } else { ink }, style(700))?;
        canvas2d::draw_text_styled(canvas, what, gfx::Point { x: b.x + 16.0, y: b.y + 55.0 }, 13.5, muted, style(450))?;
    }
    let rows = FEELS.len().div_ceil(cols);
    y += rows as f32 * (bh + gap) + 18.0;

    // The impact bar: where the finger lands is how hard the hit is.
    canvas2d::draw_text_styled(canvas, "Impact", gfx::Point { x, y }, 18.0, ink, style(700))?;
    let label = match page.strength {
        Some(s) => format!("{}%", (s * 100.0 + 0.5) as u32),
        None => String::from("Tap along the bar"),
    };
    let lm = canvas2d::measure_text_styled(canvas, &label, 14.0, style(500))?;
    canvas2d::draw_text_styled(canvas, &label, gfx::Point { x: x + col - lm.width, y }, 14.0, muted, style(500))?;
    y += 14.0;
    page.bar = Rect::new(x, y, col, 56.0);
    canvas2d::fill_round_rect(canvas, rect(page.bar), radii(16.0), rgb(1.0, 1.0, 1.0))?;
    canvas2d::linear_gradient_stops(
        canvas,
        rect(page.bar.inset(8.0, 8.0, 8.0, 8.0)),
        0.0,
        &[
            gfx::GradientStop { offset: 0.0, color: rgb(0.80, 0.83, 0.99) },
            gfx::GradientStop { offset: 1.0, color: accent },
        ],
    )?;
    if let Some(s) = page.strength {
        let inner = page.bar.inset(8.0, 8.0, 8.0, 8.0);
        let kx = inner.x + inner.w * s;
        canvas2d::fill_circle(canvas, gfx::Point { x: kx, y: inner.y + inner.h / 2.0 }, 18.0, rgb(1.0, 1.0, 1.0))?;
        canvas2d::stroke_circle(canvas, gfx::Point { x: kx, y: inner.y + inner.h / 2.0 }, 18.0, 2.0, accent)?;
    }
    canvas2d::present(canvas)
}

struct Component;

impl Guest for Component {
    fn run() -> i32 {
        let available = haptics::available();
        if args::raw().split('\n').any(|a| a == "quick") {
            // Every call answers on any device: a computer does nothing.
            for (feel, _, _) in FEELS {
                haptics::play(feel);
            }
            haptics::impact(0.5);
            haptics::impact(7.0);
            haptics::impact(f32::NAN);
            say(&format!("haptics:available={}", if available { "yes" } else { "no" }));
            return 0;
        }
        let Ok(win) = window::create("Tap Back", types::WindowSize { width: 420, height: 720 }) else {
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
        let mut page = Page { buttons: Vec::new(), bar: Rect::new(0.0, 0.0, 0.0, 0.0), pressed: None, strength: None };
        loop {
            let screen = Screen::of(win);
            if draw(canvas, &screen, available, &mut page).is_err() {
                return 1;
            }
            let _ = window::request_redraw(win);
            let mut next = events::wait(None);
            while let Some(event) = next {
                match event {
                    Event::CloseRequested(_) => return 0,
                    Event::Pointer(p) if p.button.is_some() && p.pressed => {
                        page.pressed = page.buttons.iter().position(|b| b.contains(p.x, p.y));
                        if page.bar.contains(p.x, p.y) {
                            let inner = page.bar.inset(8.0, 8.0, 8.0, 8.0);
                            let s = ((p.x - inner.x) / inner.w).clamp(0.0, 1.0);
                            page.strength = Some(s);
                            haptics::impact(s);
                        }
                    }
                    Event::Pointer(p) if p.button.is_some() => {
                        if let Some(i) = page.pressed.take() {
                            if page.buttons[i].contains(p.x, p.y) {
                                haptics::play(FEELS[i].0);
                            }
                        }
                    }
                    _ => {}
                }
                next = events::poll();
            }
        }
    }
}

krate::export!(Component);

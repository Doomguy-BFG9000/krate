// Look: a reading page that follows the phone's dark mode, contrast and
// text size (krate:ui/appearance, IC-925), through the layout kit's Look.
// Change them in Settings with the app open and it follows within a frame.
//
//   krate run look-probe.krate -- quick   draws one frame, then one line:
//                                         `look:dark=yes contrast=no text=1.35`
#![no_std]
extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use krate::gfx::{canvas2d, types as gfx};
use krate::layout::{Look, Screen};
use krate::{
    io::{args, stdio},
    ui::{
        events, tree,
        types::{self, Event},
        window,
    },
    Guest,
};

const PARAGRAPHS: [&str; 3] = [
    "Dark mode and large text belong to the person holding the phone, not to the app.",
    "This page reads both every frame: switch dark mode or drag the text size in Settings and it follows.",
    "Lines wrap to the width they have, so larger text grows the page instead of being cut off at the side.",
];

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

/// Break `text` into lines no wider than `width` at `size`.
fn wrap(canvas: u64, text: &str, size: f32, width: f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        let tried = if line.is_empty() { String::from(word) } else { format!("{line} {word}") };
        let fits = canvas2d::measure_text_styled(canvas, &tried, size, style(450))
            .map(|m| m.width <= width)
            .unwrap_or(true);
        if fits || line.is_empty() {
            line = tried;
        } else {
            lines.push(core::mem::take(&mut line));
            line = String::from(word);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn draw(canvas: u64, screen: &Screen) -> Result<(), gfx::GfxError> {
    let look: Look = screen.look;
    let colors = look.palette();
    canvas2d::clear(canvas, colors.background)?;
    let area = screen.safe_area().inset(16.0, 16.0, 16.0, 16.0);
    let title = look.text(30.0);
    let mut y = area.y + title;
    canvas2d::draw_text_styled(canvas, "Look", gfx::Point { x: area.x, y }, title, colors.ink, style(800))?;
    y += look.text(14.0) + 12.0;
    let state = format!(
        "{} mode{}, text at {:.0}%",
        if look.dark { "Dark" } else { "Light" },
        if look.high_contrast { ", more contrast" } else { "" },
        look.text_scale * 100.0
    );
    canvas2d::draw_text_styled(canvas, &state, gfx::Point { x: area.x, y }, look.text(14.0), colors.muted, style(500))?;
    y += 20.0;
    let body = screen.body();
    let pad = 16.0;
    for paragraph in PARAGRAPHS {
        let lines = wrap(canvas, paragraph, body, area.w - pad * 2.0);
        let line_h = body * 1.35;
        let h = lines.len() as f32 * line_h + pad * 1.5;
        let radii = gfx::CornerRadii { top_left: 14.0, top_right: 14.0, bottom_right: 14.0, bottom_left: 14.0 };
        let card = gfx::Rect { x: area.x, y, width: area.w, height: h };
        canvas2d::fill_round_rect(canvas, card, radii, colors.surface)?;
        canvas2d::stroke_round_rect(canvas, card, radii, if look.high_contrast { 1.5 } else { 1.0 }, colors.line)?;
        let mut ly = y + pad + body;
        for line in &lines {
            canvas2d::draw_text_styled(canvas, line, gfx::Point { x: area.x + pad, y: ly }, body, colors.ink, style(450))?;
            ly += line_h;
        }
        y += h + 12.0;
    }
    canvas2d::present(canvas)
}

struct Component;

impl Guest for Component {
    fn run() -> i32 {
        let Ok(win) = window::create("Look", types::WindowSize { width: 420, height: 720 }) else {
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
        if args::raw().split('\n').any(|a| a == "quick") {
            let _ = draw(canvas, &Screen::of(win));
            let look = Look::now();
            say(&format!(
                "look:dark={} contrast={} text={:.2}",
                if look.dark { "yes" } else { "no" },
                if look.high_contrast { "yes" } else { "no" },
                look.text_scale
            ));
            return 0;
        }
        loop {
            if draw(canvas, &Screen::of(win)).is_err() {
                return 1;
            }
            // Twice a second is enough to follow a change made in Settings.
            if let Some(Event::CloseRequested(_)) = events::wait(Some(500)) {
                return 0;
            }
        }
    }
}

krate::export!(Component);

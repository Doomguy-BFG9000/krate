// Vault: a password vault that opens only for its owner, through
// krate:ui/unlock (IC-924) -- Face ID or the passcode on an iPhone, Touch ID
// or the password on a Mac. A no keeps it locked.
//
//   krate run unlock-probe.krate -- quick   one line, then exit:
//                                           `unlock:available=yes answer=yes entries=3`,
//                                           `... answer=no entries=hidden` or
//                                           `unlock:available=no answer=unavailable`
#![no_std]
extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use krate::gfx::{canvas2d, types as gfx};
use krate::layout::{Rect, Screen};
use krate::secret;
use krate::ui::unlock::{self, UnlockAnswer};
use krate::{
    io::{args, stdio},
    ui::{
        events, tree,
        types::{self, Event},
        window,
    },
    Guest,
};

const REASON: &str = "Unlock your vault";

/// The vault's first entries, kept in the secret store on first run.
const STARTERS: [(&str, &str); 3] = [
    ("Home Wi-Fi", "krate-home / 7Fq-29xL-a1"),
    ("Bank PIN", "4 8 1 6"),
    ("Locker", "Row C, 0423"),
];

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

/// Read the vault, writing its first entries the first time.
fn entries() -> Vec<(String, String)> {
    let mut names = secret::names().unwrap_or_default();
    names.retain(|n| n.starts_with("vault:"));
    if names.is_empty() {
        for (name, value) in STARTERS {
            let _ = secret::set_text(&format!("vault:{name}"), value);
        }
        names = STARTERS.iter().map(|(n, _)| format!("vault:{n}")).collect();
    }
    names.sort();
    names
        .into_iter()
        .filter_map(|n| {
            let value = secret::get_text(&n).ok().flatten()?;
            Some((String::from(n.trim_start_matches("vault:")), value))
        })
        .collect()
}

fn quick() -> i32 {
    let available = unlock::available();
    let (answer, shown) = match unlock::check(REASON) {
        UnlockAnswer::Yes => ("yes", format!("{}", entries().len())),
        UnlockAnswer::No => ("no", String::from("hidden")),
        UnlockAnswer::Unavailable(_) => ("unavailable", String::from("hidden")),
    };
    say(&format!(
        "unlock:available={} answer={answer} entries={shown}",
        if available { "yes" } else { "no" }
    ));
    0
}

struct Page {
    open: Option<Vec<(String, String)>>,
    note: String,
}

fn button(screen: &Screen) -> Rect {
    let area = screen.safe_area().inset(16.0, 16.0, 16.0, 16.0);
    Rect::new(area.x, area.y + 90.0, area.w, 52.0)
}

fn draw(canvas: u64, screen: &Screen, page: &Page) -> Result<(), gfx::GfxError> {
    let ink = rgb(0.07, 0.07, 0.09);
    let muted = rgb(0.43, 0.44, 0.48);
    canvas2d::clear(canvas, rgb(0.96, 0.96, 0.97))?;
    let area = screen.safe_area().inset(16.0, 16.0, 16.0, 16.0);
    canvas2d::draw_text_styled(canvas, "Vault", gfx::Point { x: area.x, y: area.y + 32.0 }, 30.0, ink, style(800))?;
    let line = if page.open.is_some() { "Open. It locks again when you tap Lock." } else { "Locked." };
    canvas2d::draw_text_styled(canvas, line, gfx::Point { x: area.x, y: area.y + 60.0 }, 15.0, muted, style(450))?;
    let b = button(screen);
    let radii = gfx::CornerRadii { top_left: 12.0, top_right: 12.0, bottom_right: 12.0, bottom_left: 12.0 };
    canvas2d::fill_round_rect(canvas, gfx::Rect { x: b.x, y: b.y, width: b.w, height: b.h }, radii, ink)?;
    let label = if page.open.is_some() { "Lock" } else { "Unlock" };
    let m = canvas2d::measure_text_styled(canvas, label, 17.0, style(600))?;
    canvas2d::draw_text_styled(canvas, label, gfx::Point { x: b.x + (b.w - m.width) / 2.0, y: b.y + 33.0 }, 17.0, rgb(1.0, 1.0, 1.0), style(600))?;
    let mut y = b.bottom() + 34.0;
    if !page.note.is_empty() {
        canvas2d::draw_text_styled(canvas, &page.note, gfx::Point { x: area.x, y }, 15.0, rgb(0.75, 0.25, 0.2), style(500))?;
        y += 34.0;
    }
    if let Some(entries) = &page.open {
        for (name, value) in entries {
            let card = gfx::Rect { x: area.x, y, width: area.w, height: 64.0 };
            canvas2d::fill_round_rect(canvas, card, radii, rgb(1.0, 1.0, 1.0))?;
            canvas2d::draw_text_styled(canvas, name, gfx::Point { x: area.x + 16.0, y: y + 26.0 }, 14.0, muted, style(500))?;
            canvas2d::draw_text_styled(canvas, value, gfx::Point { x: area.x + 16.0, y: y + 50.0 }, 17.0, ink, style(600))?;
            y += 76.0;
        }
    }
    canvas2d::present(canvas)
}

struct Component;

impl Guest for Component {
    fn run() -> i32 {
        if args::raw().split('\n').any(|a| a == "quick") {
            return quick();
        }
        let Ok(win) = window::create("Vault", types::WindowSize { width: 420, height: 720 }) else {
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
        let mut page = Page { open: None, note: String::new() };
        if !unlock::available() {
            page.note = String::from("This device cannot check it is you, so the vault stays shut.");
        }
        loop {
            if draw(canvas, &Screen::of(win), &page).is_err() {
                return 1;
            }
            let mut tapped = false;
            let mut next = events::wait(Some(250));
            while let Some(event) = next {
                match event {
                    Event::CloseRequested(_) => return 0,
                    Event::Pointer(p) if p.button.is_some() && !p.pressed => {
                        tapped |= button(&Screen::of(win)).contains(p.x, p.y);
                    }
                    _ => {}
                }
                next = events::poll();
            }
            if !tapped {
                continue;
            }
            if page.open.is_some() {
                page.open = None;
                page.note.clear();
                say("unlock:locked");
                continue;
            }
            match unlock::check(REASON) {
                UnlockAnswer::Yes => {
                    page.note.clear();
                    page.open = Some(entries());
                    say("unlock:answer=yes");
                }
                UnlockAnswer::No => {
                    page.note = String::from("Not you, or cancelled: the vault stays locked.");
                    say("unlock:answer=no");
                }
                UnlockAnswer::Unavailable(why) => {
                    page.note = why;
                    say("unlock:answer=unavailable");
                }
            }
        }
    }
}

krate::export!(Component);

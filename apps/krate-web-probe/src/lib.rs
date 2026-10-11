// Help: Krate's help page shown inside an app (krate:ui/web-page, IC-927),
// under a header the app draws, with a button that tries a host the app was
// never granted -- refused, and said.
//
//   krate run web-probe.krate -- quick   one line per step, then exit:
//                                        `web:open=ok`, `web:go-ungranted=refused`,
//                                        `web:open-ungranted=refused`, `web:open-http=refused`
#![no_std]
extern crate alloc;

use alloc::format;
use alloc::string::String;
use krate::gfx::{canvas2d, types as gfx};
use krate::layout::{Look, Rect, Said, Screen};
use krate::ui::web_page;
use krate::{
    io::{args, stdio},
    ui::{
        events, tree,
        types::{self, Event},
        window,
    },
    Guest,
};

const HELP: &str = "https://krate.tech/";
const ELSEWHERE: &str = "https://example.com/";

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

struct Layout {
    title: Rect,
    button: Rect,
    note: Rect,
    page: Rect,
}

fn layout(screen: &Screen) -> Layout {
    let area = screen.safe_area().inset(12.0, 16.0, 12.0, 16.0);
    let title = Rect::new(area.x, area.y, area.w, 40.0);
    let button = Rect::new(area.x, area.y + 48.0, area.w, 44.0);
    let note = Rect::new(area.x, area.y + 100.0, area.w, 22.0);
    let page = Rect::new(area.x, area.y + 130.0, area.w, (area.h - 130.0).max(100.0));
    Layout { title, button, note, page }
}

fn draw(win: u64, canvas: u64, screen: &Screen, note: &str) -> Result<(), gfx::GfxError> {
    let look: Look = screen.look;
    let colors = look.palette();
    let at = layout(screen);
    canvas2d::clear(canvas, colors.background)?;
    canvas2d::draw_text_styled(canvas, "Help", gfx::Point { x: at.title.x, y: at.title.y + 30.0 }, look.text(28.0), colors.ink, style(800))?;
    let radii = gfx::CornerRadii { top_left: 12.0, top_right: 12.0, bottom_right: 12.0, bottom_left: 12.0 };
    let b = at.button;
    canvas2d::fill_round_rect(canvas, gfx::Rect { x: b.x, y: b.y, width: b.w, height: b.h }, radii, colors.accent)?;
    let label = "Try example.com (not allowed)";
    let m = canvas2d::measure_text_styled(canvas, label, look.text(15.0), style(600))?;
    canvas2d::draw_text_styled(canvas, label, gfx::Point { x: b.x + (b.w - m.width) / 2.0, y: b.y + 28.0 }, look.text(15.0), colors.on_accent, style(600))?;
    canvas2d::draw_text_styled(canvas, note, gfx::Point { x: at.note.x, y: at.note.y + 16.0 }, look.text(13.0), colors.muted, style(500))?;
    canvas2d::present(canvas)?;
    let mut said = Said::new();
    said.heading(at.title, "Help");
    said.button(at.button, label);
    said.text(at.note, note);
    said.send(win);
    Ok(())
}

fn quick(win: u64) -> i32 {
    match web_page::open(win, HELP, 0.0, 120.0, 390.0, 600.0) {
        Ok(page) => {
            say("web:open=ok");
            match web_page::go(page, ELSEWHERE) {
                Err(_) => {
                    let problem = web_page::state(page).map(|s| s.problem).unwrap_or_default();
                    say(&format!("web:go-ungranted=refused problem={problem}"));
                }
                Ok(()) => say("web:go-ungranted=allowed"),
            }
            web_page::close(page);
        }
        Err(e) => say(&format!("web:open=failed {e:?}")),
    }
    say(match web_page::open(win, ELSEWHERE, 0.0, 0.0, 100.0, 100.0) {
        Err(_) => "web:open-ungranted=refused",
        Ok(_) => "web:open-ungranted=allowed",
    });
    say(match web_page::open(win, "http://krate.tech/", 0.0, 0.0, 100.0, 100.0) {
        Err(_) => "web:open-http=refused",
        Ok(_) => "web:open-http=allowed",
    });
    0
}

struct Component;

impl Guest for Component {
    fn run() -> i32 {
        let Ok(win) = window::create("Help", types::WindowSize { width: 420, height: 760 }) else {
            return 1;
        };
        if args::raw().split('\n').any(|a| a == "quick") {
            return quick(win);
        }
        if tree::set_root(win, &node(1, None, types::WidgetKind::Stack)).is_err()
            || tree::upsert_node(win, &node(2, Some(1), types::WidgetKind::Canvas)).is_err()
        {
            return 1;
        }
        let Ok(canvas) = canvas2d::bind(win, 2) else {
            return 1;
        };
        let screen = Screen::of(win);
        let at = layout(&screen);
        let page = web_page::open(win, HELP, at.page.x, at.page.y, at.page.w, at.page.h).ok();
        let mut note = String::from(if page.is_some() { "krate.tech, inside the app." } else { "The page could not open here." });
        let mut placed = at.page;
        loop {
            let screen = Screen::of(win);
            let at = layout(&screen);
            if let Some(page) = page {
                if at.page != placed {
                    let _ = web_page::place(page, at.page.x, at.page.y, at.page.w, at.page.h);
                    placed = at.page;
                }
                if let Ok(state) = web_page::state(page) {
                    let line = if !state.problem.is_empty() {
                        format!("Refused: {}", state.problem)
                    } else if state.loading {
                        String::from("Loading...")
                    } else if !state.title.is_empty() {
                        state.title.clone()
                    } else {
                        note.clone()
                    };
                    note = line;
                }
            }
            if draw(win, canvas, &screen, &note).is_err() {
                return 1;
            }
            let mut next = events::wait(Some(250));
            while let Some(event) = next {
                match event {
                    Event::CloseRequested(_) => return 0,
                    Event::Pointer(p) if p.button.is_some() && !p.pressed => {
                        if at.button.contains(p.x, p.y) {
                            if let Some(page) = page {
                                let _ = web_page::go(page, ELSEWHERE);
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

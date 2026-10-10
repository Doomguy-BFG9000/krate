//! Shelf: a record collection laid out for whatever screen it opens on.
//!
//! The worked example for `krate::layout` (IC-912). The same code is a
//! sidebar beside a wide grid on a desktop, a sidebar and three columns on
//! a tablet or a phone turned sideways, and two columns over bottom tabs on
//! a phone held upright -- every rectangle comes from the screen in front of
//! it (`Screen::of`), asked again on every resize, and every tap is tested
//! against the rectangle the thing was drawn in.

#![no_std]

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;

use krate::gfx::{canvas2d, types as gfx};
use krate::layout::{Rect, Screen};
use krate::ui::{events, tree, types, window};

const ROOT_ID: u64 = 1;
const CANVAS_ID: u64 = 2;

const BG: gfx::Color = rgb(0.973, 0.973, 0.980);
const PANEL: gfx::Color = rgb(0.937, 0.937, 0.953);
const INK: gfx::Color = rgb(0.067, 0.067, 0.078);
const MUTED: gfx::Color = rgb(0.46, 0.46, 0.50);
const ACCENT: gfx::Color = rgb(0.96, 0.20, 0.33);
const HAIRLINE: gfx::Color = rgb(0.86, 0.86, 0.89);
const WHITE: gfx::Color = rgb(1.0, 1.0, 1.0);

const fn rgb(r: f32, g: f32, b: f32) -> gfx::Color {
    gfx::Color { r, g, b, a: 1.0 }
}

/// A record: title, artist, year, and the two colours of its sleeve.
struct Record {
    title: &'static str,
    artist: &'static str,
    year: u16,
    top: (f32, f32, f32),
    bottom: (f32, f32, f32),
    tracks: &'static [&'static str],
}

const RECORDS: &[Record] = &[
    Record { title: "Slow Rivers", artist: "Marlow Vane", year: 2021, top: (0.98, 0.45, 0.35), bottom: (0.62, 0.10, 0.32), tracks: &["Golden Undertow", "Low Water", "The Long Bend", "Estuary"] },
    Record { title: "Tin Roof Sermon", artist: "The Cedar Line", year: 2019, top: (0.36, 0.55, 0.95), bottom: (0.10, 0.16, 0.45), tracks: &["Tin Roof Sermon", "Porchlight", "Gravel Hymn"] },
    Record { title: "Cassette Sunrise", artist: "Juno Pell", year: 2023, top: (0.99, 0.80, 0.30), bottom: (0.93, 0.38, 0.18), tracks: &["Side A", "Rewind", "Morning Tape", "Side B"] },
    Record { title: "Paper Lanterns", artist: "Ise & the Quiet", year: 2018, top: (0.55, 0.85, 0.75), bottom: (0.12, 0.45, 0.45), tracks: &["Lanterns", "Rice Paper", "Night Market"] },
    Record { title: "Static Bloom", artist: "Vesper Lake", year: 2022, top: (0.80, 0.55, 0.98), bottom: (0.30, 0.12, 0.55), tracks: &["Static Bloom", "Antenna", "Weather Radio", "Sleepwalk"] },
    Record { title: "Kite Season", artist: "Odalie", year: 2020, top: (0.50, 0.85, 0.98), bottom: (0.15, 0.40, 0.80), tracks: &["Kite Season", "Updraft", "String"] },
    Record { title: "Blue Hour Drive", artist: "Renn Halloway", year: 2017, top: (0.25, 0.30, 0.65), bottom: (0.05, 0.06, 0.18), tracks: &["Blue Hour", "Interstate", "Last Exit", "Dashboard Light"] },
    Record { title: "Marble Stairs", artist: "Kamau Reeve", year: 2024, top: (0.85, 0.85, 0.82), bottom: (0.45, 0.44, 0.42), tracks: &["Marble Stairs", "Atrium", "Echo"] },
    Record { title: "Salt & Ember", artist: "Fen Carrow", year: 2016, top: (0.98, 0.60, 0.45), bottom: (0.35, 0.15, 0.12), tracks: &["Salt", "Ember", "Driftwood Fire"] },
    Record { title: "Glasshouse", artist: "Mira Oduya", year: 2025, top: (0.70, 0.95, 0.60), bottom: (0.20, 0.50, 0.25), tracks: &["Glasshouse", "Fern", "Condensation", "Bloom Cycle"] },
    Record { title: "Northern Lines", artist: "The Aldersons", year: 2015, top: (0.60, 0.98, 0.90), bottom: (0.10, 0.30, 0.50), tracks: &["Northern Lines", "Aurora", "Coldwater"] },
    Record { title: "Velvet Static", artist: "Juno Pell", year: 2021, top: (0.95, 0.40, 0.65), bottom: (0.40, 0.08, 0.30), tracks: &["Velvet", "Static", "Afterglow"] },
];

/// The places the app has: a sidebar's rows on a wide screen, tabs on a
/// phone. Same list, same order, either way.
#[derive(Clone, Copy, PartialEq)]
enum Place {
    ListenNow,
    Library,
    Recent,
}

const PLACES: [(Place, &str); 3] = [
    (Place::ListenNow, "Listen Now"),
    (Place::Library, "Library"),
    (Place::Recent, "Recent"),
];

#[derive(Clone, Copy)]
enum Hit {
    Go(Place),
    Open(usize),
    Back,
}

struct App {
    canvas: u64,
    screen: Screen,
    place: Place,
    open: Option<usize>,
    scroll: f32,
    content_h: f32,
    view_h: f32,
    hits: Vec<(Rect, Hit)>,
}

fn r(rect: Rect) -> gfx::Rect {
    gfx::Rect { x: rect.x, y: rect.y, width: rect.w, height: rect.h }
}

fn radii(all: f32) -> gfx::CornerRadii {
    gfx::CornerRadii { top_left: all, top_right: all, bottom_right: all, bottom_left: all }
}

fn style(weight: u16, spacing: f32) -> gfx::TextStyle {
    gfx::TextStyle { weight, italic: false, letter_spacing: spacing, family: gfx::FontFamily::Sans }
}

fn color((red, green, blue): (f32, f32, f32)) -> gfx::Color {
    rgb(red, green, blue)
}

/// `text` shortened with an ellipsis until it fits `width` points.
fn fit(canvas: u64, text: &str, size: f32, weight: u16, width: f32) -> String {
    let fits = |t: &str| {
        canvas2d::measure_text_styled(canvas, t, size, style(weight, 0.0))
            .map(|m| m.width <= width)
            .unwrap_or(true)
    };
    if fits(text) {
        return String::from(text);
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let mut t: String = chars.iter().collect();
        t = String::from(t.trim_end());
        t.push('\u{2026}');
        if fits(&t) {
            return t;
        }
    }
    String::new()
}

fn text(canvas: u64, s: &str, x: f32, baseline: f32, size: f32, ink: gfx::Color, weight: u16) {
    let _ = canvas2d::draw_text_styled(canvas, s, gfx::Point { x, y: baseline }, size, ink, style(weight, 0.0));
}

impl App {
    fn records(&self) -> Vec<usize> {
        let mut all: Vec<usize> = (0..RECORDS.len()).collect();
        match self.place {
            Place::ListenNow => all,
            Place::Library => {
                all.sort_by_key(|&i| RECORDS[i].artist);
                all
            }
            Place::Recent => {
                all.sort_by_key(|&i| core::cmp::Reverse(RECORDS[i].year));
                all.truncate(6);
                all
            }
        }
    }

    fn title(&self) -> &'static str {
        PLACES.iter().find(|(p, _)| *p == self.place).map(|(_, t)| *t).unwrap_or("")
    }

    fn draw(&mut self) {
        let c = self.canvas;
        let s = self.screen;
        self.hits.clear();
        let _ = canvas2d::clear(c, BG);

        // A phone turned sideways has little height: a narrower sidebar,
        // and smaller sleeves so more than one row shows.
        let nav = s.nav(s.safe_area(), if s.short() { 170.0 } else { 220.0 });
        if let Some(side) = nav.sidebar {
            self.draw_sidebar(side);
        }
        if let Some(tabs) = nav.tabs {
            self.draw_tabs(tabs);
        }
        let mut content = nav.content;
        // Room at the sides of a phone's content for a thumb; more on a
        // desktop, where the sidebar already frames it.
        content = content.inset(0.0, 16.0, 0.0, if nav.sidebar.is_some() { 24.0 } else { 16.0 });
        match self.open {
            Some(i) => self.draw_record(content, i),
            None => self.draw_grid(content),
        }
    }

    fn draw_sidebar(&mut self, side: Rect) {
        let c = self.canvas;
        let s = self.screen;
        // The panel runs to the window's left and top edges, under any
        // covered strip; its rows stay inside `side`.
        let _ = canvas2d::fill_rect(c, r(Rect::new(0.0, 0.0, side.right(), s.height)), PANEL);
        let mut rows = side.pad(12.0);
        let brand = rows.take_top(if s.short() { 36.0 } else { 52.0 });
        text(c, "Shelf", brand.x + 8.0, brand.y + brand.h - 12.0, 22.0, INK, 800);
        for (place, label) in PLACES {
            let row = rows.take_top(s.tap() + 4.0);
            if place == self.place && self.open.is_none() {
                let _ = canvas2d::fill_round_rect(c, r(row.inset(2.0, 0.0, 2.0, 0.0)), radii(10.0), WHITE);
            }
            let ink = if place == self.place { ACCENT } else { INK };
            text(c, label, row.x + 12.0, row.y + row.h / 2.0 + 5.5, s.body(), ink, 600);
            self.hits.push((row, Hit::Go(place)));
        }
    }

    fn draw_tabs(&mut self, tabs: Rect) {
        let c = self.canvas;
        let s = self.screen;
        // The bar's background runs down under the home bar to the edge.
        let _ = canvas2d::fill_rect(c, r(Rect::new(0.0, tabs.y, s.width, s.height - tabs.y)), PANEL);
        let _ = canvas2d::fill_rect(c, r(Rect::new(0.0, tabs.y, s.width, 0.5)), HAIRLINE);
        for ((place, label), cell) in PLACES.iter().zip(tabs.columns(PLACES.len(), 0.0)) {
            let ink = if *place == self.place { ACCENT } else { MUTED };
            let dot = cell.center(28.0, 6.0);
            let dot = Rect::new(dot.x, cell.y + 12.0, dot.w, dot.h);
            let _ = canvas2d::fill_round_rect(c, r(dot), radii(3.0), ink);
            let m = canvas2d::measure_text_styled(c, label, 13.0, style(600, 0.0)).map(|m| m.width).unwrap_or(0.0);
            text(c, label, cell.x + (cell.w - m) / 2.0, cell.y + 40.0, 13.0, ink, 600);
            self.hits.push((cell, Hit::Go(*place)));
        }
    }

    fn header(&mut self, content: &mut Rect, title: &str) -> Rect {
        let c = self.canvas;
        let s = self.screen;
        let size = if s.short() { 24.0 } else if s.compact() { 32.0 } else { 30.0 };
        let band = content.take_top(if s.short() { 48.0 } else { 72.0 });
        let t = fit(c, title, size, 800, band.w);
        text(c, &t, band.x, band.y + band.h - 14.0, size, INK, 800);
        band
    }

    fn draw_grid(&mut self, mut content: Rect) {
        let c = self.canvas;
        let s = self.screen;
        let title = self.title();
        self.header(&mut content, title);
        let list = self.records();
        // Columns from the width: two on a phone, more as room allows.
        let min_w = if s.short() {
            118.0
        } else if s.compact() {
            150.0
        } else {
            170.0
        };
        let gap = if s.compact() { 14.0 } else { 20.0 };
        let probe = content.grid(1.max(list.len()), min_w, 1.0, gap);
        let art = probe.first().map(|cell| cell.w).unwrap_or(min_w);
        let cell_h = art + 52.0;
        let cells = content.grid(list.len(), min_w, cell_h, gap);
        self.view_h = content.h;
        self.content_h = cells.last().map(|cell| cell.bottom() - content.y + 24.0).unwrap_or(0.0);
        let _ = canvas2d::set_clip(c, content.x - 8.0, content.y, content.w + 16.0, content.h);
        for (&i, cell) in list.iter().zip(cells.iter()) {
            let cell = Rect::new(cell.x, cell.y - self.scroll, cell.w, cell.h);
            if cell.bottom() < content.y || cell.y > content.bottom() {
                continue;
            }
            let rec = &RECORDS[i];
            let sleeve = Rect::new(cell.x, cell.y, cell.w, cell.w);
            let _ = canvas2d::drop_shadow_round_rect(c, r(sleeve), radii(12.0), 14.0, gfx::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.14 });
            let _ = canvas2d::linear_gradient_stops(
                c,
                r(sleeve),
                135.0,
                &[
                    gfx::GradientStop { offset: 0.0, color: color(rec.top) },
                    gfx::GradientStop { offset: 1.0, color: color(rec.bottom) },
                ],
            );
            let initial: String = rec.title.chars().take(1).collect();
            text(c, &initial, sleeve.x + 14.0, sleeve.bottom() - 16.0, sleeve.w * 0.32, gfx::Color { r: 1.0, g: 1.0, b: 1.0, a: 0.85 }, 800);
            let t = fit(c, rec.title, 15.0, 650, cell.w);
            text(c, &t, cell.x, sleeve.bottom() + 22.0, 15.0, INK, 650);
            let a = fit(c, rec.artist, 13.0, 450, cell.w);
            text(c, &a, cell.x, sleeve.bottom() + 40.0, 13.0, MUTED, 450);
            self.hits.push((cell, Hit::Open(i)));
        }
        let _ = canvas2d::clear_clip(c);
    }

    fn draw_record(&mut self, mut content: Rect, i: usize) {
        let c = self.canvas;
        let s = self.screen;
        let rec = &RECORDS[i];
        let back = content.take_top(s.tap() + 8.0);
        let back_hit = Rect::new(back.x - 8.0, back.y, 140.0_f32.min(back.w), back.h);
        text(c, &alloc::format!("\u{2039} {}", self.title()), back.x, back.y + back.h / 2.0 + 6.0, s.body(), ACCENT, 600);
        self.hits.push((back_hit, Hit::Back));

        // Sleeve beside the details when there is room, above them when not.
        let side_by_side = !s.compact();
        let art = if side_by_side { (content.w * 0.4).min(content.h - 16.0).min(320.0) } else { content.w.min(320.0) };
        let mut body = content;
        let sleeve = if side_by_side {
            let column = body.take_left(art + 24.0);
            Rect::new(column.x, column.y + 8.0, art, art)
        } else {
            let band = body.take_top(art + 20.0);
            Rect::new(band.x + (band.w - art) / 2.0, band.y + 4.0, art, art)
        };
        let _ = canvas2d::drop_shadow_round_rect(c, r(sleeve), radii(16.0), 20.0, gfx::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.18 });
        let _ = canvas2d::linear_gradient_stops(
            c,
            r(sleeve),
            135.0,
            &[
                gfx::GradientStop { offset: 0.0, color: color(rec.top) },
                gfx::GradientStop { offset: 1.0, color: color(rec.bottom) },
            ],
        );
        let t = fit(c, rec.title, 26.0, 800, body.w);
        text(c, &t, body.x, body.y + 34.0, 26.0, INK, 800);
        let a = fit(c, &alloc::format!("{} \u{00b7} {}", rec.artist, rec.year), s.body(), 500, body.w);
        text(c, &a, body.x, body.y + 60.0, s.body(), ACCENT, 500);
        let mut list = body.inset(80.0, 0.0, 0.0, 0.0);
        for (n, track) in rec.tracks.iter().enumerate() {
            let row = list.take_top(s.tap() + 4.0);
            if row.h <= 0.0 {
                break;
            }
            let _ = canvas2d::fill_rect(c, r(Rect::new(row.x, row.y, row.w, 0.5)), HAIRLINE);
            text(c, &alloc::format!("{}", n + 1), row.x, row.y + row.h / 2.0 + 6.0, s.body(), MUTED, 500);
            let name = fit(c, track, s.body(), 500, row.w - 36.0);
            text(c, &name, row.x + 32.0, row.y + row.h / 2.0 + 6.0, s.body(), INK, 500);
        }
        self.content_h = 0.0;
        self.view_h = content.h;
    }

    fn tap(&mut self, x: f32, y: f32) -> bool {
        let Some((_, hit)) = self.hits.iter().rev().find(|(rect, _)| rect.contains(x, y)).copied() else {
            return false;
        };
        match hit {
            Hit::Go(place) => {
                self.place = place;
                self.open = None;
                self.scroll = 0.0;
            }
            Hit::Open(i) => self.open = Some(i),
            Hit::Back => self.open = None,
        }
        true
    }
}

fn node(id: u64, parent: Option<u64>, kind: types::WidgetKind) -> types::WidgetNode {
    types::WidgetNode {
        id,
        parent,
        kind,
        label: None,
        role: None,
        style: types::Style { width: None, height: None, grow: 1.0, padding: 0.0, text: None, place: None, box_: None },
        checked: None,
        value: None,
        selected: None,
        text_cursor: None,
    }
}

struct Component;

impl krate::Guest for Component {
    fn run() -> i32 {
        // The size a desktop opens it at. A phone or tablet gives it the
        // whole screen instead, and `Screen::of` says which.
        let Ok(win) = window::create("Shelf", types::WindowSize { width: 1100, height: 760 }) else {
            return 1;
        };
        if tree::set_root(win, &node(ROOT_ID, None, types::WidgetKind::Stack)).is_err() {
            return 1;
        }
        let _ = tree::upsert_node(win, &node(CANVAS_ID, Some(ROOT_ID), types::WidgetKind::Canvas));
        let Ok(canvas) = canvas2d::bind(win, CANVAS_ID) else {
            return 1;
        };
        let mut app = App {
            canvas,
            screen: Screen::of(win),
            place: Place::ListenNow,
            open: None,
            scroll: 0.0,
            content_h: 0.0,
            view_h: 0.0,
            hits: Vec::new(),
        };
        let quick = krate::bindings::krate::io::args::raw().split('\n').any(|a| a == "quick");
        let mut dirty = true;
        let mut pressed: Option<(f32, f32)> = None;
        loop {
            if dirty {
                app.draw();
                let _ = canvas2d::present(canvas);
                let _ = window::request_redraw(win);
                dirty = false;
                if quick {
                    return 0;
                }
            }
            match events::wait(None) {
                Some(types::Event::CloseRequested(_)) => return 0,
                Some(types::Event::Resized(_)) => {
                    // A phone turned, a window narrowed: lay out again.
                    app.screen = Screen::of(win);
                    dirty = true;
                }
                Some(types::Event::Wheel(wheel)) => {
                    let max = (app.content_h - app.view_h).max(0.0);
                    let next = (app.scroll + wheel.dy).clamp(0.0, max);
                    if next != app.scroll {
                        app.scroll = next;
                        dirty = true;
                    }
                }
                Some(types::Event::Pointer(p)) if p.button.is_some() => {
                    if p.pressed {
                        pressed = Some((p.x, p.y));
                    } else if let Some((x0, y0)) = pressed.take() {
                        // A press that moved is a scroll, not a tap.
                        if (p.x - x0).abs() < 10.0 && (p.y - y0).abs() < 10.0 && app.tap(p.x, p.y) {
                            dirty = true;
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

krate::export!(Component);

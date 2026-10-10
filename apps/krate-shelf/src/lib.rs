//! Shelf: a record collection that plays, laid out for whatever screen it
//! opens on.
//!
//! The worked example for `krate::layout` (IC-912) and for a media app. The
//! same code is a sidebar beside wide grids on a desktop, a sidebar and
//! three columns on a tablet or a phone turned sideways, and tabs with a
//! now-playing bar on a phone held upright. Every rectangle comes from the
//! screen in front of it (`Screen::of`, asked again on every resize), and
//! every tap is tested against the rectangle the thing was drawn in.
//!
//! The records play: each track is a short song made in `music`, played
//! through `krate:audio/playback`. The sleeves are pictures made in `art`
//! and kept by the host, drawn by handle.

#![no_std]

extern crate alloc;
mod art;
mod music;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use krate::gfx::{canvas2d, canvas_images, types as gfx};
use krate::layout::{Rect, Screen};
use krate::ui::{events, tree, types, window};

const ROOT_ID: u64 = 1;
const CANVAS_ID: u64 = 2;

const BG: gfx::Color = rgb(1.0, 1.0, 1.0);
const PANEL: gfx::Color = rgb(0.965, 0.965, 0.973);
const INK: gfx::Color = rgb(0.067, 0.067, 0.078);
const MUTED: gfx::Color = rgb(0.53, 0.53, 0.56);
const ACCENT: gfx::Color = rgb(0.98, 0.17, 0.33);
const HAIRLINE: gfx::Color = rgb(0.88, 0.88, 0.90);
const WHITE: gfx::Color = rgb(1.0, 1.0, 1.0);
const PILL: gfx::Color = rgb(0.94, 0.94, 0.96);

const fn rgb(r: f32, g: f32, b: f32) -> gfx::Color {
    gfx::Color { r, g, b, a: 1.0 }
}

const fn rgba(r: f32, g: f32, b: f32, a: f32) -> gfx::Color {
    gfx::Color { r, g, b, a }
}

/// A record: title, artist, genre, year, the two colours of its sleeve, and
/// its tracks.
struct Record {
    title: &'static str,
    artist: &'static str,
    genre: &'static str,
    year: u16,
    top: (f32, f32, f32),
    bottom: (f32, f32, f32),
    tracks: &'static [&'static str],
}

const RECORDS: &[Record] = &[
    Record { title: "Slow Rivers", artist: "Marlow Vane", genre: "Folk", year: 2021, top: (0.98, 0.52, 0.38), bottom: (0.55, 0.10, 0.30), tracks: &["Golden Undertow", "Low Water", "The Long Bend", "Estuary"] },
    Record { title: "Tin Roof Sermon", artist: "The Cedar Line", genre: "Americana", year: 2019, top: (0.38, 0.58, 0.98), bottom: (0.08, 0.14, 0.42), tracks: &["Tin Roof Sermon", "Porchlight", "Gravel Hymn"] },
    Record { title: "Cassette Sunrise", artist: "Juno Pell", genre: "Indie Pop", year: 2023, top: (0.99, 0.82, 0.34), bottom: (0.92, 0.36, 0.20), tracks: &["Side A", "Rewind", "Morning Tape", "Side B"] },
    Record { title: "Paper Lanterns", artist: "Ise & the Quiet", genre: "Ambient", year: 2018, top: (0.56, 0.88, 0.78), bottom: (0.10, 0.40, 0.42), tracks: &["Lanterns", "Rice Paper", "Night Market"] },
    Record { title: "Static Bloom", artist: "Vesper Lake", genre: "Dream Pop", year: 2022, top: (0.82, 0.58, 0.99), bottom: (0.28, 0.10, 0.52), tracks: &["Static Bloom", "Antenna", "Weather Radio", "Sleepwalk"] },
    Record { title: "Kite Season", artist: "Odalie", genre: "Pop", year: 2020, top: (0.52, 0.86, 0.99), bottom: (0.14, 0.38, 0.80), tracks: &["Kite Season", "Updraft", "String"] },
    Record { title: "Blue Hour Drive", artist: "Renn Halloway", genre: "Synthwave", year: 2017, top: (0.30, 0.34, 0.72), bottom: (0.05, 0.05, 0.17), tracks: &["Blue Hour", "Interstate", "Last Exit", "Dashboard Light"] },
    Record { title: "Marble Stairs", artist: "Kamau Reeve", genre: "Jazz", year: 2024, top: (0.86, 0.84, 0.80), bottom: (0.40, 0.38, 0.36), tracks: &["Marble Stairs", "Atrium", "Echo"] },
    Record { title: "Salt & Ember", artist: "Fen Carrow", genre: "Folk", year: 2016, top: (0.99, 0.62, 0.42), bottom: (0.32, 0.12, 0.10), tracks: &["Salt", "Ember", "Driftwood Fire"] },
    Record { title: "Glasshouse", artist: "Mira Oduya", genre: "Electronic", year: 2025, top: (0.72, 0.96, 0.62), bottom: (0.16, 0.46, 0.24), tracks: &["Glasshouse", "Fern", "Condensation", "Bloom Cycle"] },
    Record { title: "Northern Lines", artist: "The Aldersons", genre: "Indie", year: 2015, top: (0.60, 0.98, 0.90), bottom: (0.08, 0.26, 0.48), tracks: &["Northern Lines", "Aurora", "Coldwater"] },
    Record { title: "Velvet Static", artist: "Juno Pell", genre: "Indie Pop", year: 2021, top: (0.96, 0.44, 0.66), bottom: (0.38, 0.06, 0.28), tracks: &["Velvet", "Static", "Afterglow"] },
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

/// What fills the content area.
#[derive(Clone, Copy, PartialEq)]
enum View {
    Place,
    Record(usize),
    Playing,
}

#[derive(Clone, Copy)]
enum Hit {
    Go(Place),
    Open(usize),
    Back,
    Play(usize, usize),
    Shuffle(usize),
    Toggle,
    Next,
    Prev,
    OpenPlayer,
    ClosePlayer,
    /// The scrubber: a tap at x seeks there.
    Seek(Rect),
}

struct App {
    canvas: u64,
    screen: Screen,
    place: Place,
    view: View,
    /// Scroll offsets: the place's page, and a record's page, kept apart so
    /// going back finds the page where it was left.
    scroll: f32,
    page_scroll: f32,
    content_h: f32,
    view_h: f32,
    hits: Vec<(Rect, Hit)>,
    player: music::Player,
    /// Each record's sleeve, kept by the host.
    sleeves: Vec<Option<u64>>,
    /// Each track's length in milliseconds.
    lengths: Vec<Vec<u32>>,
    shuffle_seed: u32,
    /// The dark fade under the top pick's words: a picture the card's own
    /// shape, so it takes the card's rounded corners. Remade when the card
    /// changes shape (the screen turned).
    fade: Option<((u32, u32), u64)>,
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

fn darker((red, green, blue): (f32, f32, f32), by: f32) -> gfx::Color {
    rgb(red * by, green * by, blue * by)
}

fn width_of(canvas: u64, text: &str, size: f32, weight: u16) -> f32 {
    canvas2d::measure_text_styled(canvas, text, size, style(weight, 0.0))
        .map(|m| m.width)
        .unwrap_or(0.0)
}

/// `text` shortened with an ellipsis until it fits `width` points.
fn fit(canvas: u64, text: &str, size: f32, weight: u16, width: f32) -> String {
    if width_of(canvas, text, size, weight) <= width {
        return String::from(text);
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let mut t: String = chars.iter().collect();
        t = String::from(t.trim_end());
        t.push('\u{2026}');
        if width_of(canvas, &t, size, weight) <= width {
            return t;
        }
    }
    String::new()
}

fn text(canvas: u64, s: &str, x: f32, baseline: f32, size: f32, ink: gfx::Color, weight: u16) {
    let _ = canvas2d::draw_text_styled(canvas, s, gfx::Point { x, y: baseline }, size, ink, style(weight, 0.0));
}

/// Text centred in `area` horizontally, on `baseline`.
fn text_center(canvas: u64, s: &str, area: Rect, baseline: f32, size: f32, ink: gfx::Color, weight: u16) {
    let t = fit(canvas, s, size, weight, area.w);
    let w = width_of(canvas, &t, size, weight);
    text(canvas, &t, area.x + (area.w - w) / 2.0, baseline, size, ink, weight);
}

/// A triangle pointing right (`dir` 1) or left (-1), drawn as thin bands --
/// the canvas has no polygon.
fn triangle(canvas: u64, b: Rect, dir: f32, ink: gfx::Color) {
    let rows = 28;
    for i in 0..rows {
        let y = b.y + b.h * i as f32 / rows as f32;
        let mid = (i as f32 + 0.5) / rows as f32;
        let w = b.w * (1.0 - (mid - 0.5).abs() * 2.0);
        let x = if dir > 0.0 { b.x } else { b.right() - w };
        let _ = canvas2d::fill_rect(canvas, r(Rect::new(x, y, w, b.h / rows as f32 + 0.6)), ink);
    }
}

/// Play or pause, centred in `area`, `size` points tall.
fn play_pause(canvas: u64, area: Rect, size: f32, playing: bool, ink: gfx::Color) {
    let b = area.center(size, size);
    if playing {
        let bar = size * 0.3;
        let _ = canvas2d::fill_round_rect(canvas, r(Rect::new(b.x + size * 0.1, b.y, bar, b.h)), radii(bar * 0.25), ink);
        let _ = canvas2d::fill_round_rect(canvas, r(Rect::new(b.right() - size * 0.1 - bar, b.y, bar, b.h)), radii(bar * 0.25), ink);
    } else {
        triangle(canvas, Rect::new(b.x + size * 0.16, b.y, size * 0.84, size), 1.0, ink);
    }
}

/// Skip forward (`dir` 1) or back (-1): two triangles, centred in `area`.
fn skip(canvas: u64, area: Rect, size: f32, dir: f32, ink: gfx::Color) {
    let b = area.center(size * 1.4, size);
    let half = b.w / 2.0;
    triangle(canvas, Rect::new(b.x, b.y, half + 1.0, b.h), dir, ink);
    triangle(canvas, Rect::new(b.x + half - 1.0, b.y, half + 1.0, b.h), dir, ink);
}

/// The icon for a place, centred in `area`.
fn place_icon(canvas: u64, place: Place, area: Rect, ink: gfx::Color) {
    let b = area.center(22.0, 22.0);
    let c = gfx::Point { x: b.x + 11.0, y: b.y + 11.0 };
    match place {
        Place::ListenNow => {
            let _ = canvas2d::fill_circle(canvas, c, 11.0, ink);
            triangle(canvas, Rect::new(c.x - 3.0, c.y - 5.5, 9.0, 11.0), 1.0, WHITE);
        }
        Place::Library => {
            for (i, w) in [22.0f32, 18.0, 14.0].iter().enumerate() {
                let row = Rect::new(c.x - w / 2.0, b.bottom() - 6.0 - i as f32 * 8.0, *w, 6.0);
                let _ = canvas2d::fill_round_rect(canvas, r(row), radii(2.0), ink);
            }
        }
        Place::Recent => {
            let _ = canvas2d::stroke_circle(canvas, c, 10.0, 2.2, ink);
            let _ = canvas2d::fill_rect(canvas, r(Rect::new(c.x - 1.1, c.y - 6.5, 2.2, 7.5)), ink);
            let _ = canvas2d::fill_rect(canvas, r(Rect::new(c.x - 1.1, c.y - 1.1, 6.0, 2.2)), ink);
        }
    }
}

/// "1:05" from milliseconds.
fn clock(ms: u32) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

impl App {
    fn sleeve(&self, i: usize, area: Rect, corner: f32) {
        let c = self.canvas;
        match self.sleeves.get(i).copied().flatten() {
            Some(handle) => {
                let _ = canvas_images::draw_round(c, handle, r(area), radii(corner));
            }
            None => {
                let rec = &RECORDS[i];
                let _ = canvas2d::fill_round_rect(c, r(area), radii(corner), color(rec.top));
            }
        }
    }

    fn sleeve_with_shadow(&self, i: usize, area: Rect, corner: f32, shadow: f32) {
        let _ = canvas2d::drop_shadow_round_rect(self.canvas, r(area), radii(corner), shadow, rgba(0.0, 0.0, 0.0, 0.18));
        self.sleeve(i, area, corner);
    }

    fn records(&self) -> Vec<usize> {
        let mut all: Vec<usize> = (0..RECORDS.len()).collect();
        match self.place {
            Place::ListenNow => all,
            Place::Library => {
                all.sort_by_key(|&i| RECORDS[i].title);
                all
            }
            Place::Recent => {
                all.sort_by_key(|&i| core::cmp::Reverse(RECORDS[i].year));
                all
            }
        }
    }

    fn place_title(&self) -> &'static str {
        PLACES.iter().find(|(p, _)| *p == self.place).map(|(_, t)| *t).unwrap_or("")
    }

    fn draw(&mut self) {
        let c = self.canvas;
        let s = self.screen;
        self.hits.clear();
        let _ = canvas2d::clear(c, BG);

        if self.view == View::Playing {
            self.draw_player();
            return;
        }

        // A phone turned sideways has little height: a narrower sidebar.
        let nav = s.nav(s.safe_area(), if s.short() { 180.0 } else { 232.0 });
        if let Some(side) = nav.sidebar {
            self.draw_sidebar(side);
        }
        if let Some(tabs) = nav.tabs {
            self.draw_tabs(tabs);
        }
        let mut content = nav.content;
        // What is playing sits above the tabs on a phone and along the
        // bottom of the content on a wider screen: the same bar, carved out
        // of the content either way.
        if self.player.now.is_some() {
            let bar = content.take_bottom(if s.short() { 58.0 } else { 68.0 });
            self.draw_mini_player(bar);
        }
        let side = if nav.sidebar.is_some() { 28.0 } else { 20.0 };
        let content = content.inset(0.0, side, 0.0, side);
        self.view_h = content.h;
        let _ = canvas2d::set_clip(c, content.x - side, content.y, content.w + side * 2.0, content.h);
        match self.view {
            View::Record(i) => self.draw_record(content, i),
            _ => match self.place {
                Place::ListenNow => self.draw_listen_now(content),
                _ => self.draw_collection(content),
            },
        }
        let _ = canvas2d::clear_clip(c);
    }

    fn draw_sidebar(&mut self, side: Rect) {
        let c = self.canvas;
        let s = self.screen;
        // The panel runs to the window's edges, under any covered strip; its
        // rows stay inside `side`.
        let _ = canvas2d::fill_rect(c, r(Rect::new(0.0, 0.0, side.right(), s.height)), PANEL);
        let _ = canvas2d::fill_rect(c, r(Rect::new(side.right() - 0.5, 0.0, 0.5, s.height)), HAIRLINE);
        let mut rows = side.pad(14.0);
        let brand = rows.take_top(if s.short() { 40.0 } else { 58.0 });
        let _ = canvas2d::fill_round_rect(c, r(Rect::new(brand.x + 6.0, brand.y + brand.h / 2.0 - 13.0, 26.0, 26.0)), radii(7.0), ACCENT);
        triangle(c, Rect::new(brand.x + 16.0, brand.y + brand.h / 2.0 - 6.0, 9.0, 12.0), 1.0, WHITE);
        text(c, "Shelf", brand.x + 42.0, brand.y + brand.h / 2.0 + 8.0, 22.0, INK, 800);
        for (place, label) in PLACES {
            let row = rows.take_top(s.tap() + 6.0);
            let here = place == self.place && self.view == View::Place;
            if here {
                let _ = canvas2d::fill_round_rect(c, r(row.inset(2.0, 0.0, 2.0, 0.0)), radii(10.0), rgba(0.0, 0.0, 0.0, 0.06));
            }
            let ink = if place == self.place { ACCENT } else { INK };
            place_icon(c, place, Rect::new(row.x + 6.0, row.y, 30.0, row.h), ink);
            text(c, label, row.x + 44.0, row.y + row.h / 2.0 + 5.5, 16.0, INK, if here { 650 } else { 500 });
            self.hits.push((row, Hit::Go(place)));
        }
    }

    fn draw_tabs(&mut self, tabs: Rect) {
        let c = self.canvas;
        let s = self.screen;
        // The bar runs down under the home bar to the screen's edge.
        let _ = canvas2d::fill_rect(c, r(Rect::new(0.0, tabs.y, s.width, s.height - tabs.y)), rgba(0.98, 0.98, 0.985, 1.0));
        let _ = canvas2d::fill_rect(c, r(Rect::new(0.0, tabs.y, s.width, 0.5)), HAIRLINE);
        for ((place, label), cell) in PLACES.iter().zip(tabs.columns(PLACES.len(), 0.0)) {
            let ink = if *place == self.place { ACCENT } else { MUTED };
            place_icon(c, *place, Rect::new(cell.x, cell.y + 6.0, cell.w, 26.0), ink);
            let w = width_of(c, label, 11.5, 600);
            text(c, label, cell.x + (cell.w - w) / 2.0, cell.y + 47.0, 11.5, ink, 600);
            self.hits.push((cell, Hit::Go(*place)));
        }
    }

    fn draw_mini_player(&mut self, bar: Rect) {
        let c = self.canvas;
        let s = self.screen;
        let Some((record, track)) = self.player.now else {
            return;
        };
        let rec = &RECORDS[record];
        let card = bar.inset(6.0, 10.0, 6.0, 10.0);
        let _ = canvas2d::drop_shadow_round_rect(c, r(card), radii(14.0), 20.0, rgba(0.0, 0.0, 0.0, 0.14));
        let _ = canvas2d::fill_round_rect(c, r(card), radii(14.0), rgba(0.985, 0.985, 0.99, 1.0));
        let mut row = card.pad(7.0);
        let art = row.take_left(row.h);
        self.sleeve(record, art, 7.0);
        let next = row.take_right(s.tap());
        let toggle = row.take_right(s.tap());
        play_pause(c, toggle, 18.0, self.player.playing, INK);
        skip(c, next, 13.0, 1.0, INK);
        let words = row.inset(0.0, 6.0, 0.0, 12.0);
        let title = fit(c, rec.tracks.get(track).copied().unwrap_or(rec.title), 15.0, 600, words.w);
        text(c, &title, words.x, words.y + words.h / 2.0 - 2.0, 15.0, INK, 600);
        let artist = fit(c, rec.artist, 13.0, 450, words.w);
        text(c, &artist, words.x, words.y + words.h / 2.0 + 15.0, 13.0, MUTED, 450);
        // Progress along the card's bottom edge.
        let done = self.progress();
        let line = Rect::new(card.x + 14.0, card.bottom() - 3.0, card.w - 28.0, 2.0);
        let _ = canvas2d::fill_rect(c, r(Rect { w: line.w * done, ..line }), ACCENT);
        self.hits.push((Rect::new(card.x, card.y, words.right() - card.x, card.h), Hit::OpenPlayer));
        self.hits.push((toggle, Hit::Toggle));
        self.hits.push((next, Hit::Next));
    }

    fn progress(&self) -> f32 {
        let length = self.player.length_ms();
        if length == 0 {
            0.0
        } else {
            (self.player.position_ms() as f32 / length as f32).min(1.0)
        }
    }

    /// A fade the shape of `card`: clear over its top third, darkening to
    /// 55% black at its bottom edge.
    fn fade_for(&mut self, card: Rect) -> Option<u64> {
        let w = 48u32;
        let h = ((w as f32 * card.h / card.w.max(1.0)) as u32).max(2);
        if let Some(((fw, fh), handle)) = self.fade {
            if (fw, fh) == (w, h) {
                return Some(handle);
            }
            canvas_images::forget(self.canvas, handle);
        }
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            let t = ((y as f32 / h as f32 - 0.35) / 0.65).max(0.0);
            let a = (t * t * 0.55 * 255.0) as u8;
            for _ in 0..w {
                px.extend_from_slice(&[0, 0, 0, a]);
            }
        }
        let handle = canvas_images::keep(self.canvas, w, h, &px).ok()?;
        self.fade = Some(((w, h), handle));
        Some(handle)
    }

    /// A section heading, cut from the top of `area`.
    fn heading(&self, area: &mut Rect, title: &str) {
        let s = self.screen;
        let band = area.take_top(if s.short() { 40.0 } else { 46.0 });
        let t = fit(self.canvas, title, 22.0, 750, band.w);
        text(self.canvas, &t, band.x, band.bottom() - 12.0, 22.0, INK, 750);
    }

    /// The page's large title, cut from the top of `area`.
    fn large_title(&self, area: &mut Rect, title: &str) {
        let s = self.screen;
        let size = if s.short() { 26.0 } else { 34.0 };
        let band = area.take_top(if s.short() { 48.0 } else { 64.0 });
        let t = fit(self.canvas, title, size, 800, band.w);
        text(self.canvas, &t, band.x, band.bottom() - 12.0, size, INK, 800);
    }

    /// A grid of records: sleeve, title, artist. Returns its bottom.
    fn record_grid(&mut self, area: Rect, list: &[usize]) -> f32 {
        let c = self.canvas;
        let s = self.screen;
        let min_w = if s.short() { 120.0 } else if s.compact() { 150.0 } else { 172.0 };
        let gap = if s.compact() { 16.0 } else { 22.0 };
        let probe = area.grid(1, min_w, 1.0, gap);
        let art = probe.first().map(|cell| cell.w).unwrap_or(min_w);
        let cells = area.grid(list.len(), min_w, art + 50.0, gap);
        for (&i, cell) in list.iter().zip(cells.iter()) {
            if cell.bottom() < 0.0 || cell.y > s.height {
                continue;
            }
            let rec = &RECORDS[i];
            let sleeve = Rect::new(cell.x, cell.y, cell.w, cell.w);
            self.sleeve_with_shadow(i, sleeve, 10.0, 12.0);
            let t = fit(c, rec.title, 14.5, 600, cell.w);
            text(c, &t, cell.x, sleeve.bottom() + 20.0, 14.5, INK, 600);
            let a = fit(c, rec.artist, 13.0, 450, cell.w);
            text(c, &a, cell.x, sleeve.bottom() + 37.0, 13.0, MUTED, 450);
            self.hits.push((*cell, Hit::Open(i)));
        }
        cells.last().map(|cell| cell.bottom()).unwrap_or(area.y)
    }

    /// How many records one grid row holds in `area`.
    fn per_row(&self, area: Rect) -> usize {
        let s = self.screen;
        let min_w = if s.short() { 120.0 } else if s.compact() { 150.0 } else { 172.0 };
        let gap = if s.compact() { 16.0 } else { 22.0 };
        area.grid(16, min_w, 1.0, gap).iter().filter(|cell| cell.y == area.y).count().max(1)
    }

    /// Rows of songs: sleeve, title, artist, length. Returns its bottom.
    fn song_rows(&mut self, mut area: Rect, songs: &[(usize, usize)]) -> f32 {
        let c = self.canvas;
        let s = self.screen;
        let row_h = s.tap() + 16.0;
        let mut bottom = area.y;
        for &(record, track) in songs {
            let row = area.take_top(row_h);
            bottom = row.bottom();
            if row.bottom() < 0.0 || row.y > s.height {
                continue;
            }
            let rec = &RECORDS[record];
            let art = Rect::new(row.x, row.y + 8.0, row.h - 16.0, row.h - 16.0);
            self.sleeve(record, art, 6.0);
            let length = clock(self.lengths[record][track]);
            let lw = width_of(c, &length, 14.0, 450);
            let words = Rect::new(art.right() + 12.0, row.y, row.right() - art.right() - 24.0 - lw, row.h);
            let playing = self.player.now == Some((record, track));
            let t = fit(c, rec.tracks[track], 16.0, 500, words.w);
            text(c, &t, words.x, row.y + row.h / 2.0 - 2.0, 16.0, if playing { ACCENT } else { INK }, 500);
            let a = fit(c, &format!("{} \u{00b7} {}", rec.artist, rec.title), 13.5, 450, words.w);
            text(c, &a, words.x, row.y + row.h / 2.0 + 16.0, 13.5, MUTED, 450);
            text(c, &length, row.right() - lw, row.y + row.h / 2.0 + 5.0, 14.0, MUTED, 450);
            let _ = canvas2d::fill_rect(c, r(Rect::new(words.x, row.bottom() - 0.5, row.right() - words.x, 0.5)), HAIRLINE);
            self.hits.push((row, Hit::Play(record, track)));
        }
        bottom
    }

    fn draw_listen_now(&mut self, content: Rect) {
        let c = self.canvas;
        let s = self.screen;
        let top = content.y;
        let mut page = Rect { y: content.y - self.scroll, h: f32::MAX / 4.0, ..content };
        self.large_title(&mut page, "Listen Now");

        // The top pick: a big card with its sleeve behind the words.
        let pick = 4; // Static Bloom: its low sun reads well on a wide card
        let label = page.take_top(24.0);
        text(c, "TOP PICK", label.x, label.bottom() - 6.0, 12.0, MUTED, 650);
        let hero_h = if s.short() { 170.0 } else { (page.w * 0.66).clamp(200.0, 340.0) };
        let hero = page.take_top(hero_h);
        let _ = canvas2d::drop_shadow_round_rect(c, r(hero), radii(18.0), 22.0, rgba(0.0, 0.0, 0.0, 0.2));
        self.sleeve(pick, hero, 18.0);
        if let Some(fade) = self.fade_for(hero) {
            let _ = canvas_images::draw_round(c, fade, r(hero), radii(18.0));
        }
        let rec = &RECORDS[pick];
        let words = hero.pad(18.0);
        let play = Rect::new(words.right() - 96.0, words.bottom() - s.tap(), 96.0, s.tap());
        let tw = words.w - play.w - 12.0;
        let t = fit(c, rec.title, 26.0, 800, tw);
        text(c, &t, words.x, words.bottom() - 26.0, 26.0, WHITE, 800);
        let a = fit(c, &format!("{} \u{00b7} {}", rec.artist, rec.genre), 15.0, 500, tw);
        text(c, &a, words.x, words.bottom() - 4.0, 15.0, rgba(1.0, 1.0, 1.0, 0.85), 500);
        let _ = canvas2d::fill_round_rect(c, r(play), radii(play.h / 2.0), WHITE);
        play_pause(c, Rect::new(play.x + 12.0, play.y, 20.0, play.h), 14.0, false, INK);
        text(c, "Play", play.x + 38.0, play.y + play.h / 2.0 + 5.5, 16.0, INK, 700);
        self.hits.push((hero, Hit::Open(pick)));
        self.hits.push((play, Hit::Play(pick, 0)));
        page.take_top(10.0);

        // Recently played: exactly one full row, however many that is.
        self.heading(&mut page, "Recently Played");
        let n = self.per_row(page).max(2);
        let recent: Vec<usize> = [2, 0, 4, 6, 1, 5, 3, 7].iter().copied().take(n).collect();
        let bottom = self.record_grid(page, &recent);
        page.take_top(bottom - page.y + 8.0);

        self.heading(&mut page, "Top Songs");
        let songs = [(2, 0), (9, 1), (0, 0), (4, 2), (6, 1), (11, 0)];
        let bottom = self.song_rows(page, &songs);
        page.take_top(bottom - page.y + 8.0);

        self.heading(&mut page, "Made for You");
        let rest: Vec<usize> = (0..RECORDS.len()).filter(|i| !recent.contains(i)).collect();
        let bottom = self.record_grid(page, &rest);
        self.content_h = bottom + self.scroll - top + 24.0;
    }

    fn draw_collection(&mut self, content: Rect) {
        let c = self.canvas;
        let s = self.screen;
        let top = content.y;
        let mut page = Rect { y: content.y - self.scroll, h: f32::MAX / 4.0, ..content };
        let title = self.place_title();
        self.large_title(&mut page, title);
        let list = self.records();
        let bottom = if self.place == Place::Library && s.compact() {
            // A phone's library is a list: more of it fits, and every row is
            // a full-width target.
            let mut rows = page;
            let mut bottom = rows.y;
            for i in list {
                let row = rows.take_top(s.tap() + 28.0);
                bottom = row.bottom();
                if row.bottom() < 0.0 || row.y > s.height {
                    continue;
                }
                let rec = &RECORDS[i];
                let art = Rect::new(row.x, row.y + 8.0, row.h - 16.0, row.h - 16.0);
                self.sleeve(i, art, 8.0);
                let words = Rect::new(art.right() + 14.0, row.y, row.right() - art.right() - 34.0, row.h);
                let t = fit(c, rec.title, 17.0, 600, words.w);
                text(c, &t, words.x, row.y + row.h / 2.0 - 2.0, 17.0, INK, 600);
                let a = fit(c, &format!("{} \u{00b7} {}", rec.artist, rec.year), 14.0, 450, words.w);
                text(c, &a, words.x, row.y + row.h / 2.0 + 17.0, 14.0, MUTED, 450);
                // A chevron: two short diagonal strokes of dots.
                let cx = row.right() - 8.0;
                let cy = row.y + row.h / 2.0;
                for k in 0..7 {
                    let f = k as f32;
                    let _ = canvas2d::fill_rect(c, r(Rect::new(cx - 6.0 + f, cy - 7.0 + f, 2.0, 2.0)), MUTED);
                    let _ = canvas2d::fill_rect(c, r(Rect::new(cx - 6.0 + f, cy + 5.0 - f, 2.0, 2.0)), MUTED);
                }
                let _ = canvas2d::fill_rect(c, r(Rect::new(words.x, row.bottom() - 0.5, row.right() - words.x, 0.5)), HAIRLINE);
                self.hits.push((row, Hit::Open(i)));
            }
            bottom
        } else {
            self.record_grid(page, &list)
        };
        self.content_h = bottom + self.scroll - top + 24.0;
    }

    fn draw_record(&mut self, content: Rect, i: usize) {
        let c = self.canvas;
        let s = self.screen;
        let rec = &RECORDS[i];
        let top = content.y;
        let mut page = Rect { y: content.y - self.page_scroll, h: f32::MAX / 4.0, ..content };
        let back = page.take_top(s.tap() + 4.0);
        let label = format!("\u{2039} {}", self.place_title());
        let bw = width_of(c, &label, 17.0, 500);
        text(c, &label, back.x, back.y + back.h / 2.0 + 6.0, 17.0, ACCENT, 500);
        self.hits.push((Rect::new(back.x - 8.0, back.y, bw + 24.0, back.h), Hit::Back));

        // Upright phone: the sleeve above, centred. Wider: beside the words.
        if s.compact() {
            let size = (page.w * 0.72).min(300.0);
            let band = page.take_top(size + 24.0);
            let sleeve = Rect::new(band.x + (band.w - size) / 2.0, band.y + 8.0, size, size);
            self.sleeve_with_shadow(i, sleeve, 14.0, 26.0);
            let words = page.take_top(96.0);
            text_center(c, rec.title, words, words.y + 30.0, 24.0, INK, 800);
            text_center(c, rec.artist, words, words.y + 56.0, 19.0, ACCENT, 500);
            let meta = format!("{} \u{00b7} {}", rec.genre.to_uppercase(), rec.year);
            text_center(c, &meta, words, words.y + 80.0, 12.5, MUTED, 600);
            let buttons = page.take_top(s.tap() + 20.0);
            self.play_buttons(i, Rect { h: s.tap(), ..buttons });
        } else {
            let size = (page.w * 0.34).clamp(160.0, 280.0);
            let mut row = page.take_top(size + 24.0);
            let sleeve = row.take_left(size);
            let sleeve = Rect { h: size, ..sleeve };
            self.sleeve_with_shadow(i, sleeve, 14.0, 26.0);
            let words = row.inset((size - 150.0).max(0.0) / 2.0, 0.0, 0.0, 28.0);
            let t = fit(c, rec.title, 30.0, 800, words.w);
            text(c, &t, words.x, words.y + 34.0, 30.0, INK, 800);
            let a = fit(c, rec.artist, 20.0, 500, words.w);
            text(c, &a, words.x, words.y + 64.0, 20.0, ACCENT, 500);
            let meta = format!("{} \u{00b7} {}", rec.genre.to_uppercase(), rec.year);
            text(c, &meta, words.x, words.y + 88.0, 12.5, MUTED, 600);
            // Play and Shuffle under the words, beside the sleeve.
            let buttons = Rect::new(words.x, words.y + 106.0, words.w.min(320.0), s.tap());
            self.play_buttons(i, buttons);
        }
        // The tracks.
        let mut list = page;
        let mut bottom = list.y;
        let here = self.player.now.filter(|(r, _)| *r == i).map(|(_, t)| t);
        for (n, track) in rec.tracks.iter().enumerate() {
            let row = list.take_top(s.tap() + 8.0);
            bottom = row.bottom();
            let _ = canvas2d::fill_rect(c, r(Rect::new(row.x, row.y, row.w, 0.5)), HAIRLINE);
            let current = here == Some(n);
            if current && self.player.playing {
                // Three bars for "playing now", where the number was.
                for (k, h) in [0.45f32, 0.85, 0.6].iter().enumerate() {
                    let bh = 14.0 * h;
                    let _ = canvas2d::fill_rect(c, r(Rect::new(row.x + k as f32 * 5.0, row.y + row.h / 2.0 + 7.0 - bh, 3.0, bh)), ACCENT);
                }
            } else {
                text(c, &format!("{}", n + 1), row.x + 2.0, row.y + row.h / 2.0 + 6.0, 16.0, MUTED, 500);
            }
            let length = clock(self.lengths[i][n]);
            let lw = width_of(c, &length, 15.0, 450);
            let name = fit(c, track, 17.0, 500, row.w - 40.0 - lw - 12.0);
            text(c, &name, row.x + 34.0, row.y + row.h / 2.0 + 6.0, 17.0, if current { ACCENT } else { INK }, 500);
            text(c, &length, row.right() - lw, row.y + row.h / 2.0 + 6.0, 15.0, MUTED, 450);
            self.hits.push((row, Hit::Play(i, n)));
        }
        let total: u32 = self.lengths[i].iter().sum();
        let footer = Rect::new(list.x, bottom + 14.0, list.w, 20.0);
        let line = format!("{} songs, {} minutes", rec.tracks.len(), (total + 30_000) / 60_000);
        text(c, &line, footer.x, footer.y + 14.0, 13.5, MUTED, 450);
        self.content_h = footer.bottom() + self.page_scroll - top + 24.0;
    }

    /// Play and Shuffle, side by side, filling `area`.
    fn play_buttons(&mut self, i: usize, area: Rect) {
        let c = self.canvas;
        let halves = area.columns(2, 12.0);
        let (play, shuffle) = (halves[0], halves[1]);
        let here = self.player.now.map(|(r, _)| r) == Some(i);
        let playing = here && self.player.playing;
        let _ = canvas2d::fill_round_rect(c, r(play), radii(12.0), ACCENT);
        let label = if playing { "Pause" } else { "Play" };
        let lw = width_of(c, label, 16.0, 700);
        let start = play.x + (play.w - lw - 26.0) / 2.0;
        play_pause(c, Rect::new(start, play.y, 16.0, play.h), 14.0, playing, WHITE);
        text(c, label, start + 26.0, play.y + play.h / 2.0 + 5.5, 16.0, WHITE, 700);
        self.hits.push((play, if here { Hit::Toggle } else { Hit::Play(i, 0) }));
        let _ = canvas2d::fill_round_rect(c, r(shuffle), radii(12.0), PILL);
        let sw = width_of(c, "Shuffle", 16.0, 700);
        text(c, "Shuffle", shuffle.x + (shuffle.w - sw) / 2.0, shuffle.y + shuffle.h / 2.0 + 5.5, 16.0, ACCENT, 700);
        self.hits.push((shuffle, Hit::Shuffle(i)));
    }

    /// Now playing, over the whole screen, in the record's own colours.
    fn draw_player(&mut self) {
        let c = self.canvas;
        let s = self.screen;
        let Some((record, track)) = self.player.now else {
            self.view = View::Place;
            return;
        };
        let rec = &RECORDS[record];
        let _ = canvas2d::linear_gradient_stops(
            c,
            r(s.area()),
            90.0,
            &[
                gfx::GradientStop { offset: 0.0, color: darker(rec.top, 0.62) },
                gfx::GradientStop { offset: 1.0, color: darker(rec.bottom, 0.45) },
            ],
        );
        let mut area = s.safe_area().inset(0.0, 24.0, 12.0, 24.0);
        // A grabber to close, at the top, the way a sheet closes.
        let grab = area.take_top(s.tap());
        let handle = grab.center(40.0, 5.0);
        let _ = canvas2d::fill_round_rect(c, r(handle), radii(2.5), rgba(1.0, 1.0, 1.0, 0.45));
        self.hits.push((Rect::new(0.0, 0.0, s.width, grab.bottom()), Hit::ClosePlayer));

        // Upright: the sleeve above the controls. Wide or turned: beside.
        let (sleeve_area, mut controls) = if s.compact() || s.height > s.width * 1.2 {
            let size = area.w.min(area.h * 0.5);
            let band = area.take_top(size + 28.0);
            (Rect::new(band.x + (band.w - size) / 2.0, band.y + 8.0, size, size), area)
        } else {
            let size = (area.h - 8.0).min(area.w * 0.45);
            let col = area.take_left(size + 40.0);
            (Rect::new(col.x, col.y + (col.h - size) / 2.0, size, size), area)
        };
        self.sleeve_with_shadow(record, sleeve_area, 16.0, 36.0);

        // Title, artist, the scrubber, the transport.
        let block_h = 230.0;
        if controls.h > block_h {
            controls = Rect { y: controls.y + (controls.h - block_h) / 2.0, h: block_h, ..controls };
        }
        let words = controls.take_top(64.0);
        let t = fit(c, rec.tracks[track], 24.0, 750, words.w);
        text(c, &t, words.x, words.y + 28.0, 24.0, WHITE, 750);
        let a = fit(c, &format!("{} \u{00b7} {}", rec.artist, rec.title), 17.0, 500, words.w);
        text(c, &a, words.x, words.y + 54.0, 17.0, rgba(1.0, 1.0, 1.0, 0.72), 500);

        let scrub = controls.take_top(52.0);
        let line = Rect::new(scrub.x, scrub.y + 16.0, scrub.w, 5.0);
        let _ = canvas2d::fill_round_rect(c, r(line), radii(2.5), rgba(1.0, 1.0, 1.0, 0.25));
        let done = self.progress();
        let _ = canvas2d::fill_round_rect(c, r(Rect { w: (line.w * done).max(5.0), ..line }), radii(2.5), WHITE);
        let _ = canvas2d::fill_circle(c, gfx::Point { x: line.x + line.w * done, y: line.y + 2.5 }, 7.0, WHITE);
        let pos = clock(self.player.position_ms());
        let left = format!("-{}", clock(self.player.length_ms().saturating_sub(self.player.position_ms())));
        text(c, &pos, line.x, line.y + 26.0, 13.0, rgba(1.0, 1.0, 1.0, 0.72), 500);
        let lw = width_of(c, &left, 13.0, 500);
        text(c, &left, line.right() - lw, line.y + 26.0, 13.0, rgba(1.0, 1.0, 1.0, 0.72), 500);
        self.hits.push((Rect::new(scrub.x - 8.0, scrub.y, scrub.w + 16.0, 34.0), Hit::Seek(line)));

        let transport = controls.take_top(96.0);
        let cells = transport.center(transport.w.min(320.0), 88.0).columns(3, 0.0);
        skip(c, cells[0], 26.0, -1.0, WHITE);
        play_pause(c, cells[1], 40.0, self.player.playing, WHITE);
        skip(c, cells[2], 26.0, 1.0, WHITE);
        self.hits.push((cells[0], Hit::Prev));
        self.hits.push((cells[1], Hit::Toggle));
        self.hits.push((cells[2], Hit::Next));
        self.content_h = 0.0;
    }

    /// The next track, onto the next record after the last one.
    fn next(&mut self) {
        let Some((record, track)) = self.player.now else {
            return;
        };
        if track + 1 < RECORDS[record].tracks.len() {
            self.player.play(record, track + 1);
        } else {
            self.player.play((record + 1) % RECORDS.len(), 0);
        }
    }

    /// Back to the start of this track, or to the one before when it has
    /// barely begun -- the way a music app's back button behaves.
    fn prev(&mut self) {
        let Some((record, track)) = self.player.now else {
            return;
        };
        if self.player.position_ms() > 3000 || track == 0 {
            self.player.play(record, track);
        } else {
            self.player.play(record, track - 1);
        }
    }

    fn tap(&mut self, x: f32, y: f32) -> bool {
        let Some((_, hit)) = self.hits.iter().rev().find(|(rect, _)| rect.contains(x, y)).copied() else {
            return false;
        };
        match hit {
            Hit::Go(place) => {
                if self.place != place {
                    self.scroll = 0.0;
                }
                self.place = place;
                self.view = View::Place;
            }
            Hit::Open(i) => {
                self.view = View::Record(i);
                self.page_scroll = 0.0;
            }
            Hit::Back => self.view = View::Place,
            Hit::Play(record, track) => self.player.play(record, track),
            Hit::Shuffle(record) => {
                self.shuffle_seed = self.shuffle_seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let n = RECORDS[record].tracks.len() as u32;
                self.player.play(record, ((self.shuffle_seed >> 16) % n) as usize);
            }
            Hit::Toggle => self.player.toggle(),
            Hit::Next => self.next(),
            Hit::Prev => self.prev(),
            Hit::OpenPlayer => self.view = View::Playing,
            Hit::ClosePlayer => {
                self.view = match self.player.now {
                    Some((record, _)) => View::Record(record),
                    None => View::Place,
                };
            }
            Hit::Seek(line) => {
                let at = ((x - line.x) / line.w).clamp(0.0, 1.0);
                self.player.seek((at * self.player.length_ms() as f32) as u32);
            }
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
        // Every sleeve crosses into the host once, here.
        let sleeves = RECORDS
            .iter()
            .enumerate()
            .map(|(i, rec)| canvas_images::keep(canvas, art::SIZE, art::SIZE, &art::sleeve(i, rec.top, rec.bottom)).ok())
            .collect();
        let lengths = RECORDS
            .iter()
            .enumerate()
            .map(|(i, rec)| (0..rec.tracks.len()).map(|t| music::Song::new(i, t).length_ms()).collect())
            .collect();
        let mut app = App {
            canvas,
            screen: Screen::of(win),
            place: Place::ListenNow,
            view: View::Place,
            scroll: 0.0,
            page_scroll: 0.0,
            content_h: 0.0,
            view_h: 0.0,
            hits: Vec::new(),
            player: music::Player::new(),
            sleeves,
            lengths,
            shuffle_seed: (krate::time::monotonic_nanos() & 0xffff_ffff) as u32,
            fade: None,
        };
        let args = krate::bindings::krate::io::args::raw();
        let quick = args.split('\n').any(|a| a == "quick");
        // `play`, `record` and `library`: start somewhere other than Listen
        // Now, so a check (or a person in a hurry) sees that screen at once.
        for arg in args.split('\n') {
            match arg {
                "play" => {
                    app.player.play(0, 0);
                    app.view = View::Playing;
                }
                "record" => app.view = View::Record(0),
                "library" => app.place = Place::Library,
                _ => {}
            }
        }
        let mut dirty = true;
        let mut pressed: Option<(f32, f32)> = None;
        let mut shown_second = u32::MAX;
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
            // While a song plays, wake often enough to keep it in time and
            // move the clock on screen once a second.
            let wait = if app.player.playing { Some(15) } else { None };
            let event = events::wait(wait);
            if app.player.now.is_some() {
                if app.player.tick() {
                    app.next();
                    dirty = true;
                }
                let second = app.player.position_ms() / 1000;
                if second != shown_second {
                    shown_second = second;
                    dirty = true;
                }
            }
            match event {
                Some(types::Event::CloseRequested(_)) => return 0,
                Some(types::Event::Resized(_)) => {
                    // A phone turned, a window narrowed: lay out again.
                    app.screen = Screen::of(win);
                    dirty = true;
                }
                Some(types::Event::Wheel(wheel)) => {
                    let max = (app.content_h - app.view_h).max(0.0);
                    let scroll = match app.view {
                        View::Record(_) => &mut app.page_scroll,
                        View::Place => &mut app.scroll,
                        View::Playing => continue,
                    };
                    let next = (*scroll + wheel.dy).clamp(0.0, max);
                    if next != *scroll {
                        *scroll = next;
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

//! Krate's own screens on a phone: Library (your apps) and Discover (the
//! apps on the hub).
//!
//! A trusted guest the phone player runs. The player does what a sandboxed
//! screen should not -- the network, picture decoding -- and writes what to
//! show into this app's folder:
//!
//! - `library.txt`: one app per line, `name<TAB>line<TAB>art`
//! - `discover.txt`: one app per line,
//!   `hub id<TAB>name<TAB>author<TAB>category<TAB>description<TAB>art<TAB>size`
//! - `art/*.rgba`: pictures, an 8-byte header (width, height, little-endian
//!   u32) then straight RGBA. `-` names no picture.
//! - `start.txt` (optional): `discover` opens on Discover.
//!
//! The answer is one stdout line: `home:open:<n>` (the n-th library app),
//! `home:get:<hub id>`, `home:files` (open a .krate from Files), or
//! `home:closed`.
//!
//! Every picture is kept once (krate:gfx/canvas-images) and drawn by handle,
//! and only what is on screen is drawn: a scrolling list of screenshots
//! sent as pixels every frame lagged on an iPhone and broke its pictures.

#![no_std]

extern crate alloc;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use krate::bindings::krate::fs::files;
use krate::bindings::krate::io::stdio;
use krate::bindings::krate::time::clock;
use krate::gfx::{canvas2d, canvas_images, types as gfx};
use krate::ui::{events, tree, types, window};

const ROOT_ID: u64 = 1;
const CANVAS_ID: u64 = 2;

/// The Krate mark, 120 px square, straight RGBA.
const MARK: &[u8] = include_bytes!("../assets/mark-120.rgba");
const MARK_SIZE: u32 = 120;

// A light, quiet surface so the apps' own pictures carry the colour; the
// accent is the mark's own blue.
const BG: gfx::Color = rgb(0.949, 0.953, 0.969);
const CARD: gfx::Color = rgb(1.0, 1.0, 1.0);
const INK: gfx::Color = rgb(0.043, 0.047, 0.059);
const MUTED: gfx::Color = rgb(0.43, 0.43, 0.45);
const HAIRLINE: gfx::Color = rgb(0.88, 0.89, 0.91);
const ACCENT: gfx::Color = rgb(0.23, 0.30, 0.95);
const PILL: gfx::Color = rgb(0.92, 0.93, 0.98);
const BLACK: gfx::Color = rgb(0.043, 0.043, 0.047);

const fn rgb(r: f32, g: f32, b: f32) -> gfx::Color {
    gfx::Color { r, g, b, a: 1.0 }
}

const fn rgba(r: f32, g: f32, b: f32, a: f32) -> gfx::Color {
    gfx::Color { r, g, b, a }
}

fn rect(x: f32, y: f32, width: f32, height: f32) -> gfx::Rect {
    gfx::Rect {
        x,
        y,
        width,
        height,
    }
}

fn radii(all: f32) -> gfx::CornerRadii {
    gfx::CornerRadii {
        top_left: all,
        top_right: all,
        bottom_right: all,
        bottom_left: all,
    }
}

fn bottom_radii(r: f32) -> gfx::CornerRadii {
    gfx::CornerRadii {
        top_left: 0.0,
        top_right: 0.0,
        bottom_right: r,
        bottom_left: r,
    }
}

fn style(weight: u16, spacing: f32) -> gfx::TextStyle {
    gfx::TextStyle {
        weight,
        italic: false,
        letter_spacing: spacing,
        family: gfx::FontFamily::Sans,
    }
}

fn pt(x: f32, y: f32) -> gfx::Point {
    gfx::Point { x, y }
}

fn out(line: &str) {
    let handle = stdio::stdout();
    let _ = handle.write(line.as_bytes());
    let _ = handle.write(b"\n");
}

fn read_all(path: &str) -> Option<Vec<u8>> {
    let size = files::stat(path).ok()?.size as usize;
    let file = files::open(path, files::OpenMode::Read).ok()?;
    let mut bytes = Vec::with_capacity(size);
    while bytes.len() < size {
        let chunk = file.read((size - bytes.len()).min(1 << 20) as u32).ok()?;
        if chunk.is_empty() {
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    Some(bytes)
}

fn lines(path: &str) -> Vec<Vec<String>> {
    let Some(bytes) = read_all(path) else {
        return Vec::new();
    };
    let text = core::str::from_utf8(&bytes).unwrap_or("");
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.split('\t').map(|f| f.trim().to_string()).collect())
        .collect()
}

/// A picture kept by the host: its handle, its size, and its overall colour
/// (for the backdrop a tall picture sits on).
#[derive(Clone, Copy)]
struct Kept {
    handle: u64,
    width: u32,
    height: u32,
    tint: (f32, f32, f32),
}

/// Read a picture file and keep it for the canvas.
fn keep_art(canvas: u64, name: &str) -> Option<Kept> {
    if name == "-" || name.is_empty() {
        return None;
    }
    let bytes = read_all(&format!("art/{name}"))?;
    if bytes.len() < 8 {
        return None;
    }
    let width = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
    let height = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
    let rgba = &bytes[8..];
    if rgba.len() != (width * height * 4) as usize {
        return None;
    }
    let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
    for p in rgba.chunks_exact(4).step_by(97) {
        r += p[0] as u32;
        g += p[1] as u32;
        b += p[2] as u32;
        n += 1;
    }
    let n = n.max(1) as f32 * 255.0;
    let handle = canvas_images::keep(canvas, width, height, rgba).ok()?;
    Some(Kept {
        handle,
        width,
        height,
        tint: (r as f32 / n, g as f32 / n, b as f32 / n),
    })
}

/// A small diagonal gradient in a colour picked from the name, so an app
/// with no picture still keeps a face of its own.
fn keep_gradient(canvas: u64, name: &str) -> Option<Kept> {
    let seed = name
        .bytes()
        .fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    let hues: [([f32; 3], [f32; 3]); 5] = [
        ([0.42, 0.50, 1.0], [0.16, 0.22, 0.86]),
        ([1.0, 0.55, 0.42], [0.82, 0.22, 0.42]),
        ([0.30, 0.82, 0.66], [0.06, 0.45, 0.55]),
        ([0.70, 0.52, 1.0], [0.36, 0.20, 0.78]),
        ([1.0, 0.78, 0.36], [0.94, 0.44, 0.18]),
    ];
    let (a, b) = hues[(seed as usize) % hues.len()];
    let (w, h) = (48u32, 24u32);
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let t =
                (x as f32 / (w - 1) as f32 * 0.6 + y as f32 / (h - 1) as f32 * 0.4).clamp(0.0, 1.0);
            for c in 0..3 {
                px.push(((a[c] * (1.0 - t) + b[c] * t) * 255.0) as u8);
            }
            px.push(255);
        }
    }
    let handle = canvas_images::keep(canvas, w, h, &px).ok()?;
    Some(Kept {
        handle,
        width: w,
        height: h,
        tint: (b[0], b[1], b[2]),
    })
}

/// A dark fade from clear to `alpha`, top to bottom, at the shape it will
/// be drawn (an eighth of the size): text over a picture. Drawn with
/// rounded corners it fills its rectangle, so it must be made in that
/// rectangle's proportions or only its middle shows, as a hard band.
fn keep_fade(canvas: u64, w: u32, h: u32, alpha: f32) -> Option<u64> {
    let (w, h) = (w.max(2), h.max(2));
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let t = y as f32 / (h - 1) as f32;
        let a = (t * t * (3.0 - 2.0 * t) * alpha * 255.0) as u8;
        for _ in 0..w {
            px.extend_from_slice(&[0, 0, 0, a]);
        }
    }
    canvas_images::keep(canvas, w, h, &px).ok()
}

struct LibraryApp {
    name: String,
    line: String,
    art: Option<Kept>,
    initial: bool,
}

struct HubApp {
    /// No picture of its own: a colour tile with its initial.
    initial: bool,
    id: String,
    name: String,
    author: String,
    category: String,
    description: String,
    art: Option<Kept>,
    size: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Library,
    Discover,
}

/// Where something tappable was drawn this frame.
enum Hit {
    Open(usize),
    Get(usize),
    Tab(Tab),
    Files,
}

struct Screen {
    canvas: u64,
    library: Vec<LibraryApp>,
    hub: Vec<HubApp>,
    mark: Option<u64>,
    /// Fades kept by the size they were made for.
    fades: Vec<((u32, u32), u64)>,
    tab: Tab,
    scroll: [f32; 2],
    content: f32,
    hits: Vec<(gfx::Rect, Hit)>,
}

/// A picture drawn to fill a card: a close fit fills it; a tall one (a
/// phone-shaped window) sits whole on a backdrop of its own colour, above
/// the `text_room` (a fraction of the card) kept free for the card's words.
fn picture(
    canvas: u64,
    area: gfx::Rect,
    art: &Kept,
    corner: f32,
    text_room: f32,
) -> Result<(), gfx::GfxError> {
    let ratio = (art.width as f32 / art.height as f32) / (area.width / area.height);
    if ratio > 0.75 && ratio < 1.33 {
        return canvas_images::draw_round(canvas, art.handle, area, radii(corner));
    }
    let (r, g, b) = art.tint;
    canvas2d::fill_round_rect(canvas, area, radii(corner), rgb(r * 0.8, g * 0.8, b * 0.8))?;
    let inset = area.height * 0.07;
    let inner_h = area.height * (1.0 - text_room) - inset * 2.0;
    let inner_w = (inner_h * art.width as f32 / art.height as f32).min(area.width - inset * 2.0);
    let inner = rect(
        area.x + (area.width - inner_w) / 2.0,
        area.y + inset,
        inner_w,
        inner_h,
    );
    canvas2d::drop_shadow_round_rect(canvas, inner, radii(10.0), 14.0, rgba(0.0, 0.0, 0.0, 0.25))?;
    canvas_images::draw_round(canvas, art.handle, inner, radii(10.0))
}

/// A pill button with a word on it.
fn pill_button(
    canvas: u64,
    area: gfx::Rect,
    word: &str,
    fill: gfx::Color,
    ink: gfx::Color,
) -> Result<(), gfx::GfxError> {
    canvas2d::fill_round_rect(canvas, area, radii(area.height / 2.0), fill)?;
    let m = canvas2d::measure_text_styled(canvas, word, 15.0, style(800, 0.5))?;
    canvas2d::draw_text_styled(
        canvas,
        word,
        pt(
            area.x + (area.width - m.width) / 2.0,
            area.y + area.height / 2.0 + 5.5,
        ),
        15.0,
        ink,
        style(800, 0.5),
    )
}

impl Screen {
    fn scroll(&mut self) -> &mut f32 {
        match self.tab {
            Tab::Library => &mut self.scroll[0],
            Tab::Discover => &mut self.scroll[1],
        }
    }

    fn draw(&mut self, w: f32, h: f32) -> Result<(), gfx::GfxError> {
        let canvas = self.canvas;
        self.hits.clear();
        canvas2d::clear(canvas, BG)?;
        let pad = 20.0;
        let col = (w - pad * 2.0).min(600.0);
        let x = (w - col) / 2.0;
        let scroll = *self.scroll();
        let mut y = 58.0 - scroll;

        // The header: the mark and the name, then the screen's title.
        if let Some(mark) = self.mark {
            canvas_images::draw(canvas, mark, rect(x, y, 30.0, 30.0))?;
        }
        canvas2d::draw_text_styled(
            canvas,
            "Krate",
            pt(x + 38.0, y + 22.0),
            19.0,
            INK,
            style(700, -0.3),
        )?;
        y += 78.0;
        let title = match self.tab {
            Tab::Library => "Library",
            Tab::Discover => "Discover",
        };
        canvas2d::draw_text_styled(canvas, title, pt(x, y), 34.0, INK, style(800, -1.0))?;
        y += 22.0;

        y = match self.tab {
            Tab::Library if self.library.is_empty() => self.draw_empty(x, col, y, h)?,
            Tab::Library => self.draw_library(x, col, y, h)?,
            Tab::Discover => self.draw_discover(x, col, y, h)?,
        };
        self.content = y + scroll + 130.0;

        self.draw_bar(w, h)?;
        canvas2d::present(canvas)
    }

    /// True when a band from `top` to `bottom` is on screen at all.
    fn visible(top: f32, bottom: f32, h: f32) -> bool {
        bottom > 0.0 && top < h
    }

    /// A card's foot: a soft dark fade, so white words over a picture read.
    fn card_fade(&mut self, area: gfx::Rect, from: f32, corner: f32) -> Result<(), gfx::GfxError> {
        let top = area.y + area.height * from;
        let (fw, fh) = (area.width, area.y + area.height - top);
        let key = ((fw / 8.0) as u32, (fh / 8.0) as u32);
        let fade = match self.fades.iter().find(|(k, _)| *k == key) {
            Some((_, fade)) => *fade,
            None => {
                let Some(fade) = keep_fade(self.canvas, key.0, key.1, 0.82) else {
                    return Ok(());
                };
                self.fades.push((key, fade));
                fade
            }
        };
        canvas_images::draw_round(
            self.canvas,
            fade,
            rect(area.x, top, fw, fh),
            bottom_radii(corner),
        )
    }

    fn draw_library(&mut self, x: f32, col: f32, mut y: f32, h: f32) -> Result<f32, gfx::GfxError> {
        let canvas = self.canvas;
        y += 18.0;
        for index in 0..self.library.len() {
            let card_h = (col * 0.62).min(260.0);
            let area = rect(x, y, col, card_h);
            if Self::visible(y, y + card_h, h) {
                let app = &self.library[index];
                canvas2d::drop_shadow_round_rect(
                    canvas,
                    area,
                    radii(26.0),
                    22.0,
                    rgba(0.0, 0.0, 0.0, 0.12),
                )?;
                if let Some(art) = &app.art {
                    picture(canvas, area, art, 26.0, 0.30)?;
                }
                if app.initial {
                    let initial: String = app
                        .name
                        .chars()
                        .next()
                        .map(|c| c.to_uppercase().collect())
                        .unwrap_or_default();
                    let m = canvas2d::measure_text_styled(canvas, &initial, 64.0, style(800, 0.0))?;
                    canvas2d::draw_text_styled(
                        canvas,
                        &initial,
                        pt(x + (col - m.width) / 2.0, y + card_h * 0.42),
                        64.0,
                        rgba(1.0, 1.0, 1.0, 0.92),
                        style(800, 0.0),
                    )?;
                }
                self.card_fade(area, 0.45, 26.0)?;
                let app = &self.library[index];
                canvas2d::draw_text_styled(
                    canvas,
                    &app.name,
                    pt(x + 20.0, y + card_h - 42.0),
                    22.0,
                    CARD,
                    style(700, -0.4),
                )?;
                canvas2d::draw_text_styled(
                    canvas,
                    &app.line,
                    pt(x + 20.0, y + card_h - 20.0),
                    14.0,
                    rgba(1.0, 1.0, 1.0, 0.8),
                    style(500, 0.0),
                )?;
                pill_button(
                    canvas,
                    rect(x + col - 92.0, y + card_h - 56.0, 72.0, 34.0),
                    "OPEN",
                    CARD,
                    ACCENT,
                )?;
            }
            self.hits.push((area, Hit::Open(index)));
            y += card_h + 22.0;
        }
        Ok(y)
    }

    fn draw_empty(&mut self, x: f32, col: f32, mut y: f32, h: f32) -> Result<f32, gfx::GfxError> {
        let canvas = self.canvas;
        let cx = x + col / 2.0;
        // Three real apps from the hub, stacked like a hand of cards.
        y += 54.0;
        let picks: Vec<Kept> = self
            .hub
            .iter()
            .filter(|a| !a.initial)
            .filter_map(|a| a.art)
            .take(3)
            .collect();
        let deck = [
            (-56.0f32, 24.0f32, 0.84f32, 1usize),
            (56.0, 24.0, 0.84, 2),
            (0.0, 0.0, 1.0, 0),
        ];
        for (dx, dy, s, pick) in deck {
            let Some(art) = picks.get(pick) else {
                continue;
            };
            let (cw, ch) = (150.0 * s, 190.0 * s);
            let card = rect(cx + dx - cw / 2.0, y + dy, cw, ch);
            if Self::visible(card.y, card.y + ch, h) {
                canvas2d::drop_shadow_round_rect(
                    canvas,
                    card,
                    radii(22.0),
                    18.0,
                    rgba(0.0, 0.0, 0.0, 0.16),
                )?;
                canvas2d::fill_round_rect(canvas, card, radii(22.0), CARD)?;
                let inner = rect(card.x + 6.0, card.y + 6.0, cw - 12.0, ch - 12.0);
                canvas_images::draw_round(canvas, art.handle, inner, radii(17.0))?;
            }
        }
        y += 236.0;
        for (text, size, ink, weight) in [
            ("It starts with an app", 24.0, INK, 750u16),
            (
                "Open a .krate from Messages, Mail or Files,",
                15.0,
                MUTED,
                450,
            ),
            ("or get one people made in Discover.", 15.0, MUTED, 450),
        ] {
            let m = canvas2d::measure_text_styled(canvas, text, size, style(weight, -0.2))?;
            canvas2d::draw_text_styled(
                canvas,
                text,
                pt(cx - m.width / 2.0, y),
                size,
                ink,
                style(weight, -0.2),
            )?;
            y += if size > 20.0 { 28.0 } else { 21.0 };
        }
        y += 18.0;
        let button = rect(cx - 116.0, y, 232.0, 50.0);
        canvas2d::fill_round_rect(canvas, button, radii(25.0), ACCENT)?;
        let label = "Discover apps";
        let m = canvas2d::measure_text_styled(canvas, label, 17.0, style(650, 0.0))?;
        canvas2d::draw_text_styled(
            canvas,
            label,
            pt(cx - m.width / 2.0, y + 31.0),
            17.0,
            CARD,
            style(650, 0.0),
        )?;
        self.hits.push((button, Hit::Tab(Tab::Discover)));
        Ok(y + 50.0)
    }

    fn draw_discover(
        &mut self,
        x: f32,
        col: f32,
        mut y: f32,
        h: f32,
    ) -> Result<f32, gfx::GfxError> {
        let canvas = self.canvas;
        if self.hub.is_empty() {
            y += 24.0;
            for text in [
                "The hub could not be reached.",
                "Apps people share appear here when you are online.",
            ] {
                canvas2d::draw_text_styled(canvas, text, pt(x, y), 15.0, MUTED, style(450, 0.0))?;
                y += 22.0;
            }
            return Ok(y);
        }
        y += 18.0;

        // The featured app: the newest with a picture, on a big card.
        let featured = self.hub.iter().position(|a| !a.initial).unwrap_or(0);
        let card_h = (col * 1.05).min(420.0);
        let area = rect(x, y, col, card_h);
        if Self::visible(y, y + card_h, h) {
            canvas2d::drop_shadow_round_rect(
                canvas,
                area,
                radii(28.0),
                26.0,
                rgba(0.0, 0.0, 0.0, 0.16),
            )?;
            if let Some(art) = &self.hub[featured].art {
                picture(canvas, area, art, 28.0, 0.36)?;
            }
            self.card_fade(area, 0.42, 28.0)?;
            let app = &self.hub[featured];
            canvas2d::draw_text_styled(
                canvas,
                "NEW ON THE HUB",
                pt(x + 20.0, y + 32.0),
                12.0,
                rgba(1.0, 1.0, 1.0, 0.86),
                style(750, 0.8),
            )?;
            let mut ty = y + card_h - 112.0;
            canvas2d::draw_text_styled(
                canvas,
                &app.name,
                pt(x + 20.0, ty),
                28.0,
                CARD,
                style(800, -0.6),
            )?;
            ty += 24.0;
            for line in wrap(canvas, &app.description, 15.0, col - 130.0, 2)? {
                canvas2d::draw_text_styled(
                    canvas,
                    &line,
                    pt(x + 20.0, ty),
                    15.0,
                    rgba(1.0, 1.0, 1.0, 0.84),
                    style(450, 0.0),
                )?;
                ty += 20.0;
            }
            pill_button(
                canvas,
                rect(x + col - 92.0, y + card_h - 58.0, 72.0, 34.0),
                "GET",
                CARD,
                ACCENT,
            )?;
        }
        self.hits.push((area, Hit::Get(featured)));
        y += card_h + 34.0;

        // Every app, the way a store lists them.
        canvas2d::draw_text_styled(canvas, "All apps", pt(x, y), 22.0, INK, style(750, -0.4))?;
        y += 14.0;
        let row_h = 84.0;
        for index in 0..self.hub.len() {
            if index == featured {
                continue;
            }
            if Self::visible(y, y + row_h, h) {
                let app = &self.hub[index];
                let thumb = rect(x, y + 12.0, 60.0, 60.0);
                canvas2d::drop_shadow_round_rect(
                    canvas,
                    thumb,
                    radii(14.0),
                    8.0,
                    rgba(0.0, 0.0, 0.0, 0.12),
                )?;
                match &app.art {
                    Some(art) => canvas_images::draw_round(canvas, art.handle, thumb, radii(14.0))?,
                    None => canvas2d::fill_round_rect(canvas, thumb, radii(14.0), PILL)?,
                }
                if app.initial {
                    let initial: String = app
                        .name
                        .chars()
                        .next()
                        .map(|c| c.to_uppercase().collect())
                        .unwrap_or_default();
                    let m = canvas2d::measure_text_styled(canvas, &initial, 26.0, style(800, 0.0))?;
                    canvas2d::draw_text_styled(
                        canvas,
                        &initial,
                        pt(thumb.x + (60.0 - m.width) / 2.0, thumb.y + 39.0),
                        26.0,
                        CARD,
                        style(800, 0.0),
                    )?;
                }
                let tx = x + 74.0;
                let text_w = col - 74.0 - 86.0;
                let name = ellipsize(canvas, &app.name, 17.0, text_w, style(650, -0.2))?;
                canvas2d::draw_text_styled(
                    canvas,
                    &name,
                    pt(tx, y + 36.0),
                    17.0,
                    INK,
                    style(650, -0.2),
                )?;
                let by = if app.author.is_empty() {
                    format!("{} \u{00b7} {}", title_case(&app.category), app.size)
                } else {
                    format!("{} \u{00b7} {}", app.author, title_case(&app.category))
                };
                let by = ellipsize(canvas, &by, 13.0, text_w, style(450, 0.0))?;
                canvas2d::draw_text_styled(
                    canvas,
                    &by,
                    pt(tx, y + 56.0),
                    13.0,
                    MUTED,
                    style(450, 0.0),
                )?;
                pill_button(
                    canvas,
                    rect(x + col - 72.0, y + 26.0, 72.0, 32.0),
                    "GET",
                    PILL,
                    ACCENT,
                )?;
                canvas2d::fill_rect(canvas, rect(tx, y + row_h - 0.5, col - 74.0, 0.5), HAIRLINE)?;
            }
            self.hits.push((rect(x, y, col, row_h), Hit::Get(index)));
            y += row_h;
        }
        Ok(y)
    }

    /// The floating bar: the two tabs in a pill -- the open one shows its
    /// name -- and open-from-Files on its own black button.
    fn draw_bar(&mut self, w: f32, h: f32) -> Result<(), gfx::GfxError> {
        let canvas = self.canvas;
        let y = h - 92.0;
        // The list fades out under the bar.
        let fade = 40.0;
        let bands = 10;
        for i in 0..bands {
            let t = (i + 1) as f32 / bands as f32;
            let band = fade / bands as f32;
            canvas2d::fill_rect(
                canvas,
                rect(0.0, y - fade + i as f32 * band, w, band + 0.5),
                rgba(BG.r, BG.g, BG.b, t * t),
            )?;
        }
        canvas2d::fill_rect(canvas, rect(0.0, y, w, h - y), BG)?;

        let pill = rect(20.0, y, 196.0, 58.0);
        canvas2d::drop_shadow_round_rect(
            canvas,
            pill,
            radii(29.0),
            18.0,
            rgba(0.0, 0.0, 0.0, 0.12),
        )?;
        canvas2d::fill_round_rect(canvas, pill, radii(29.0), CARD)?;
        let mut tx = pill.x + 6.0;
        for (tab, label) in [(Tab::Library, "Library"), (Tab::Discover, "Discover")] {
            let on = self.tab == tab;
            let width = if on { 122.0 } else { 62.0 };
            let slot = rect(tx, pill.y + 6.0, width, 46.0);
            if on {
                canvas2d::fill_round_rect(canvas, slot, radii(23.0), INK)?;
            }
            let ink = if on { CARD } else { MUTED };
            let ix = slot.x + if on { 26.0 } else { width / 2.0 };
            let iy = slot.y + 23.0;
            match tab {
                Tab::Library => {
                    // Two stacked cards.
                    canvas2d::fill_round_rect(
                        canvas,
                        rect(ix - 8.0, iy - 9.0, 16.0, 7.5),
                        radii(2.5),
                        ink,
                    )?;
                    canvas2d::fill_round_rect(
                        canvas,
                        rect(ix - 8.0, iy + 1.5, 16.0, 7.5),
                        radii(2.5),
                        ink,
                    )?;
                }
                Tab::Discover => {
                    // A grid of four apps.
                    for (gx, gy) in [(-8.5f32, -8.5f32), (1.0, -8.5), (-8.5, 1.0), (1.0, 1.0)] {
                        canvas2d::fill_round_rect(
                            canvas,
                            rect(ix + gx, iy + gy, 7.5, 7.5),
                            radii(2.2),
                            ink,
                        )?;
                    }
                }
            }
            if on {
                canvas2d::draw_text_styled(
                    canvas,
                    label,
                    pt(ix + 16.0, iy + 5.5),
                    15.0,
                    CARD,
                    style(650, -0.1),
                )?;
            }
            self.hits.push((slot, Hit::Tab(tab)));
            tx += width;
        }

        let plus = pt(w - 49.0, y + 29.0);
        canvas2d::drop_shadow_round_rect(
            canvas,
            rect(plus.x - 29.0, plus.y - 29.0, 58.0, 58.0),
            radii(29.0),
            16.0,
            rgba(0.0, 0.0, 0.0, 0.2),
        )?;
        canvas2d::fill_circle(canvas, plus, 29.0, BLACK)?;
        canvas2d::fill_round_rect(
            canvas,
            rect(plus.x - 9.0, plus.y - 1.25, 18.0, 2.5),
            radii(1.25),
            CARD,
        )?;
        canvas2d::fill_round_rect(
            canvas,
            rect(plus.x - 1.25, plus.y - 9.0, 2.5, 18.0),
            radii(1.25),
            CARD,
        )?;
        self.hits
            .push((rect(plus.x - 30.0, plus.y - 30.0, 60.0, 60.0), Hit::Files));
        Ok(())
    }
}

fn title_case(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Cut a line to `width` with an ellipsis.
fn ellipsize(
    canvas: u64,
    text: &str,
    size: f32,
    width: f32,
    st: gfx::TextStyle,
) -> Result<String, gfx::GfxError> {
    if canvas2d::measure_text_styled(canvas, text, size, st)?.width <= width {
        return Ok(text.to_string());
    }
    let mut cut: String = text.to_string();
    while !cut.is_empty() {
        cut.pop();
        let candidate = format!("{}\u{2026}", cut.trim_end());
        if canvas2d::measure_text_styled(canvas, &candidate, size, st)?.width <= width {
            return Ok(candidate);
        }
    }
    Ok(String::new())
}

/// Word-wrap to `max` lines, the last one ending in an ellipsis if cut.
fn wrap(
    canvas: u64,
    text: &str,
    size: f32,
    width: f32,
    max: usize,
) -> Result<Vec<String>, gfx::GfxError> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{current} {word}")
        };
        if canvas2d::measure_text_styled(canvas, &candidate, size, style(450, 0.0))?.width <= width
        {
            current = candidate;
            continue;
        }
        if lines.len() + 1 == max {
            lines.push(format!("{current}\u{2026}"));
            return Ok(lines);
        }
        if !current.is_empty() {
            lines.push(current);
        }
        current = word.to_string();
    }
    if !current.is_empty() && lines.len() < max {
        lines.push(current);
    }
    Ok(lines)
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

struct Component;

impl krate::Guest for Component {
    fn run() -> i32 {
        let Ok(win) = window::create(
            "Krate",
            types::WindowSize {
                width: 390,
                height: 844,
            },
        ) else {
            out("home:closed");
            return 1;
        };
        if tree::set_root(win, &node(ROOT_ID, None, types::WidgetKind::Stack)).is_err() {
            out("home:closed");
            return 1;
        }
        let _ = tree::upsert_node(
            win,
            &node(CANVAS_ID, Some(ROOT_ID), types::WidgetKind::Canvas),
        );
        let Ok(canvas) = canvas2d::bind(win, CANVAS_ID) else {
            out("home:closed");
            return 1;
        };

        // Every picture crosses into the host once, here.
        let library: Vec<LibraryApp> = lines("library.txt")
            .into_iter()
            .filter(|f| f.len() >= 3)
            .map(|f| {
                let art = keep_art(canvas, &f[2]);
                let initial = art.is_none();
                LibraryApp {
                    art: art.or_else(|| keep_gradient(canvas, &f[0])),
                    initial,
                    name: f[0].clone(),
                    line: f[1].clone(),
                }
            })
            .collect();
        let hub: Vec<HubApp> = lines("discover.txt")
            .into_iter()
            .filter(|f| f.len() >= 7)
            .map(|f| {
                let art = keep_art(canvas, &f[5]);
                HubApp {
                    initial: art.is_none(),
                    art: art.or_else(|| keep_gradient(canvas, &f[1])),
                    id: f[0].clone(),
                    name: f[1].clone(),
                    author: f[2].clone(),
                    category: f[3].clone(),
                    description: f[4].clone(),
                    size: f[6].clone(),
                }
            })
            .collect();
        let start = match read_all("start.txt") {
            Some(text) if text.trim_ascii() == b"discover" => Tab::Discover,
            _ => Tab::Library,
        };
        let mut screen = Screen {
            canvas,
            library,
            hub,
            mark: canvas_images::keep(canvas, MARK_SIZE, MARK_SIZE, MARK).ok(),
            fades: Vec::new(),
            tab: start,
            scroll: [0.0; 2],
            content: 0.0,
            hits: Vec::new(),
        };

        // Draw only after a change. A flick keeps gliding after the finger
        // lifts and slows to a stop, the way a phone list moves: velocity is
        // in points per millisecond and every step uses the time that really
        // passed, so an uneven frame shows as a longer step, not a stutter.
        let mut dirty = true;
        let mut velocity = 0.0f32;
        let mut last_wheel = 0u64;
        let mut last_step = 0u64;
        let mut pressed_at: Option<usize> = None;
        loop {
            let (w, h) = match canvas2d::canvas_size(canvas) {
                Ok(size) => (size.width.max(1.0), size.height.max(1.0)),
                Err(_) => (390.0, 844.0),
            };
            if dirty {
                if screen.draw(w, h).is_err() {
                    out("home:closed");
                    return 1;
                }
                let _ = window::request_redraw(win);
                dirty = false;
            }
            let gliding = velocity.abs() > 0.02;
            let event = events::wait(if gliding { Some(8) } else { None });
            let now = clock::monotonic_nanos();
            match event {
                Some(types::Event::CloseRequested(_)) => {
                    out("home:closed");
                    return 0;
                }
                Some(types::Event::Resized(_)) => dirty = true,
                Some(types::Event::Wheel(wheel)) => {
                    // The finger's own speed, smoothed; a pause forgets the
                    // speed before it.
                    let gap = (now - last_wheel) as f32 / 1_000_000.0;
                    let speed = wheel.dy / gap.clamp(4.0, 100.0);
                    velocity = if gap > 100.0 { speed } else { velocity * 0.3 + speed * 0.7 };
                    last_wheel = now;
                    last_step = now;
                    let max = (screen.content - h).max(0.0);
                    let s = screen.scroll();
                    let next = (*s + wheel.dy).clamp(0.0, max);
                    if next != *s {
                        *s = next;
                        dirty = true;
                    }
                }
                // Act on release over the item that was pressed, as iOS
                // does: a press that turns into a scroll is let go elsewhere
                // (or far outside the window), and opens nothing.
                Some(types::Event::Pointer(p)) if p.pressed && p.button.is_some() => {
                    velocity = 0.0;
                    pressed_at = screen.hits.iter().rposition(|(r, _)| {
                        p.x >= r.x && p.x <= r.x + r.width && p.y >= r.y && p.y <= r.y + r.height
                    });
                }
                Some(types::Event::Pointer(p)) if !p.pressed && p.button.is_some() => {
                    let released_at = screen.hits.iter().rposition(|(r, _)| {
                        p.x >= r.x && p.x <= r.x + r.width && p.y >= r.y && p.y <= r.y + r.height
                    });
                    let same = released_at.is_some() && released_at == pressed_at.take();
                    let hit = if same {
                        released_at.map(|i| &screen.hits[i])
                    } else {
                        None
                    };
                    match hit.map(|(_, hit)| hit) {
                        Some(Hit::Open(index)) => {
                            out(&format!("home:open:{index}"));
                            return 0;
                        }
                        Some(Hit::Get(index)) => {
                            out(&format!("home:get:{}", screen.hub[*index].id));
                            return 0;
                        }
                        Some(Hit::Files) => {
                            out("home:files");
                            return 0;
                        }
                        Some(Hit::Tab(tab)) => {
                            if screen.tab != *tab {
                                screen.tab = *tab;
                                dirty = true;
                            }
                        }
                        None => {}
                    }
                }
                None if gliding && now - last_wheel > 40_000_000 => {
                    // The finger has lifted: keep going, slowing down at
                    // iOS's own rate (0.998 per millisecond).
                    let dt = ((now - last_step) as f32 / 1_000_000.0).min(50.0);
                    last_step = now;
                    let max = (screen.content - h).max(0.0);
                    let s = screen.scroll();
                    let next = (*s + velocity * dt).clamp(0.0, max);
                    if next == *s {
                        velocity = 0.0;
                    } else {
                        *s = next;
                        // 0.998 to the power dt, without a float library.
                        for _ in 0..dt as u32 {
                            velocity *= 0.998;
                        }
                        velocity *= 1.0 - 0.002 * (dt - (dt as u32) as f32);
                        dirty = true;
                    }
                }
                _ => {}
            }
        }
    }
}

krate::export!(Component);

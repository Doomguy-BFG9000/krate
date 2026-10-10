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
//!
//! - `start.txt` (optional): `discover` opens on Discover.
//!
//! The answer is one stdout line: `home:open:<n>` (the n-th library app),
//! `home:get:<hub id>`, `home:files` (open a .krate from Files), or
//! `home:closed`.

#![no_std]

extern crate alloc;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use krate::bindings::krate::fs::files;
use krate::bindings::krate::io::stdio;
use krate::gfx::{canvas2d, types as gfx};
use krate::ui::{events, tree, types, window};

const ROOT_ID: u64 = 1;
const CANVAS_ID: u64 = 2;

/// The Krate mark, 120 px square, straight RGBA.
const MARK: &[u8] = include_bytes!("../assets/mark-120.rgba");
const MARK_SIZE: u32 = 120;

// The palette: a light, quiet surface so the apps' own pictures carry the
// colour.
const BG: gfx::Color = rgb(0.953, 0.957, 0.965);
const CARD: gfx::Color = rgb(1.0, 1.0, 1.0);
const INK: gfx::Color = rgb(0.067, 0.071, 0.082);
const MUTED: gfx::Color = rgb(0.42, 0.44, 0.48);
const ACCENT: gfx::Color = rgb(0.114, 0.392, 0.886);
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

/// A decoded picture.
struct Art {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Art {
    /// The picture's overall colour, from a sparse sample.
    fn average(&self) -> (f32, f32, f32) {
        let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
        for p in self.rgba.chunks_exact(4).step_by(97) {
            r += p[0] as u32;
            g += p[1] as u32;
            b += p[2] as u32;
            n += 1;
        }
        let n = n.max(1) as f32 * 255.0;
        (r as f32 / n, g as f32 / n, b as f32 / n)
    }
}

/// Whether a picture is close enough to a card's shape to fill it.
fn fits(art: &Art, area: gfx::Rect) -> bool {
    let ratio = (art.width as f32 / art.height as f32) / (area.width / area.height);
    ratio > 0.75 && ratio < 1.33
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

fn load_art(name: &str) -> Option<Art> {
    if name == "-" || name.is_empty() {
        return None;
    }
    let bytes = read_all(&format!("art/{name}"))?;
    if bytes.len() < 8 {
        return None;
    }
    let width = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
    let height = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
    let rgba = bytes[8..].to_vec();
    (rgba.len() == (width * height * 4) as usize).then_some(Art {
        width,
        height,
        rgba,
    })
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

struct LibraryApp {
    name: String,
    line: String,
    art: Option<Art>,
}

struct HubApp {
    id: String,
    name: String,
    author: String,
    category: String,
    description: String,
    art: Option<Art>,
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
    library: Vec<LibraryApp>,
    hub: Vec<HubApp>,
    tab: Tab,
    scroll: [f32; 2],
    content: f32,
    hits: Vec<(gfx::Rect, Hit)>,
}

impl Screen {
    fn scroll(&mut self) -> &mut f32 {
        match self.tab {
            Tab::Library => &mut self.scroll[0],
            Tab::Discover => &mut self.scroll[1],
        }
    }

    fn draw(&mut self, canvas: u64, w: f32, h: f32) -> Result<(), gfx::GfxError> {
        self.hits.clear();
        canvas2d::clear(canvas, BG)?;
        let pad = 20.0;
        let col = (w - pad * 2.0).min(560.0);
        let x = (w - col) / 2.0;
        let scroll = *self.scroll();
        let mut y = 62.0 - scroll;

        // The header: the mark, then the screen's name.
        canvas2d::drop_shadow_round_rect(
            canvas,
            rect(x, y, 40.0, 40.0),
            radii(11.0),
            8.0,
            rgba(0.0, 0.0, 0.0, 0.12),
        )?;
        canvas2d::draw_pixels_round(
            canvas,
            rect(x, y, 40.0, 40.0),
            radii(11.0),
            MARK_SIZE,
            MARK_SIZE,
            MARK,
        )?;
        let title = match self.tab {
            Tab::Library => "Library",
            Tab::Discover => "Discover",
        };
        y += 80.0;
        canvas2d::draw_text_styled(canvas, title, pt(x, y), 34.0, INK, style(750, -0.8))?;
        y += 26.0;
        let subtitle = match self.tab {
            Tab::Library => "Apps you have opened with Krate",
            Tab::Discover => "Apps people made and shared",
        };
        canvas2d::draw_text_styled(canvas, subtitle, pt(x, y), 15.0, MUTED, style(450, 0.0))?;
        y += 28.0;

        y = match self.tab {
            Tab::Library if self.library.is_empty() => self.draw_empty(canvas, x, col, y)?,
            Tab::Library => self.draw_library(canvas, x, col, y)?,
            Tab::Discover => self.draw_discover(canvas, x, col, y)?,
        };
        self.content = y + scroll + 120.0;

        self.draw_bar(canvas, w, h)?;
        canvas2d::present(canvas)
    }

    /// A card's picture: the app's own art, or a quiet gradient with its
    /// initial when it has none.
    fn art(
        canvas: u64,
        area: gfx::Rect,
        art: Option<&Art>,
        name: &str,
        corner: f32,
    ) -> Result<(), gfx::GfxError> {
        canvas2d::drop_shadow_round_rect(
            canvas,
            area,
            radii(corner),
            18.0,
            rgba(0.0, 0.0, 0.0, 0.10),
        )?;
        match art {
            // A picture close to the card's shape fills it.
            Some(art) if fits(art, area) => canvas2d::draw_pixels_round(
                canvas,
                area,
                radii(corner),
                art.width,
                art.height,
                &art.rgba,
            )?,
            // A tall one (a phone-shaped window) sits whole on a backdrop of
            // its own colour, instead of being cut to a strip.
            Some(art) => {
                let (r, g, b) = art.average();
                canvas2d::fill_round_rect(
                    canvas,
                    area,
                    radii(corner),
                    rgb(r * 0.85, g * 0.85, b * 0.85),
                )?;
                let inset = area.height * 0.08;
                let inner_h = area.height - inset * 2.0;
                let inner_w =
                    (inner_h * art.width as f32 / art.height as f32).min(area.width - inset * 2.0);
                let inner = rect(
                    area.x + (area.width - inner_w) / 2.0,
                    area.y + inset,
                    inner_w,
                    inner_h,
                );
                canvas2d::drop_shadow_round_rect(
                    canvas,
                    inner,
                    radii(10.0),
                    14.0,
                    rgba(0.0, 0.0, 0.0, 0.25),
                )?;
                canvas2d::draw_pixels_round(
                    canvas,
                    inner,
                    radii(10.0),
                    art.width,
                    art.height,
                    &art.rgba,
                )?
            }
            None => {
                // A colour from the name, so each app keeps its own: a small
                // diagonal gradient, drawn scaled -- smooth at any size.
                let seed = name
                    .bytes()
                    .fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
                let hues: [([f32; 3], [f32; 3]); 5] = [
                    ([0.33, 0.55, 1.0], [0.10, 0.22, 0.70]),
                    ([1.0, 0.55, 0.42], [0.78, 0.20, 0.38]),
                    ([0.30, 0.82, 0.66], [0.06, 0.45, 0.50]),
                    ([0.66, 0.50, 1.0], [0.32, 0.18, 0.70]),
                    ([1.0, 0.78, 0.36], [0.92, 0.42, 0.16]),
                ];
                let (a, b) = hues[(seed as usize) % hues.len()];
                let (gw, gh) = (32u32, 16u32);
                let mut px = Vec::with_capacity((gw * gh * 4) as usize);
                for y in 0..gh {
                    for x in 0..gw {
                        let t = (x as f32 / (gw - 1) as f32 * 0.6
                            + y as f32 / (gh - 1) as f32 * 0.4)
                            .clamp(0.0, 1.0);
                        for c in 0..3 {
                            px.push(((a[c] * (1.0 - t) + b[c] * t) * 255.0) as u8);
                        }
                        px.push(255);
                    }
                }
                canvas2d::draw_pixels_round(canvas, area, radii(corner), gw, gh, &px)?;
                let initial: String = name
                    .chars()
                    .next()
                    .map(|c| c.to_uppercase().collect())
                    .unwrap_or_default();
                let size = (area.height * 0.34).min(64.0);
                let m = canvas2d::measure_text_styled(canvas, &initial, size, style(750, 0.0))?;
                canvas2d::draw_text_styled(
                    canvas,
                    &initial,
                    pt(
                        area.x + (area.width - m.width) / 2.0,
                        area.y + area.height / 2.0 + size * 0.36,
                    ),
                    size,
                    rgba(1.0, 1.0, 1.0, 0.95),
                    style(750, 0.0),
                )?;
            }
        }
        Ok(())
    }

    fn draw_library(
        &mut self,
        canvas: u64,
        x: f32,
        col: f32,
        mut y: f32,
    ) -> Result<f32, gfx::GfxError> {
        for (index, app) in self.library.iter().enumerate() {
            let art_h = (col * 0.5).min(220.0);
            let area = rect(x, y, col, art_h);
            Self::art(canvas, area, app.art.as_ref(), &app.name, 28.0)?;
            let name_y = y + art_h + 34.0;
            let m = canvas2d::measure_text_styled(canvas, &app.name, 22.0, style(650, -0.3))?;
            canvas2d::draw_text_styled(
                canvas,
                &app.name,
                pt(x + (col - m.width) / 2.0, name_y),
                22.0,
                INK,
                style(650, -0.3),
            )?;
            let lm = canvas2d::measure_text_styled(canvas, &app.line, 15.0, style(450, 0.0))?;
            canvas2d::draw_text_styled(
                canvas,
                &app.line,
                pt(x + (col - lm.width) / 2.0, name_y + 22.0),
                15.0,
                MUTED,
                style(450, 0.0),
            )?;
            let bottom = name_y + 34.0;
            self.hits
                .push((rect(x, y, col, bottom - y), Hit::Open(index)));
            y = bottom + 28.0;
        }
        Ok(y)
    }

    fn draw_empty(
        &mut self,
        canvas: u64,
        x: f32,
        col: f32,
        mut y: f32,
    ) -> Result<f32, gfx::GfxError> {
        // Three real apps from the hub, fanned like a hand of cards.
        y += 40.0;
        let cx = x + col / 2.0;
        let fan = [(-46.0f32, -0.16f32, 1usize), (46.0, 0.14, 2), (0.0, 0.0, 0)];
        for (dx, angle, pick) in fan {
            let Some(art) = self.hub.iter().filter_map(|a| a.art.as_ref()).nth(pick) else {
                continue;
            };
            let framed = framed(&crop(art, 4, 5), 12);
            canvas2d::draw_sprite(
                canvas,
                pt(cx + dx, y + 110.0),
                gfx::Size {
                    width: 152.0,
                    height: 190.0,
                },
                angle,
                framed.width,
                framed.height,
                &framed.rgba,
            )?;
        }
        y += 270.0;
        for (text, size, ink, weight) in [
            ("It starts with an app", 24.0, INK, 680u16),
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
        let button = rect(cx - 120.0, y, 240.0, 50.0);
        canvas2d::fill_round_rect(canvas, button, radii(25.0), ACCENT)?;
        let label = "Discover apps";
        let m = canvas2d::measure_text_styled(canvas, label, 17.0, style(600, 0.0))?;
        canvas2d::draw_text_styled(
            canvas,
            label,
            pt(cx - m.width / 2.0, y + 31.0),
            17.0,
            CARD,
            style(600, 0.0),
        )?;
        self.hits.push((button, Hit::Tab(Tab::Discover)));
        Ok(y + 50.0)
    }

    fn draw_discover(
        &mut self,
        canvas: u64,
        x: f32,
        col: f32,
        mut y: f32,
    ) -> Result<f32, gfx::GfxError> {
        if self.hub.is_empty() {
            for text in [
                "The hub could not be reached.",
                "Apps people share appear here when you are online.",
            ] {
                canvas2d::draw_text_styled(
                    canvas,
                    text,
                    pt(x, y + 20.0),
                    15.0,
                    MUTED,
                    style(450, 0.0),
                )?;
                y += 22.0;
            }
            return Ok(y);
        }
        for (index, app) in self.hub.iter().enumerate() {
            let art_h = (col * 0.56).min(240.0);
            Self::art(
                canvas,
                rect(x, y, col, art_h),
                app.art.as_ref(),
                &app.name,
                24.0,
            )?;
            let text_y = y + art_h + 30.0;
            canvas2d::draw_text_styled(
                canvas,
                &app.name,
                pt(x, text_y),
                20.0,
                INK,
                style(650, -0.3),
            )?;
            let by = if app.author.is_empty() {
                format!("{} \u{00b7} {}", title_case(&app.category), app.size)
            } else {
                format!(
                    "{} \u{00b7} {} \u{00b7} {}",
                    app.author,
                    title_case(&app.category),
                    app.size
                )
            };
            canvas2d::draw_text_styled(
                canvas,
                &by,
                pt(x, text_y + 20.0),
                14.0,
                MUTED,
                style(450, 0.0),
            )?;

            // Get: a pill on the right, the way a store says it.
            let get = rect(x + col - 72.0, text_y - 24.0, 72.0, 32.0);
            canvas2d::fill_round_rect(canvas, get, radii(16.0), rgb(0.91, 0.93, 0.97))?;
            let m = canvas2d::measure_text_styled(canvas, "GET", 15.0, style(750, 0.4))?;
            canvas2d::draw_text_styled(
                canvas,
                "GET",
                pt(get.x + (get.width - m.width) / 2.0, get.y + 21.5),
                15.0,
                ACCENT,
                style(750, 0.4),
            )?;

            let mut bottom = text_y + 30.0;
            for line in wrap(canvas, &app.description, 15.0, col, 2)? {
                bottom += 20.0;
                canvas2d::draw_text_styled(
                    canvas,
                    &line,
                    pt(x, bottom),
                    15.0,
                    INK,
                    style(400, 0.0),
                )?;
            }
            self.hits
                .push((rect(x, y, col, bottom - y), Hit::Get(index)));
            y = bottom + 34.0;
        }
        Ok(y)
    }

    /// The floating bar: Library and Discover in a pill, open-from-Files on
    /// its own black button.
    fn draw_bar(&mut self, canvas: u64, w: f32, h: f32) -> Result<(), gfx::GfxError> {
        let y = h - 96.0;
        // Fade the list out under the bar: bands of the background, more
        // opaque toward the bottom, then solid behind the bar itself.
        let fade = 44.0;
        let bands = 11;
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
        let pill = rect(22.0, y, 128.0, 60.0);
        canvas2d::drop_shadow_round_rect(
            canvas,
            pill,
            radii(30.0),
            16.0,
            rgba(0.0, 0.0, 0.0, 0.10),
        )?;
        canvas2d::fill_round_rect(canvas, pill, radii(30.0), CARD)?;
        for (i, tab) in [Tab::Library, Tab::Discover].into_iter().enumerate() {
            let cx = pill.x + 34.0 + i as f32 * 60.0;
            let cy = pill.y + 30.0;
            let on = self.tab == tab;
            if on {
                canvas2d::fill_circle(canvas, pt(cx, cy), 22.0, rgb(0.93, 0.94, 0.96))?;
            }
            let ink = if on { INK } else { MUTED };
            match tab {
                Tab::Library => {
                    // Two stacked cards.
                    canvas2d::fill_round_rect(
                        canvas,
                        rect(cx - 8.0, cy - 9.0, 16.0, 8.0),
                        radii(2.5),
                        ink,
                    )?;
                    canvas2d::fill_round_rect(
                        canvas,
                        rect(cx - 8.0, cy + 1.0, 16.0, 8.0),
                        radii(2.5),
                        ink,
                    )?;
                }
                Tab::Discover => {
                    // A compass.
                    canvas2d::fill_circle(canvas, pt(cx, cy), 9.5, ink)?;
                    canvas2d::fill_circle(
                        canvas,
                        pt(cx, cy),
                        7.5,
                        if on { rgb(0.93, 0.94, 0.96) } else { CARD },
                    )?;
                    canvas2d::fill_circle(canvas, pt(cx, cy), 2.5, ink)?;
                }
            }
            self.hits
                .push((rect(cx - 30.0, pill.y, 60.0, 60.0), Hit::Tab(tab)));
        }
        let plus = pt(w - 52.0, y + 30.0);
        canvas2d::drop_shadow_round_rect(
            canvas,
            rect(plus.x - 29.0, plus.y - 29.0, 58.0, 58.0),
            radii(29.0),
            16.0,
            rgba(0.0, 0.0, 0.0, 0.18),
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

/// The centre of a picture at the aspect `w:h`, so fanned cards match.
fn crop(art: &Art, w: u32, h: u32) -> Art {
    let (cw, ch) = if art.width * h > art.height * w {
        (art.height * w / h, art.height)
    } else {
        (art.width, art.width * h / w)
    };
    let (ox, oy) = ((art.width - cw) / 2, (art.height - ch) / 2);
    let mut rgba = Vec::with_capacity((cw * ch * 4) as usize);
    for row in oy..oy + ch {
        let from = ((row * art.width + ox) * 4) as usize;
        rgba.extend_from_slice(&art.rgba[from..from + (cw * 4) as usize]);
    }
    Art {
        width: cw,
        height: ch,
        rgba,
    }
}

/// A picture with a white frame, for the fanned cards.
fn framed(art: &Art, border: u32) -> Art {
    let (w, h) = (art.width + border * 2, art.height + border * 2);
    let mut rgba = alloc::vec![255u8; (w * h * 4) as usize];
    for row in 0..art.height {
        let from = (row * art.width * 4) as usize;
        let to = (((row + border) * w + border) * 4) as usize;
        rgba[to..to + (art.width * 4) as usize]
            .copy_from_slice(&art.rgba[from..from + (art.width * 4) as usize]);
    }
    Art {
        width: w,
        height: h,
        rgba,
    }
}

fn title_case(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
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
        if canvas2d::measure_text_styled(canvas, &candidate, size, style(400, 0.0))?.width <= width
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
        let library: Vec<LibraryApp> = lines("library.txt")
            .into_iter()
            .filter(|f| f.len() >= 3)
            .map(|f| LibraryApp {
                name: f[0].clone(),
                line: f[1].clone(),
                art: load_art(&f[2]),
            })
            .collect();
        let hub: Vec<HubApp> = lines("discover.txt")
            .into_iter()
            .filter(|f| f.len() >= 7)
            .map(|f| HubApp {
                id: f[0].clone(),
                name: f[1].clone(),
                author: f[2].clone(),
                category: f[3].clone(),
                description: f[4].clone(),
                art: load_art(&f[5]),
                size: f[6].clone(),
            })
            .collect();
        // Library first, even empty: its empty state is the welcome, with a
        // way into Discover.
        let start = match read_all("start.txt") {
            Some(text) if text.trim_ascii() == b"discover" => Tab::Discover,
            _ => Tab::Library,
        };
        let mut screen = Screen {
            library,
            hub,
            tab: start,
            scroll: [0.0; 2],
            content: 0.0,
            hits: Vec::new(),
        };

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

        // Draw only when something changed: a screen that redraws while
        // nobody touches it spends the battery on nothing (K-1022).
        let mut dirty = true;
        loop {
            let (w, h) = match canvas2d::canvas_size(canvas) {
                Ok(size) => (size.width.max(1.0), size.height.max(1.0)),
                Err(_) => (390.0, 844.0),
            };
            if dirty {
                if screen.draw(canvas, w, h).is_err() {
                    out("home:closed");
                    return 1;
                }
                let _ = window::request_redraw(win);
                dirty = false;
            }
            match events::wait(None) {
                Some(types::Event::CloseRequested(_)) => {
                    out("home:closed");
                    return 0;
                }
                Some(types::Event::Resized(_)) => dirty = true,
                Some(types::Event::Wheel(wheel)) => {
                    let max = (screen.content - h).max(0.0);
                    let s = screen.scroll();
                    let next = (*s + wheel.dy).clamp(0.0, max);
                    if next != *s {
                        *s = next;
                        dirty = true;
                    }
                }
                Some(types::Event::Pointer(p)) if p.pressed && p.button.is_some() => {
                    let hit = screen.hits.iter().rev().find(|(r, _)| {
                        p.x >= r.x && p.x <= r.x + r.width && p.y >= r.y && p.y <= r.y + r.height
                    });
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
                _ => {}
            }
        }
    }
}

krate::export!(Component);

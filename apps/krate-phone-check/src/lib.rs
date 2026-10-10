// Phone Check: every interface IC-918 makes work on a phone, one button
// each -- copy and paste, record and play back, speak, the camera, a
// notification, a link. Each row says what happened in plain words.
#![no_std]
extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use krate::audio::{capture, playback};
use krate::camera::capture as camera;
use krate::gfx::{canvas2d, canvas_images, types as gfx};
use krate::layout::{Rect, Screen};
use krate::speech::synthesis;
use krate::ui::{clipboard, launcher, notify, photos, share};
use krate::{
    io::{args, stdio},
    time::clock,
    ui::{
        events, tree,
        types::{self, Event},
        window,
    },
    Guest,
};

const RATE: u32 = 16_000;

fn rgb(r: f32, g: f32, b: f32) -> gfx::Color {
    gfx::Color { r, g, b, a: 1.0 }
}

fn style(weight: u16) -> gfx::TextStyle {
    gfx::TextStyle { weight, italic: false, letter_spacing: 0.0, family: gfx::FontFamily::Sans }
}

fn radii(all: f32) -> gfx::CornerRadii {
    gfx::CornerRadii { top_left: all, top_right: all, bottom_right: all, bottom_left: all }
}

fn say_line(line: &str) {
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

#[derive(Clone, Copy, PartialEq)]
enum Check {
    Copy,
    Paste,
    Record,
    Speak,
    Camera,
    Notify,
    Link,
    Share,
    Pick,
    SavePhoto,
}

const CHECKS: [(Check, &str); 10] = [
    (Check::Copy, "Copy a line"),
    (Check::Paste, "Paste"),
    (Check::Record, "Record 3 s and play back"),
    (Check::Speak, "Speak a sentence"),
    (Check::Camera, "Camera"),
    (Check::Notify, "Post a notification"),
    (Check::Link, "Open krate.tech"),
    (Check::Share, "Share a note and a file"),
    (Check::Pick, "Pick photos"),
    (Check::SavePhoto, "Save a picture to Photos"),
];

struct Page {
    rows: Vec<Rect>,
    results: [String; 10],
    /// A recording in progress: the stream, when it started, the bytes so far.
    recording: Option<(u64, u64, Vec<u8>)>,
    camera: Option<u64>,
    picture: Option<(u64, u32, u32)>,
    /// Frames drawn in the current second, and when that second began.
    frames: (u32, u64),
}

fn index(check: Check) -> usize {
    CHECKS.iter().position(|(c, _)| *c == check).unwrap_or(0)
}

fn draw(canvas: u64, screen: &Screen, page: &mut Page) -> Result<(), gfx::GfxError> {
    let ink = rgb(0.07, 0.07, 0.09);
    let muted = rgb(0.43, 0.44, 0.48);
    canvas2d::clear(canvas, rgb(0.96, 0.96, 0.97))?;
    let area = screen.safe_area().inset(12.0, 16.0, 12.0, 16.0);
    canvas2d::draw_text_styled(canvas, "Phone Check", gfx::Point { x: area.x, y: area.y + 30.0 }, 28.0, ink, style(800))?;
    let mut y = area.y + 48.0;
    if let Some((image, w, h)) = page.picture {
        let ph = (area.w * h as f32 / w.max(1) as f32).min(area.h * 0.32);
        let pw = ph * w as f32 / h.max(1) as f32;
        canvas_images::draw_round(canvas, image, gfx::Rect { x: area.x + (area.w - pw) / 2.0, y, width: pw, height: ph }, radii(12.0))?;
        y += ph + 10.0;
    }
    page.rows.clear();
    let row_h = ((area.y + area.h - y) / CHECKS.len() as f32).clamp(46.0, 64.0);
    for (i, (_, label)) in CHECKS.iter().enumerate() {
        let r = Rect::new(area.x, y, area.w, row_h - 8.0);
        page.rows.push(r);
        canvas2d::fill_round_rect(canvas, gfx::Rect { x: r.x, y: r.y, width: r.w, height: r.h }, radii(14.0), rgb(1.0, 1.0, 1.0))?;
        canvas2d::draw_text_styled(canvas, label, gfx::Point { x: r.x + 14.0, y: r.y + r.h / 2.0 - 2.0 }, 16.0, ink, style(650))?;
        let result = &page.results[i];
        let colour = if result.starts_with("ok") { rgb(0.13, 0.62, 0.30) } else { muted };
        canvas2d::draw_text_styled(canvas, result, gfx::Point { x: r.x + 14.0, y: r.y + r.h / 2.0 + 16.0 }, 13.0, colour, style(500))?;
        y += row_h;
    }
    canvas2d::present(canvas)
}

fn run_check(win: u64, check: Check, page: &mut Page) {
    let slot = index(check);
    page.results[slot] = match check {
        Check::Copy => match clipboard::write_text("Copied by Phone Check, a Krate app") {
            Ok(()) => String::from("ok: copied -- paste it in Notes to see"),
            Err(e) => format!("failed: {e:?}"),
        },
        Check::Paste => match clipboard::read_text() {
            Ok(text) if text.is_empty() => String::from("ok, but the clipboard is empty"),
            Ok(text) => format!("ok: \"{}\"", text.chars().take(40).collect::<String>()),
            Err(e) => format!("failed: {e:?}"),
        },
        Check::Record => {
            let config = capture::StreamConfig { sample_rate: RATE, channels: 1, format: krate::audio::types::SampleFormat::PcmS16, buffer_frames: 1024 };
            match capture::open(config).and_then(|id| capture::start(id).map(|()| id)) {
                Ok(id) => {
                    // From when the microphone opened, not the tap: the
                    // phone's own question can sit in between, and three
                    // seconds of it were "recorded" as nothing.
                    page.recording = Some((id, clock::monotonic_nanos(), Vec::new()));
                    String::from("recording\u{2026} speak now")
                }
                Err(e) => format!("failed: {e:?}"),
            }
        }
        Check::Speak => match synthesis::say("Hello from Krate on your phone.", None, None) {
            Ok(()) => String::from("ok: speaking"),
            Err(e) => format!("failed: {e:?}"),
        },
        Check::Camera => {
            if let Some(id) = page.camera.take() {
                let _ = camera::close(id);
                page.picture = None;
                String::from("ok: camera off")
            } else {
                let config = camera::StreamConfig { width: 640, height: 480, fps: 30, format: krate::camera::types::FrameFormat::Rgba8 };
                let opened = camera::devices().and_then(|devices| {
                    let device = devices.first().map(|d| d.id.clone()).unwrap_or_default();
                    camera::open(&device, config)
                });
                match opened.and_then(|id| camera::start(id).map(|()| id)) {
                    Ok(id) => {
                        page.camera = Some(id);
                        String::from("ok: camera on -- tap again to stop")
                    }
                    Err(e) => format!("failed: {e:?}"),
                }
            }
        }
        Check::Notify => match notify::show("Phone Check", "Notifications work on this phone.") {
            Ok(()) => String::from("ok: posted -- look at the top of the screen"),
            Err(e) => format!("failed: {e:?}"),
        },
        Check::Link => match launcher::open_url("https://krate.tech") {
            Ok(()) => String::from("ok: opened"),
            Err(e) => format!("failed: {e:?}"),
        },
        Check::Pick => match photos::pick(win, 3) {
            Ok(picked) if picked.is_empty() => String::from("ok: the picker opened, and you cancelled"),
            Ok(picked) => {
                let mut bytes = 0usize;
                for file in &picked {
                    if let Ok(f) = krate::bindings::krate::fs::files::open_chosen(&file.token, krate::bindings::krate::fs::types::OpenMode::Read) {
                        while let Ok(chunk) = f.read(1 << 20) {
                            if chunk.is_empty() {
                                break;
                            }
                            bytes += chunk.len();
                        }
                    }
                }
                format!("ok: {} photo(s), {:.1} MB read", picked.len(), bytes as f32 / 1_048_576.0)
            }
            Err(e) => format!("failed: {e:?}"),
        },
        Check::SavePhoto => match photos::save(win, &gradient_png(240, 160)) {
            Ok(()) => String::from("ok: added -- look in Photos"),
            Err(e) => format!("failed: {e:?}"),
        },
        Check::Share => {
            let note = share::SharedFile {
                name: String::from("phone-check.txt"),
                data: b"Every check passed on this phone.\n".to_vec(),
            };
            match share::share(win, Some("Shared from Phone Check, a Krate app"), Some("https://krate.tech"), &[note]) {
                Ok(true) => String::from("ok: sent"),
                Ok(false) => String::from("ok: the sheet opened, and you cancelled"),
                Err(e) => format!("failed: {e:?}"),
            }
        }
    };
    say_line(&format!("check:{} {}", CHECKS[slot].1, page.results[slot]));
}

/// A small PNG of Krate's blue fading to violet, for Save to Photos: a
/// stored (uncompressed) zlib stream, which every decoder reads.
fn gradient_png(w: usize, h: usize) -> Vec<u8> {
    let mut table = [0u32; 256];
    for (n, slot) in table.iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *slot = c;
    }
    let crc = |bytes: &[u8]| {
        let mut c = 0xFFFF_FFFFu32;
        for b in bytes {
            c = table[((c ^ *b as u32) & 0xFF) as usize] ^ (c >> 8);
        }
        !c
    };
    let mut raw = Vec::with_capacity((1 + w * 3) * h);
    for y in 0..h {
        raw.push(0);
        for x in 0..w {
            let t = x as f32 / w as f32;
            raw.extend_from_slice(&[(59.0 + 90.0 * t) as u8, (77.0 - 30.0 * t) as u8, (242.0 - 20.0 * (y as f32 / h as f32)) as u8]);
        }
    }
    let mut z = Vec::with_capacity(raw.len() + raw.len() / 65535 * 5 + 16);
    z.extend_from_slice(&[0x78, 0x01]);
    let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, block) in blocks.iter().enumerate() {
        let n = block.len() as u16;
        z.push(if i + 1 == blocks.len() { 1 } else { 0 });
        z.extend_from_slice(&n.to_le_bytes());
        z.extend_from_slice(&(!n).to_le_bytes());
        z.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for v in &raw {
        a = (a + *v as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = alloc::vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut chunk = |kind: &[u8], body: &[u8]| {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        let mut tagged = kind.to_vec();
        tagged.extend_from_slice(body);
        out.extend_from_slice(&tagged);
        out.extend_from_slice(&crc(&tagged).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}

/// Keep a recording going, and play it back once three seconds are in.
fn follow_recording(page: &mut Page, now: u64) {
    let Some((id, started, mut bytes)) = page.recording.take() else {
        return;
    };
    if let Ok(chunk) = capture::read(id, 64 * 1024) {
        bytes.extend_from_slice(&chunk);
    }
    if now.saturating_sub(started) < 3_000_000_000 {
        page.recording = Some((id, started, bytes));
        return;
    }
    let _ = capture::stop(id);
    let slot = index(Check::Record);
    if bytes.is_empty() {
        page.results[slot] = String::from("failed: the microphone sent nothing");
        return;
    }
    let config = playback::StreamConfig { sample_rate: RATE, channels: 1, format: krate::audio::types::SampleFormat::PcmS16, buffer_frames: 1024 };
    page.results[slot] = match playback::open(config).and_then(|out| playback::start(out).map(|()| out)) {
        Ok(out) => {
            let mut sent = 0usize;
            while sent < bytes.len() {
                match playback::write(out, &bytes[sent..]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => sent += n as usize,
                }
            }
            format!("ok: recorded {:.1} s, playing it back", bytes.len() as f32 / (RATE as f32 * 2.0))
        }
        Err(e) => format!("failed to play: {e:?}"),
    };
}

/// The newest camera frame, kept for drawing.
fn follow_camera(canvas: u64, page: &mut Page, now: u64) {
    let Some(id) = page.camera else {
        return;
    };
    if let Ok(Some(frame)) = camera::read(id) {
        if let Some((old, _, _)) = page.picture.take() {
            canvas_images::forget(canvas, old);
        }
        if let Ok(image) = canvas_images::keep(canvas, frame.width, frame.height, &frame.bytes) {
            page.picture = Some((image, frame.width, frame.height));
            page.frames.0 += 1;
        }
    }
    if now.saturating_sub(page.frames.1) >= 1_000_000_000 {
        let slot = index(Check::Camera);
        page.results[slot] = format!(
            "ok: {} frames a second, {}x{} -- tap to stop",
            page.frames.0,
            page.picture.map(|p| p.1).unwrap_or(0),
            page.picture.map(|p| p.2).unwrap_or(0)
        );
        say_line(&format!("check:camera {}", page.results[slot]));
        page.frames = (0, now);
    }
}

struct Component;

impl Guest for Component {
    fn run() -> i32 {
        let Ok(win) = window::create("Phone Check", types::WindowSize { width: 420, height: 760 }) else {
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
        if args::raw().split('\n').any(|a| a == "quick-photos") {
            // Picking where nothing is on screen answers as cancelled;
            // adding refuses what is not a picture before anything else,
            // then finds no library here.
            let word = |r: Result<(), photos::PhotosError>| match r {
                Ok(()) => String::from("ok"),
                Err(photos::PhotosError::NotAllowed) => String::from("not-allowed"),
                Err(photos::PhotosError::Invalid(_)) => String::from("invalid"),
                Err(photos::PhotosError::Unavailable(_)) => String::from("unavailable"),
            };
            say_line(&format!(
                "photos:pick={} junk={} png={}",
                photos::pick(win, 3).map(|p| p.len()).unwrap_or(99),
                word(photos::save(win, b"not a picture")),
                word(photos::save(win, &gradient_png(8, 8))),
            ));
            return 0;
        }
        if args::raw().split('\n').any(|a| a == "quick-share") {
            // What the share sheet refuses before it opens, then whether it
            // opens at all here.
            let word = |r: Result<bool, share::ShareError>| match r {
                Ok(sent) => format!("ok:{sent}"),
                Err(share::ShareError::Invalid(_)) => String::from("invalid"),
                Err(share::ShareError::Unavailable(_)) => String::from("unavailable"),
            };
            let pathy = share::SharedFile { name: String::from("../escape.txt"), data: b"x".to_vec() };
            say_line(&format!(
                "share:empty={} path={} note={}",
                word(share::share(win, None, None, &[])),
                word(share::share(win, None, None, &[pathy])),
                word(share::share(win, Some("hello"), None, &[])),
            ));
            return 0;
        }
        let mut page = Page {
            rows: Vec::new(),
            results: Default::default(),
            recording: None,
            camera: None,
            picture: None,
            frames: (0, 0),
        };
        loop {
            let now = clock::monotonic_nanos();
            follow_recording(&mut page, now);
            follow_camera(canvas, &mut page, now);
            if draw(canvas, &Screen::of(win), &mut page).is_err() {
                return 1;
            }
            let busy = page.recording.is_some() || page.camera.is_some();
            let mut tapped = None;
            let mut next = events::wait(Some(if busy { 33 } else { 250 }));
            while let Some(event) = next {
                match event {
                    Event::CloseRequested(_) => {
                        if let Some(id) = page.camera {
                            let _ = camera::close(id);
                        }
                        return 0;
                    }
                    Event::Pointer(p) if p.button.is_some() && !p.pressed => {
                        tapped = page.rows.iter().position(|r| r.contains(p.x, p.y));
                    }
                    _ => {}
                }
                next = events::poll();
            }
            if let Some(i) = tapped {
                run_check(win, CHECKS[i].0, &mut page);
            }
        }
    }
}

krate::export!(Component);

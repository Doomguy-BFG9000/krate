// Heart: a heart-rate monitor the person picks, through krate:ui/bluetooth
// (IC-917). Heart Rate service 180D, its measurement 2A37 (sent on each
// beat), and the Battery service 180F, level 2A19, when the strap has it.
//
//   krate run bluetooth-probe.krate -- quick   one line, then exit:
//                                              `bluetooth:<name> bpm=<n> battery=<n|none>`
//                                              or `bluetooth:<why not>`
#![no_std]
extern crate alloc;

use alloc::format;
use alloc::string::String;
use krate::gfx::{canvas2d, canvas_images, types as gfx};
use krate::layout::{Rect, Screen};
use krate::ui::bluetooth::{self, BluetoothError, Device};
use krate::ui::haptics::{self, Feel};
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

const HEART: &str = "180D";
const MEASUREMENT: &str = "2A37";
const BATTERY: &str = "180F";
const LEVEL: &str = "2A19";

fn rgb(r: f32, g: f32, b: f32) -> gfx::Color {
    gfx::Color { r, g, b, a: 1.0 }
}

fn style(weight: u16) -> gfx::TextStyle {
    gfx::TextStyle { weight, italic: false, letter_spacing: 0.0, family: gfx::FontFamily::Sans }
}

fn radii(all: f32) -> gfx::CornerRadii {
    gfx::CornerRadii { top_left: all, top_right: all, bottom_right: all, bottom_left: all }
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

/// Beats a minute from a Heart Rate Measurement: one byte, or two when the
/// first bit of the flags says so (the Bluetooth SIG's format).
fn bpm(data: &[u8]) -> Option<u16> {
    let flags = *data.first()?;
    if flags & 1 == 0 {
        data.get(1).map(|b| *b as u16)
    } else {
        Some(u16::from_le_bytes([*data.get(1)?, *data.get(2)?]))
    }
}

fn why(error: &BluetoothError) -> String {
    match error {
        BluetoothError::NotAllowed => String::from("Not allowed. Krate asks before an app can reach a Bluetooth device."),
        BluetoothError::Unavailable => String::from("Bluetooth is off, or this device has none."),
        BluetoothError::NotFound => String::from("No heart-rate monitor was found nearby."),
        BluetoothError::Disconnected => String::from("The monitor went away. Connect it again."),
        BluetoothError::Failed(words) => format!("The monitor said: {words}"),
    }
}

/// A smooth heart, drawn once from the heart curve
/// (x^2 + y^2 - 1)^3 - x^2 y^3 <= 0, with soft edges, as RGBA.
fn heart_picture(size: u32) -> alloc::vec::Vec<u8> {
    let mut px = alloc::vec::Vec::with_capacity((size * size * 4) as usize);
    let inside = |x: f32, y: f32| {
        let a = x * x + y * y - 1.0;
        a * a * a - x * x * y * y * y
    };
    for row in 0..size {
        for col in 0..size {
            // Four samples a pixel for a smooth edge.
            let mut hits = 0;
            for (dx, dy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                let x = ((col as f32 + dx) / size as f32 - 0.5) * 2.7;
                let y = (0.5 - (row as f32 + dy) / size as f32) * 2.7 + 0.15;
                if inside(x, y) <= 0.0 {
                    hits += 1;
                }
            }
            let a = (hits * 255 / 4) as u8;
            px.extend_from_slice(&[246, 51, 77, a]);
        }
    }
    px
}

struct Page {
    heart: Option<u64>,
    device: Option<Device>,
    bpm: Option<u16>,
    battery: Option<u8>,
    note: String,
    /// When the last beat came, for the pulse.
    beat_at: u64,
    button: Rect,
    /// Read any device's battery: headphones, a keyboard, a tracker.
    battery_button: Rect,
    /// The last battery read that way: the device's name and its level.
    other_battery: Option<(String, u8)>,
}

fn draw(canvas: u64, screen: &Screen, page: &mut Page, now: u64) -> Result<(), gfx::GfxError> {
    let ink = rgb(0.07, 0.07, 0.09);
    let muted = rgb(0.43, 0.44, 0.48);
    let red = rgb(0.96, 0.20, 0.30);
    canvas2d::clear(canvas, rgb(0.96, 0.96, 0.97))?;
    let area = screen.safe_area().inset(16.0, 20.0, 16.0, 20.0);
    canvas2d::draw_text_styled(canvas, "Heart", gfx::Point { x: area.x, y: area.y + 34.0 }, 32.0, ink, style(800))?;
    let subtitle = match &page.device {
        Some(d) => format!("Connected to {}", d.name),
        None => String::from("Pick a heart-rate monitor near you"),
    };
    canvas2d::draw_text_styled(canvas, &subtitle, gfx::Point { x: area.x, y: area.y + 60.0 }, 15.0, muted, style(500))?;

    let (cx, cy) = (area.x + area.w / 2.0, area.y + area.h * 0.40);
    // The heart beats: it swells for a moment on each measurement.
    let since = now.saturating_sub(page.beat_at) as f32 / 1_000_000.0;
    let swell = if page.bpm.is_some() && since < 250.0 { 1.0 + 0.12 * (1.0 - since / 250.0) } else { 1.0 };
    let r = 56.0 * swell;
    if page.heart.is_none() {
        page.heart = canvas_images::keep(canvas, 160, 160, &heart_picture(160)).ok();
    }
    match page.heart {
        Some(heart) => canvas_images::draw(canvas, heart, gfx::Rect { x: cx - r, y: cy - r, width: r * 2.0, height: r * 2.0 })?,
        None => canvas2d::fill_circle(canvas, gfx::Point { x: cx, y: cy }, r * 0.7, red)?,
    }
    let number = match page.bpm {
        Some(n) => format!("{n}"),
        None => String::from("--"),
    };
    let nm = canvas2d::measure_text_styled(canvas, &number, 84.0, style(700))?;
    canvas2d::draw_text_styled(canvas, &number, gfx::Point { x: cx - nm.width / 2.0, y: cy + r + 100.0 }, 84.0, ink, style(700))?;
    let unit = "beats a minute";
    let um = canvas2d::measure_text_styled(canvas, unit, 17.0, style(500))?;
    canvas2d::draw_text_styled(canvas, unit, gfx::Point { x: cx - um.width / 2.0, y: cy + r + 128.0 }, 17.0, muted, style(500))?;
    if let Some(level) = page.battery {
        let text = format!("Monitor battery {level}%");
        let tm = canvas2d::measure_text_styled(canvas, &text, 15.0, style(500))?;
        canvas2d::draw_text_styled(canvas, &text, gfx::Point { x: cx - tm.width / 2.0, y: cy + r + 156.0 }, 15.0, muted, style(500))?;
    }
    if let Some((name, level)) = &page.other_battery {
        let text = format!("{name}: battery {level}%");
        let tm = canvas2d::measure_text_styled(canvas, &text, 17.0, style(600))?;
        canvas2d::draw_text_styled(canvas, &text, gfx::Point { x: cx - tm.width / 2.0, y: cy + r + 190.0 }, 17.0, ink, style(600))?;
    }
    if !page.note.is_empty() {
        canvas2d::draw_text_styled(canvas, &page.note, gfx::Point { x: area.x, y: area.y + area.h - 140.0 }, 14.0, muted, style(500))?;
    }
    // Any device's battery, by the standard Battery service.
    page.battery_button = Rect::new(area.x, area.y + area.h - 120.0, area.w, 48.0);
    let bb = page.battery_button;
    canvas2d::fill_round_rect(canvas, gfx::Rect { x: bb.x, y: bb.y, width: bb.w, height: bb.h }, radii(14.0), rgb(1.0, 1.0, 1.0))?;
    let bl = "Read a device's battery";
    let blm = canvas2d::measure_text_styled(canvas, bl, 16.0, style(600))?;
    canvas2d::draw_text_styled(canvas, bl, gfx::Point { x: bb.x + (bb.w - blm.width) / 2.0, y: bb.y + 30.0 }, 16.0, rgb(0.23, 0.30, 0.95), style(600))?;
    page.button = Rect::new(area.x, area.y + area.h - 64.0, area.w, 54.0);
    let label = if page.device.is_some() { "Disconnect" } else { "Connect a monitor" };
    let fill = if page.device.is_some() { rgb(0.90, 0.90, 0.92) } else { rgb(0.23, 0.30, 0.95) };
    let text_ink = if page.device.is_some() { ink } else { rgb(1.0, 1.0, 1.0) };
    let b = page.button;
    canvas2d::fill_round_rect(canvas, gfx::Rect { x: b.x, y: b.y, width: b.w, height: b.h }, radii(16.0), fill)?;
    let lm = canvas2d::measure_text_styled(canvas, label, 17.0, style(650))?;
    canvas2d::draw_text_styled(canvas, label, gfx::Point { x: b.x + (b.w - lm.width) / 2.0, y: b.y + 33.0 }, 17.0, text_ink, style(650))?;
    canvas2d::present(canvas)
}

/// Pick a monitor, start its beats, read its battery.
fn connect(win: u64, page: &mut Page) {
    page.note.clear();
    match bluetooth::choose(win, HEART, "Pick your heart-rate monitor") {
        Ok(Some(device)) => {
            if let Err(error) = bluetooth::subscribe(device.id, HEART, MEASUREMENT) {
                page.note = why(&error);
                return;
            }
            page.battery = bluetooth::read(device.id, BATTERY, LEVEL).ok().and_then(|v| v.first().copied());
            page.device = Some(device);
            haptics::play(Feel::Success);
        }
        Ok(None) => {}
        Err(error) => page.note = why(&error),
    }
}

/// Pick any device offering the Battery service, read its level once, let
/// it go.
fn read_battery(win: u64, page: &mut Page) {
    page.note.clear();
    match bluetooth::choose(win, BATTERY, "Pick a device to read its battery") {
        Ok(Some(device)) => {
            match bluetooth::read(device.id, BATTERY, LEVEL) {
                Ok(level) if !level.is_empty() => {
                    page.other_battery = Some((device.name.clone(), level[0]));
                    haptics::play(Feel::Success);
                }
                Ok(_) => page.note = String::from("That device sent an empty battery reading."),
                Err(error) => page.note = why(&error),
            }
            bluetooth::disconnect(device.id);
        }
        Ok(None) => {}
        Err(error) => page.note = why(&error),
    }
}

/// Take every beat sent since the last frame; true when there was one.
fn collect(page: &mut Page, now: u64) -> bool {
    let Some(device) = &page.device else {
        return false;
    };
    if !bluetooth::connected(device.id) {
        page.device = None;
        page.note = why(&BluetoothError::Disconnected);
        return false;
    }
    let mut beat = false;
    for n in bluetooth::notifications(device.id) {
        if n.characteristic.eq_ignore_ascii_case(MEASUREMENT) {
            if let Some(value) = bpm(&n.data) {
                page.bpm = Some(value);
                page.beat_at = now;
                beat = true;
            }
        }
    }
    beat
}

struct Component;

impl Guest for Component {
    fn run() -> i32 {
        let quick = args::raw().split('\n').any(|a| a == "quick");
        let Ok(win) = window::create("Heart", types::WindowSize { width: 420, height: 720 }) else {
            return 1;
        };
        let mut page = Page {
            heart: None,
            device: None,
            bpm: None,
            battery: None,
            note: String::new(),
            beat_at: 0,
            button: Rect::new(0.0, 0.0, 0.0, 0.0),
            battery_button: Rect::new(0.0, 0.0, 0.0, 0.0),
            other_battery: None,
        };
        if quick {
            if !bluetooth::available() {
                say("bluetooth:unavailable");
                return 0;
            }
            match bluetooth::choose(win, HEART, "Pick your heart-rate monitor") {
                Ok(Some(device)) => {
                    let subscribed = bluetooth::subscribe(device.id, HEART, MEASUREMENT);
                    let beats = bluetooth::notifications(device.id);
                    let value = beats.iter().find_map(|n| bpm(&n.data));
                    let battery = bluetooth::read(device.id, BATTERY, LEVEL).ok().and_then(|v| v.first().copied());
                    say(&format!(
                        "bluetooth:{} subscribed={} bpm={} battery={}",
                        device.name,
                        if subscribed.is_ok() { "yes" } else { "no" },
                        value.map(|v| format!("{v}")).unwrap_or_else(|| String::from("none")),
                        battery.map(|v| format!("{v}")).unwrap_or_else(|| String::from("none")),
                    ));
                }
                Ok(None) => say("bluetooth:cancelled"),
                Err(BluetoothError::NotAllowed) => say("bluetooth:not-allowed"),
                Err(BluetoothError::Unavailable) => say("bluetooth:unavailable"),
                Err(BluetoothError::NotFound) => say("bluetooth:not-found"),
                Err(BluetoothError::Disconnected) => say("bluetooth:disconnected"),
                Err(BluetoothError::Failed(w)) => say(&format!("bluetooth:failed {w}")),
            }
            return 0;
        }
        if tree::set_root(win, &node(1, None, types::WidgetKind::Stack)).is_err()
            || tree::upsert_node(win, &node(2, Some(1), types::WidgetKind::Canvas)).is_err()
        {
            return 1;
        }
        let Ok(canvas) = canvas2d::bind(win, 2) else {
            return 1;
        };
        if !bluetooth::available() {
            page.note = why(&BluetoothError::Unavailable);
        }
        loop {
            let now = clock::monotonic_nanos();
            if collect(&mut page, now) {
                haptics::play(Feel::Light);
            }
            if draw(canvas, &Screen::of(win), &mut page, now).is_err() {
                return 1;
            }
            let mut tapped = false;
            let mut battery_tapped = false;
            let mut next = events::wait(Some(33));
            while let Some(event) = next {
                match event {
                    Event::CloseRequested(_) => {
                        if let Some(device) = &page.device {
                            bluetooth::disconnect(device.id);
                        }
                        return 0;
                    }
                    Event::Pointer(p) if p.button.is_some() && !p.pressed && page.button.contains(p.x, p.y) => {
                        tapped = true;
                    }
                    Event::Pointer(p) if p.button.is_some() && !p.pressed && page.battery_button.contains(p.x, p.y) => {
                        battery_tapped = true;
                    }
                    _ => {}
                }
                next = events::poll();
            }
            if battery_tapped {
                read_battery(win, &mut page);
            }
            if tapped {
                match page.device.take() {
                    Some(device) => {
                        bluetooth::disconnect(device.id);
                        page.bpm = None;
                        page.battery = None;
                    }
                    None => connect(win, &mut page),
                }
            }
        }
    }
}

krate::export!(Component);

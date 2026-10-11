//! On-screen controls for a keyboard game on a touch screen (IC-914).
//!
//! A game written for a PC reads keys -- `key-held("ArrowLeft")`, Space to
//! jump -- and on a phone there are none, so it cannot be played at all.
//! The host knows which keys a game asks about, so on a touch screen, for a
//! game that does not read touch itself, it draws a pad built from exactly
//! those keys: a D-pad for the directions and a button for each other key.
//! Pressing them makes the game's own key questions answer true (and sends
//! the key events an event-driven game listens for), so the game works on
//! a phone unchanged.
//!
//! This module is the pure part: which keys become which controls, where
//! they sit on a screen, and which are held under which fingers. Drawing
//! and input are the host's (`phase3_gui_host`).

use std::collections::BTreeSet;

/// A key's name in one spelling, whatever the game wrote: `" "` and
/// `"Space"` are one key, `"r"` and `"R"` are one key.
pub fn canonical(key: &str) -> String {
    let k = key.trim_matches('\0');
    if k == " " || k.eq_ignore_ascii_case("space") || k.eq_ignore_ascii_case("spacebar") {
        return "space".to_string();
    }
    k.to_ascii_lowercase()
}

/// The four directions of the D-pad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

const DIRS: [Dir; 4] = [Dir::Left, Dir::Right, Dir::Up, Dir::Down];

fn arrow(dir: Dir) -> &'static str {
    match dir {
        Dir::Left => "arrowleft",
        Dir::Right => "arrowright",
        Dir::Up => "arrowup",
        Dir::Down => "arrowdown",
    }
}

fn wasd(dir: Dir) -> &'static str {
    match dir {
        Dir::Left => "a",
        Dir::Right => "d",
        Dir::Up => "w",
        Dir::Down => "s",
    }
}

/// The controls a game's keys call for.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Pad {
    /// Each direction the game reads, with every key it reads for it, in
    /// the game's own spellings.
    pub dirs: Vec<(Dir, Vec<String>)>,
    /// Each other key: a label and the game's spellings of it.
    pub buttons: Vec<(String, Vec<String>)>,
}

/// The most action buttons a pad shows: more do not fit under a thumb.
pub const MAX_BUTTONS: usize = 4;

impl Pad {
    /// The pad for the keys a game has asked about, in the order it asked;
    /// None when nothing it asked is a control (no keys at all).
    pub fn from_keys(asked: &[String]) -> Option<Pad> {
        let names: Vec<String> = asked.iter().map(|k| canonical(k)).collect();
        // W, A, S and D are directions only when the game reads at least two
        // of them; one alone ("a" for an attack) is an action key.
        let wasd_count = DIRS
            .iter()
            .filter(|d| names.iter().any(|n| n == wasd(**d)))
            .count();
        let wasd_dirs = wasd_count >= 2;
        let mut pad = Pad::default();
        let mut used: BTreeSet<String> = BTreeSet::new();
        for dir in DIRS {
            let mut keys = Vec::new();
            for (key, name) in asked.iter().zip(&names) {
                if name == arrow(dir) || (wasd_dirs && name == wasd(dir)) {
                    keys.push(key.clone());
                    used.insert(name.clone());
                }
            }
            if !keys.is_empty() {
                pad.dirs.push((dir, keys));
            }
        }
        for (key, name) in asked.iter().zip(&names) {
            if used.contains(name) {
                if let Some((_, keys)) = pad
                    .buttons
                    .iter_mut()
                    .find(|(_, keys)| keys.iter().any(|k| canonical(k) == *name))
                {
                    if !keys.contains(key) {
                        keys.push(key.clone());
                    }
                }
                continue;
            }
            if pad.buttons.len() >= MAX_BUTTONS {
                continue;
            }
            used.insert(name.clone());
            pad.buttons.push((label(name), vec![key.clone()]));
        }
        // Space, else Enter, is a game's main action wherever it was asked:
        // it gets the big button under the thumb. (Track Dash asks for Z, a
        // second way to run left, before Space, its jump.)
        if let Some(main) = pad
            .buttons
            .iter()
            .position(|(_, keys)| keys.iter().any(|k| canonical(k) == "space"))
            .or_else(|| {
                pad.buttons.iter().position(|(_, keys)| {
                    keys.iter()
                        .any(|k| matches!(canonical(k).as_str(), "enter" | "return"))
                })
            })
        {
            let button = pad.buttons.remove(main);
            pad.buttons.insert(0, button);
        }
        if pad.dirs.is_empty() && pad.buttons.is_empty() {
            None
        } else {
            Some(pad)
        }
    }
}

/// What a button says: the key, short enough to fit a thumb's circle.
fn label(name: &str) -> String {
    match name {
        "space" => "Space".to_string(),
        "enter" | "return" => "Enter".to_string(),
        "escape" => "Esc".to_string(),
        "shift" | "shiftleft" | "shiftright" => "Shift".to_string(),
        "control" | "controlleft" | "controlright" => "Ctrl".to_string(),
        _ if name.chars().count() == 1 => name.to_ascii_uppercase(),
        _ => name.chars().take(5).collect(),
    }
}

/// Where a pad sits on a screen, in the window's points.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Layout {
    /// The D-pad's centre and radius.
    pub dpad: Option<(f32, f32, f32)>,
    /// Each button's centre and radius, in the pad's button order.
    pub buttons: Vec<(f32, f32, f32)>,
}

/// Lay a pad out on a `w` by `h` screen whose edges are covered by `safe`
/// (top, right, bottom, left): the D-pad under the left thumb, the buttons
/// under the right, clear of the notch and home bar.
pub fn layout(pad: &Pad, w: f32, h: f32, safe: (f32, f32, f32, f32)) -> Layout {
    let (_, right, bottom, left) = safe;
    let margin = 22.0;
    let short = w.min(h);
    let r = (short * 0.17).clamp(48.0, 70.0);
    let b = (r * 0.52).clamp(28.0, 38.0);
    let bottom_y = h - bottom - margin;
    let dpad = (!pad.dirs.is_empty()).then_some((left + margin + r, bottom_y - r, r));
    // Buttons in a diamond's lower-right half, the first under the thumb's
    // rest.
    let ax = w - right - margin - b;
    let ay = bottom_y - b - b * 0.6;
    // Each at least two and a half radii from the next, so a thumb on
    // one never presses its neighbour.
    let spots = [
        (ax, ay),
        (ax - b * 2.6, ay + b * 0.7),
        (ax - b * 0.5, ay - b * 2.6),
        (ax - b * 3.1, ay - b * 1.9),
    ];
    // The first button -- the key a game asked about first, its main
    // action -- is the biggest, under the thumb's rest.
    let buttons = pad
        .buttons
        .iter()
        .zip(spots)
        .enumerate()
        .map(|(i, (_, (x, y)))| (x, y, if i == 0 { b * 1.18 } else { b }))
        .collect();
    Layout { dpad, buttons }
}

/// Which directions a finger at (x, y) presses on a D-pad at (cx, cy, r):
/// anywhere within half again the pad's radius, diagonals pressing two.
fn dirs_at(x: f32, y: f32, (cx, cy, r): (f32, f32, f32)) -> Vec<Dir> {
    let (dx, dy) = (x - cx, y - cy);
    if (dx * dx + dy * dy).sqrt() > r * 1.5 {
        return Vec::new();
    }
    let dead = r * 0.3;
    let mut out = Vec::new();
    if dx < -dead {
        out.push(Dir::Left);
    }
    if dx > dead {
        out.push(Dir::Right);
    }
    if dy < -dead {
        out.push(Dir::Up);
    }
    if dy > dead {
        out.push(Dir::Down);
    }
    out
}

/// The game's own key spellings held by the fingers now on the screen.
pub fn held(pad: &Pad, layout: &Layout, fingers: &[(f32, f32)]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for &(x, y) in fingers {
        if let Some(d) = layout.dpad {
            for dir in dirs_at(x, y, d) {
                if let Some((_, keys)) = pad.dirs.iter().find(|(pd, _)| *pd == dir) {
                    out.extend(keys.iter().cloned());
                }
            }
        }
        for ((_, keys), (bx, by, br)) in pad.buttons.iter().zip(&layout.buttons) {
            let (dx, dy) = (x - bx, y - by);
            if (dx * dx + dy * dy).sqrt() <= br * 1.25 {
                out.extend(keys.iter().cloned());
            }
        }
    }
    out
}

/// Where the stick's knob sits for the fingers now on the screen: under the
/// finger that is on the stick, held within its base; None when no finger
/// is on it (the knob rests in the middle).
pub fn knob(layout: &Layout, fingers: &[(f32, f32)]) -> Option<(f32, f32)> {
    let (cx, cy, r) = layout.dpad?;
    let &(x, y) = fingers.iter().find(|&&(x, y)| {
        let (dx, dy) = (x - cx, y - cy);
        (dx * dx + dy * dy).sqrt() <= r * 1.5
    })?;
    let (dx, dy) = (x - cx, y - cy);
    let d = (dx * dx + dy * dy).sqrt();
    let reach = r * 0.5;
    if d <= reach {
        Some((x, y))
    } else {
        Some((cx + dx / d * reach, cy + dy / d * reach))
    }
}

/// Whether a point is on the pad: a press there belongs to the pad, not the
/// game underneath it.
pub fn covers(layout: &Layout, x: f32, y: f32) -> bool {
    let near = |(cx, cy, r): (f32, f32, f32), reach: f32| {
        let (dx, dy) = (x - cx, y - cy);
        (dx * dx + dy * dy).sqrt() <= r * reach
    };
    layout.dpad.is_some_and(|d| near(d, 1.5)) || layout.buttons.iter().any(|b| near(*b, 1.25))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(list: &[&str]) -> Vec<String> {
        list.iter().map(|k| k.to_string()).collect()
    }

    #[test]
    fn a_racing_game_gets_arrows_and_its_action_keys() {
        // Track Dash's keys, in the order it asks them.
        let pad = Pad::from_keys(&keys(&[
            "ArrowLeft",
            "a",
            "ArrowRight",
            "d",
            "ArrowUp",
            "w",
            " ",
            "Space",
            "z",
            "x",
            "R",
            "r",
        ]))
        .unwrap();
        let dirs: Vec<Dir> = pad.dirs.iter().map(|(d, _)| *d).collect();
        assert_eq!(
            dirs,
            vec![Dir::Left, Dir::Right, Dir::Up],
            "it never reads down"
        );
        assert_eq!(pad.dirs[0].1, keys(&["ArrowLeft", "a"]));
        let labels: Vec<&str> = pad.buttons.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, vec!["Space", "Z", "X", "R"]);
        assert_eq!(
            pad.buttons[0].1,
            keys(&[" ", "Space"]),
            "both spellings of Space are pressed"
        );
        assert_eq!(pad.buttons[3].1, keys(&["R", "r"]));
    }

    #[test]
    fn space_is_the_main_button_wherever_it_was_asked() {
        // Track Dash's real order: Z and X (other ways to run) come first.
        let pad = Pad::from_keys(&keys(&[
            "ArrowLeft",
            "a",
            "z",
            "ArrowRight",
            "d",
            "x",
            "Space",
            " ",
            "ArrowUp",
            "w",
            "r",
            "R",
        ]))
        .unwrap();
        let labels: Vec<&str> = pad.buttons.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, vec!["Space", "Z", "X", "R"]);
    }

    #[test]
    fn one_wasd_letter_alone_is_an_action_key() {
        let pad = Pad::from_keys(&keys(&["ArrowLeft", "ArrowRight", "a"])).unwrap();
        assert_eq!(pad.dirs.len(), 2);
        assert_eq!(pad.buttons, vec![("A".to_string(), keys(&["a"]))]);
    }

    #[test]
    fn no_keys_no_pad() {
        assert!(Pad::from_keys(&[]).is_none());
    }

    #[test]
    fn fingers_hold_the_keys_under_them_and_nothing_else() {
        let pad = Pad::from_keys(&keys(&["ArrowLeft", "ArrowRight", "ArrowUp", "Space"])).unwrap();
        let lay = layout(&pad, 844.0, 390.0, (50.0, 47.0, 21.0, 47.0));
        let (cx, cy, r) = lay.dpad.unwrap();
        let (bx, by, _) = lay.buttons[0];
        // Thumb up-left on the D-pad, other thumb on Space.
        let held_now = held(&pad, &lay, &[(cx - r * 0.7, cy - r * 0.7), (bx, by)]);
        assert_eq!(
            held_now,
            ["ArrowLeft", "ArrowUp", "Space"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        );
        // A finger in the middle of the screen holds nothing and is the game's.
        assert!(held(&pad, &lay, &[(422.0, 195.0)]).is_empty());
        assert!(!covers(&lay, 422.0, 195.0));
        assert!(covers(&lay, cx, cy));
    }

    #[test]
    fn the_knob_follows_the_thumb_and_stays_in_its_base() {
        let pad = Pad::from_keys(&keys(&["ArrowLeft", "ArrowRight"])).unwrap();
        let lay = layout(&pad, 844.0, 390.0, (50.0, 47.0, 21.0, 47.0));
        let (cx, cy, r) = lay.dpad.unwrap();
        assert_eq!(knob(&lay, &[]), None);
        assert_eq!(knob(&lay, &[(cx + 5.0, cy)]), Some((cx + 5.0, cy)));
        let (kx, ky) = knob(&lay, &[(cx + r * 1.2, cy)]).unwrap();
        assert!((kx - (cx + r * 0.5)).abs() < 0.01 && (ky - cy).abs() < 0.01);
        assert_eq!(knob(&lay, &[(422.0, 195.0)]), None, "a finger elsewhere");
    }

    #[test]
    fn the_pad_stays_clear_of_the_covered_edges() {
        let pad = Pad::from_keys(&keys(&["ArrowLeft", "Space"])).unwrap();
        let (w, h, safe) = (844.0, 390.0, (50.0, 47.0, 21.0, 47.0));
        let lay = layout(&pad, w, h, safe);
        let (cx, cy, r) = lay.dpad.unwrap();
        assert!(cx - r >= safe.3 && cy + r <= h - safe.2, "{lay:?}");
        let (bx, by, br) = lay.buttons[0];
        assert!(bx + br <= w - safe.1 && by + br <= h - safe.2, "{lay:?}");
    }
}

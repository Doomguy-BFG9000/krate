// Stuck: an app that never draws (IC-934). It opens its window and never
// presents a frame, so Krate's opening cover stays up; after five seconds it
// stops answering the host altogether and spins. Touch and hold the top of
// the screen: Krate's menu must still open over it, and Close and Restart
// must still work, whatever the app is doing.
#![no_std]

use krate::gfx::canvas2d;
use krate::{
    time::clock,
    ui::{
        events, tree,
        types::{self, Event},
        window,
    },
    Guest,
};

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

impl Guest for Component {
    fn run() -> i32 {
        let Ok(win) = window::create("Stuck", types::WindowSize { width: 420, height: 720 }) else {
            return 1;
        };
        if tree::set_root(win, &node(1, None, types::WidgetKind::Stack)).is_err()
            || tree::upsert_node(win, &node(2, Some(1), types::WidgetKind::Canvas)).is_err()
        {
            return 1;
        }
        // Bound, never presented: the cover never lifts.
        let _canvas = canvas2d::bind(win, 2);
        let start = clock::monotonic_nanos();
        // Five seconds of waiting quietly, still answering a close.
        while clock::monotonic_nanos().saturating_sub(start) < 5_000_000_000 {
            if let Some(Event::CloseRequested(_)) = events::wait(Some(250)) {
                return 0;
            }
        }
        // Then never calls the host again.
        let mut n: u64 = 0;
        loop {
            n = core::hint::black_box(n.wrapping_add(1));
        }
    }
}

krate::export!(Component);

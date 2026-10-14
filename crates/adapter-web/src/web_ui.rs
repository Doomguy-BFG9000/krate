//! The `krate:ui` host's drawing half, for a browser tab (IC-956 Phase 0).
//!
//! The app itself runs on the browser's own WebAssembly engine (its
//! component transpiled to core wasm plus JS glue by `jco`). The glue hands
//! every `tree.set-root` / `tree.upsert-node` call to JS, and JS forwards it
//! here: one window's widget tree, laid out by `krate-layout` and painted by
//! `krate-adapter-common` -- the same two crates a Mac uses -- into an RGBA
//! buffer a canvas takes with `putImageData`. Hit-testing a pointer press
//! uses the layout crate's own `hit_test`, so `pointer.widget` names the
//! widget the person pressed.
//!
//! What this is NOT: the runtime's full lowering (`phase3_gui_host.rs`:
//! scroll offsets, focus, text measurement, vector text). This is the plain
//! case only, enough for `apps/krate-hello-gui`, and it exists to measure
//! the browser path. Phase 1 replaces it with the runtime's lowering built
//! for wasm32, so the tab cannot drift from the desktop.

use krate_adapter_common::painter::{self, PaintInteraction};
use krate_adapter_common::ui::{
    WidgetId, WidgetKind, WidgetNode, WidgetPlacement, WidgetStyle, WidgetTree,
};
use krate_layout::{absolute_rect, compute_layout, hit_test, LayoutPoint, LayoutViewport};

/// The WIT `widget-kind` enum, in declaration order (wit/krate/phase4).
const KINDS: [WidgetKind; 18] = [
    WidgetKind::Stack,
    WidgetKind::Grid,
    WidgetKind::Scroll,
    WidgetKind::Tabs,
    WidgetKind::Button,
    WidgetKind::Checkbox,
    WidgetKind::Radio,
    WidgetKind::Switch,
    WidgetKind::Slider,
    WidgetKind::Progress,
    WidgetKind::Text,
    WidgetKind::TextField,
    WidgetKind::TextArea,
    WidgetKind::ListView,
    WidgetKind::TreeView,
    WidgetKind::Image,
    WidgetKind::Canvas,
    WidgetKind::Overlay,
];

/// One widget node as the guest sent it, already flattened by the JS glue.
#[derive(Debug, Clone, Default)]
pub struct NodeParts {
    pub id: u64,
    pub parent: Option<u64>,
    pub kind: u8,
    pub label: Option<String>,
    pub role: Option<String>,
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub grow: f32,
    pub padding: f32,
    pub checked: Option<bool>,
    pub value: Option<f32>,
    pub selected: Option<u32>,
}

fn node_from_parts(parts: NodeParts) -> Result<WidgetNode, String> {
    let id = WidgetId::new(parts.id).map_err(|e| e.to_string())?;
    let kind = *KINDS
        .get(parts.kind as usize)
        .ok_or_else(|| format!("unknown widget kind {}", parts.kind))?;
    let mut node = WidgetNode::new(id, kind);
    node.parent = match parts.parent {
        Some(p) => Some(WidgetId::new(p).map_err(|e| e.to_string())?),
        None => None,
    };
    node.label = parts.label;
    node.role = parts.role;
    node.style = WidgetStyle {
        width: parts.width,
        height: parts.height,
        grow: parts.grow,
        padding: parts.padding,
        ..WidgetStyle::default()
    };
    node.checked = parts.checked;
    node.value = parts.value;
    node.selected = parts.selected;
    Ok(node)
}

/// One window: its tree, its last layout, and its frame.
pub struct WindowSurface {
    pub width: u32,
    pub height: u32,
    tree: Option<WidgetTree>,
    dirty: bool,
    buffer: Vec<u32>,
    rgba: Vec<u8>,
}

impl WindowSurface {
    pub fn new(width: u32, height: u32) -> Self {
        let len = (width as usize) * (height as usize);
        Self {
            width,
            height,
            tree: None,
            dirty: true,
            buffer: vec![painter::COLOR_BACKGROUND; len],
            rgba: vec![0; len * 4],
        }
    }

    pub fn set_root(&mut self, parts: NodeParts) -> Result<(), String> {
        let node = node_from_parts(parts)?;
        self.tree = Some(WidgetTree::new(node).map_err(|e| e.to_string())?);
        self.dirty = true;
        Ok(())
    }

    pub fn upsert(&mut self, parts: NodeParts) -> Result<(), String> {
        let node = node_from_parts(parts)?;
        let tree = self.tree.as_mut().ok_or("set-root first")?;
        tree.upsert(node).map_err(|e| e.to_string())?;
        self.dirty = true;
        Ok(())
    }

    pub fn dirty(&self) -> bool {
        self.dirty
    }

    /// Lay out and paint if anything changed. Returns whether it painted.
    pub fn render(&mut self) -> bool {
        if !self.dirty {
            return false;
        }
        self.dirty = false;
        let placements = match &self.tree {
            Some(tree) => placements(tree, self.width as f32, self.height as f32),
            None => Vec::new(),
        };
        self.buffer.fill(painter::COLOR_BACKGROUND);
        painter::paint_placements(
            &mut self.buffer,
            self.width,
            self.height,
            1.0,
            &placements,
            PaintInteraction::default(),
        );
        for (chunk, word) in self.rgba.chunks_exact_mut(4).zip(self.buffer.iter()) {
            chunk[0] = (word >> 16) as u8;
            chunk[1] = (word >> 8) as u8;
            chunk[2] = *word as u8;
            chunk[3] = (word >> 24) as u8;
        }
        true
    }

    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// The deepest widget under a logical point, as the layout crate sees it.
    pub fn hit(&self, x: f32, y: f32) -> Option<u64> {
        let tree = self.tree.as_ref()?;
        let viewport = LayoutViewport::new(self.width as f32, self.height as f32).ok()?;
        let layout = compute_layout(tree, viewport).ok()?;
        let point = LayoutPoint::new(x, y).ok()?;
        hit_test(tree, &layout, point).map(|hit| hit.widget.get())
    }
}

/// Lower a tree to placements: the plain case, plus the two things
/// hello-gui needs to look right -- children of a scroll area clipped to it,
/// and a list view's selected row highlighted.
fn placements(tree: &WidgetTree, width: f32, height: f32) -> Vec<WidgetPlacement> {
    let Ok(viewport) = LayoutViewport::new(width, height) else {
        return Vec::new();
    };
    let Ok(layout) = compute_layout(tree, viewport) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (id, node) in tree.nodes() {
        let Some(rect) = absolute_rect(tree, &layout, *id) else {
            continue;
        };
        let parent = node.parent.and_then(|p| tree.node(p));
        let clip = match parent {
            Some(p) if p.kind == WidgetKind::Scroll => {
                absolute_rect(tree, &layout, p.id).map(|r| (r.x, r.y, r.width, r.height))
            }
            _ => None,
        };
        let selection = match (node.kind, node.selected) {
            (WidgetKind::ListView | WidgetKind::TreeView, Some(index)) => tree
                .nodes()
                .values()
                .filter(|child| child.parent == Some(*id))
                .nth(index as usize)
                .and_then(|child| absolute_rect(tree, &layout, child.id))
                .map(|r| (r.x, r.y, r.width, r.height)),
            _ => None,
        };
        let clickable =
            parent.is_some_and(|p| matches!(p.kind, WidgetKind::ListView | WidgetKind::TreeView));
        out.push(WidgetPlacement {
            widget: *id,
            kind: node.kind,
            label: node.label.clone(),
            checked: node.checked,
            value: node.value,
            selection,
            text_cursor: node.text_cursor,
            clip,
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
            clickable,
            role: node.role.clone(),
            pixels: None,
            text: None,
            r#box: None,
        });
    }
    out
}

#[cfg(target_arch = "wasm32")]
mod bindings {
    use super::{NodeParts, WindowSurface};
    use wasm_bindgen::prelude::*;

    /// The JS-facing window. Every argument is a flattened `widget-node`
    /// field, so no serializer sits between the guest and the painter.
    #[wasm_bindgen]
    pub struct WebWindow {
        inner: WindowSurface,
    }

    #[allow(clippy::too_many_arguments)]
    fn parts(
        id: u64,
        parent: Option<u64>,
        kind: u8,
        label: Option<String>,
        role: Option<String>,
        width: Option<f32>,
        height: Option<f32>,
        grow: f32,
        padding: f32,
        checked: Option<bool>,
        value: Option<f32>,
        selected: Option<u32>,
    ) -> NodeParts {
        NodeParts {
            id,
            parent,
            kind,
            label,
            role,
            width,
            height,
            grow,
            padding,
            checked,
            value,
            selected,
        }
    }

    #[wasm_bindgen]
    impl WebWindow {
        #[wasm_bindgen(constructor)]
        pub fn new(width: u32, height: u32) -> WebWindow {
            WebWindow {
                inner: WindowSurface::new(width, height),
            }
        }

        /// Returns an error sentence, or nothing when it was accepted.
        #[allow(clippy::too_many_arguments)]
        pub fn set_root(
            &mut self,
            id: u64,
            kind: u8,
            label: Option<String>,
            role: Option<String>,
            width: Option<f32>,
            height: Option<f32>,
            grow: f32,
            padding: f32,
            checked: Option<bool>,
            value: Option<f32>,
            selected: Option<u32>,
        ) -> Option<String> {
            let p = parts(
                id, None, kind, label, role, width, height, grow, padding, checked, value, selected,
            );
            self.inner.set_root(p).err()
        }

        #[allow(clippy::too_many_arguments)]
        pub fn upsert(
            &mut self,
            id: u64,
            parent: Option<u64>,
            kind: u8,
            label: Option<String>,
            role: Option<String>,
            width: Option<f32>,
            height: Option<f32>,
            grow: f32,
            padding: f32,
            checked: Option<bool>,
            value: Option<f32>,
            selected: Option<u32>,
        ) -> Option<String> {
            let p = parts(
                id, parent, kind, label, role, width, height, grow, padding, checked, value,
                selected,
            );
            self.inner.upsert(p).err()
        }

        pub fn dirty(&self) -> bool {
            self.inner.dirty()
        }

        pub fn render(&mut self) -> bool {
            self.inner.render()
        }

        /// Where the RGBA frame sits in this module's memory.
        pub fn rgba_ptr(&self) -> *const u8 {
            self.inner.rgba().as_ptr()
        }

        pub fn rgba_len(&self) -> usize {
            self.inner.rgba().len()
        }

        pub fn hit(&self, x: f32, y: f32) -> Option<u64> {
            self.inner.hit(x, y)
        }
    }

    /// The painter module's own linear memory, so JS can view the frame.
    #[wasm_bindgen]
    pub fn painter_memory() -> JsValue {
        wasm_bindgen::memory()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(id: u64, parent: Option<u64>, kind: u8, h: f32) -> NodeParts {
        NodeParts {
            id,
            parent,
            kind,
            label: Some("x".to_string()),
            width: Some(160.0),
            height: Some(h),
            ..NodeParts::default()
        }
    }

    /// The chain the tab runs: a root, a button, painted, and a press on
    /// the button's middle names the button.
    #[test]
    fn a_tree_paints_and_a_press_hits_the_button() {
        let mut win = WindowSurface::new(320, 200);
        let mut root = part(1, None, 0, 200.0);
        root.width = Some(320.0);
        root.padding = 16.0;
        win.set_root(root).expect("root");
        win.upsert(part(2, Some(1), 4, 32.0)).expect("button");
        assert!(win.render(), "a dirty window paints");
        assert!(!win.render(), "a clean one does not");
        assert!(win.rgba().chunks_exact(4).all(|px| px[3] == 0xFF));
        assert_eq!(win.hit(16.0 + 80.0, 16.0 + 16.0), Some(2));
        assert_eq!(win.hit(300.0, 190.0), Some(1));
    }

    #[test]
    fn an_unknown_kind_is_refused_in_words() {
        let mut win = WindowSurface::new(10, 10);
        let err = win.set_root(part(1, None, 99, 10.0)).unwrap_err();
        assert!(err.contains("unknown widget kind"), "{err}");
    }
}

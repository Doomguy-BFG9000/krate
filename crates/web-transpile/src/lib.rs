//! What a browser tab runs for one Krate app, made once at publish
//! (IC-956 Phase 1).
//!
//! [`transpile_component`] hands the app's component to js-component-bindgen
//! -- the engine inside `jco transpile`, pinned to the version jco 1.35.0
//! ships -- with the options the Player loads it with (async instantiation,
//! no Node shims, every core module a separate file), and adds what the
//! Player needs beside the glue:
//!
//! - `modules`: the SHA-256 of every core module embedded in the component,
//!   in the component's own order. The tab computes the same list from the
//!   bundle it judged and runs a `.wasm` file only if it is one of them, so
//!   the code that runs is the code that was judged, whoever served it.
//! - `refusals`: for every function the component imports, how to say "not
//!   in a tab" in that function's own WIT shape -- the error case to throw
//!   (`unsupported`, `permission-denied`...), or a neutral answer for a
//!   function with no error channel -- so an interface the tab does not
//!   implement answers in words instead of failing to instantiate.
//!
//! Compiled to wasm32 with no imports; the hub Worker calls it through the
//! same three-function ABI as the validator ([`krate_alloc`],
//! [`krate_transpile`], [`krate_free`]).

use std::alloc::{alloc as raw_alloc, dealloc, Layout};

use js_component_bindgen::{transpile, InstantiationMode, TranspileOpts};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use wit_parser::{FunctionKind, Resolve, Type, TypeDefKind, TypeId, WorldItem};

/// Which js-component-bindgen made the glue, recorded with every output so a
/// glue made by another version is never mistaken for this one's.
pub const TRANSPILER: &str = "js-component-bindgen 2.12.0 (jco 1.35.0)";

/// One transpiled app: the files a tab loads and what it needs to know.
pub struct Output {
    pub files: Vec<(String, Vec<u8>)>,
    pub meta: Value,
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The SHA-256 of every core module embedded in `component`, at any depth,
/// in the order they appear.
pub fn core_module_digests(component: &[u8]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for payload in wasmparser::Parser::new(0).parse_all(component) {
        let payload = payload.map_err(|err| format!("not a component: {err}"))?;
        if let wasmparser::Payload::ModuleSection {
            unchecked_range, ..
        } = payload
        {
            let range = usize::try_from(unchecked_range.start).map_err(|e| e.to_string())?
                ..usize::try_from(unchecked_range.end).map_err(|e| e.to_string())?;
            let slice = component
                .get(range)
                .ok_or("a module section runs past the end of the component")?;
            out.push(hex(slice));
        }
    }
    Ok(out)
}

fn dealias(resolve: &Resolve, mut id: TypeId) -> TypeId {
    while let TypeDefKind::Type(Type::Id(next)) = resolve.types[id].kind {
        id = next;
    }
    id
}

/// How a refusal is spoken for one error type: the case to throw, and
/// whether it carries words.
fn error_refusal(resolve: &Resolve, err: &Type) -> Value {
    let Type::Id(id) = err else {
        return match err {
            Type::String => json!({ "throw": "string" }),
            _ => json!({ "trap": true }),
        };
    };
    match &resolve.types[dealias(resolve, *id)].kind {
        TypeDefKind::Variant(variant) => {
            let worded = |name: &str| {
                variant
                    .cases
                    .iter()
                    .find(|c| c.name == name && matches!(c.ty, Some(Type::String)))
            };
            let bare = |name: &str| {
                variant
                    .cases
                    .iter()
                    .find(|c| c.name == name && c.ty.is_none())
            };
            for name in ["unsupported", "other", "platform", "failed"] {
                if let Some(case) = worded(name) {
                    return json!({ "throw": "variant", "case": case.name, "words": true });
                }
            }
            for name in [
                "unsupported",
                "permission-denied",
                "denied",
                "not-available",
            ] {
                if let Some(case) = bare(name) {
                    return json!({ "throw": "variant", "case": case.name, "words": false });
                }
            }
            if let Some(case) = variant
                .cases
                .iter()
                .find(|c| matches!(c.ty, Some(Type::String)))
            {
                return json!({ "throw": "variant", "case": case.name, "words": true });
            }
            match variant.cases.iter().find(|c| c.ty.is_none()) {
                Some(case) => json!({ "throw": "variant", "case": case.name, "words": false }),
                None => json!({ "trap": true }),
            }
        }
        TypeDefKind::Enum(e) => {
            let pick = [
                "unsupported",
                "permission-denied",
                "denied",
                "not-available",
            ]
            .iter()
            .find_map(|want| e.cases.iter().find(|c| c.name == *want))
            .or_else(|| e.cases.first());
            match pick {
                Some(case) => json!({ "throw": "enum", "case": case.name }),
                None => json!({ "trap": true }),
            }
        }
        _ => json!({ "trap": true }),
    }
}

/// A neutral answer for a function with no error channel, as JSON the
/// Player turns into the jco value (`bigint` marks a 64-bit zero).
fn neutral(resolve: &Resolve, ty: &Type) -> Option<Value> {
    Some(match ty {
        Type::Bool => json!({ "value": false }),
        Type::U64 | Type::S64 => json!({ "value": 0, "bigint": true }),
        Type::U8
        | Type::U16
        | Type::U32
        | Type::S8
        | Type::S16
        | Type::S32
        | Type::F32
        | Type::F64 => json!({ "value": 0 }),
        Type::String => json!({ "value": "" }),
        Type::Id(id) => match &resolve.types[dealias(resolve, *id)].kind {
            TypeDefKind::Option(_) => json!({ "value": null }),
            TypeDefKind::List(_) => json!({ "value": [] }),
            _ => return None,
        },
        Type::Char | Type::ErrorContext => return None,
    })
}

fn refusal_for(resolve: &Resolve, result: Option<&Type>) -> Value {
    let Some(result) = result else {
        return json!({ "value": null, "nothing": true });
    };
    if let Type::Id(id) = result {
        if let TypeDefKind::Result(r) = &resolve.types[dealias(resolve, *id)].kind {
            return match &r.err {
                Some(err) => error_refusal(resolve, err),
                None => json!({ "throw": "unit" }),
            };
        }
    }
    neutral(resolve, result).unwrap_or_else(|| json!({ "trap": true }))
}

/// For every interface the component imports, each freestanding function's
/// refusal shape, keyed `"ns:pkg/iface@ver" -> { "fn-name": shape }`.
pub fn refusals(component: &[u8]) -> Result<Value, String> {
    let decoded = wit_component::decode(component).map_err(|err| format!("{err:#}"))?;
    let (resolve, world) = match decoded {
        wit_component::DecodedWasm::Component(resolve, world) => (resolve, world),
        wit_component::DecodedWasm::WitPackage(..) => {
            return Err("a WIT package, not a component".to_string())
        }
    };
    let mut table = serde_json::Map::new();
    for item in resolve.worlds[world].imports.values() {
        let WorldItem::Interface { id, .. } = item else {
            continue;
        };
        let Some(name) = resolve.id_of(*id) else {
            continue;
        };
        let mut fns = serde_json::Map::new();
        for (fname, func) in &resolve.interfaces[*id].functions {
            if !matches!(func.kind, FunctionKind::Freestanding) {
                continue;
            }
            fns.insert(fname.clone(), refusal_for(&resolve, func.result.as_ref()));
        }
        table.insert(name, Value::Object(fns));
    }
    Ok(Value::Object(table))
}

/// Transpile one component for the Player.
pub fn transpile_component(component: &[u8]) -> Result<Output, String> {
    let opts = TranspileOpts {
        name: "app".to_string(),
        no_typescript: true,
        instantiation_mode: Some(InstantiationMode::Async),
        nodejs_compat_disabled: true,
        base64_cutoff: 0,
        ..TranspileOpts::default()
    };
    let transpiled = transpile(component, opts).map_err(|err| format!("{err:#}"))?;
    let modules = core_module_digests(component)?;
    let refusals = refusals(component)?;
    let files_meta: Vec<Value> = transpiled
        .files
        .iter()
        .map(|(name, bytes)| {
            let digest = hex(bytes);
            let wasm = name.ends_with(".wasm");
            json!({
                "name": name,
                "size": bytes.len(),
                "sha256": digest,
                // Whether the tab can prove this file is the judged code: a
                // core module byte-for-byte from the component.
                "embedded": wasm && modules.contains(&digest),
            })
        })
        .collect();
    let meta = json!({
        "schema": "krate.web.v1",
        "transpiler": TRANSPILER,
        "component_sha256": hex(component),
        "imports": transpiled.imports,
        "files": files_meta,
        "modules": modules,
        "refusals": refusals,
    });
    Ok(Output {
        files: transpiled.files,
        meta,
    })
}

/// The answer [`krate_transpile`] writes: a JSON header, then every file's
/// bytes in the header's order. `{"ok":true,...meta}` or
/// `{"ok":false,"problem":"..."}`.
pub fn transpile_to_bytes(component: &[u8]) -> Vec<u8> {
    let (header, files) = match transpile_component(component) {
        Ok(out) => {
            let mut meta = out.meta;
            meta["ok"] = Value::Bool(true);
            (meta, out.files)
        }
        Err(problem) => (json!({ "ok": false, "problem": problem }), Vec::new()),
    };
    let header = header.to_string().into_bytes();
    let mut out = Vec::with_capacity(4 + header.len());
    out.extend_from_slice(&(header.len() as u32).to_le_bytes());
    out.extend_from_slice(&header);
    for (_, bytes) in files {
        out.extend_from_slice(&bytes);
    }
    out
}

/// Reserve `len` bytes in the module's memory for the host to write into.
///
/// # Safety
/// The host owns the returned region until it passes it back to
/// [`krate_free`] with the same length.
#[no_mangle]
pub unsafe extern "C" fn krate_alloc(len: u32) -> *mut u8 {
    if len == 0 {
        return std::ptr::NonNull::<u8>::dangling().as_ptr();
    }
    let layout = Layout::from_size_align(len as usize, 1).expect("layout");
    raw_alloc(layout)
}

/// Release a region [`krate_alloc`] or [`krate_transpile`] handed out.
///
/// # Safety
/// `ptr` and `len` must be exactly what was handed out.
#[no_mangle]
pub unsafe extern "C" fn krate_free(ptr: *mut u8, len: u32) {
    if len == 0 {
        return;
    }
    let layout = Layout::from_size_align(len as usize, 1).expect("layout");
    dealloc(ptr, layout);
}

/// Transpile the component in the `len` bytes at `ptr`. Returns a pointer
/// to a little-endian u32 length followed by that many bytes of
/// [`transpile_to_bytes`]; free it with [`krate_free`] and `4 + length`.
///
/// # Safety
/// `ptr`/`len` must describe a region the host wrote through
/// [`krate_alloc`].
#[no_mangle]
pub unsafe extern "C" fn krate_transpile(ptr: *const u8, len: u32) -> *mut u8 {
    let component = std::slice::from_raw_parts(ptr, len as usize);
    let body = transpile_to_bytes(component);
    let total = 4 + body.len();
    let layout = Layout::from_size_align(total, 1).expect("layout");
    let out = raw_alloc(layout);
    std::ptr::copy_nonoverlapping((body.len() as u32).to_le_bytes().as_ptr(), out, 4);
    std::ptr::copy_nonoverlapping(body.as_ptr(), out.add(4), body.len());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A component built from the repo's own apps is not checked in; the
    /// bounce bundle under evidence/ is, so the tests take its component.
    fn bounce_component() -> Vec<u8> {
        let bundle = include_bytes!("../../../evidence/ported/bounce.krate");
        zip_entry(bundle, "code.wasm").expect("component entry")
    }

    /// The smallest zip reader the test needs: stored or deflated entries by
    /// name, from the central directory.
    fn zip_entry(zip: &[u8], want: &str) -> Option<Vec<u8>> {
        let eocd = (0..zip.len().saturating_sub(21))
            .rev()
            .find(|&i| zip[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])?;
        let count = u16::from_le_bytes([zip[eocd + 10], zip[eocd + 11]]) as usize;
        let mut at = u32::from_le_bytes(zip[eocd + 16..eocd + 20].try_into().ok()?) as usize;
        for _ in 0..count {
            let method = u16::from_le_bytes([zip[at + 10], zip[at + 11]]);
            let csize = u32::from_le_bytes(zip[at + 20..at + 24].try_into().ok()?) as usize;
            let nlen = u16::from_le_bytes([zip[at + 28], zip[at + 29]]) as usize;
            let elen = u16::from_le_bytes([zip[at + 30], zip[at + 31]]) as usize;
            let clen = u16::from_le_bytes([zip[at + 32], zip[at + 33]]) as usize;
            let local = u32::from_le_bytes(zip[at + 42..at + 46].try_into().ok()?) as usize;
            let name = std::str::from_utf8(&zip[at + 46..at + 46 + nlen]).ok()?;
            if name == want {
                let lnlen = u16::from_le_bytes([zip[local + 26], zip[local + 27]]) as usize;
                let lelen = u16::from_le_bytes([zip[local + 28], zip[local + 29]]) as usize;
                let data = &zip[local + 30 + lnlen + lelen..local + 30 + lnlen + lelen + csize];
                return match method {
                    0 => Some(data.to_vec()),
                    8 => miniz_inflate(data),
                    _ => None,
                };
            }
            at += 46 + nlen + elen + clen;
        }
        None
    }

    fn miniz_inflate(data: &[u8]) -> Option<Vec<u8>> {
        miniz_oxide::inflate::decompress_to_vec(data).ok()
    }

    /// A real app's component transpiles; every core file is one of the
    /// component's own modules, so the tab can prove it; the glue imports
    /// the interfaces the component imports.
    #[test]
    fn a_real_component_transpiles_and_every_core_file_is_embedded() {
        let component = bounce_component();
        let out = transpile_component(&component).expect("transpiles");
        let names: Vec<&str> = out.files.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"app.js"), "{names:?}");
        assert!(names.contains(&"app.core.wasm"), "{names:?}");
        for file in out.meta["files"].as_array().unwrap() {
            if file["name"].as_str().unwrap().ends_with(".wasm") {
                assert_eq!(file["embedded"], true, "{file}");
            }
        }
        assert!(out.meta["imports"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i.as_str().unwrap().starts_with("krate:")));
        assert_eq!(
            out.meta["modules"].as_array().unwrap().len(),
            core_module_digests(&component).unwrap().len()
        );
    }

    /// The refusal table speaks each function's own error shape.
    #[test]
    fn refusals_follow_each_functions_error_type() {
        let component = bounce_component();
        let table = refusals(&component).expect("decodes");
        let window = &table["krate:ui/window@0.1.0"]["create"];
        assert_eq!(window["throw"], "variant", "{table}");
        assert_eq!(window["case"], "unsupported");
        assert_eq!(window["words"], true);
    }

    /// The ABI answer is the header and the files, in order.
    #[test]
    fn the_answer_carries_the_files_after_the_header() {
        let component = bounce_component();
        let bytes = transpile_to_bytes(&component);
        let len = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
        let header: Value = serde_json::from_slice(&bytes[4..4 + len]).unwrap();
        assert_eq!(header["ok"], true);
        let total: u64 = header["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["size"].as_u64().unwrap())
            .sum();
        assert_eq!(bytes.len() - 4 - len, total as usize);
        let refused = transpile_to_bytes(b"not wasm");
        let len = u32::from_le_bytes(refused[..4].try_into().unwrap()) as usize;
        let header: Value = serde_json::from_slice(&refused[4..4 + len]).unwrap();
        assert_eq!(header["ok"], false);
    }
}

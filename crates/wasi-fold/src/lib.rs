//! Fold a linked app's leftover WASI imports into answers already inside it.
//!
//! A Krate app may import Krate's interfaces and nothing else. `krate-c-runtime`
//! answers WASI preview 1 inside the app: wasi-libc reaches every call through
//! a symbol named `__imported_wasi_snapshot_preview1_<name>`, and the runtime
//! defines each one, so the linker resolves the call before it can become an
//! import. That is enough for C and C++.
//!
//! Rust's standard library on `wasm32-wasip1` makes most of its calls through
//! wasi-libc too, but a few (`random_get` for `HashMap`'s keys, `args_get`,
//! `args_sizes_get`) come straight from the `wasi` crate inside std, under
//! mangled names nothing outside std can define. Those survive linking as
//! imports, and one WASI import is enough for Krate to refuse the app.
//!
//! This rewrites the linked core module: every remaining import from
//! `wasi_snapshot_preview1` becomes a call to the module's own exported
//! `__imported_wasi_snapshot_preview1_<name>` (export them with
//! `-C link-arg=--export=...`), the import is removed, and the helper export
//! with it. An import with no answer, or one whose signature differs, is an
//! error naming it -- never a silent leftover.

use std::collections::HashMap;

use wasm_encoder::reencode::{self, Reencode};
use wasmparser::{Parser, Payload, TypeRef};

/// The module WASI preview 1 imports come from.
pub const WASI_MODULE: &str = "wasi_snapshot_preview1";
/// The prefix of the names krate-c-runtime answers WASI calls under.
pub const ANSWER_PREFIX: &str = "__imported_wasi_snapshot_preview1_";

/// What a fold did.
#[derive(Debug, Default, PartialEq)]
pub struct Folded {
    /// The WASI functions folded, by name.
    pub folded: Vec<String>,
}

struct Remap {
    /// Old function index -> new function index.
    map: HashMap<u32, u32>,
    /// Old function indices of the imports being removed.
    removed: Vec<u32>,
    /// Export names to drop (the answers, once folded).
    drop_exports: Vec<String>,
}

impl Reencode for Remap {
    type Error = String;

    fn function_index(&mut self, func: u32) -> Result<u32, reencode::Error<String>> {
        Ok(*self.map.get(&func).unwrap_or(&func))
    }

    fn parse_import_section(
        &mut self,
        imports: &mut wasm_encoder::ImportSection,
        section: wasmparser::ImportSectionReader<'_>,
    ) -> Result<(), reencode::Error<String>> {
        let mut func = 0u32;
        for import in section.into_imports() {
            let import = import?;
            let is_func = matches!(import.ty, TypeRef::Func(_));
            let index = func;
            if is_func {
                func += 1;
            }
            if is_func && self.removed.contains(&index) {
                continue;
            }
            imports.import(import.module, import.name, self.entity_type(import.ty)?);
        }
        Ok(())
    }

    fn parse_export_section(
        &mut self,
        exports: &mut wasm_encoder::ExportSection,
        section: wasmparser::ExportSectionReader<'_>,
    ) -> Result<(), reencode::Error<String>> {
        for export in section {
            let export = export?;
            if self.drop_exports.iter().any(|n| n == export.name) {
                continue;
            }
            self.parse_export(exports, export)?;
        }
        Ok(())
    }

    fn parse_custom_section(
        &mut self,
        module: &mut wasm_encoder::Module,
        section: wasmparser::CustomSectionReader<'_>,
    ) -> Result<(), reencode::Error<String>> {
        // Debug names would name two functions alike after the fold; they
        // are debug information only, so they go.
        if section.name() == "name" {
            return Ok(());
        }
        reencode::utils::parse_custom_section(self, module, section)
    }
}

/// Fold `wasm`'s WASI imports into its own answers. Returns the new module.
pub fn fold(wasm: &[u8]) -> Result<(Vec<u8>, Folded), String> {
    // Pass 1: the function imports, the types, and the exports.
    let mut types: Vec<wasmparser::FuncType> = Vec::new();
    let mut func_imports: Vec<(String, String, u32)> = Vec::new(); // module, name, type
    let mut defined_types: Vec<u32> = Vec::new();
    let mut exports: HashMap<String, u32> = HashMap::new();
    for payload in Parser::new(0).parse_all(wasm) {
        match payload.map_err(|e| e.to_string())? {
            Payload::TypeSection(reader) => {
                for ty in reader.into_iter_err_on_gc_types() {
                    types.push(ty.map_err(|e| e.to_string())?);
                }
            }
            Payload::ImportSection(reader) => {
                for import in reader.into_imports() {
                    let import = import.map_err(|e| e.to_string())?;
                    if let TypeRef::Func(ty) = import.ty {
                        func_imports.push((import.module.to_string(), import.name.to_string(), ty));
                    }
                }
            }
            Payload::FunctionSection(reader) => {
                for ty in reader {
                    defined_types.push(ty.map_err(|e| e.to_string())?);
                }
            }
            Payload::ExportSection(reader) => {
                for export in reader {
                    let export = export.map_err(|e| e.to_string())?;
                    if export.kind == wasmparser::ExternalKind::Func {
                        exports.insert(export.name.to_string(), export.index);
                    }
                }
            }
            _ => {}
        }
    }
    let imported = func_imports.len() as u32;
    let type_of = |index: u32| -> Option<u32> {
        if index < imported {
            Some(func_imports[index as usize].2)
        } else {
            defined_types.get((index - imported) as usize).copied()
        }
    };

    // Which imports go, and where each one's calls go instead.
    let mut targets: Vec<(u32, u32)> = Vec::new(); // (import index, answer index)
    let mut folded = Folded::default();
    let mut drop_exports = Vec::new();
    let mut missing = Vec::new();
    for (index, (module, name, ty)) in func_imports.iter().enumerate() {
        if module != WASI_MODULE {
            continue;
        }
        let answer = format!("{ANSWER_PREFIX}{name}");
        let Some(&target) = exports.get(&answer) else {
            missing.push(name.clone());
            continue;
        };
        if target < imported {
            missing.push(format!("{name} (its answer is itself an import)"));
            continue;
        }
        let want = &types[*ty as usize];
        let have = type_of(target).map(|t| &types[t as usize]);
        if have != Some(want) {
            return Err(format!(
                "{answer} does not have the signature of WASI's {name}: {have:?} against {want:?}"
            ));
        }
        targets.push((index as u32, target));
        folded.folded.push(name.clone());
        drop_exports.push(answer);
    }
    if !missing.is_empty() {
        return Err(format!(
            "no answer inside the app for WASI's {}; export {ANSWER_PREFIX}<name> (krate-c-runtime defines them) with -C link-arg=--export=<symbol>",
            missing.join(", ")
        ));
    }

    // Every function index, shifted down past the removed imports.
    let removed: Vec<u32> = targets.iter().map(|(i, _)| *i).collect();
    let total = imported + defined_types.len() as u32;
    let mut map = HashMap::new();
    let mut shift = 0u32;
    for old in 0..total {
        if removed.contains(&old) {
            shift += 1;
            continue;
        }
        map.insert(old, old - shift);
    }
    for (import, target) in &targets {
        let new = map[target];
        map.insert(*import, new);
    }
    let mut remap = Remap {
        map,
        removed,
        drop_exports,
    };
    let mut module = wasm_encoder::Module::new();
    remap
        .parse_core_module(&mut module, Parser::new(0), wasm)
        .map_err(|e| format!("{e:?}"))?;
    let out = module.finish();
    wasmparser::Validator::new_with_features(wasmparser::WasmFeatures::all())
        .validate_all(&out)
        .map_err(|e| format!("the folded module does not validate: {e}"))?;
    Ok((out, folded))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn imports(wasm: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        for payload in Parser::new(0).parse_all(wasm) {
            if let Ok(Payload::ImportSection(reader)) = payload {
                for import in reader.into_imports() {
                    let import = import.unwrap();
                    out.push(format!("{}::{}", import.module, import.name));
                }
            }
        }
        out
    }

    const APP: &str = r#"
        (module
          (import "krate:time/clock@0.1.0" "now-millis" (func $now (result i64)))
          (import "wasi_snapshot_preview1" "random_get" (func $rg (param i32 i32) (result i32)))
          (func $answer (export "__imported_wasi_snapshot_preview1_random_get") (param i32 i32) (result i32)
            i32.const 7)
          (func (export "run") (result i32)
            (drop (call $now))
            (call $rg (i32.const 0) (i32.const 4))))
    "#;

    #[test]
    fn a_leftover_wasi_import_becomes_a_call_to_the_apps_own_answer() {
        let wasm = wat::parse_str(APP).unwrap();
        // The fixture really carries the hazard.
        assert!(imports(&wasm).contains(&"wasi_snapshot_preview1::random_get".to_string()));
        let (out, folded) = fold(&wasm).unwrap();
        assert_eq!(folded.folded, vec!["random_get".to_string()]);
        assert_eq!(
            imports(&out),
            vec!["krate:time/clock@0.1.0::now-millis".to_string()]
        );
        // run still calls the answer: 7, not a trap or a wrong function.
        let text = wasmprinter::print_bytes(&out).unwrap();
        assert!(text.contains("call 1"), "{text}");
        assert!(
            !text.contains("__imported_wasi"),
            "the answer is no longer exported: {text}"
        );
    }

    #[test]
    fn an_import_with_no_answer_is_refused_by_name() {
        let wasm = wat::parse_str(
            r#"(module (import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32))))"#,
        )
        .unwrap();
        let err = fold(&wasm).unwrap_err();
        assert!(err.contains("fd_write"), "{err}");
    }
}

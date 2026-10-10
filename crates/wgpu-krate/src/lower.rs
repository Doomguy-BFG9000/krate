//! Rewrite what WGSL cannot say into what it can, before a naga module goes
//! to the host as WGSL.
//!
//! krate:gpu takes WGSL only (the shader door, F-311), so a shader written in
//! GLSL or SPIR-V crosses as naga IR written out as WGSL. Most of naga's IR
//! has a WGSL spelling. `isinf` and `isnan` do not: WGSL has neither, and
//! naga's WGSL writer refuses them ("Unsupported relational function"). 22 of
//! Veloren's 41 GLSL shaders use one of them.
//!
//! Both are tests on the bits of an IEEE float, which WGSL can say:
//!
//! - `isinf(x)`: `(bitcast<u32>(x) & 0x7fffffff) == 0x7f800000`
//! - `isnan(x)`: `(bitcast<u32>(x) & 0x7fffffff) >  0x7f800000`
//!
//! Exact, and unlike `x != x` immune to a fast-math compiler folding it away.
//!
//! naga wants every expression to refer only to earlier ones, and every
//! function to call only earlier ones, so the tests cannot be spliced into the
//! expression that used `isinf`. Each becomes a small function placed ahead
//! of every other (`krate_isinf_f32`, `krate_isnan_vec3f`, ...), and the
//! `isinf` expression becomes the result of a call to it. Only 32-bit floats
//! are rewritten; anything else is left for the writer to refuse with its own
//! message.

use std::collections::HashMap;

use naga::{
    BinaryOperator, Block, Expression, Function, FunctionArgument, FunctionResult, Handle, Literal,
    Module, RelationalFunction, Scalar, ScalarKind, Span, Statement, Type, TypeInner, VectorSize,
};

/// One rewrite: the expression, whether it is isnan, its argument, and the
/// argument's vector size (None for a scalar).
type Site = (
    Handle<Expression>,
    bool,
    Handle<Expression>,
    Option<VectorSize>,
);

/// Rewrite every `isinf`/`isnan` on 32-bit floats in `module`. Returns how
/// many it rewrote.
pub fn lower_for_wgsl(module: &mut Module) -> Result<usize, String> {
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(module)
    .map_err(|e| format!("{e:?}"))?;
    let types = &module.types;
    let function_sites: Vec<Vec<Site>> = module
        .functions
        .iter()
        .map(|(handle, f)| sites(f, |h| info[handle][h].ty.inner_with(types).clone()))
        .collect();
    let entry_sites: Vec<Vec<Site>> = module
        .entry_points
        .iter()
        .enumerate()
        .map(|(i, e)| {
            sites(&e.function, |h| {
                info.get_entry_point(i)[h].ty.inner_with(types).clone()
            })
        })
        .collect();
    let total: usize = function_sites
        .iter()
        .chain(&entry_sites)
        .map(Vec::len)
        .sum();
    if total == 0 {
        return Ok(0);
    }

    // The helpers go first, so every function may call them.
    let mut wanted: Vec<(bool, Option<VectorSize>)> = function_sites
        .iter()
        .chain(&entry_sites)
        .flatten()
        .map(|&(_, nan, _, size)| (nan, size))
        .collect();
    wanted.sort_by_key(|&(nan, size)| (nan, size.map(|s| s as u8)));
    wanted.dedup();
    let old = std::mem::take(&mut module.functions);
    let mut helpers = HashMap::new();
    for &(nan, size) in &wanted {
        let f = helper(&mut module.types, nan, size);
        helpers.insert((nan, size), module.functions.append(f, Span::UNDEFINED));
    }
    let shift = wanted.len();
    // Old functions keep their order, after the helpers; calls follow them.
    let new_of_old: Vec<Handle<Function>> = old
        .iter()
        .map(|(h, f)| module.functions.append(f.clone(), old.get_span(h)))
        .collect();
    let moved = |h: Handle<Function>| new_of_old[h.index()];
    for (i, (_, f)) in module.functions.iter_mut().enumerate() {
        if i >= shift {
            remap_calls(f, &moved);
        }
    }
    for entry in &mut module.entry_points {
        remap_calls(&mut entry.function, &moved);
    }

    let handles: Vec<Handle<Function>> = module.functions.iter().map(|(h, _)| h).collect();
    for (i, found) in function_sites.iter().enumerate() {
        let f = &mut module.functions[handles[i + shift]];
        for &site in found {
            call_helper(f, site, helpers[&(site.1, site.3)]);
        }
    }
    for (i, found) in entry_sites.iter().enumerate() {
        let f = &mut module.entry_points[i].function;
        for &site in found {
            call_helper(f, site, helpers[&(site.1, site.3)]);
        }
    }
    Ok(total)
}

fn sites(function: &Function, type_of: impl Fn(Handle<Expression>) -> TypeInner) -> Vec<Site> {
    let f32_ = Scalar {
        kind: ScalarKind::Float,
        width: 4,
    };
    let mut found = Vec::new();
    for (h, expr) in function.expressions.iter() {
        let Expression::Relational { fun, argument } = *expr else {
            continue;
        };
        let nan = match fun {
            RelationalFunction::IsNan => true,
            RelationalFunction::IsInf => false,
            _ => continue,
        };
        match type_of(argument) {
            TypeInner::Scalar(s) if s == f32_ => found.push((h, nan, argument, None)),
            TypeInner::Vector { size, scalar } if scalar == f32_ => {
                found.push((h, nan, argument, Some(size)))
            }
            _ => {}
        }
    }
    found
}

/// `fn krate_isinf_vec3f(x: vec3<f32>) -> vec3<bool>` and its kin.
fn helper(types: &mut naga::UniqueArena<Type>, nan: bool, size: Option<VectorSize>) -> Function {
    let ty = |types: &mut naga::UniqueArena<Type>, inner| {
        types.insert(Type { name: None, inner }, Span::UNDEFINED)
    };
    let (arg_inner, result_inner) = match size {
        None => (
            TypeInner::Scalar(Scalar::F32),
            TypeInner::Scalar(Scalar::BOOL),
        ),
        Some(size) => (
            TypeInner::Vector {
                size,
                scalar: Scalar::F32,
            },
            TypeInner::Vector {
                size,
                scalar: Scalar::BOOL,
            },
        ),
    };
    let arg_ty = ty(types, arg_inner);
    let result_ty = ty(types, result_inner);
    let mut f = Function {
        name: Some(format!(
            "krate_{}_{}",
            if nan { "isnan" } else { "isinf" },
            match size {
                None => "f32",
                Some(VectorSize::Bi) => "vec2f",
                Some(VectorSize::Tri) => "vec3f",
                Some(VectorSize::Quad) => "vec4f",
            }
        )),
        arguments: vec![FunctionArgument {
            name: Some("x".to_string()),
            ty: arg_ty,
            binding: None,
        }],
        result: Some(FunctionResult {
            ty: result_ty,
            binding: None,
        }),
        ..Function::default()
    };
    let e = &mut f.expressions;
    let x = e.append(Expression::FunctionArgument(0), Span::UNDEFINED);
    let mask = e.append(
        Expression::Literal(Literal::U32(0x7fff_ffff)),
        Span::UNDEFINED,
    );
    let inf = e.append(
        Expression::Literal(Literal::U32(0x7f80_0000)),
        Span::UNDEFINED,
    );
    let bits = e.append(
        Expression::As {
            expr: x,
            kind: ScalarKind::Uint,
            convert: None,
        },
        Span::UNDEFINED,
    );
    let (mask, inf) = match size {
        Some(size) => (
            e.append(Expression::Splat { size, value: mask }, Span::UNDEFINED),
            e.append(Expression::Splat { size, value: inf }, Span::UNDEFINED),
        ),
        None => (mask, inf),
    };
    let masked = e.append(
        Expression::Binary {
            op: BinaryOperator::And,
            left: bits,
            right: mask,
        },
        Span::UNDEFINED,
    );
    let test = e.append(
        Expression::Binary {
            op: if nan {
                BinaryOperator::Greater
            } else {
                BinaryOperator::Equal
            },
            left: masked,
            right: inf,
        },
        Span::UNDEFINED,
    );
    f.body.push(
        Statement::Emit(naga::Range::new_from_bounds(bits, test)),
        Span::UNDEFINED,
    );
    f.body
        .push(Statement::Return { value: Some(test) }, Span::UNDEFINED);
    f
}

/// Point every call in `f` at the function's new handle.
fn remap_calls(f: &mut Function, moved: &impl Fn(Handle<Function>) -> Handle<Function>) {
    for (_, expr) in f.expressions.iter_mut() {
        if let Expression::CallResult(target) = expr {
            *target = moved(*target);
        }
    }
    remap_block(&mut f.body, moved);
}

fn remap_block(block: &mut Block, moved: &impl Fn(Handle<Function>) -> Handle<Function>) {
    for (statement, _) in block.span_iter_mut() {
        if let Statement::Call { function, .. } = statement {
            *function = moved(*function);
        }
        for_each_block(statement, |b| {
            remap_block(b, moved);
            false
        });
    }
}

/// Make `h` the result of a call to `helper` on its argument.
fn call_helper(f: &mut Function, (h, _, argument, _): Site, helper: Handle<Function>) {
    f.expressions[h] = Expression::CallResult(helper);
    split_emit(&mut f.body, h, helper, argument);
}

/// Take `h` out of the `Emit` that covers it and put the call that now makes
/// it in its place.
fn split_emit(
    block: &mut Block,
    h: Handle<Expression>,
    helper: Handle<Function>,
    argument: Handle<Expression>,
) -> bool {
    let mut done = false;
    let old = std::mem::take(block);
    for (statement, span) in old.span_into_iter() {
        if done {
            block.push(statement, span);
            continue;
        }
        match statement {
            Statement::Emit(range) if range.clone().any(|e| e == h) => {
                let handles: Vec<Handle<Expression>> = range.clone().collect();
                let at = handles.iter().position(|&e| e == h).unwrap_or(0);
                if at > 0 {
                    block.push(
                        Statement::Emit(naga::Range::new_from_bounds(handles[0], handles[at - 1])),
                        span,
                    );
                }
                block.push(
                    Statement::Call {
                        function: helper,
                        arguments: vec![argument],
                        result: Some(h),
                    },
                    span,
                );
                if at + 1 < handles.len() {
                    block.push(
                        Statement::Emit(naga::Range::new_from_bounds(
                            handles[at + 1],
                            handles[handles.len() - 1],
                        )),
                        span,
                    );
                }
                done = true;
            }
            mut other => {
                done = for_each_block(&mut other, |b| split_emit(b, h, helper, argument));
                block.push(other, span);
            }
        }
    }
    done
}

/// Run `f` on each block nested in `statement` until it returns true.
fn for_each_block(statement: &mut Statement, mut f: impl FnMut(&mut Block) -> bool) -> bool {
    match statement {
        Statement::Block(b) => f(b),
        Statement::If { accept, reject, .. } => f(accept) || f(reject),
        Statement::Switch { cases, .. } => cases.iter_mut().any(|c| f(&mut c.body)),
        Statement::Loop {
            body, continuing, ..
        } => f(body) || f(continuing),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GLSL: &str = r#"#version 450
layout(location = 0) in vec3 v;
layout(location = 0) out vec4 color;
float pick(float x) { return isinf(x) ? 1.0 : 0.0; }
float twice(float x) { return pick(x) * 2.0; }
void main() {
    bvec3 n = isnan(v);
    color = vec4(twice(v.x), n.x ? 1.0 : 0.0, isinf(v.y) ? 1.0 : 0.0, 1.0);
}
"#;

    fn wgsl_of(module: &Module) -> String {
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(module)
        .expect("the lowered module validates");
        naga::back::wgsl::write_string(module, &info, naga::back::wgsl::WriterFlags::empty())
            .expect("the lowered module writes as WGSL")
    }

    #[test]
    fn isinf_and_isnan_become_bit_tests_the_host_accepts() {
        let mut module = naga::front::glsl::Frontend::default()
            .parse(
                &naga::front::glsl::Options::from(naga::ShaderStage::Fragment),
                GLSL,
            )
            .expect("GLSL parses");
        // The hazard is real: unlowered, the writer refuses the module.
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
        assert!(naga::back::wgsl::write_string(
            &module,
            &info,
            naga::back::wgsl::WriterFlags::empty()
        )
        .is_err());

        assert_eq!(lower_for_wgsl(&mut module).unwrap(), 3);
        let wgsl = wgsl_of(&module);
        assert!(wgsl.contains("bitcast<u32>"), "{wgsl}");
        assert!(wgsl.contains("krate_isnan_vec3f"), "{wgsl}");
        // What the host does: parse the WGSL and validate it.
        let back = naga::front::wgsl::parse_str(&wgsl).expect("the host parses it");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&back)
        .expect("the host validates it");
    }

    #[test]
    fn a_module_without_them_is_left_alone() {
        let mut module = naga::front::wgsl::parse_str(
            "@fragment fn main() -> @location(0) vec4<f32> { return vec4<f32>(1.0); }",
        )
        .unwrap();
        let before = format!("{module:?}");
        assert_eq!(lower_for_wgsl(&mut module).unwrap(), 0);
        assert_eq!(format!("{module:?}"), before);
    }
}

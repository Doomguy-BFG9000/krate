// The smallest app that draws through krate:gpu (IC-540, IC-916): a lit,
// spinning cube from a WGSL shader, so the interface is proved through the
// binary a person runs.
//
//   krate run gpu-probe.krate                 the cube, until the window closes
//   krate run gpu-probe.krate -- seconds=5    the cube for five seconds
//   krate run --headless gpu-probe.krate -- check
//                                             one line per check, then exit:
//                                             the adapter, a shader refused by
//                                             validation with its line, a
//                                             triangle drawn and read back, a
//                                             compute pass, a bad handle, and
//                                             a lost device
//
// Every frame line in the window mode says how long the frame took.
#![no_std]
extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use krate::bindings::krate::gpu::types as t;
use krate::gpu::gpu as g;
use krate::prelude::*;
use krate::{
    io::{args, stdio, streams::OutputStream},
    time::clock,
    ui::{events, tree, types, window},
    Guest,
};

const CUBE_SHADER: &str = r#"
struct Frame {
    mvp: mat4x4<f32>,
    model: mat4x4<f32>,
    light: vec4<f32>,
};
@group(0) @binding(0) var<uniform> frame: Frame;

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec3<f32>,
};

@vertex
fn vs(@location(0) position: vec3<f32>, @location(1) normal: vec3<f32>) -> Out {
    var out: Out;
    out.position = frame.mvp * vec4<f32>(position, 1.0);
    out.normal = (frame.model * vec4<f32>(normal, 0.0)).xyz;
    // Each face its own colour, from the direction it faces.
    out.color = abs(normal) * 0.6 + vec3<f32>(0.25, 0.25, 0.3);
    return out;
}

@fragment
fn fs(in: Out) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);
    let l = normalize(frame.light.xyz);
    let diffuse = max(dot(n, l), 0.0);
    let lit = in.color * (0.18 + 0.82 * diffuse);
    return vec4<f32>(lit, 1.0);
}
"#;

/// Line 4 asks for a `vec3` from a function that returns `f32`.
const BROKEN_SHADER: &str = "@fragment\nfn fs() -> @location(0) vec4<f32> {\n    let x: f32 = 1.0;\n    let y: vec3<f32> = x;\n    return vec4<f32>(y, 1.0);\n}\n";

const RED_SHADER: &str = r#"
@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    return vec4<f32>(p[i], 0.0, 1.0);
}
@fragment
fn fs() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0, 0.0, 0.0, 1.0);
}
"#;

const DOUBLE_SHADER: &str = r#"
@group(0) @binding(0) var<storage, read_write> data: array<u32>;
@compute @workgroup_size(4)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    data[id.x] = data[id.x] * 2u;
}
"#;

struct Component;

fn write_line(out: &OutputStream, line: &str) {
    let _ = out.write(format!("{line}\n").as_bytes());
}

fn stage(module: u64, entry: &str) -> t::ProgrammableStage {
    t::ProgrammableStage {
        module,
        entry_point: Some(String::from(entry)),
        constants: Vec::new(),
    }
}

fn primitive(cull: t::CullMode) -> t::PrimitiveState {
    t::PrimitiveState {
        topology: t::PrimitiveTopology::TriangleList,
        strip_index_format: None,
        front_face: t::FrontFace::Ccw,
        cull_mode: cull,
        unclipped_depth: false,
        polygon_mode: t::PolygonMode::Fill,
        conservative: false,
    }
}

fn target(format: &str) -> Option<t::ColorTargetState> {
    Some(t::ColorTargetState {
        format: String::from(format),
        blend: None,
        write_mask: t::ColorWrites::all(),
    })
}

fn single_sample() -> t::MultisampleState {
    t::MultisampleState {
        count: 1,
        mask: u64::MAX,
        alpha_to_coverage_enabled: false,
    }
}

fn texture(device: u64, w: u32, h: u32, format: &str, usage: t::TextureUsages) -> Result<u64, t::GpuError> {
    g::create_texture(
        device,
        &t::TextureDescriptor {
            label: None,
            size: t::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: t::TextureDimension::D2,
            format: String::from(format),
            usage,
            view_formats: Vec::new(),
        },
    )
}

fn whole_view(texture: u64) -> Result<u64, t::GpuError> {
    g::create_texture_view(
        texture,
        &t::TextureViewDescriptor {
            label: None,
            format: None,
            dimension: None,
            usage: None,
            aspect: t::TextureAspect::All,
            base_mip_level: 0,
            mip_level_count: None,
            base_array_layer: 0,
            array_layer_count: None,
        },
    )
}

fn color_pass(view: u64, clear: t::Color, depth: Option<u64>) -> t::RenderPassDescriptor {
    t::RenderPassDescriptor {
        label: None,
        color_attachments: vec![Some(t::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            load: t::ColorLoadOp::Clear(clear),
            store: t::StoreOp::Store,
        })],
        depth_stencil_attachment: depth.map(|view| t::RenderPassDepthStencilAttachment {
            view,
            depth_ops: Some(t::DepthOps {
                load: t::DepthLoadOp::Clear(1.0),
                store: t::StoreOp::Discard,
            }),
            stencil_ops: None,
        }),
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    }
}

fn buffer(device: u64, size: u64, usage: t::BufferUsages) -> Result<u64, t::GpuError> {
    g::create_buffer(
        device,
        &t::BufferDescriptor {
            label: None,
            size,
            usage,
            mapped_at_creation: false,
        },
    )
}

fn open_device() -> Result<(u64, u64), String> {
    let adapter = g::request_adapter(t::AdapterOptions {
        power_preference: t::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    })
    .map_err(|e| format!("adapter={e:?}"))?;
    let device = g::request_device(
        adapter,
        &t::DeviceDescriptor {
            label: Some(String::from("probe")),
            required_features: Vec::new(),
            required_limits: Vec::new(),
        },
    )
    .map_err(|e| format!("device={e:?}"))?;
    Ok((adapter, device))
}

/// Read `size` bytes of a buffer back through a mappable copy.
fn read_back(device: u64, source: u64, size: u64) -> Result<Vec<u8>, t::GpuError> {
    let staging = buffer(device, size, t::BufferUsages::MAP_READ | t::BufferUsages::COPY_DST)?;
    let encoder = g::create_command_encoder(device, None)?;
    g::copy_buffer_to_buffer(encoder, source, 0, staging, 0, Some(size))?;
    let commands = g::finish_encoder(encoder, None)?;
    g::submit(device, &[commands])?;
    g::map_buffer(staging, t::MapMode::Read, 0, size)?;
    let bytes = g::read_mapped(staging, 0, size)?;
    g::unmap_buffer(staging)?;
    g::release(staging);
    Ok(bytes)
}

fn checks(out: &OutputStream) -> i32 {
    let (adapter, device) = match open_device() {
        Ok(pair) => pair,
        Err(why) => {
            write_line(out, &why);
            return 1;
        }
    };
    if let Ok(info) = g::describe_adapter(adapter) {
        write_line(out, &format!("adapter={} backend={}", info.name, info.backend));
    }

    // The shader door: a shader that fails validation never reaches the
    // driver, and the app learns why and where.
    match g::create_shader_module(
        device,
        &t::ShaderModuleDescriptor {
            label: None,
            code: String::from(BROKEN_SHADER),
        },
    ) {
        Ok(_) => write_line(out, "shader=accepted"),
        Err(e) => write_line(
            out,
            &format!("shader=refused line={} column={} message={}", e.line, e.column, e.message.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("")),
        ),
    }

    // A triangle over a 4x4 target, read back.
    let pixel = (|| -> Result<String, t::GpuError> {
        let module = g::create_shader_module(
            device,
            &t::ShaderModuleDescriptor {
                label: None,
                code: String::from(RED_SHADER),
            },
        )
        .map_err(|e| t::GpuError::Validation(e.message))?;
        let pipeline = g::create_render_pipeline(
            device,
            &t::RenderPipelineDescriptor {
                label: None,
                layout: None,
                vertex: t::VertexState {
                    stage: stage(module, "vs"),
                    buffers: Vec::new(),
                },
                primitive: primitive(t::CullMode::None),
                depth_stencil: None,
                multisample: single_sample(),
                fragment: Some(t::FragmentState {
                    stage: stage(module, "fs"),
                    targets: vec![target("rgba8unorm")],
                }),
                multiview_mask: None,
            },
        )?;
        let tex = texture(
            device,
            4,
            4,
            "rgba8unorm",
            t::TextureUsages::RENDER_ATTACHMENT | t::TextureUsages::COPY_SRC,
        )?;
        let view = whole_view(tex)?;
        let rows = buffer(device, 256 * 4, t::BufferUsages::COPY_SRC | t::BufferUsages::COPY_DST)?;
        let encoder = g::create_command_encoder(device, None)?;
        g::encode_render_pass(
            encoder,
            &color_pass(view, t::Color { r: 0.0, g: 0.0, b: 1.0, a: 1.0 }, None),
            &[
                t::RenderCommand::SetPipeline(pipeline),
                t::RenderCommand::Draw(t::Draw {
                    vertex_count: 3,
                    instance_count: 1,
                    first_vertex: 0,
                    first_instance: 0,
                }),
            ],
        )?;
        g::copy_texture_to_buffer(
            encoder,
            t::TexelCopyTextureInfo {
                texture: tex,
                mip_level: 0,
                origin: t::Origin3d { x: 0, y: 0, z: 0 },
                aspect: t::TextureAspect::All,
            },
            t::TexelCopyBufferInfo {
                buffer: rows,
                layout: t::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(4),
                },
            },
            t::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
        )?;
        let commands = g::finish_encoder(encoder, None)?;
        g::submit(device, &[commands])?;
        let bytes = read_back(device, rows, 256 * 4)?;
        Ok(format!("{},{},{},{}", bytes[0], bytes[1], bytes[2], bytes[3]))
    })();
    match pixel {
        Ok(p) => write_line(out, &format!("pixel={p}")),
        Err(e) => write_line(out, &format!("pixel=error {e:?}")),
    }

    // A compute pass doubling four numbers in place.
    let doubled = (|| -> Result<String, t::GpuError> {
        let module = g::create_shader_module(
            device,
            &t::ShaderModuleDescriptor {
                label: None,
                code: String::from(DOUBLE_SHADER),
            },
        )
        .map_err(|e| t::GpuError::Validation(e.message))?;
        let pipeline = g::create_compute_pipeline(
            device,
            &t::ComputePipelineDescriptor {
                label: None,
                layout: None,
                stage: stage(module, "main"),
            },
        )?;
        let data = buffer(
            device,
            16,
            t::BufferUsages::STORAGE | t::BufferUsages::COPY_SRC | t::BufferUsages::COPY_DST,
        )?;
        let mut input = Vec::new();
        for v in [1u32, 2, 3, 4] {
            input.extend_from_slice(&v.to_le_bytes());
        }
        g::write_buffer(device, data, 0, &input)?;
        let layout = g::pipeline_bind_group_layout(pipeline, 0)?;
        let group = g::create_bind_group(
            device,
            &t::BindGroupDescriptor {
                label: None,
                layout,
                entries: vec![t::BindGroupEntry {
                    binding: 0,
                    resource: t::BindingResource::Buffer(t::BufferBinding {
                        buffer: data,
                        offset: 0,
                        size: None,
                    }),
                }],
            },
        )?;
        let encoder = g::create_command_encoder(device, None)?;
        g::encode_compute_pass(
            encoder,
            &t::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            },
            &[
                t::ComputeCommand::SetPipeline(pipeline),
                t::ComputeCommand::SetBindGroup(t::SetBindGroup {
                    index: 0,
                    group: Some(group),
                    offsets: Vec::new(),
                }),
                t::ComputeCommand::Dispatch(t::Origin3d { x: 1, y: 1, z: 1 }),
            ],
        )?;
        let commands = g::finish_encoder(encoder, None)?;
        g::submit(device, &[commands])?;
        let bytes = read_back(device, data, 16)?;
        let words: Vec<String> = bytes
            .chunks_exact(4)
            .map(|c| format!("{}", u32::from_le_bytes([c[0], c[1], c[2], c[3]])))
            .collect();
        Ok(words.join(","))
    })();
    match doubled {
        Ok(w) => write_line(out, &format!("compute={w}")),
        Err(e) => write_line(out, &format!("compute=error {e:?}")),
    }

    // A handle that was never given out, and a bind group that does not fit
    // its layout: both refused with a reason, the app still running.
    let bogus = match whole_view(0xdead) {
        Err(t::GpuError::InvalidHandle(_)) => "refused",
        Err(_) => "wrong-error",
        Ok(_) => "accepted",
    };
    let oversized = match buffer(device, 1 << 50, t::BufferUsages::STORAGE) {
        Err(t::GpuError::Validation(_)) | Err(t::GpuError::OutOfMemory) => "refused",
        Err(_) => "wrong-error",
        Ok(_) => "accepted",
    };
    write_line(out, &format!("invalid-handle={bogus} oversized-buffer={oversized}"));

    // Losing the device on purpose: every later call says so.
    let _ = g::destroy_device(device);
    let lost = match buffer(device, 16, t::BufferUsages::UNIFORM) {
        Err(t::GpuError::Lost(_)) => "yes",
        _ => "no",
    };
    let recovered = match open_device() {
        Ok((_, fresh)) => buffer(fresh, 16, t::BufferUsages::UNIFORM).is_ok(),
        Err(_) => false,
    };
    write_line(out, &format!("lost={lost} recovered={}", if recovered { "yes" } else { "no" }));
    0
}

// ---- the cube --------------------------------------------------------------

type Mat = [f32; 16];

fn mul(a: &Mat, b: &Mat) -> Mat {
    let mut m = [0.0; 16];
    for c in 0..4 {
        for r in 0..4 {
            m[c * 4 + r] = (0..4).map(|k| a[k * 4 + r] * b[c * 4 + k]).sum();
        }
    }
    m
}

fn perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> Mat {
    let f = 1.0 / (fov_y * 0.5).tan();
    let mut m = [0.0; 16];
    m[0] = f / aspect;
    m[5] = f;
    m[10] = far / (near - far);
    m[11] = -1.0;
    m[14] = near * far / (near - far);
    m
}

fn rotation(yaw: f32, pitch: f32) -> Mat {
    let (sy, cy) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    let ry: Mat = [cy, 0.0, -sy, 0.0, 0.0, 1.0, 0.0, 0.0, sy, 0.0, cy, 0.0, 0.0, 0.0, 0.0, 1.0];
    let rx: Mat = [1.0, 0.0, 0.0, 0.0, 0.0, cp, sp, 0.0, 0.0, -sp, cp, 0.0, 0.0, 0.0, 0.0, 1.0];
    mul(&ry, &rx)
}

fn translation(z: f32) -> Mat {
    let mut m = [0.0; 16];
    m[0] = 1.0;
    m[5] = 1.0;
    m[10] = 1.0;
    m[15] = 1.0;
    m[14] = z;
    m
}

/// Six faces, four corners each: position then normal.
fn cube() -> (Vec<f32>, Vec<u16>) {
    let faces: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
        ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
        ([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
        ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
        ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
        ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        ([0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
    ];
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for (n, u, v) in faces {
        let base = (vertices.len() / 6) as u16;
        for (su, sv) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            for k in 0..3 {
                vertices.push(n[k] + su * u[k] + sv * v[k]);
            }
            vertices.extend_from_slice(&n);
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    (vertices, indices)
}

fn bytes_of(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

struct Cube {
    device: u64,
    surface: u64,
    format: String,
    pipeline: u64,
    uniforms: u64,
    group: u64,
    vertices: u64,
    indices: u64,
    index_count: u32,
    depth: Option<(u64, u64, (u32, u32))>,
    size: (u32, u32),
}

impl Cube {
    fn new(win: u64) -> Result<Cube, String> {
        let surface = g::create_surface(win, 2).map_err(|e| format!("surface={e:?}"))?;
        let (adapter, device) = open_device()?;
        let caps = g::get_surface_capabilities(surface, adapter).map_err(|e| format!("{e:?}"))?;
        let format = caps
            .formats
            .iter()
            .find(|f| f.ends_with("-srgb"))
            .cloned()
            .unwrap_or_else(|| caps.formats[0].clone());
        let module = g::create_shader_module(
            device,
            &t::ShaderModuleDescriptor {
                label: Some(String::from("cube")),
                code: String::from(CUBE_SHADER),
            },
        )
        .map_err(|e| format!("shader line {}: {}", e.line, e.message))?;
        let fail = |e: t::GpuError| format!("{e:?}");
        let layout = g::create_bind_group_layout(
            device,
            &t::BindGroupLayoutDescriptor {
                label: None,
                entries: vec![t::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: t::ShaderStages::VERTEX | t::ShaderStages::FRAGMENT,
                    kind: t::BindingType::Buffer(t::BufferBindingLayout {
                        kind: t::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: 0,
                    }),
                    count: None,
                }],
            },
        )
        .map_err(fail)?;
        let pipeline_layout = g::create_pipeline_layout(
            device,
            &t::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: vec![Some(layout)],
                immediate_size: 0,
            },
        )
        .map_err(fail)?;
        let attribute = |format: &str, offset, location| t::VertexAttribute {
            format: String::from(format),
            offset,
            shader_location: location,
        };
        let pipeline = g::create_render_pipeline(
            device,
            &t::RenderPipelineDescriptor {
                label: Some(String::from("cube")),
                layout: Some(pipeline_layout),
                vertex: t::VertexState {
                    stage: stage(module, "vs"),
                    buffers: vec![t::VertexBufferLayout {
                        array_stride: 24,
                        step_mode: t::VertexStepMode::Vertex,
                        attributes: vec![attribute("float32x3", 0, 0), attribute("float32x3", 12, 1)],
                    }],
                },
                primitive: primitive(t::CullMode::Back),
                depth_stencil: Some(t::DepthStencilState {
                    format: String::from("depth32float"),
                    depth_write_enabled: true,
                    depth_compare: t::CompareFunction::Less,
                    stencil_front: keep_stencil(),
                    stencil_back: keep_stencil(),
                    stencil_read_mask: 0,
                    stencil_write_mask: 0,
                    depth_bias: 0,
                    depth_bias_slope_scale: 0.0,
                    depth_bias_clamp: 0.0,
                }),
                multisample: single_sample(),
                fragment: Some(t::FragmentState {
                    stage: stage(module, "fs"),
                    targets: vec![target(&format)],
                }),
                multiview_mask: None,
            },
        )
        .map_err(fail)?;
        let (verts, idx) = cube();
        let vertices = buffer(device, (verts.len() * 4) as u64, t::BufferUsages::VERTEX | t::BufferUsages::COPY_DST).map_err(fail)?;
        g::write_buffer(device, vertices, 0, &bytes_of(&verts)).map_err(fail)?;
        let index_bytes: Vec<u8> = idx.iter().flat_map(|i| i.to_le_bytes()).collect();
        let indices = buffer(device, index_bytes.len() as u64, t::BufferUsages::INDEX | t::BufferUsages::COPY_DST).map_err(fail)?;
        g::write_buffer(device, indices, 0, &index_bytes).map_err(fail)?;
        let uniforms = buffer(device, 144, t::BufferUsages::UNIFORM | t::BufferUsages::COPY_DST).map_err(fail)?;
        let group = g::create_bind_group(
            device,
            &t::BindGroupDescriptor {
                label: None,
                layout,
                entries: vec![t::BindGroupEntry {
                    binding: 0,
                    resource: t::BindingResource::Buffer(t::BufferBinding {
                        buffer: uniforms,
                        offset: 0,
                        size: None,
                    }),
                }],
            },
        )
        .map_err(fail)?;
        Ok(Cube {
            device,
            surface,
            format,
            pipeline,
            uniforms,
            group,
            vertices,
            indices,
            index_count: idx.len() as u32,
            depth: None,
            size: (0, 0),
        })
    }

    /// Match the surface to the canvas's size in pixels.
    fn fit(&mut self) -> Result<(), String> {
        let size = g::surface_pixel_size(self.surface).map_err(|e| format!("{e:?}"))?;
        if size == self.size {
            return Ok(());
        }
        g::configure_surface(
            self.surface,
            self.device,
            &t::SurfaceConfiguration {
                usage: t::TextureUsages::RENDER_ATTACHMENT,
                format: self.format.clone(),
                width: size.0,
                height: size.1,
                present_mode: t::PresentMode::Fifo,
                alpha_mode: t::CompositeAlphaMode::Opaque,
                view_formats: Vec::new(),
                desired_maximum_frame_latency: 2,
            },
        )
        .map_err(|e| format!("configure={e:?}"))?;
        if let Some((tex, view, _)) = self.depth.take() {
            g::release(view);
            g::release(tex);
        }
        let tex = texture(self.device, size.0, size.1, "depth32float", t::TextureUsages::RENDER_ATTACHMENT)
            .map_err(|e| format!("{e:?}"))?;
        let view = whole_view(tex).map_err(|e| format!("{e:?}"))?;
        self.depth = Some((tex, view, size));
        self.size = size;
        Ok(())
    }

    fn draw(&mut self, seconds: f32) -> Result<(), String> {
        self.fit()?;
        let frame = match g::current_texture(self.surface) {
            Ok(frame) => frame,
            Err(t::SurfaceError::Outdated) | Err(t::SurfaceError::Occluded) => return Ok(()),
            Err(e) => return Err(format!("frame={e:?}")),
        };
        let view = whole_view(frame.texture).map_err(|e| format!("{e:?}"))?;
        let aspect = self.size.0 as f32 / self.size.1.max(1) as f32;
        let model = rotation(0.7 + seconds * 0.9, 0.5 + seconds * 0.6);
        let view_proj = mul(&perspective(0.9, aspect, 0.1, 100.0), &translation(-5.0));
        let mvp = mul(&view_proj, &model);
        let mut uniform = Vec::with_capacity(36);
        uniform.extend_from_slice(&mvp);
        uniform.extend_from_slice(&model);
        uniform.extend_from_slice(&[0.4, 0.8, 0.6, 0.0]);
        g::write_buffer(self.device, self.uniforms, 0, &bytes_of(&uniform)).map_err(|e| format!("{e:?}"))?;
        let encoder = g::create_command_encoder(self.device, None).map_err(|e| format!("{e:?}"))?;
        let depth = self.depth.as_ref().map(|(_, v, _)| *v);
        g::encode_render_pass(
            encoder,
            &color_pass(view, t::Color { r: 0.07, g: 0.08, b: 0.11, a: 1.0 }, depth),
            &[
                t::RenderCommand::SetPipeline(self.pipeline),
                t::RenderCommand::SetBindGroup(t::SetBindGroup {
                    index: 0,
                    group: Some(self.group),
                    offsets: Vec::new(),
                }),
                t::RenderCommand::SetVertexBuffer(t::SetVertexBuffer {
                    slot: 0,
                    buffer: self.vertices,
                    offset: 0,
                    size: None,
                }),
                t::RenderCommand::SetIndexBuffer(t::SetIndexBuffer {
                    buffer: self.indices,
                    format: t::IndexFormat::Uint16,
                    offset: 0,
                    size: None,
                }),
                t::RenderCommand::DrawIndexed(t::DrawIndexed {
                    index_count: self.index_count,
                    instance_count: 1,
                    first_index: 0,
                    base_vertex: 0,
                    first_instance: 0,
                }),
            ],
        )
        .map_err(|e| format!("pass={e:?}"))?;
        let commands = g::finish_encoder(encoder, None).map_err(|e| format!("{e:?}"))?;
        g::submit(self.device, &[commands]).map_err(|e| format!("{e:?}"))?;
        g::present(self.surface).map_err(|e| format!("present={e:?}"))?;
        g::release(view);
        Ok(())
    }
}

fn keep_stencil() -> t::StencilFaceState {
    t::StencilFaceState {
        compare: t::CompareFunction::Always,
        fail_op: t::StencilOperation::Keep,
        depth_fail_op: t::StencilOperation::Keep,
        pass_op: t::StencilOperation::Keep,
    }
}

fn layout(win: u64) {
    let node = |id, parent, kind| types::WidgetNode {
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
    };
    let _ = tree::set_root(win, &node(1, None, types::WidgetKind::Stack));
    let _ = tree::upsert_node(win, &node(2, Some(1), types::WidgetKind::Canvas));
}

impl Guest for Component {
    fn run() -> i32 {
        let out = stdio::stdout();
        let words = args::raw();
        if words.split('\n').any(|w| w == "check") {
            return checks(&out);
        }
        let limit: Option<f32> = words
            .split('\n')
            .find_map(|w| w.strip_prefix("seconds="))
            .and_then(|s| s.parse().ok());
        let Ok(win) = window::create(
            "GPU probe",
            types::WindowSize {
                width: 640,
                height: 480,
            },
        ) else {
            write_line(&out, "window=refused");
            return 1;
        };
        layout(win);
        let mut cube = match Cube::new(win) {
            Ok(cube) => cube,
            Err(why) => {
                write_line(&out, &why);
                return 1;
            }
        };
        let start = clock::monotonic_nanos();
        let mut frames = 0u32;
        let mut busy_ns = 0u64;
        let mut last_report = start;
        loop {
            let mut closed = false;
            while let Some(event) = events::poll() {
                if let types::Event::CloseRequested(_) = event {
                    closed = true;
                }
            }
            if closed {
                break;
            }
            let now = clock::monotonic_nanos();
            let seconds = (now - start) as f32 / 1e9;
            if limit.is_some_and(|l| seconds >= l) {
                break;
            }
            let began = clock::monotonic_nanos();
            if let Err(why) = cube.draw(seconds) {
                write_line(&out, &why);
                return 1;
            }
            busy_ns += clock::monotonic_nanos() - began;
            frames += 1;
            let now = clock::monotonic_nanos();
            if now - last_report >= 1_000_000_000 {
                let elapsed = (now - last_report) as f32 / 1e9;
                write_line(
                    &out,
                    &format!(
                        "frames={frames} fps={:.1} frame-ms={:.2} size={}x{}",
                        frames as f32 / elapsed,
                        busy_ns as f32 / 1e6 / frames.max(1) as f32,
                        cube.size.0,
                        cube.size.1
                    ),
                );
                frames = 0;
                busy_ns = 0;
                last_report = now;
            }
        }
        let _ = window::close(win);
        0
    }
}

krate::export!(Component);

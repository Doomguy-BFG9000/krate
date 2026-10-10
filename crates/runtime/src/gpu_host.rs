//! `krate:gpu` on the host: WebGPU's object model over the runtime's wgpu
//! (IC-540, IC-916).
//!
//! Every object an app makes is a wgpu object held here under a `u64` handle.
//! The app never holds a pointer, never names a driver, never compiles a
//! shader itself: it hands WGSL to `create-shader-module`, wgpu's validator
//! (naga) checks it, and a shader that fails is refused with its reason and
//! line before the driver sees a byte of it (the "shader door", F-311).
//!
//! Errors come back from the call that caused them. Each call runs inside
//! wgpu error scopes on its device, so a validation failure is an `Err` the
//! app can read rather than a callback fired later -- or, worse, wgpu's
//! default handler, which panics, and in a release build (panic = "abort")
//! takes the whole runtime with it. For the same reason every input that the
//! `wgpu` wrapper would `assert!` on (empty or out-of-range buffer slices, map
//! ranges, more than eight bind groups) is checked here first and refused.
//!
//! A surface is an offscreen texture the size the app configures. `present`
//! copies the frame into the canvas widget's picture and re-lowers the
//! window, exactly as a `scene3d` frame does, so it reaches every host --
//! headless screenshots included -- by the path canvas frames already take.
//! The copy goes through the CPU; presenting the texture directly is the
//! next step, and it waits on the presenter sharing this device (K-407).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use krate_adapter_common::ui::{ImagePixels, WidgetId, WindowId};
use krate_gpu_names as names;
use krate_presenter_gpu::vello::wgpu;

use super::Phase3GuiHost;
use crate::phase4_gui_bindings::krate::gpu::{gpu as g, types as t};

/// Handles start here so 0 is never a valid one.
const FIRST_HANDLE: u64 = 1;

/// wgpu's own cap on bind groups per pipeline layout; the wrapper asserts it.
const MAX_BIND_GROUPS: usize = 8;

/// Everything the app's GPU objects are.
#[derive(Default)]
pub(crate) struct GpuState {
    objects: HashMap<u64, Entry>,
    next: u64,
    instance: Option<wgpu::Instance>,
}

struct Entry {
    /// The device the object belongs to; 0 for adapters and surfaces.
    device: u64,
    obj: Obj,
}

enum Obj {
    Adapter(wgpu::Adapter),
    Device(Box<DeviceEntry>),
    Buffer(BufferEntry),
    Texture(TextureEntry),
    View(wgpu::TextureView),
    Sampler(wgpu::Sampler),
    QuerySet(wgpu::QuerySet),
    BindGroupLayout(wgpu::BindGroupLayout),
    BindGroup(wgpu::BindGroup),
    PipelineLayout(wgpu::PipelineLayout),
    Shader(wgpu::ShaderModule, Vec<t::CompilationMessage>),
    RenderPipeline(wgpu::RenderPipeline),
    ComputePipeline(wgpu::ComputePipeline),
    Encoder(Option<wgpu::CommandEncoder>),
    CommandBuffer(Option<wgpu::CommandBuffer>),
    Surface(Box<SurfaceEntry>),
}

impl Obj {
    fn kind(&self) -> &'static str {
        match self {
            Obj::Adapter(_) => "adapter",
            Obj::Device(_) => "device",
            Obj::Buffer(_) => "buffer",
            Obj::Texture(_) => "texture",
            Obj::View(_) => "texture view",
            Obj::Sampler(_) => "sampler",
            Obj::QuerySet(_) => "query set",
            Obj::BindGroupLayout(_) => "bind group layout",
            Obj::BindGroup(_) => "bind group",
            Obj::PipelineLayout(_) => "pipeline layout",
            Obj::Shader(..) => "shader module",
            Obj::RenderPipeline(_) => "render pipeline",
            Obj::ComputePipeline(_) => "compute pipeline",
            Obj::Encoder(_) => "command encoder",
            Obj::CommandBuffer(_) => "command buffer",
            Obj::Surface(_) => "surface",
        }
    }
}

struct DeviceEntry {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Set by wgpu's lost callback, or by `destroy-device`.
    lost: Arc<Mutex<Option<String>>>,
    /// Errors no scope caught (from submit or poll), reported on the next call.
    uncaptured: Arc<Mutex<Vec<String>>>,
}

struct BufferEntry {
    buffer: wgpu::Buffer,
    size: u64,
    usage: wgpu::BufferUsages,
    /// The mapped range and whether it is writable, once mapping succeeded.
    mapped: Option<(std::ops::Range<u64>, bool)>,
}

struct TextureEntry {
    texture: wgpu::Texture,
    /// A surface's frame: the app may not destroy it, only present it.
    surface_owned: bool,
}

struct SurfaceEntry {
    window: WindowId,
    widget: WidgetId,
    config: Option<SurfaceConfig>,
    /// The texture frames are drawn into, made at configure time.
    texture: Option<wgpu::Texture>,
    /// The handle given out for the current frame, if one is out.
    frame: Option<u64>,
    /// Where a frame is copied to on its way to the canvas.
    readback: Option<(wgpu::Buffer, u64)>,
}

#[derive(Clone)]
struct SurfaceConfig {
    device: u64,
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
    width: u32,
    height: u32,
    view_formats: Vec<wgpu::TextureFormat>,
}

type GpuResult<T> = Result<T, t::GpuError>;

fn invalid(handle: u64, wanted: &str) -> t::GpuError {
    t::GpuError::InvalidHandle(format!("{handle} is not a live {wanted}"))
}

fn validation(text: impl Into<String>) -> t::GpuError {
    t::GpuError::Validation(text.into())
}

fn wgpu_error(error: wgpu::Error) -> t::GpuError {
    match error {
        wgpu::Error::OutOfMemory { .. } => t::GpuError::OutOfMemory,
        wgpu::Error::Validation { description, .. } => t::GpuError::Validation(description),
        wgpu::Error::Internal { description, .. } => t::GpuError::Internal(description),
    }
}

/// Run `f` with every error it raises on `device` caught and returned.
fn scoped<T>(device: &wgpu::Device, f: impl FnOnce() -> T) -> (T, Option<t::GpuError>) {
    let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
    let oom = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let valid = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let out = f();
    let v = pollster::block_on(valid.pop());
    let o = pollster::block_on(oom.pop());
    let i = pollster::block_on(internal.pop());
    (out, v.or(o).or(i).map(wgpu_error))
}

fn label(text: &Option<String>) -> Option<&str> {
    text.as_deref()
}

fn format(name: &str) -> GpuResult<wgpu::TextureFormat> {
    names::parse_format(name)
        .ok_or_else(|| t::GpuError::Unsupported(format!("texture format {name:?}")))
}

fn formats(list: &[String]) -> GpuResult<Vec<wgpu::TextureFormat>> {
    list.iter().map(|name| format(name)).collect()
}

fn features(list: &[String]) -> GpuResult<wgpu::Features> {
    let mut out = wgpu::Features::empty();
    for name in list {
        out |= names::parse_feature(name)
            .ok_or_else(|| t::GpuError::Unsupported(format!("feature {name:?}")))?;
    }
    Ok(out)
}

fn feature_list(features: wgpu::Features) -> Vec<String> {
    names::feature_names(features)
        .into_iter()
        .map(String::from)
        .collect()
}

fn limit_list(limits: &wgpu::Limits) -> Vec<t::Limit> {
    names::limit_pairs(limits)
        .into_iter()
        .map(|(name, value)| t::Limit {
            name: name.to_string(),
            value,
        })
        .collect()
}

fn buffer_usages(u: t::BufferUsages) -> wgpu::BufferUsages {
    let mut out = wgpu::BufferUsages::empty();
    for (flag, usage) in [
        (t::BufferUsages::MAP_READ, wgpu::BufferUsages::MAP_READ),
        (t::BufferUsages::MAP_WRITE, wgpu::BufferUsages::MAP_WRITE),
        (t::BufferUsages::COPY_SRC, wgpu::BufferUsages::COPY_SRC),
        (t::BufferUsages::COPY_DST, wgpu::BufferUsages::COPY_DST),
        (t::BufferUsages::INDEX, wgpu::BufferUsages::INDEX),
        (t::BufferUsages::VERTEX, wgpu::BufferUsages::VERTEX),
        (t::BufferUsages::UNIFORM, wgpu::BufferUsages::UNIFORM),
        (t::BufferUsages::STORAGE, wgpu::BufferUsages::STORAGE),
        (t::BufferUsages::INDIRECT, wgpu::BufferUsages::INDIRECT),
        (
            t::BufferUsages::QUERY_RESOLVE,
            wgpu::BufferUsages::QUERY_RESOLVE,
        ),
    ] {
        if u.contains(flag) {
            out |= usage;
        }
    }
    out
}

fn texture_usages(u: t::TextureUsages) -> wgpu::TextureUsages {
    let mut out = wgpu::TextureUsages::empty();
    for (flag, usage) in [
        (t::TextureUsages::COPY_SRC, wgpu::TextureUsages::COPY_SRC),
        (t::TextureUsages::COPY_DST, wgpu::TextureUsages::COPY_DST),
        (
            t::TextureUsages::TEXTURE_BINDING,
            wgpu::TextureUsages::TEXTURE_BINDING,
        ),
        (
            t::TextureUsages::STORAGE_BINDING,
            wgpu::TextureUsages::STORAGE_BINDING,
        ),
        (
            t::TextureUsages::RENDER_ATTACHMENT,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        ),
    ] {
        if u.contains(flag) {
            out |= usage;
        }
    }
    out
}

fn texture_usages_out(u: wgpu::TextureUsages) -> t::TextureUsages {
    let mut out = t::TextureUsages::empty();
    for (flag, usage) in [
        (t::TextureUsages::COPY_SRC, wgpu::TextureUsages::COPY_SRC),
        (t::TextureUsages::COPY_DST, wgpu::TextureUsages::COPY_DST),
        (
            t::TextureUsages::TEXTURE_BINDING,
            wgpu::TextureUsages::TEXTURE_BINDING,
        ),
        (
            t::TextureUsages::STORAGE_BINDING,
            wgpu::TextureUsages::STORAGE_BINDING,
        ),
        (
            t::TextureUsages::RENDER_ATTACHMENT,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        ),
    ] {
        if u.contains(usage) {
            out |= flag;
        }
    }
    out
}

fn shader_stages(s: t::ShaderStages) -> wgpu::ShaderStages {
    let mut out = wgpu::ShaderStages::empty();
    if s.contains(t::ShaderStages::VERTEX) {
        out |= wgpu::ShaderStages::VERTEX;
    }
    if s.contains(t::ShaderStages::FRAGMENT) {
        out |= wgpu::ShaderStages::FRAGMENT;
    }
    if s.contains(t::ShaderStages::COMPUTE) {
        out |= wgpu::ShaderStages::COMPUTE;
    }
    out
}

fn color_writes(c: t::ColorWrites) -> wgpu::ColorWrites {
    let mut out = wgpu::ColorWrites::empty();
    if c.contains(t::ColorWrites::RED) {
        out |= wgpu::ColorWrites::RED;
    }
    if c.contains(t::ColorWrites::GREEN) {
        out |= wgpu::ColorWrites::GREEN;
    }
    if c.contains(t::ColorWrites::BLUE) {
        out |= wgpu::ColorWrites::BLUE;
    }
    if c.contains(t::ColorWrites::ALPHA) {
        out |= wgpu::ColorWrites::ALPHA;
    }
    out
}

fn extent(e: &t::Extent3d) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width: e.width,
        height: e.height,
        depth_or_array_layers: e.depth_or_array_layers,
    }
}

fn texture_dimension(d: t::TextureDimension) -> wgpu::TextureDimension {
    match d {
        t::TextureDimension::D1 => wgpu::TextureDimension::D1,
        t::TextureDimension::D2 => wgpu::TextureDimension::D2,
        t::TextureDimension::D3 => wgpu::TextureDimension::D3,
    }
}

fn view_dimension(d: t::TextureViewDimension) -> wgpu::TextureViewDimension {
    match d {
        t::TextureViewDimension::D1 => wgpu::TextureViewDimension::D1,
        t::TextureViewDimension::D2 => wgpu::TextureViewDimension::D2,
        t::TextureViewDimension::D2Array => wgpu::TextureViewDimension::D2Array,
        t::TextureViewDimension::Cube => wgpu::TextureViewDimension::Cube,
        t::TextureViewDimension::CubeArray => wgpu::TextureViewDimension::CubeArray,
        t::TextureViewDimension::D3 => wgpu::TextureViewDimension::D3,
    }
}

fn aspect(a: t::TextureAspect) -> wgpu::TextureAspect {
    match a {
        t::TextureAspect::All => wgpu::TextureAspect::All,
        t::TextureAspect::StencilOnly => wgpu::TextureAspect::StencilOnly,
        t::TextureAspect::DepthOnly => wgpu::TextureAspect::DepthOnly,
    }
}

fn address_mode(a: t::AddressMode) -> wgpu::AddressMode {
    match a {
        t::AddressMode::ClampToEdge => wgpu::AddressMode::ClampToEdge,
        t::AddressMode::Repeat => wgpu::AddressMode::Repeat,
        t::AddressMode::MirrorRepeat => wgpu::AddressMode::MirrorRepeat,
        t::AddressMode::ClampToBorder => wgpu::AddressMode::ClampToBorder,
    }
}

fn filter_mode(f: t::FilterMode) -> wgpu::FilterMode {
    match f {
        t::FilterMode::Nearest => wgpu::FilterMode::Nearest,
        t::FilterMode::Linear => wgpu::FilterMode::Linear,
    }
}

fn mipmap_filter_mode(f: t::FilterMode) -> wgpu::MipmapFilterMode {
    match f {
        t::FilterMode::Nearest => wgpu::MipmapFilterMode::Nearest,
        t::FilterMode::Linear => wgpu::MipmapFilterMode::Linear,
    }
}

fn compare(c: t::CompareFunction) -> wgpu::CompareFunction {
    match c {
        t::CompareFunction::Never => wgpu::CompareFunction::Never,
        t::CompareFunction::Less => wgpu::CompareFunction::Less,
        t::CompareFunction::Equal => wgpu::CompareFunction::Equal,
        t::CompareFunction::LessEqual => wgpu::CompareFunction::LessEqual,
        t::CompareFunction::Greater => wgpu::CompareFunction::Greater,
        t::CompareFunction::NotEqual => wgpu::CompareFunction::NotEqual,
        t::CompareFunction::GreaterEqual => wgpu::CompareFunction::GreaterEqual,
        t::CompareFunction::Always => wgpu::CompareFunction::Always,
    }
}

fn border_color(b: t::BorderColor) -> wgpu::SamplerBorderColor {
    match b {
        t::BorderColor::TransparentBlack => wgpu::SamplerBorderColor::TransparentBlack,
        t::BorderColor::OpaqueBlack => wgpu::SamplerBorderColor::OpaqueBlack,
        t::BorderColor::OpaqueWhite => wgpu::SamplerBorderColor::OpaqueWhite,
        t::BorderColor::Zero => wgpu::SamplerBorderColor::Zero,
    }
}

fn index_format(f: t::IndexFormat) -> wgpu::IndexFormat {
    match f {
        t::IndexFormat::Uint16 => wgpu::IndexFormat::Uint16,
        t::IndexFormat::Uint32 => wgpu::IndexFormat::Uint32,
    }
}

fn stencil_op(o: t::StencilOperation) -> wgpu::StencilOperation {
    match o {
        t::StencilOperation::Keep => wgpu::StencilOperation::Keep,
        t::StencilOperation::Zero => wgpu::StencilOperation::Zero,
        t::StencilOperation::Replace => wgpu::StencilOperation::Replace,
        t::StencilOperation::Invert => wgpu::StencilOperation::Invert,
        t::StencilOperation::IncrementClamp => wgpu::StencilOperation::IncrementClamp,
        t::StencilOperation::DecrementClamp => wgpu::StencilOperation::DecrementClamp,
        t::StencilOperation::IncrementWrap => wgpu::StencilOperation::IncrementWrap,
        t::StencilOperation::DecrementWrap => wgpu::StencilOperation::DecrementWrap,
    }
}

fn stencil_face(s: &t::StencilFaceState) -> wgpu::StencilFaceState {
    wgpu::StencilFaceState {
        compare: compare(s.compare),
        fail_op: stencil_op(s.fail_op),
        depth_fail_op: stencil_op(s.depth_fail_op),
        pass_op: stencil_op(s.pass_op),
    }
}

fn blend_factor(f: t::BlendFactor) -> wgpu::BlendFactor {
    use wgpu::BlendFactor as B;
    match f {
        t::BlendFactor::Zero => B::Zero,
        t::BlendFactor::One => B::One,
        t::BlendFactor::Src => B::Src,
        t::BlendFactor::OneMinusSrc => B::OneMinusSrc,
        t::BlendFactor::SrcAlpha => B::SrcAlpha,
        t::BlendFactor::OneMinusSrcAlpha => B::OneMinusSrcAlpha,
        t::BlendFactor::Dst => B::Dst,
        t::BlendFactor::OneMinusDst => B::OneMinusDst,
        t::BlendFactor::DstAlpha => B::DstAlpha,
        t::BlendFactor::OneMinusDstAlpha => B::OneMinusDstAlpha,
        t::BlendFactor::SrcAlphaSaturated => B::SrcAlphaSaturated,
        t::BlendFactor::Constant => B::Constant,
        t::BlendFactor::OneMinusConstant => B::OneMinusConstant,
        t::BlendFactor::Src1 => B::Src1,
        t::BlendFactor::OneMinusSrc1 => B::OneMinusSrc1,
        t::BlendFactor::Src1Alpha => B::Src1Alpha,
        t::BlendFactor::OneMinusSrc1Alpha => B::OneMinusSrc1Alpha,
    }
}

fn blend_component(c: &t::BlendComponent) -> wgpu::BlendComponent {
    wgpu::BlendComponent {
        src_factor: blend_factor(c.src_factor),
        dst_factor: blend_factor(c.dst_factor),
        operation: match c.operation {
            t::BlendOperation::Add => wgpu::BlendOperation::Add,
            t::BlendOperation::Subtract => wgpu::BlendOperation::Subtract,
            t::BlendOperation::ReverseSubtract => wgpu::BlendOperation::ReverseSubtract,
            t::BlendOperation::Min => wgpu::BlendOperation::Min,
            t::BlendOperation::Max => wgpu::BlendOperation::Max,
        },
    }
}

fn store_op(s: t::StoreOp) -> wgpu::StoreOp {
    match s {
        t::StoreOp::Store => wgpu::StoreOp::Store,
        t::StoreOp::Discard => wgpu::StoreOp::Discard,
    }
}

fn origin(o: &t::Origin3d) -> wgpu::Origin3d {
    wgpu::Origin3d {
        x: o.x,
        y: o.y,
        z: o.z,
    }
}

fn copy_layout(l: &t::TexelCopyBufferLayout) -> wgpu::TexelCopyBufferLayout {
    wgpu::TexelCopyBufferLayout {
        offset: l.offset,
        bytes_per_row: l.bytes_per_row,
        rows_per_image: l.rows_per_image,
    }
}

fn nonzero(v: Option<u32>) -> Option<std::num::NonZeroU32> {
    v.and_then(std::num::NonZeroU32::new)
}

fn present_mode_out(m: wgpu::PresentMode) -> Option<t::PresentMode> {
    Some(match m {
        wgpu::PresentMode::Fifo => t::PresentMode::Fifo,
        wgpu::PresentMode::FifoRelaxed => t::PresentMode::FifoRelaxed,
        wgpu::PresentMode::Immediate => t::PresentMode::Immediate,
        wgpu::PresentMode::Mailbox => t::PresentMode::Mailbox,
        _ => return None,
    })
}

impl GpuState {
    fn insert(&mut self, device: u64, obj: Obj) -> u64 {
        if self.next < FIRST_HANDLE {
            self.next = FIRST_HANDLE;
        }
        let handle = self.next;
        self.next += 1;
        self.objects.insert(handle, Entry { device, obj });
        handle
    }

    fn instance(&mut self) -> wgpu::Instance {
        if let Some(instance) = &self.instance {
            return instance.clone();
        }
        // The runtime's shared instance when there is one, so a device made
        // here lives where the window presenter's can reach it later (K-407).
        let instance = krate_presenter_gpu::shared_gpu()
            .map(|shared| shared.instance)
            .unwrap_or_else(|| {
                wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle())
            });
        self.instance = Some(instance.clone());
        instance
    }

    fn get(&self, handle: u64) -> GpuResult<&Entry> {
        self.objects
            .get(&handle)
            .ok_or_else(|| t::GpuError::InvalidHandle(format!("{handle} is not a live handle")))
    }

    fn device(&self, handle: u64) -> GpuResult<&DeviceEntry> {
        match self.objects.get(&handle) {
            Some(Entry {
                obj: Obj::Device(d),
                ..
            }) => {
                if let Some(reason) = d.lost.lock().ok().and_then(|l| l.clone()) {
                    return Err(t::GpuError::Lost(reason));
                }
                Ok(d)
            }
            _ => Err(invalid(handle, "device")),
        }
    }

    /// The device an object belongs to, live.
    fn owner(&self, handle: u64) -> GpuResult<&DeviceEntry> {
        let device = self.get(handle)?.device;
        self.device(device)
    }

    fn adapter(&self, handle: u64) -> GpuResult<&wgpu::Adapter> {
        match self.objects.get(&handle) {
            Some(Entry {
                obj: Obj::Adapter(a),
                ..
            }) => Ok(a),
            _ => Err(invalid(handle, "adapter")),
        }
    }

    fn buffer(&self, handle: u64) -> GpuResult<&BufferEntry> {
        match self.objects.get(&handle) {
            Some(Entry {
                obj: Obj::Buffer(b),
                ..
            }) => Ok(b),
            _ => Err(invalid(handle, "buffer")),
        }
    }

    fn texture(&self, handle: u64) -> GpuResult<&wgpu::Texture> {
        match self.objects.get(&handle) {
            Some(Entry {
                obj: Obj::Texture(t),
                ..
            }) => Ok(&t.texture),
            _ => Err(invalid(handle, "texture")),
        }
    }

    fn view(&self, handle: u64) -> GpuResult<&wgpu::TextureView> {
        match self.objects.get(&handle) {
            Some(Entry {
                obj: Obj::View(v), ..
            }) => Ok(v),
            _ => Err(invalid(handle, "texture view")),
        }
    }

    fn sampler(&self, handle: u64) -> GpuResult<&wgpu::Sampler> {
        match self.objects.get(&handle) {
            Some(Entry {
                obj: Obj::Sampler(s),
                ..
            }) => Ok(s),
            _ => Err(invalid(handle, "sampler")),
        }
    }

    fn query_set(&self, handle: u64) -> GpuResult<&wgpu::QuerySet> {
        match self.objects.get(&handle) {
            Some(Entry {
                obj: Obj::QuerySet(q),
                ..
            }) => Ok(q),
            _ => Err(invalid(handle, "query set")),
        }
    }

    fn bind_group_layout(&self, handle: u64) -> GpuResult<&wgpu::BindGroupLayout> {
        match self.objects.get(&handle) {
            Some(Entry {
                obj: Obj::BindGroupLayout(l),
                ..
            }) => Ok(l),
            _ => Err(invalid(handle, "bind group layout")),
        }
    }

    fn bind_group(&self, handle: u64) -> GpuResult<&wgpu::BindGroup> {
        match self.objects.get(&handle) {
            Some(Entry {
                obj: Obj::BindGroup(b),
                ..
            }) => Ok(b),
            _ => Err(invalid(handle, "bind group")),
        }
    }

    fn pipeline_layout(&self, handle: u64) -> GpuResult<&wgpu::PipelineLayout> {
        match self.objects.get(&handle) {
            Some(Entry {
                obj: Obj::PipelineLayout(l),
                ..
            }) => Ok(l),
            _ => Err(invalid(handle, "pipeline layout")),
        }
    }

    fn shader(&self, handle: u64) -> GpuResult<&wgpu::ShaderModule> {
        match self.objects.get(&handle) {
            Some(Entry {
                obj: Obj::Shader(s, _),
                ..
            }) => Ok(s),
            _ => Err(invalid(handle, "shader module")),
        }
    }

    fn render_pipeline(&self, handle: u64) -> GpuResult<&wgpu::RenderPipeline> {
        match self.objects.get(&handle) {
            Some(Entry {
                obj: Obj::RenderPipeline(p),
                ..
            }) => Ok(p),
            _ => Err(invalid(handle, "render pipeline")),
        }
    }

    fn compute_pipeline(&self, handle: u64) -> GpuResult<&wgpu::ComputePipeline> {
        match self.objects.get(&handle) {
            Some(Entry {
                obj: Obj::ComputePipeline(p),
                ..
            }) => Ok(p),
            _ => Err(invalid(handle, "compute pipeline")),
        }
    }

    /// A buffer slice the wgpu wrapper will not panic on.
    fn slice(
        &self,
        handle: u64,
        offset: u64,
        size: Option<u64>,
    ) -> GpuResult<wgpu::BufferSlice<'_>> {
        let b = self.buffer(handle)?;
        let size = match size {
            Some(size) => size,
            None => b.size.checked_sub(offset).ok_or_else(|| {
                validation(format!(
                    "offset {offset} is past the end of a {}-byte buffer",
                    b.size
                ))
            })?,
        };
        let end = offset.checked_add(size);
        if size == 0 || end.is_none_or(|end| end > b.size) {
            return Err(validation(format!(
                "bytes {offset}..+{size} are not inside a {}-byte buffer",
                b.size
            )));
        }
        Ok(b.buffer.slice(offset..offset + size))
    }

    fn programmable<'a>(
        &'a self,
        stage: &'a t::ProgrammableStage,
        constants: &'a [(&'a str, f64)],
    ) -> GpuResult<(
        &'a wgpu::ShaderModule,
        Option<&'a str>,
        wgpu::PipelineCompilationOptions<'a>,
    )> {
        Ok((
            self.shader(stage.module)?,
            stage.entry_point.as_deref(),
            wgpu::PipelineCompilationOptions {
                constants,
                zero_initialize_workgroup_memory: true,
            },
        ))
    }

    fn binding_type(kind: &t::BindingType) -> GpuResult<wgpu::BindingType> {
        Ok(match kind {
            t::BindingType::Buffer(b) => wgpu::BindingType::Buffer {
                ty: match b.kind {
                    t::BufferBindingType::Uniform => wgpu::BufferBindingType::Uniform,
                    t::BufferBindingType::Storage(read_only) => {
                        wgpu::BufferBindingType::Storage { read_only }
                    }
                },
                has_dynamic_offset: b.has_dynamic_offset,
                min_binding_size: wgpu::BufferSize::new(b.min_binding_size),
            },
            t::BindingType::Sampler(s) => wgpu::BindingType::Sampler(match s {
                t::SamplerBindingType::Filtering => wgpu::SamplerBindingType::Filtering,
                t::SamplerBindingType::NonFiltering => wgpu::SamplerBindingType::NonFiltering,
                t::SamplerBindingType::Comparison => wgpu::SamplerBindingType::Comparison,
            }),
            t::BindingType::Texture(tex) => wgpu::BindingType::Texture {
                sample_type: match tex.sample_type {
                    t::TextureSampleType::Float(filterable) => {
                        wgpu::TextureSampleType::Float { filterable }
                    }
                    t::TextureSampleType::Depth => wgpu::TextureSampleType::Depth,
                    t::TextureSampleType::Sint => wgpu::TextureSampleType::Sint,
                    t::TextureSampleType::Uint => wgpu::TextureSampleType::Uint,
                },
                view_dimension: view_dimension(tex.view_dimension),
                multisampled: tex.multisampled,
            },
            t::BindingType::StorageTexture(s) => wgpu::BindingType::StorageTexture {
                access: match s.access {
                    t::StorageTextureAccess::WriteOnly => wgpu::StorageTextureAccess::WriteOnly,
                    t::StorageTextureAccess::ReadOnly => wgpu::StorageTextureAccess::ReadOnly,
                    t::StorageTextureAccess::ReadWrite => wgpu::StorageTextureAccess::ReadWrite,
                },
                format: format(&s.format)?,
                view_dimension: view_dimension(s.view_dimension),
            },
        })
    }

    fn buffer_binding(&self, b: &t::BufferBinding) -> GpuResult<wgpu::BufferBinding<'_>> {
        let entry = self.buffer(b.buffer)?;
        if b.offset > entry.size {
            return Err(validation(format!(
                "binding offset {} is past the end of a {}-byte buffer",
                b.offset, entry.size
            )));
        }
        Ok(wgpu::BufferBinding {
            buffer: &entry.buffer,
            offset: b.offset,
            size: match b.size {
                Some(0) => return Err(validation("a buffer binding of 0 bytes")),
                Some(size) => wgpu::BufferSize::new(size),
                None => None,
            },
        })
    }
}

/// Run a closure on a device's error scopes and turn its result into the
/// call's answer.
macro_rules! on_device {
    ($device:expr, $body:expr) => {{
        let device: &wgpu::Device = $device;
        let (value, error) = scoped(device, || $body);
        match error {
            Some(error) => Err(error),
            None => Ok(value),
        }
    }};
}

impl t::Host for Phase3GuiHost {}

impl g::Host for Phase3GuiHost {
    fn request_adapter(&mut self, options: t::AdapterOptions) -> wasmtime::Result<GpuResult<u64>> {
        let gpu = self.gpu.get_mut();
        let instance = gpu.instance();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: match options.power_preference {
                t::PowerPreference::None => wgpu::PowerPreference::None,
                t::PowerPreference::LowPower => wgpu::PowerPreference::LowPower,
                t::PowerPreference::HighPerformance => wgpu::PowerPreference::HighPerformance,
            },
            force_fallback_adapter: options.force_fallback_adapter,
            compatible_surface: None,
        }));
        Ok(match adapter {
            Ok(adapter) => Ok(gpu.insert(0, Obj::Adapter(adapter))),
            Err(error) => Err(t::GpuError::Unsupported(format!("no GPU fits: {error}"))),
        })
    }

    fn describe_adapter(&mut self, adapter: u64) -> wasmtime::Result<GpuResult<t::AdapterInfo>> {
        let gpu = self.gpu.get_mut();
        Ok(gpu.adapter(adapter).map(|a| {
            let info = a.get_info();
            t::AdapterInfo {
                name: info.name,
                vendor: info.vendor,
                device: info.device,
                device_type: match info.device_type {
                    wgpu::DeviceType::Other => t::DeviceType::Other,
                    wgpu::DeviceType::IntegratedGpu => t::DeviceType::IntegratedGpu,
                    wgpu::DeviceType::DiscreteGpu => t::DeviceType::DiscreteGpu,
                    wgpu::DeviceType::VirtualGpu => t::DeviceType::VirtualGpu,
                    wgpu::DeviceType::Cpu => t::DeviceType::Cpu,
                },
                backend: info.backend.to_str().to_string(),
                driver: info.driver,
                driver_info: info.driver_info,
            }
        }))
    }

    fn adapter_features(&mut self, adapter: u64) -> wasmtime::Result<GpuResult<Vec<String>>> {
        let gpu = self.gpu.get_mut();
        // Experimental features need a separate opt-in krate:gpu does not
        // offer; advertising them made an engine that asks for everything
        // offered (Bevy) have its device refused.
        Ok(gpu
            .adapter(adapter)
            .map(|a| feature_list(a.features() - wgpu::Features::all_experimental_mask())))
    }

    fn adapter_limits(&mut self, adapter: u64) -> wasmtime::Result<GpuResult<Vec<t::Limit>>> {
        let gpu = self.gpu.get_mut();
        Ok(gpu.adapter(adapter).map(|a| limit_list(&a.limits())))
    }

    fn adapter_format_features(
        &mut self,
        adapter: u64,
        format_name: String,
    ) -> wasmtime::Result<GpuResult<t::FormatFeatures>> {
        let gpu = self.gpu.get_mut();
        let a = match gpu.adapter(adapter) {
            Ok(a) => a,
            Err(e) => return Ok(Err(e)),
        };
        let f = match format(&format_name) {
            Ok(f) => f,
            Err(e) => return Ok(Err(e)),
        };
        let features = a.get_texture_format_features(f);
        let flags = features.flags;
        Ok(Ok(t::FormatFeatures {
            allowed_usages: texture_usages_out(features.allowed_usages),
            sample_counts: flags.supported_sample_counts(),
            filterable: flags.contains(wgpu::TextureFormatFeatureFlags::FILTERABLE),
            blendable: flags.contains(wgpu::TextureFormatFeatureFlags::BLENDABLE),
            storage_read_write: flags.contains(wgpu::TextureFormatFeatureFlags::STORAGE_READ_WRITE),
        }))
    }

    fn request_device(
        &mut self,
        adapter: u64,
        descriptor: t::DeviceDescriptor,
    ) -> wasmtime::Result<GpuResult<u64>> {
        let gpu = self.gpu.get_mut();
        let a = match gpu.adapter(adapter) {
            Ok(a) => a.clone(),
            Err(e) => return Ok(Err(e)),
        };
        let required_features = match features(&descriptor.required_features) {
            Ok(f) => f,
            Err(e) => return Ok(Err(e)),
        };
        let mut required_limits = wgpu::Limits::default();
        for limit in &descriptor.required_limits {
            if !names::set_limit(&mut required_limits, &limit.name, limit.value) {
                return Ok(Err(t::GpuError::Unsupported(format!(
                    "limit {:?}",
                    limit.name
                ))));
            }
        }
        let result = pollster::block_on(a.request_device(&wgpu::DeviceDescriptor {
            label: descriptor.label.as_deref(),
            required_features,
            required_limits,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }));
        let (device, queue) = match result {
            Ok(pair) => pair,
            Err(error) => return Ok(Err(t::GpuError::Unsupported(error.to_string()))),
        };
        let lost = Arc::new(Mutex::new(None));
        let uncaptured = Arc::new(Mutex::new(Vec::new()));
        {
            // wgpu's DEFAULT handler panics, and a panic here is the whole
            // runtime under panic = "abort". Keep the text for the app.
            let uncaptured = Arc::clone(&uncaptured);
            device.on_uncaptured_error(Arc::new(move |error| {
                if let Ok(mut list) = uncaptured.lock() {
                    list.push(error.to_string());
                }
            }));
            let lost = Arc::clone(&lost);
            device.set_device_lost_callback(move |reason, message| {
                if let Ok(mut slot) = lost.lock() {
                    slot.get_or_insert_with(|| format!("{reason:?}: {message}"));
                }
            });
        }
        Ok(Ok(gpu.insert(
            0,
            Obj::Device(Box::new(DeviceEntry {
                device,
                queue,
                lost,
                uncaptured,
            })),
        )))
    }

    fn device_features(&mut self, device: u64) -> wasmtime::Result<GpuResult<Vec<String>>> {
        let gpu = self.gpu.get_mut();
        Ok(gpu
            .device(device)
            .map(|d| feature_list(d.device.features())))
    }

    fn device_limits(&mut self, device: u64) -> wasmtime::Result<GpuResult<Vec<t::Limit>>> {
        let gpu = self.gpu.get_mut();
        Ok(gpu.device(device).map(|d| limit_list(&d.device.limits())))
    }

    fn destroy_device(&mut self, device: u64) -> wasmtime::Result<GpuResult<()>> {
        let gpu = self.gpu.get_mut();
        Ok(gpu.device(device).map(|d| {
            if let Ok(mut slot) = d.lost.lock() {
                slot.get_or_insert_with(|| "destroyed by the app".to_string());
            }
            d.device.destroy();
        }))
    }

    fn device_lost_reason(&mut self, device: u64) -> wasmtime::Result<Option<String>> {
        let gpu = self.gpu.get_mut();
        Ok(match gpu.objects.get(&device) {
            Some(Entry {
                obj: Obj::Device(d),
                ..
            }) => d.lost.lock().ok().and_then(|l| l.clone()),
            _ => Some("no such device".to_string()),
        })
    }

    fn poll(&mut self, device: u64, wait: bool) -> wasmtime::Result<GpuResult<bool>> {
        let gpu = self.gpu.get_mut();
        let d = match gpu.device(device) {
            Ok(d) => d,
            Err(e) => return Ok(Err(e)),
        };
        let poll = if wait {
            wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(std::time::Duration::from_secs(10)),
            }
        } else {
            wgpu::PollType::Poll
        };
        let status = d.device.poll(poll);
        // Errors raised outside any call's scope (inside the driver, while
        // work finished) are reported here, the next time the app asks.
        let late: Vec<String> = d
            .uncaptured
            .lock()
            .map(|mut list| std::mem::take(&mut *list))
            .unwrap_or_default();
        if !late.is_empty() {
            return Ok(Err(validation(late.join("; "))));
        }
        Ok(match status {
            Ok(status) => Ok(status.is_queue_empty()),
            Err(error) => Err(t::GpuError::Internal(error.to_string())),
        })
    }

    fn create_buffer(
        &mut self,
        device: u64,
        descriptor: t::BufferDescriptor,
    ) -> wasmtime::Result<GpuResult<u64>> {
        let gpu = self.gpu.get_mut();
        let d = match gpu.device(device) {
            Ok(d) => d,
            Err(e) => return Ok(Err(e)),
        };
        let usage = buffer_usages(descriptor.usage);
        let made = on_device!(
            &d.device,
            d.device.create_buffer(&wgpu::BufferDescriptor {
                label: label(&descriptor.label),
                size: descriptor.size,
                usage,
                mapped_at_creation: descriptor.mapped_at_creation,
            })
        );
        Ok(made.map(|buffer| {
            let mapped = descriptor
                .mapped_at_creation
                .then_some((0..descriptor.size, true));
            gpu.insert(
                device,
                Obj::Buffer(BufferEntry {
                    buffer,
                    size: descriptor.size,
                    usage,
                    mapped,
                }),
            )
        }))
    }

    fn map_buffer(
        &mut self,
        buffer: u64,
        mode: t::MapMode,
        offset: u64,
        size: u64,
    ) -> wasmtime::Result<GpuResult<()>> {
        let gpu = self.gpu.get_mut();
        let d = match gpu.owner(buffer) {
            Ok(d) => d.device.clone(),
            Err(e) => return Ok(Err(e)),
        };
        let b = match gpu.buffer(buffer) {
            Ok(b) => b,
            Err(e) => return Ok(Err(e)),
        };
        if b.mapped.is_some() {
            return Ok(Err(validation("the buffer is already mapped")));
        }
        let (wanted, write) = match mode {
            t::MapMode::Read => (wgpu::BufferUsages::MAP_READ, false),
            t::MapMode::Write => (wgpu::BufferUsages::MAP_WRITE, true),
        };
        if !b.usage.contains(wanted) {
            return Ok(Err(validation(format!(
                "a buffer without {wanted:?} cannot be mapped that way"
            ))));
        }
        if !offset.is_multiple_of(wgpu::MAP_ALIGNMENT)
            || !size.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT)
        {
            return Ok(Err(validation(
                "a map offset must be a multiple of 8 and its size of 4",
            )));
        }
        let slice = match gpu.slice(buffer, offset, Some(size)) {
            Ok(s) => s,
            Err(e) => return Ok(Err(e)),
        };
        let result = Arc::new(Mutex::new(None));
        {
            let result = Arc::clone(&result);
            let (_, error) = scoped(&d, || {
                slice.map_async(
                    if write {
                        wgpu::MapMode::Write
                    } else {
                        wgpu::MapMode::Read
                    },
                    move |r| {
                        if let Ok(mut slot) = result.lock() {
                            *slot = Some(r);
                        }
                    },
                )
            });
            if let Some(error) = error {
                let b = gpu.buffer(buffer).map(|b| b.buffer.clone());
                if let Ok(b) = b {
                    let _ = scoped(&d, || b.unmap());
                }
                return Ok(Err(error));
            }
        }
        let _ = d.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_secs(10)),
        });
        let outcome = result.lock().ok().and_then(|mut r| r.take());
        let Some(Entry {
            obj: Obj::Buffer(b),
            ..
        }) = gpu.objects.get_mut(&buffer)
        else {
            return Ok(Err(invalid(buffer, "buffer")));
        };
        Ok(match outcome {
            Some(Ok(())) => {
                b.mapped = Some((offset..offset + size, write));
                Ok(())
            }
            other => {
                // The wrapper marked the range mapped when map_async was
                // called; clear it, or the next map asserts.
                let buf = b.buffer.clone();
                let _ = scoped(&d, || buf.unmap());
                Err(validation(match other {
                    Some(Err(error)) => format!("the buffer could not be mapped: {error}"),
                    _ => "the buffer did not finish mapping".to_string(),
                }))
            }
        })
    }

    fn read_mapped(
        &mut self,
        buffer: u64,
        offset: u64,
        size: u64,
    ) -> wasmtime::Result<GpuResult<Vec<u8>>> {
        let gpu = self.gpu.get_mut();
        let b = match gpu.buffer(buffer) {
            Ok(b) => b,
            Err(e) => return Ok(Err(e)),
        };
        let Some((range, _)) = b.mapped.clone() else {
            return Ok(Err(validation("the buffer is not mapped")));
        };
        let end = offset.checked_add(size);
        if offset < range.start || end.is_none_or(|end| end > range.end) {
            return Ok(Err(validation(format!(
                "bytes {offset}..+{size} are outside the mapped range {}..{}",
                range.start, range.end
            ))));
        }
        if range.is_empty() {
            return Ok(Ok(Vec::new()));
        }
        // The whole mapped range, always: it is aligned (the map checked),
        // where an arbitrary sub-range may not be, and an unaligned one is a
        // fatal error inside wgpu.
        let view = b.buffer.slice(range.clone()).get_mapped_range();
        let start = (offset - range.start) as usize;
        Ok(Ok(view[start..start + size as usize].to_vec()))
    }

    fn write_mapped(
        &mut self,
        buffer: u64,
        offset: u64,
        data: Vec<u8>,
    ) -> wasmtime::Result<GpuResult<()>> {
        let gpu = self.gpu.get_mut();
        let b = match gpu.buffer(buffer) {
            Ok(b) => b,
            Err(e) => return Ok(Err(e)),
        };
        let Some((range, writable)) = b.mapped.clone() else {
            return Ok(Err(validation("the buffer is not mapped")));
        };
        if !writable {
            return Ok(Err(validation("the buffer is mapped for reading")));
        }
        let end = offset.checked_add(data.len() as u64);
        if offset < range.start || end.is_none_or(|end| end > range.end) {
            return Ok(Err(validation(format!(
                "bytes {offset}..+{} are outside the mapped range {}..{}",
                data.len(),
                range.start,
                range.end
            ))));
        }
        if data.is_empty() || range.is_empty() {
            return Ok(Ok(()));
        }
        let mut view = b.buffer.slice(range.clone()).get_mapped_range_mut();
        let start = (offset - range.start) as usize;
        view.slice(start..start + data.len()).copy_from_slice(&data);
        Ok(Ok(()))
    }

    fn unmap_buffer(&mut self, buffer: u64) -> wasmtime::Result<GpuResult<()>> {
        let gpu = self.gpu.get_mut();
        let d = match gpu.owner(buffer) {
            Ok(d) => d.device.clone(),
            Err(e) => return Ok(Err(e)),
        };
        let Some(Entry {
            obj: Obj::Buffer(b),
            ..
        }) = gpu.objects.get_mut(&buffer)
        else {
            return Ok(Err(invalid(buffer, "buffer")));
        };
        if b.mapped.take().is_none() {
            return Ok(Ok(()));
        }
        let buf = b.buffer.clone();
        Ok(on_device!(&d, buf.unmap()))
    }

    fn destroy_buffer(&mut self, buffer: u64) -> wasmtime::Result<GpuResult<()>> {
        let gpu = self.gpu.get_mut();
        let Some(Entry {
            obj: Obj::Buffer(b),
            ..
        }) = gpu.objects.get_mut(&buffer)
        else {
            return Ok(Err(invalid(buffer, "buffer")));
        };
        b.mapped = None;
        b.buffer.destroy();
        Ok(Ok(()))
    }

    fn create_texture(
        &mut self,
        device: u64,
        descriptor: t::TextureDescriptor,
    ) -> wasmtime::Result<GpuResult<u64>> {
        let gpu = self.gpu.get_mut();
        let d = match gpu.device(device) {
            Ok(d) => d,
            Err(e) => return Ok(Err(e)),
        };
        let (fmt, view_formats) = match (
            format(&descriptor.format),
            formats(&descriptor.view_formats),
        ) {
            (Ok(f), Ok(v)) => (f, v),
            (Err(e), _) | (_, Err(e)) => return Ok(Err(e)),
        };
        let made = on_device!(
            &d.device,
            d.device.create_texture(&wgpu::TextureDescriptor {
                label: label(&descriptor.label),
                size: extent(&descriptor.size),
                mip_level_count: descriptor.mip_level_count,
                sample_count: descriptor.sample_count,
                dimension: texture_dimension(descriptor.dimension),
                format: fmt,
                usage: texture_usages(descriptor.usage),
                view_formats: &view_formats,
            })
        );
        Ok(made.map(|texture| {
            gpu.insert(
                device,
                Obj::Texture(TextureEntry {
                    texture,
                    surface_owned: false,
                }),
            )
        }))
    }

    fn destroy_texture(&mut self, texture: u64) -> wasmtime::Result<GpuResult<()>> {
        let gpu = self.gpu.get_mut();
        Ok(match gpu.objects.get(&texture) {
            Some(Entry {
                obj: Obj::Texture(t),
                ..
            }) => {
                // A surface's frame belongs to the surface; WebGPU forbids
                // destroying it and so does this.
                if !t.surface_owned {
                    t.texture.destroy();
                }
                Ok(())
            }
            _ => Err(invalid(texture, "texture")),
        })
    }

    fn create_texture_view(
        &mut self,
        texture: u64,
        descriptor: t::TextureViewDescriptor,
    ) -> wasmtime::Result<GpuResult<u64>> {
        let gpu = self.gpu.get_mut();
        let device = match gpu.get(texture) {
            Ok(e) => e.device,
            Err(e) => return Ok(Err(e)),
        };
        let d = match gpu.device(device) {
            Ok(d) => d,
            Err(e) => return Ok(Err(e)),
        };
        let tex = match gpu.texture(texture) {
            Ok(t) => t,
            Err(e) => return Ok(Err(e)),
        };
        let fmt = match descriptor.format.as_deref().map(format).transpose() {
            Ok(f) => f,
            Err(e) => return Ok(Err(e)),
        };
        let made = on_device!(
            &d.device,
            tex.create_view(&wgpu::TextureViewDescriptor {
                label: label(&descriptor.label),
                format: fmt,
                dimension: descriptor.dimension.map(view_dimension),
                usage: descriptor.usage.map(texture_usages),
                aspect: aspect(descriptor.aspect),
                base_mip_level: descriptor.base_mip_level,
                mip_level_count: descriptor.mip_level_count,
                base_array_layer: descriptor.base_array_layer,
                array_layer_count: descriptor.array_layer_count,
            })
        );
        Ok(made.map(|view| gpu.insert(device, Obj::View(view))))
    }

    fn create_sampler(
        &mut self,
        device: u64,
        descriptor: t::SamplerDescriptor,
    ) -> wasmtime::Result<GpuResult<u64>> {
        let gpu = self.gpu.get_mut();
        let d = match gpu.device(device) {
            Ok(d) => d,
            Err(e) => return Ok(Err(e)),
        };
        let made = on_device!(
            &d.device,
            d.device.create_sampler(&wgpu::SamplerDescriptor {
                label: label(&descriptor.label),
                address_mode_u: address_mode(descriptor.address_mode_u),
                address_mode_v: address_mode(descriptor.address_mode_v),
                address_mode_w: address_mode(descriptor.address_mode_w),
                mag_filter: filter_mode(descriptor.mag_filter),
                min_filter: filter_mode(descriptor.min_filter),
                mipmap_filter: mipmap_filter_mode(descriptor.mipmap_filter),
                lod_min_clamp: descriptor.lod_min_clamp,
                lod_max_clamp: descriptor.lod_max_clamp,
                compare: descriptor.compare.map(compare),
                anisotropy_clamp: descriptor.anisotropy_clamp.max(1),
                border_color: descriptor.border_color.map(border_color),
            })
        );
        Ok(made.map(|s| gpu.insert(device, Obj::Sampler(s))))
    }

    fn create_query_set(
        &mut self,
        device: u64,
        descriptor: t::QuerySetDescriptor,
    ) -> wasmtime::Result<GpuResult<u64>> {
        let gpu = self.gpu.get_mut();
        let d = match gpu.device(device) {
            Ok(d) => d,
            Err(e) => return Ok(Err(e)),
        };
        let made = on_device!(
            &d.device,
            d.device.create_query_set(&wgpu::QuerySetDescriptor {
                label: label(&descriptor.label),
                ty: match descriptor.kind {
                    t::QueryType::Occlusion => wgpu::QueryType::Occlusion,
                    t::QueryType::Timestamp => wgpu::QueryType::Timestamp,
                },
                count: descriptor.count,
            })
        );
        Ok(made.map(|q| gpu.insert(device, Obj::QuerySet(q))))
    }

    fn create_bind_group_layout(
        &mut self,
        device: u64,
        descriptor: t::BindGroupLayoutDescriptor,
    ) -> wasmtime::Result<GpuResult<u64>> {
        let gpu = self.gpu.get_mut();
        let d = match gpu.device(device) {
            Ok(d) => d,
            Err(e) => return Ok(Err(e)),
        };
        let mut entries = Vec::with_capacity(descriptor.entries.len());
        for e in &descriptor.entries {
            let ty = match GpuState::binding_type(&e.kind) {
                Ok(ty) => ty,
                Err(err) => return Ok(Err(err)),
            };
            entries.push(wgpu::BindGroupLayoutEntry {
                binding: e.binding,
                visibility: shader_stages(e.visibility),
                ty,
                count: nonzero(e.count),
            });
        }
        let made = on_device!(
            &d.device,
            d.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: label(&descriptor.label),
                    entries: &entries,
                })
        );
        Ok(made.map(|l| gpu.insert(device, Obj::BindGroupLayout(l))))
    }

    fn create_bind_group(
        &mut self,
        device: u64,
        descriptor: t::BindGroupDescriptor,
    ) -> wasmtime::Result<GpuResult<u64>> {
        let gpu = self.gpu.get_mut();
        let made = (|| -> GpuResult<wgpu::BindGroup> {
            let d = gpu.device(device)?;
            let layout = gpu.bind_group_layout(descriptor.layout)?;
            // Arrays borrow slices; build every array first, then the entries.
            let mut buffer_arrays: Vec<Vec<wgpu::BufferBinding<'_>>> = Vec::new();
            let mut sampler_arrays: Vec<Vec<&wgpu::Sampler>> = Vec::new();
            let mut view_arrays: Vec<Vec<&wgpu::TextureView>> = Vec::new();
            for e in &descriptor.entries {
                match &e.resource {
                    t::BindingResource::BufferArray(list) => buffer_arrays.push(
                        list.iter()
                            .map(|b| gpu.buffer_binding(b))
                            .collect::<GpuResult<_>>()?,
                    ),
                    t::BindingResource::SamplerArray(list) => sampler_arrays.push(
                        list.iter()
                            .map(|h| gpu.sampler(*h))
                            .collect::<GpuResult<_>>()?,
                    ),
                    t::BindingResource::TextureViewArray(list) => view_arrays.push(
                        list.iter()
                            .map(|h| gpu.view(*h))
                            .collect::<GpuResult<_>>()?,
                    ),
                    _ => {}
                }
            }
            let (mut bi, mut si, mut vi) = (0, 0, 0);
            let mut entries = Vec::with_capacity(descriptor.entries.len());
            for e in &descriptor.entries {
                let resource = match &e.resource {
                    t::BindingResource::Buffer(b) => {
                        wgpu::BindingResource::Buffer(gpu.buffer_binding(b)?)
                    }
                    t::BindingResource::Sampler(h) => {
                        wgpu::BindingResource::Sampler(gpu.sampler(*h)?)
                    }
                    t::BindingResource::TextureView(h) => {
                        wgpu::BindingResource::TextureView(gpu.view(*h)?)
                    }
                    t::BindingResource::BufferArray(_) => {
                        bi += 1;
                        wgpu::BindingResource::BufferArray(&buffer_arrays[bi - 1])
                    }
                    t::BindingResource::SamplerArray(_) => {
                        si += 1;
                        wgpu::BindingResource::SamplerArray(&sampler_arrays[si - 1])
                    }
                    t::BindingResource::TextureViewArray(_) => {
                        vi += 1;
                        wgpu::BindingResource::TextureViewArray(&view_arrays[vi - 1])
                    }
                };
                entries.push(wgpu::BindGroupEntry {
                    binding: e.binding,
                    resource,
                });
            }
            on_device!(
                &d.device,
                d.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: label(&descriptor.label),
                    layout,
                    entries: &entries,
                })
            )
        })();
        Ok(made.map(|b| gpu.insert(device, Obj::BindGroup(b))))
    }

    fn create_pipeline_layout(
        &mut self,
        device: u64,
        descriptor: t::PipelineLayoutDescriptor,
    ) -> wasmtime::Result<GpuResult<u64>> {
        let gpu = self.gpu.get_mut();
        if descriptor.bind_group_layouts.len() > MAX_BIND_GROUPS {
            return Ok(Err(validation(format!(
                "{} bind group layouts; a pipeline may have at most {MAX_BIND_GROUPS}",
                descriptor.bind_group_layouts.len()
            ))));
        }
        let made = (|| -> GpuResult<wgpu::PipelineLayout> {
            let d = gpu.device(device)?;
            let layouts = descriptor
                .bind_group_layouts
                .iter()
                .map(|h| h.map(|h| gpu.bind_group_layout(h)).transpose())
                .collect::<GpuResult<Vec<_>>>()?;
            on_device!(
                &d.device,
                d.device
                    .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: label(&descriptor.label),
                        bind_group_layouts: &layouts,
                        immediate_size: descriptor.immediate_size,
                    })
            )
        })();
        Ok(made.map(|l| gpu.insert(device, Obj::PipelineLayout(l))))
    }

    fn create_shader_module(
        &mut self,
        device: u64,
        descriptor: t::ShaderModuleDescriptor,
    ) -> wasmtime::Result<Result<u64, t::ShaderError>> {
        let gpu = self.gpu.get_mut();
        let d = match gpu.device(device) {
            Ok(d) => d,
            Err(e) => {
                return Ok(Err(t::ShaderError {
                    message: gpu_error_text(&e),
                    line: 0,
                    column: 0,
                    offset: 0,
                    length: 0,
                }))
            }
        };
        // wgpu parses and validates the WGSL with naga inside this call; a
        // shader that fails never reaches the driver.
        let (module, error) = scoped(&d.device, || {
            d.device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: label(&descriptor.label),
                source: wgpu::ShaderSource::Wgsl(descriptor.code.as_str().into()),
            })
        });
        let info = pollster::block_on(module.get_compilation_info());
        let messages: Vec<t::CompilationMessage> = info
            .messages
            .iter()
            .map(|m| {
                let loc = m.location.unwrap_or(wgpu::SourceLocation {
                    line_number: 0,
                    line_position: 0,
                    offset: 0,
                    length: 0,
                });
                t::CompilationMessage {
                    message: m.message.trim().to_string(),
                    kind: match m.message_type {
                        wgpu::CompilationMessageType::Error => "error",
                        wgpu::CompilationMessageType::Warning => "warning",
                        wgpu::CompilationMessageType::Info => "info",
                    }
                    .to_string(),
                    line: loc.line_number,
                    column: loc.line_position,
                    offset: loc.offset,
                    length: loc.length,
                }
            })
            .collect();
        if let Some(error) = error {
            let first = messages.iter().find(|m| m.kind == "error");
            return Ok(Err(t::ShaderError {
                message: first
                    .map(|m| m.message.clone())
                    .unwrap_or_else(|| gpu_error_text(&error)),
                line: first.map(|m| m.line).unwrap_or(0),
                column: first.map(|m| m.column).unwrap_or(0),
                offset: first.map(|m| m.offset).unwrap_or(0),
                length: first.map(|m| m.length).unwrap_or(0),
            }));
        }
        Ok(Ok(gpu.insert(device, Obj::Shader(module, messages))))
    }

    fn shader_compilation_info(
        &mut self,
        module: u64,
    ) -> wasmtime::Result<GpuResult<Vec<t::CompilationMessage>>> {
        let gpu = self.gpu.get_mut();
        Ok(match gpu.objects.get(&module) {
            Some(Entry {
                obj: Obj::Shader(_, messages),
                ..
            }) => Ok(messages.clone()),
            _ => Err(invalid(module, "shader module")),
        })
    }

    fn create_render_pipeline(
        &mut self,
        device: u64,
        descriptor: t::RenderPipelineDescriptor,
    ) -> wasmtime::Result<GpuResult<u64>> {
        let gpu = self.gpu.get_mut();
        let made = (|| -> GpuResult<wgpu::RenderPipeline> {
            let d = gpu.device(device)?;
            let layout = descriptor
                .layout
                .map(|h| gpu.pipeline_layout(h))
                .transpose()?;
            let vconstants: Vec<(&str, f64)> = descriptor
                .vertex
                .stage
                .constants
                .iter()
                .map(|c| (c.name.as_str(), c.value))
                .collect();
            let (vmodule, ventry, voptions) =
                gpu.programmable(&descriptor.vertex.stage, &vconstants)?;
            let attributes: Vec<Vec<wgpu::VertexAttribute>> = descriptor
                .vertex
                .buffers
                .iter()
                .map(|b| {
                    b.attributes
                        .iter()
                        .map(|a| {
                            Ok(wgpu::VertexAttribute {
                                format: names::parse_vertex_format(&a.format).ok_or_else(|| {
                                    t::GpuError::Unsupported(format!(
                                        "vertex format {:?}",
                                        a.format
                                    ))
                                })?,
                                offset: a.offset,
                                shader_location: a.shader_location,
                            })
                        })
                        .collect::<GpuResult<Vec<_>>>()
                })
                .collect::<GpuResult<_>>()?;
            let buffers: Vec<wgpu::VertexBufferLayout> = descriptor
                .vertex
                .buffers
                .iter()
                .zip(&attributes)
                .map(|(b, attributes)| wgpu::VertexBufferLayout {
                    array_stride: b.array_stride,
                    step_mode: match b.step_mode {
                        t::VertexStepMode::Vertex => wgpu::VertexStepMode::Vertex,
                        t::VertexStepMode::Instance => wgpu::VertexStepMode::Instance,
                    },
                    attributes,
                })
                .collect();
            let p = &descriptor.primitive;
            let primitive = wgpu::PrimitiveState {
                topology: match p.topology {
                    t::PrimitiveTopology::PointList => wgpu::PrimitiveTopology::PointList,
                    t::PrimitiveTopology::LineList => wgpu::PrimitiveTopology::LineList,
                    t::PrimitiveTopology::LineStrip => wgpu::PrimitiveTopology::LineStrip,
                    t::PrimitiveTopology::TriangleList => wgpu::PrimitiveTopology::TriangleList,
                    t::PrimitiveTopology::TriangleStrip => wgpu::PrimitiveTopology::TriangleStrip,
                },
                strip_index_format: p.strip_index_format.map(index_format),
                front_face: match p.front_face {
                    t::FrontFace::Ccw => wgpu::FrontFace::Ccw,
                    t::FrontFace::Cw => wgpu::FrontFace::Cw,
                },
                cull_mode: match p.cull_mode {
                    t::CullMode::None => None,
                    t::CullMode::Front => Some(wgpu::Face::Front),
                    t::CullMode::Back => Some(wgpu::Face::Back),
                },
                unclipped_depth: p.unclipped_depth,
                polygon_mode: match p.polygon_mode {
                    t::PolygonMode::Fill => wgpu::PolygonMode::Fill,
                    t::PolygonMode::Line => wgpu::PolygonMode::Line,
                    t::PolygonMode::Point => wgpu::PolygonMode::Point,
                },
                conservative: p.conservative,
            };
            let depth_stencil = match &descriptor.depth_stencil {
                Some(ds) => Some(wgpu::DepthStencilState {
                    format: format(&ds.format)?,
                    depth_write_enabled: Some(ds.depth_write_enabled),
                    depth_compare: Some(compare(ds.depth_compare)),
                    stencil: wgpu::StencilState {
                        front: stencil_face(&ds.stencil_front),
                        back: stencil_face(&ds.stencil_back),
                        read_mask: ds.stencil_read_mask,
                        write_mask: ds.stencil_write_mask,
                    },
                    bias: wgpu::DepthBiasState {
                        constant: ds.depth_bias,
                        slope_scale: ds.depth_bias_slope_scale,
                        clamp: ds.depth_bias_clamp,
                    },
                }),
                None => None,
            };
            let fconstants: Vec<(&str, f64)> = descriptor
                .fragment
                .as_ref()
                .map(|f| {
                    f.stage
                        .constants
                        .iter()
                        .map(|c| (c.name.as_str(), c.value))
                        .collect()
                })
                .unwrap_or_default();
            let targets: Vec<Option<wgpu::ColorTargetState>> = match &descriptor.fragment {
                Some(f) => f
                    .targets
                    .iter()
                    .map(|target| {
                        target
                            .as_ref()
                            .map(|c| {
                                Ok(wgpu::ColorTargetState {
                                    format: format(&c.format)?,
                                    blend: c.blend.as_ref().map(|b| wgpu::BlendState {
                                        color: blend_component(&b.color),
                                        alpha: blend_component(&b.alpha),
                                    }),
                                    write_mask: color_writes(c.write_mask),
                                })
                            })
                            .transpose()
                    })
                    .collect::<GpuResult<_>>()?,
                None => Vec::new(),
            };
            let fragment = match &descriptor.fragment {
                Some(f) => {
                    let (module, entry_point, compilation_options) =
                        gpu.programmable(&f.stage, &fconstants)?;
                    Some(wgpu::FragmentState {
                        module,
                        entry_point,
                        compilation_options,
                        targets: &targets,
                    })
                }
                None => None,
            };
            on_device!(
                &d.device,
                d.device
                    .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                        label: label(&descriptor.label),
                        layout,
                        vertex: wgpu::VertexState {
                            module: vmodule,
                            entry_point: ventry,
                            compilation_options: voptions,
                            buffers: &buffers,
                        },
                        primitive,
                        depth_stencil,
                        multisample: wgpu::MultisampleState {
                            count: descriptor.multisample.count,
                            mask: descriptor.multisample.mask,
                            alpha_to_coverage_enabled: descriptor
                                .multisample
                                .alpha_to_coverage_enabled,
                        },
                        fragment,
                        multiview_mask: nonzero(descriptor.multiview_mask),
                        cache: None,
                    })
            )
        })();
        Ok(made.map(|p| gpu.insert(device, Obj::RenderPipeline(p))))
    }

    fn create_compute_pipeline(
        &mut self,
        device: u64,
        descriptor: t::ComputePipelineDescriptor,
    ) -> wasmtime::Result<GpuResult<u64>> {
        let gpu = self.gpu.get_mut();
        let made = (|| -> GpuResult<wgpu::ComputePipeline> {
            let d = gpu.device(device)?;
            let layout = descriptor
                .layout
                .map(|h| gpu.pipeline_layout(h))
                .transpose()?;
            let constants: Vec<(&str, f64)> = descriptor
                .stage
                .constants
                .iter()
                .map(|c| (c.name.as_str(), c.value))
                .collect();
            let (module, entry_point, compilation_options) =
                gpu.programmable(&descriptor.stage, &constants)?;
            on_device!(
                &d.device,
                d.device
                    .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                        label: label(&descriptor.label),
                        layout,
                        module,
                        entry_point,
                        compilation_options,
                        cache: None,
                    })
            )
        })();
        Ok(made.map(|p| gpu.insert(device, Obj::ComputePipeline(p))))
    }

    fn pipeline_bind_group_layout(
        &mut self,
        pipeline: u64,
        index: u32,
    ) -> wasmtime::Result<GpuResult<u64>> {
        let gpu = self.gpu.get_mut();
        let made = (|| -> GpuResult<(u64, wgpu::BindGroupLayout)> {
            let device = gpu.get(pipeline)?.device;
            let d = gpu.device(device)?;
            if index as usize >= MAX_BIND_GROUPS {
                return Err(validation(format!(
                    "bind group index {index} is out of range"
                )));
            }
            let layout = match &gpu.get(pipeline)?.obj {
                Obj::RenderPipeline(p) => on_device!(&d.device, p.get_bind_group_layout(index))?,
                Obj::ComputePipeline(p) => on_device!(&d.device, p.get_bind_group_layout(index))?,
                other => {
                    return Err(t::GpuError::InvalidHandle(format!(
                        "{pipeline} is a {}, not a pipeline",
                        other.kind()
                    )))
                }
            };
            Ok((device, layout))
        })();
        Ok(made.map(|(device, l)| gpu.insert(device, Obj::BindGroupLayout(l))))
    }

    fn create_command_encoder(
        &mut self,
        device: u64,
        label_text: Option<String>,
    ) -> wasmtime::Result<GpuResult<u64>> {
        let gpu = self.gpu.get_mut();
        let made = (|| {
            let d = gpu.device(device)?;
            on_device!(
                &d.device,
                d.device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: label(&label_text),
                    })
            )
        })();
        Ok(made.map(|e| gpu.insert(device, Obj::Encoder(Some(e)))))
    }

    fn encode_render_pass(
        &mut self,
        encoder: u64,
        descriptor: t::RenderPassDescriptor,
        commands: Vec<t::RenderCommand>,
    ) -> wasmtime::Result<GpuResult<()>> {
        let gpu = self.gpu.get_mut();
        Ok(gpu.with_encoder(encoder, |gpu, d, enc| {
            gpu.record_render_pass(d, enc, &descriptor, &commands)
        }))
    }

    fn encode_compute_pass(
        &mut self,
        encoder: u64,
        descriptor: t::ComputePassDescriptor,
        commands: Vec<t::ComputeCommand>,
    ) -> wasmtime::Result<GpuResult<()>> {
        let gpu = self.gpu.get_mut();
        Ok(gpu.with_encoder(encoder, |gpu, d, enc| {
            gpu.record_compute_pass(d, enc, &descriptor, &commands)
        }))
    }

    fn copy_buffer_to_buffer(
        &mut self,
        encoder: u64,
        source: u64,
        source_offset: u64,
        destination: u64,
        destination_offset: u64,
        size: Option<u64>,
    ) -> wasmtime::Result<GpuResult<()>> {
        let gpu = self.gpu.get_mut();
        Ok(gpu.with_encoder(encoder, |gpu, d, enc| {
            let src = &gpu.buffer(source)?.buffer;
            let dst = &gpu.buffer(destination)?.buffer;
            on_device!(
                d,
                enc.copy_buffer_to_buffer(src, source_offset, dst, destination_offset, size)
            )
        }))
    }

    fn copy_buffer_to_texture(
        &mut self,
        encoder: u64,
        source: t::TexelCopyBufferInfo,
        destination: t::TexelCopyTextureInfo,
        size: t::Extent3d,
    ) -> wasmtime::Result<GpuResult<()>> {
        let gpu = self.gpu.get_mut();
        Ok(gpu.with_encoder(encoder, |gpu, d, enc| {
            let src = gpu.buffer_copy(&source)?;
            let dst = gpu.texture_copy(&destination)?;
            on_device!(d, enc.copy_buffer_to_texture(src, dst, extent(&size)))
        }))
    }

    fn copy_texture_to_buffer(
        &mut self,
        encoder: u64,
        source: t::TexelCopyTextureInfo,
        destination: t::TexelCopyBufferInfo,
        size: t::Extent3d,
    ) -> wasmtime::Result<GpuResult<()>> {
        let gpu = self.gpu.get_mut();
        Ok(gpu.with_encoder(encoder, |gpu, d, enc| {
            let src = gpu.texture_copy(&source)?;
            let dst = gpu.buffer_copy(&destination)?;
            on_device!(d, enc.copy_texture_to_buffer(src, dst, extent(&size)))
        }))
    }

    fn copy_texture_to_texture(
        &mut self,
        encoder: u64,
        source: t::TexelCopyTextureInfo,
        destination: t::TexelCopyTextureInfo,
        size: t::Extent3d,
    ) -> wasmtime::Result<GpuResult<()>> {
        let gpu = self.gpu.get_mut();
        Ok(gpu.with_encoder(encoder, |gpu, d, enc| {
            let src = gpu.texture_copy(&source)?;
            let dst = gpu.texture_copy(&destination)?;
            on_device!(d, enc.copy_texture_to_texture(src, dst, extent(&size)))
        }))
    }

    fn clear_buffer(
        &mut self,
        encoder: u64,
        buffer: u64,
        offset: u64,
        size: Option<u64>,
    ) -> wasmtime::Result<GpuResult<()>> {
        let gpu = self.gpu.get_mut();
        Ok(gpu.with_encoder(encoder, |gpu, d, enc| {
            let b = &gpu.buffer(buffer)?.buffer;
            on_device!(d, enc.clear_buffer(b, offset, size))
        }))
    }

    fn resolve_query_set(
        &mut self,
        encoder: u64,
        query_set: u64,
        first_query: u32,
        query_count: u32,
        destination: u64,
        destination_offset: u64,
    ) -> wasmtime::Result<GpuResult<()>> {
        let gpu = self.gpu.get_mut();
        Ok(gpu.with_encoder(encoder, |gpu, d, enc| {
            let q = gpu.query_set(query_set)?;
            let b = &gpu.buffer(destination)?.buffer;
            let end = first_query
                .checked_add(query_count)
                .ok_or_else(|| validation("query range overflows"))?;
            on_device!(
                d,
                enc.resolve_query_set(q, first_query..end, b, destination_offset)
            )
        }))
    }

    fn write_timestamp(
        &mut self,
        encoder: u64,
        query_set: u64,
        index: u32,
    ) -> wasmtime::Result<GpuResult<()>> {
        let gpu = self.gpu.get_mut();
        Ok(gpu.with_encoder(encoder, |gpu, d, enc| {
            let q = gpu.query_set(query_set)?;
            on_device!(d, enc.write_timestamp(q, index))
        }))
    }

    fn finish_encoder(
        &mut self,
        encoder: u64,
        _label: Option<String>,
    ) -> wasmtime::Result<GpuResult<u64>> {
        let gpu = self.gpu.get_mut();
        let device = match gpu.get(encoder) {
            Ok(e) => e.device,
            Err(e) => return Ok(Err(e)),
        };
        let d = match gpu.device(device) {
            Ok(d) => d.device.clone(),
            Err(e) => return Ok(Err(e)),
        };
        let enc = match gpu.objects.get_mut(&encoder) {
            Some(Entry {
                obj: Obj::Encoder(e),
                ..
            }) => e.take(),
            _ => return Ok(Err(invalid(encoder, "command encoder"))),
        };
        let Some(enc) = enc else {
            return Ok(Err(validation("the encoder is already finished")));
        };
        gpu.objects.remove(&encoder);
        let made = on_device!(&d, enc.finish());
        Ok(made.map(|cb| gpu.insert(device, Obj::CommandBuffer(Some(cb)))))
    }

    fn submit(
        &mut self,
        device: u64,
        command_buffers: Vec<u64>,
    ) -> wasmtime::Result<GpuResult<u64>> {
        let gpu = self.gpu.get_mut();
        let mut buffers = Vec::with_capacity(command_buffers.len());
        for h in &command_buffers {
            match gpu.objects.get_mut(h) {
                Some(Entry {
                    obj: Obj::CommandBuffer(cb),
                    device: owner,
                }) if *owner == device => match cb.take() {
                    Some(cb) => buffers.push(cb),
                    None => return Ok(Err(validation("a command buffer was submitted twice"))),
                },
                _ => return Ok(Err(invalid(*h, "command buffer of this device"))),
            }
        }
        for h in &command_buffers {
            gpu.objects.remove(h);
        }
        let d = match gpu.device(device) {
            Ok(d) => d,
            Err(e) => return Ok(Err(e)),
        };
        let made = on_device!(&d.device, d.queue.submit(buffers));
        Ok(made.map(|index| {
            // A submission index is opaque in wgpu; hand out a counter.
            let _ = index;
            gpu.next
        }))
    }

    fn write_buffer(
        &mut self,
        device: u64,
        buffer: u64,
        offset: u64,
        data: Vec<u8>,
    ) -> wasmtime::Result<GpuResult<()>> {
        let gpu = self.gpu.get_mut();
        Ok((|| {
            let d = gpu.device(device)?;
            let b = gpu.buffer(buffer)?;
            if offset
                .checked_add(data.len() as u64)
                .is_none_or(|end| end > b.size)
            {
                return Err(validation(format!(
                    "writing {} bytes at {offset} overruns a {}-byte buffer",
                    data.len(),
                    b.size
                )));
            }
            on_device!(&d.device, d.queue.write_buffer(&b.buffer, offset, &data))
        })())
    }

    fn write_texture(
        &mut self,
        device: u64,
        destination: t::TexelCopyTextureInfo,
        data: Vec<u8>,
        layout: t::TexelCopyBufferLayout,
        size: t::Extent3d,
    ) -> wasmtime::Result<GpuResult<()>> {
        let gpu = self.gpu.get_mut();
        Ok((|| {
            let d = gpu.device(device)?;
            let dst = gpu.texture_copy(&destination)?;
            on_device!(
                &d.device,
                d.queue
                    .write_texture(dst, &data, copy_layout(&layout), extent(&size))
            )
        })())
    }

    fn timestamp_period(&mut self, device: u64) -> wasmtime::Result<GpuResult<f32>> {
        let gpu = self.gpu.get_mut();
        Ok(gpu.device(device).map(|d| d.queue.get_timestamp_period()))
    }

    fn create_surface(&mut self, window: u64, widget: u64) -> wasmtime::Result<GpuResult<u64>> {
        let Ok(window_id) = self.window_id(window) else {
            return Ok(Err(invalid(window, "window of this app")));
        };
        let Ok(widget_id) = WidgetId::new(widget) else {
            return Ok(Err(invalid(widget, "widget")));
        };
        if let Err(error) = self.canvas_widget_rect(window_id, widget_id) {
            return Ok(Err(t::GpuError::InvalidHandle(format!(
                "widget {widget} is not a canvas in window {window}: {error:?}"
            ))));
        }
        let gpu = self.gpu.get_mut();
        Ok(Ok(gpu.insert(
            0,
            Obj::Surface(Box::new(SurfaceEntry {
                window: window_id,
                widget: widget_id,
                config: None,
                texture: None,
                frame: None,
                readback: None,
            })),
        )))
    }

    fn surface_pixel_size(&mut self, surface: u64) -> wasmtime::Result<GpuResult<(u32, u32)>> {
        let (window, widget) = match self.gpu.get_mut().objects.get(&surface) {
            Some(Entry {
                obj: Obj::Surface(s),
                ..
            }) => (s.window, s.widget),
            _ => return Ok(Err(invalid(surface, "surface"))),
        };
        let rect = match self.canvas_widget_rect(window, widget) {
            Ok(rect) => rect,
            Err(error) => {
                return Ok(Err(t::GpuError::InvalidHandle(format!(
                    "the surface's canvas is gone: {error:?}"
                ))))
            }
        };
        let scale = self.canvas_backing_scale(window);
        Ok(Ok((
            ((rect.0 * scale).round() as u32).max(1),
            ((rect.1 * scale).round() as u32).max(1),
        )))
    }

    fn get_surface_capabilities(
        &mut self,
        surface: u64,
        adapter: u64,
    ) -> wasmtime::Result<GpuResult<t::SurfaceCapabilities>> {
        let gpu = self.gpu.get_mut();
        if let Err(e) = gpu.adapter(adapter) {
            return Ok(Err(e));
        }
        match gpu.objects.get(&surface) {
            Some(Entry {
                obj: Obj::Surface(_),
                ..
            }) => {}
            _ => return Ok(Err(invalid(surface, "surface"))),
        }
        // The frame is copied into the canvas as RGBA; these are the formats
        // that copy can read, best first (an engine takes the first sRGB one).
        Ok(Ok(t::SurfaceCapabilities {
            formats: SURFACE_FORMATS
                .iter()
                .map(|f| names::format_name(*f))
                .collect(),
            present_modes: [wgpu::PresentMode::Fifo, wgpu::PresentMode::Immediate]
                .into_iter()
                .filter_map(present_mode_out)
                .collect(),
            alpha_modes: vec![t::CompositeAlphaMode::Opaque],
            usages: texture_usages_out(
                wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::TEXTURE_BINDING,
            ),
        }))
    }

    fn configure_surface(
        &mut self,
        surface: u64,
        device: u64,
        configuration: t::SurfaceConfiguration,
    ) -> wasmtime::Result<GpuResult<()>> {
        let gpu = self.gpu.get_mut();
        let d = match gpu.device(device) {
            Ok(d) => d.device.clone(),
            Err(e) => return Ok(Err(e)),
        };
        let fmt = match format(&configuration.format) {
            Ok(f) if SURFACE_FORMATS.contains(&f) => f,
            Ok(f) => {
                return Ok(Err(t::GpuError::Unsupported(format!(
                    "a surface of format {}",
                    names::format_name(f)
                ))))
            }
            Err(e) => return Ok(Err(e)),
        };
        let view_formats = match formats(&configuration.view_formats) {
            Ok(v) => v,
            Err(e) => return Ok(Err(e)),
        };
        let max = d.limits().max_texture_dimension_2d;
        if configuration.width == 0
            || configuration.height == 0
            || configuration.width > max
            || configuration.height > max
        {
            return Ok(Err(validation(format!(
                "a surface of {}x{}; each side must be 1 to {max}",
                configuration.width, configuration.height
            ))));
        }
        let usage = texture_usages(configuration.usage) | wgpu::TextureUsages::COPY_SRC;
        let made = on_device!(
            &d,
            d.create_texture(&wgpu::TextureDescriptor {
                label: Some("krate surface"),
                size: wgpu::Extent3d {
                    width: configuration.width,
                    height: configuration.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: fmt,
                usage,
                view_formats: &view_formats,
            })
        );
        let texture = match made {
            Ok(t) => t,
            Err(e) => return Ok(Err(e)),
        };
        let Some(Entry {
            obj: Obj::Surface(s),
            ..
        }) = gpu.objects.get_mut(&surface)
        else {
            return Ok(Err(invalid(surface, "surface")));
        };
        s.config = Some(SurfaceConfig {
            device,
            format: fmt,
            usage,
            width: configuration.width,
            height: configuration.height,
            view_formats,
        });
        s.texture = Some(texture);
        s.readback = None;
        Ok(Ok(()))
    }

    fn current_texture(
        &mut self,
        surface: u64,
    ) -> wasmtime::Result<Result<t::SurfaceFrame, t::SurfaceError>> {
        let (window, widget, config, texture, previous) =
            match self.gpu.get_mut().objects.get(&surface) {
                Some(Entry {
                    obj: Obj::Surface(s),
                    ..
                }) => match (&s.config, &s.texture) {
                    (Some(c), Some(t)) => (s.window, s.widget, c.clone(), t.clone(), s.frame),
                    _ => return Ok(Err(t::SurfaceError::NotConfigured)),
                },
                _ => {
                    return Ok(Err(t::SurfaceError::Other(format!(
                        "{surface} is not a live surface"
                    ))))
                }
            };
        if let Some(reason) = self
            .gpu
            .get_mut()
            .device(config.device)
            .err()
            .and_then(|e| match e {
                t::GpuError::Lost(reason) => Some(reason),
                _ => None,
            })
        {
            return Ok(Err(t::SurfaceError::Lost(reason)));
        }
        // Suboptimal once the canvas is shown at another size: the app
        // should configure again to stay sharp.
        let suboptimal = match self.canvas_widget_rect(window, widget) {
            Ok(rect) => {
                let scale = self.canvas_backing_scale(window);
                let want = (
                    ((rect.0 * scale).round() as u32).max(1),
                    ((rect.1 * scale).round() as u32).max(1),
                );
                want != (config.width, config.height)
            }
            Err(_) => return Ok(Err(t::SurfaceError::Lost("the canvas is gone".to_string()))),
        };
        let gpu = self.gpu.get_mut();
        if let Some(previous) = previous {
            gpu.objects.remove(&previous);
        }
        let handle = gpu.insert(
            config.device,
            Obj::Texture(TextureEntry {
                texture,
                surface_owned: true,
            }),
        );
        if let Some(Entry {
            obj: Obj::Surface(s),
            ..
        }) = gpu.objects.get_mut(&surface)
        {
            s.frame = Some(handle);
        }
        Ok(Ok(t::SurfaceFrame {
            texture: handle,
            suboptimal,
        }))
    }

    fn present(&mut self, surface: u64) -> wasmtime::Result<GpuResult<()>> {
        // A GPU game loop lives in present the way a canvas one does; the
        // second unanswered close click ends the app here too (K-121).
        if self.close_ignored_by_guest() {
            std::process::exit(0);
        }
        // KRATE_GPU_UNPACED=1 measures what a frame costs rather than what the
        // display allows: present stops waiting for the 60 Hz budget.
        if std::env::var_os("KRATE_GPU_UNPACED").is_none() {
            self.pace_frame();
        }
        let (window, widget, image) = {
            let gpu = self.gpu.get_mut();
            match gpu.read_frame(surface) {
                Ok(found) => found,
                Err(e) => return Ok(Err(e)),
            }
        };
        self.images
            .borrow_mut()
            .insert((window, widget), Arc::new(image));
        let result = self
            .sync_native_widgets(window)
            .map_err(|error| t::GpuError::Internal(error.to_string()));
        if result.is_ok() {
            self.maybe_take_screenshot();
        }
        self.frame_ended.set(true);
        self.end_stepped_frame();
        Ok(result)
    }

    fn release(&mut self, object: u64) -> wasmtime::Result<()> {
        let gpu = self.gpu.get_mut();
        gpu.objects.remove(&object);
        Ok(())
    }
}

/// Formats a surface may be configured with: the ones whose bytes the
/// present path can hand the canvas as RGBA.
const SURFACE_FORMATS: [wgpu::TextureFormat; 4] = [
    wgpu::TextureFormat::Rgba8UnormSrgb,
    wgpu::TextureFormat::Bgra8UnormSrgb,
    wgpu::TextureFormat::Rgba8Unorm,
    wgpu::TextureFormat::Bgra8Unorm,
];

fn gpu_error_text(error: &t::GpuError) -> String {
    match error {
        t::GpuError::InvalidHandle(s)
        | t::GpuError::Validation(s)
        | t::GpuError::Lost(s)
        | t::GpuError::Unsupported(s)
        | t::GpuError::Internal(s) => s.clone(),
        t::GpuError::OutOfMemory => "out of memory".to_string(),
    }
}

impl GpuState {
    /// Run `f` with an encoder taken out of its slot and put back after,
    /// so the encoder can be mutably borrowed while every other object is
    /// read.
    fn with_encoder(
        &mut self,
        encoder: u64,
        f: impl FnOnce(&GpuState, &wgpu::Device, &mut wgpu::CommandEncoder) -> GpuResult<()>,
    ) -> GpuResult<()> {
        let device = self.get(encoder)?.device;
        let d = self.device(device)?.device.clone();
        let mut enc = match self.objects.get_mut(&encoder) {
            Some(Entry {
                obj: Obj::Encoder(e),
                ..
            }) => e
                .take()
                .ok_or_else(|| validation("the encoder is already finished"))?,
            _ => return Err(invalid(encoder, "command encoder")),
        };
        let result = f(self, &d, &mut enc);
        if let Some(Entry {
            obj: Obj::Encoder(slot),
            ..
        }) = self.objects.get_mut(&encoder)
        {
            *slot = Some(enc);
        }
        result
    }

    fn buffer_copy(
        &self,
        info: &t::TexelCopyBufferInfo,
    ) -> GpuResult<wgpu::TexelCopyBufferInfo<'_>> {
        Ok(wgpu::TexelCopyBufferInfo {
            buffer: &self.buffer(info.buffer)?.buffer,
            layout: copy_layout(&info.layout),
        })
    }

    fn texture_copy(
        &self,
        info: &t::TexelCopyTextureInfo,
    ) -> GpuResult<wgpu::TexelCopyTextureInfo<'_>> {
        Ok(wgpu::TexelCopyTextureInfo {
            texture: self.texture(info.texture)?,
            mip_level: info.mip_level,
            origin: origin(&info.origin),
            aspect: aspect(info.aspect),
        })
    }

    fn record_render_pass(
        &self,
        d: &wgpu::Device,
        enc: &mut wgpu::CommandEncoder,
        desc: &t::RenderPassDescriptor,
        commands: &[t::RenderCommand],
    ) -> GpuResult<()> {
        let mut colors = Vec::with_capacity(desc.color_attachments.len());
        for a in &desc.color_attachments {
            colors.push(match a {
                Some(a) => Some(wgpu::RenderPassColorAttachment {
                    view: self.view(a.view)?,
                    depth_slice: a.depth_slice,
                    resolve_target: a.resolve_target.map(|h| self.view(h)).transpose()?,
                    ops: wgpu::Operations {
                        load: match &a.load {
                            t::ColorLoadOp::Clear(c) => wgpu::LoadOp::Clear(wgpu::Color {
                                r: c.r,
                                g: c.g,
                                b: c.b,
                                a: c.a,
                            }),
                            t::ColorLoadOp::Load => wgpu::LoadOp::Load,
                        },
                        store: store_op(a.store),
                    },
                }),
                None => None,
            });
        }
        let depth = match &desc.depth_stencil_attachment {
            Some(ds) => Some(wgpu::RenderPassDepthStencilAttachment {
                view: self.view(ds.view)?,
                depth_ops: ds.depth_ops.as_ref().map(|o| wgpu::Operations {
                    load: match o.load {
                        t::DepthLoadOp::Clear(v) => wgpu::LoadOp::Clear(v),
                        t::DepthLoadOp::Load => wgpu::LoadOp::Load,
                    },
                    store: store_op(o.store),
                }),
                stencil_ops: ds.stencil_ops.as_ref().map(|o| wgpu::Operations {
                    load: match o.load {
                        t::StencilLoadOp::Clear(v) => wgpu::LoadOp::Clear(v),
                        t::StencilLoadOp::Load => wgpu::LoadOp::Load,
                    },
                    store: store_op(o.store),
                }),
            }),
            None => None,
        };
        let timestamps = match &desc.timestamp_writes {
            Some(w) => Some(wgpu::RenderPassTimestampWrites {
                query_set: self.query_set(w.query_set)?,
                beginning_of_pass_write_index: w.beginning_of_pass_write_index,
                end_of_pass_write_index: w.end_of_pass_write_index,
            }),
            None => None,
        };
        let occlusion = desc
            .occlusion_query_set
            .map(|h| self.query_set(h))
            .transpose()?;
        // Resolve every handle the commands name before the pass opens, so
        // a bad handle refuses the pass whole rather than half-recorded.
        for c in commands {
            self.check_render_command(c)?;
        }
        let (result, error) = scoped(d, || -> GpuResult<()> {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: label(&desc.label),
                color_attachments: &colors,
                depth_stencil_attachment: depth,
                timestamp_writes: timestamps,
                occlusion_query_set: occlusion,
                multiview_mask: nonzero(desc.multiview_mask),
            });
            for c in commands {
                self.replay_render(&mut pass, c)?;
            }
            Ok(())
        });
        result?;
        match error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn check_render_command(&self, c: &t::RenderCommand) -> GpuResult<()> {
        use t::RenderCommand as C;
        match c {
            C::SetPipeline(h) => self.render_pipeline(*h).map(|_| ()),
            C::SetBindGroup(s) => s.group.map(|h| self.bind_group(h)).transpose().map(|_| ()),
            C::SetVertexBuffer(v) => self.slice(v.buffer, v.offset, v.size).map(|_| ()),
            C::SetIndexBuffer(i) => self.slice(i.buffer, i.offset, i.size).map(|_| ()),
            C::DrawIndirect(i) | C::DrawIndexedIndirect(i) => self.buffer(i.buffer).map(|_| ()),
            C::MultiDrawIndirect(i) | C::MultiDrawIndexedIndirect(i) => {
                self.buffer(i.buffer).map(|_| ())
            }
            C::MultiDrawIndirectCount(i) | C::MultiDrawIndexedIndirectCount(i) => {
                self.buffer(i.buffer)?;
                self.buffer(i.count_buffer).map(|_| ())
            }
            C::WriteTimestamp(q) => self.query_set(q.query_set).map(|_| ()),
            _ => Ok(()),
        }
    }

    fn replay_render(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        c: &t::RenderCommand,
    ) -> GpuResult<()> {
        use t::RenderCommand as C;
        match c {
            C::SetPipeline(h) => pass.set_pipeline(self.render_pipeline(*h)?),
            C::SetBindGroup(s) => {
                let group = s.group.map(|h| self.bind_group(h)).transpose()?;
                pass.set_bind_group(s.index, group, &s.offsets);
            }
            C::SetVertexBuffer(v) => {
                pass.set_vertex_buffer(v.slot, self.slice(v.buffer, v.offset, v.size)?)
            }
            C::SetIndexBuffer(i) => pass.set_index_buffer(
                self.slice(i.buffer, i.offset, i.size)?,
                index_format(i.format),
            ),
            C::SetViewport(v) => {
                pass.set_viewport(v.x, v.y, v.width, v.height, v.min_depth, v.max_depth)
            }
            C::SetScissorRect(r) => pass.set_scissor_rect(r.x, r.y, r.width, r.height),
            C::SetBlendConstant(c) => pass.set_blend_constant(wgpu::Color {
                r: c.r,
                g: c.g,
                b: c.b,
                a: c.a,
            }),
            C::SetStencilReference(r) => pass.set_stencil_reference(*r),
            C::SetImmediates(i) => pass.set_immediates(i.offset, &i.data),
            C::Draw(dr) => pass.draw(
                dr.first_vertex..dr.first_vertex.saturating_add(dr.vertex_count),
                dr.first_instance..dr.first_instance.saturating_add(dr.instance_count),
            ),
            C::DrawIndexed(dr) => pass.draw_indexed(
                dr.first_index..dr.first_index.saturating_add(dr.index_count),
                dr.base_vertex,
                dr.first_instance..dr.first_instance.saturating_add(dr.instance_count),
            ),
            C::DrawIndirect(i) => pass.draw_indirect(&self.buffer(i.buffer)?.buffer, i.offset),
            C::DrawIndexedIndirect(i) => {
                pass.draw_indexed_indirect(&self.buffer(i.buffer)?.buffer, i.offset)
            }
            C::MultiDrawIndirect(i) => {
                pass.multi_draw_indirect(&self.buffer(i.buffer)?.buffer, i.offset, i.count)
            }
            C::MultiDrawIndexedIndirect(i) => {
                pass.multi_draw_indexed_indirect(&self.buffer(i.buffer)?.buffer, i.offset, i.count)
            }
            C::MultiDrawIndirectCount(i) => pass.multi_draw_indirect_count(
                &self.buffer(i.buffer)?.buffer,
                i.offset,
                &self.buffer(i.count_buffer)?.buffer,
                i.count_offset,
                i.max_count,
            ),
            C::MultiDrawIndexedIndirectCount(i) => pass.multi_draw_indexed_indirect_count(
                &self.buffer(i.buffer)?.buffer,
                i.offset,
                &self.buffer(i.count_buffer)?.buffer,
                i.count_offset,
                i.max_count,
            ),
            C::BeginOcclusionQuery(i) => pass.begin_occlusion_query(*i),
            C::EndOcclusionQuery => pass.end_occlusion_query(),
            C::WriteTimestamp(q) => pass.write_timestamp(self.query_set(q.query_set)?, q.index),
            C::PushDebugGroup(s) => pass.push_debug_group(s),
            C::PopDebugGroup => pass.pop_debug_group(),
            C::InsertDebugMarker(s) => pass.insert_debug_marker(s),
        }
        Ok(())
    }

    fn record_compute_pass(
        &self,
        d: &wgpu::Device,
        enc: &mut wgpu::CommandEncoder,
        desc: &t::ComputePassDescriptor,
        commands: &[t::ComputeCommand],
    ) -> GpuResult<()> {
        use t::ComputeCommand as C;
        let timestamps = match &desc.timestamp_writes {
            Some(w) => Some(wgpu::ComputePassTimestampWrites {
                query_set: self.query_set(w.query_set)?,
                beginning_of_pass_write_index: w.beginning_of_pass_write_index,
                end_of_pass_write_index: w.end_of_pass_write_index,
            }),
            None => None,
        };
        for c in commands {
            match c {
                C::SetPipeline(h) => {
                    self.compute_pipeline(*h)?;
                }
                C::SetBindGroup(s) => {
                    s.group.map(|h| self.bind_group(h)).transpose()?;
                }
                C::DispatchIndirect(i) => {
                    self.buffer(i.buffer)?;
                }
                C::WriteTimestamp(q) => {
                    self.query_set(q.query_set)?;
                }
                _ => {}
            }
        }
        let (result, error) = scoped(d, || -> GpuResult<()> {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: label(&desc.label),
                timestamp_writes: timestamps,
            });
            for c in commands {
                match c {
                    C::SetPipeline(h) => pass.set_pipeline(self.compute_pipeline(*h)?),
                    C::SetBindGroup(s) => {
                        let group = s.group.map(|h| self.bind_group(h)).transpose()?;
                        pass.set_bind_group(s.index, group, &s.offsets);
                    }
                    C::SetImmediates(i) => pass.set_immediates(i.offset, &i.data),
                    C::Dispatch(o) => pass.dispatch_workgroups(o.x, o.y, o.z),
                    C::DispatchIndirect(i) => {
                        pass.dispatch_workgroups_indirect(&self.buffer(i.buffer)?.buffer, i.offset)
                    }
                    C::WriteTimestamp(q) => {
                        pass.write_timestamp(self.query_set(q.query_set)?, q.index)
                    }
                    C::PushDebugGroup(s) => pass.push_debug_group(s),
                    C::PopDebugGroup => pass.pop_debug_group(),
                    C::InsertDebugMarker(s) => pass.insert_debug_marker(s),
                }
            }
            Ok(())
        });
        result?;
        match error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Copy a surface's current frame out as RGBA for the canvas.
    fn read_frame(&mut self, surface: u64) -> GpuResult<(WindowId, WidgetId, ImagePixels)> {
        let (window, widget, config, texture, frame) = match self.objects.get(&surface) {
            Some(Entry {
                obj: Obj::Surface(s),
                ..
            }) => match (&s.config, &s.texture) {
                (Some(c), Some(t)) => (s.window, s.widget, c.clone(), t.clone(), s.frame),
                _ => return Err(validation("the surface is not configured")),
            },
            _ => return Err(invalid(surface, "surface")),
        };
        if frame.is_none() {
            return Err(validation(
                "present needs a frame: call current-texture and draw into it first",
            ));
        }
        let d = self.device(config.device)?;
        let (device, queue) = (d.device.clone(), d.queue.clone());
        let (width, height) = (config.width, config.height);
        let padded = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let size = padded as u64 * height as u64;
        let readback = {
            let Some(Entry {
                obj: Obj::Surface(s),
                ..
            }) = self.objects.get_mut(&surface)
            else {
                return Err(invalid(surface, "surface"));
            };
            match &s.readback {
                Some((buffer, have)) if *have == size => buffer.clone(),
                _ => {
                    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("krate surface readback"),
                        size,
                        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                        mapped_at_creation: false,
                    });
                    s.readback = Some((buffer.clone(), size));
                    buffer
                }
            }
        };
        let (_, error) = scoped(&device, || {
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("krate surface present"),
            });
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(padded),
                        rows_per_image: Some(height),
                    },
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
            queue.submit([encoder.finish()]);
        });
        if let Some(error) = error {
            return Err(error);
        }
        let mapped = Arc::new(Mutex::new(None));
        {
            let mapped = Arc::clone(&mapped);
            readback.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                if let Ok(mut slot) = mapped.lock() {
                    *slot = Some(r);
                }
            });
        }
        let _ = device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_secs(10)),
        });
        let ok = matches!(mapped.lock().ok().and_then(|mut m| m.take()), Some(Ok(())));
        if !ok {
            readback.unmap();
            return Err(t::GpuError::Internal(
                "the frame could not be read back".to_string(),
            ));
        }
        let swap = matches!(
            config.format,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
        );
        let row = (width * 4) as usize;
        let mut rgba = Vec::with_capacity(row * height as usize);
        {
            let view = readback.slice(..).get_mapped_range();
            for y in 0..height as usize {
                let start = y * padded as usize;
                rgba.extend_from_slice(&view[start..start + row]);
            }
        }
        readback.unmap();
        if swap {
            for px in rgba.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
        }
        // The canvas is opaque; a frame drawn with alpha below one must not
        // show the window through it.
        for px in rgba.chunks_exact_mut(4) {
            px[3] = 255;
        }
        let _ = &config.view_formats;
        let _ = config.usage;
        let image = ImagePixels::new(width, height, rgba)
            .map_err(|error| t::GpuError::Internal(error.to_string()))?;
        Ok((window, widget, image))
    }
}

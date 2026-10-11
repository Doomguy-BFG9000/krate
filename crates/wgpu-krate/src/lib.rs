//! wgpu's custom backend over `krate:gpu` (IC-540, IC-916).
//!
//! A program written for wgpu -- or an engine built on it, like Bevy -- makes
//! its `wgpu::Instance` with [`instance`] instead of `wgpu::Instance::new`,
//! and every call it makes after that is answered by the Krate host's GPU
//! through `krate:gpu`. The rendering code does not change.
//!
//! What this side keeps, because WebGPU puts it on the app's side:
//! - error scopes and the uncaptured-error handler. The host returns every
//!   error from the call that caused it; here each one goes to the innermost
//!   scope that catches it, or to the handler, or is printed.
//! - map callbacks and submitted-work callbacks, run on the next `poll` or
//!   `submit` -- never inside the call that queued them, where wgpu still
//!   holds its own locks.
//! - mapped ranges, as a copy that is written back when it is dropped.
//! - render bundles, as recorded commands replayed into the pass.
//! - render and compute passes, recorded and sent whole when they end.
//!
//! Surfaces: [`surface_target`] turns a window and its canvas widget into a
//! target for `Instance::create_surface_unsafe` (or use [`KrateWindow`] with
//! `create_surface`).

use std::borrow::Cow;
use std::fmt;
use std::future::{ready, Future};
use std::ops::Range;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use krate::bindings::krate::gpu::types as t;
use krate::gpu::gpu as g;
#[cfg(not(feature = "wgpu-27"))]
use krate_gpu_names as names;
#[cfg(feature = "wgpu-27")]
use krate_gpu_names::wgpu27 as names;
#[cfg(feature = "wgpu-27")]
extern crate naga27 as naga;
#[cfg(feature = "wgpu-27")]
extern crate wgpu27 as wgpu;

mod lower;

/// SPIR-V words as the bytes naga's reader takes.
#[cfg(feature = "spirv")]
fn bytemuck_words(words: &[u32]) -> &[u8] {
    // SAFETY: u32 has no padding and u8 has alignment 1.
    unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), words.len() * 4) }
}
use wgpu::custom::*;
use wgpu::rwh;

/// A `wgpu::Instance` whose every call goes to Krate's GPU.
pub fn instance() -> wgpu::Instance {
    wgpu::Instance::from_custom(KInstance)
}

/// A surface target for the canvas widget `widget` in window `window`.
///
/// # Safety
/// Nothing is dereferenced; it is unsafe only because wgpu's API is.
pub fn surface_target(window: u64, widget: u64) -> wgpu::SurfaceTargetUnsafe {
    wgpu::SurfaceTargetUnsafe::RawHandle {
        #[cfg(not(feature = "wgpu-27"))]
        raw_display_handle: None,
        #[cfg(feature = "wgpu-27")]
        raw_display_handle: rwh::RawDisplayHandle::Web(rwh::WebDisplayHandle::new()),
        raw_window_handle: KrateWindow::new(window, widget).raw(),
    }
}

/// A window and canvas a surface can be made on, as raw-window-handle sees
/// one: what an engine that asks for `HasWindowHandle` is given.
#[derive(Clone, Copy, Debug)]
pub struct KrateWindow {
    id: u32,
}

impl KrateWindow {
    /// Name the canvas `widget` of `window`.
    ///
    /// The id is `window * 256 + widget`: stateless, so anything that makes
    /// a raw window handle for a Krate canvas -- winit's Krate backend, an
    /// engine's own seam -- names it the same way without sharing a table
    /// with this crate. Window ids are small and a canvas widget's id is
    /// under 256; 0 is never a window, so the id is never 0 (raw-window-handle
    /// reserves it).
    pub fn new(window: u64, widget: u64) -> KrateWindow {
        assert!(
            widget < 256 && window > 0 && window < (1 << 24),
            "a Krate canvas id out of range"
        );
        KrateWindow {
            id: (window as u32) * 256 + widget as u32,
        }
    }

    fn raw(&self) -> rwh::RawWindowHandle {
        rwh::RawWindowHandle::Web(rwh::WebWindowHandle::new(self.id))
    }

    fn lookup(id: u32) -> Option<(u64, u64)> {
        (id >= 256).then_some(((id / 256) as u64, (id % 256) as u64))
    }
}

impl rwh::HasWindowHandle for KrateWindow {
    fn window_handle(&self) -> Result<rwh::WindowHandle<'_>, rwh::HandleError> {
        // SAFETY: a Web handle is an id, not a pointer; it stays valid.
        Ok(unsafe { rwh::WindowHandle::borrow_raw(self.raw()) })
    }
}

impl rwh::HasDisplayHandle for KrateWindow {
    fn display_handle(&self) -> Result<rwh::DisplayHandle<'_>, rwh::HandleError> {
        Ok(rwh::DisplayHandle::web())
    }
}

// ---------------------------------------------------------------------------
// Handles, errors, callbacks

/// A host handle, released when the last owner drops it.
struct Owned(u64);

impl fmt::Debug for Owned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        if self.0 != 0 {
            g::release(self.0);
        }
    }
}

type Callback = Box<dyn FnOnce() + Send>;

/// Callbacks waiting for the next poll or submit.
static PENDING: Mutex<Vec<Callback>> = Mutex::new(Vec::new());

fn defer(callback: Callback) {
    PENDING.lock().unwrap().push(callback);
}

/// A map the host has started and not yet finished.
struct PendingMap {
    buffer: u64,
    mapped: Arc<Mutex<Option<Mapping>>>,
    callback: BufferMapCallback,
}

static PENDING_MAPS: Mutex<Vec<PendingMap>> = Mutex::new(Vec::new());

/// Finish the maps the GPU is done with, calling their callbacks.
fn finish_maps() {
    let waiting: Vec<PendingMap> = std::mem::take(&mut *PENDING_MAPS.lock().unwrap());
    let mut still = Vec::new();
    for map in waiting {
        match g::map_buffer_ready(map.buffer, false) {
            Ok(false) => still.push(map),
            Ok(true) => {
                *map.mapped.lock().unwrap() = Some(Mapping { zeroed: false });
                (map.callback)(Ok(()));
            }
            Err(e) => {
                eprintln!("wgpu-krate: a buffer map failed: {}", error_text(&e));
                (map.callback)(Err(wgpu::BufferAsyncError));
            }
        }
    }
    PENDING_MAPS.lock().unwrap().extend(still);
}

fn run_pending() {
    finish_maps();
    loop {
        let batch: Vec<Callback> = std::mem::take(&mut *PENDING.lock().unwrap());
        if batch.is_empty() {
            return;
        }
        for callback in batch {
            callback();
        }
    }
}

#[derive(Debug)]
struct HostError(String);

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HostError {}

fn error_text(e: &t::GpuError) -> String {
    match e {
        t::GpuError::InvalidHandle(s) => format!("invalid handle: {s}"),
        t::GpuError::Validation(s) => s.clone(),
        t::GpuError::OutOfMemory => "out of memory".to_string(),
        t::GpuError::Lost(s) => format!("device lost: {s}"),
        t::GpuError::Unsupported(s) => format!("unsupported: {s}"),
        t::GpuError::Internal(s) => format!("internal: {s}"),
    }
}

fn to_wgpu_error(e: &t::GpuError) -> (wgpu::ErrorFilter, wgpu::Error) {
    let text = error_text(e);
    let source: wgpu::ErrorSource = Box::new(HostError(text.clone()));
    match e {
        t::GpuError::OutOfMemory => (
            wgpu::ErrorFilter::OutOfMemory,
            wgpu::Error::OutOfMemory { source },
        ),
        t::GpuError::Internal(_) | t::GpuError::Lost(_) => (
            wgpu::ErrorFilter::Internal,
            wgpu::Error::Internal {
                source,
                description: text,
            },
        ),
        _ => (
            wgpu::ErrorFilter::Validation,
            wgpu::Error::Validation {
                source,
                description: text,
            },
        ),
    }
}

/// What a device keeps on this side: its handle, what it can do, and
/// WebGPU's error machinery.
struct DeviceCtx {
    handle: Owned,
    #[cfg_attr(feature = "wgpu-27", allow(dead_code))]
    info: wgpu::AdapterInfo,
    features: wgpu::Features,
    limits: wgpu::Limits,
    scopes: Mutex<Vec<(wgpu::ErrorFilter, Option<wgpu::Error>)>>,
    handler: Mutex<Option<Arc<dyn wgpu::UncapturedErrorHandler>>>,
    lost: Mutex<Option<BoxDeviceLostCallback>>,
}

impl fmt::Debug for DeviceCtx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeviceCtx({:?})", self.handle)
    }
}

impl DeviceCtx {
    fn h(&self) -> u64 {
        self.handle.0
    }

    /// Hand a host error to WebGPU's machinery.
    fn report(&self, e: t::GpuError) {
        if let t::GpuError::Lost(reason) = &e {
            if let Some(callback) = self.lost.lock().unwrap().take() {
                let reason = reason.clone();
                defer(Box::new(move || {
                    callback(wgpu::DeviceLostReason::Unknown, reason)
                }));
            }
        }
        let (filter, error) = to_wgpu_error(&e);
        {
            let mut scopes = self.scopes.lock().unwrap();
            if let Some(scope) = scopes.iter_mut().rev().find(|(f, _)| *f == filter) {
                if scope.1.is_none() {
                    scope.1 = Some(error);
                }
                return;
            }
        }
        let handler = self.handler.lock().unwrap().clone();
        match handler {
            Some(handler) => handler(error),
            None => eprintln!("wgpu-krate: uncaptured GPU error: {error}"),
        }
    }

    /// The handle a call returned, or 0 with the error reported.
    fn take(&self, r: Result<u64, t::GpuError>) -> u64 {
        match r {
            Ok(h) => h,
            Err(e) => {
                self.report(e);
                0
            }
        }
    }

    fn check(&self, r: Result<(), t::GpuError>) {
        if let Err(e) = r {
            self.report(e);
        }
    }
}

macro_rules! object {
    ($name:ident) => {
        #[derive(Debug)]
        struct $name {
            h: Owned,
        }
        impl $name {
            fn new(h: u64) -> Self {
                $name { h: Owned(h) }
            }
        }
    };
}

object!(KBindGroupLayout);
object!(KBindGroup);
object!(KTextureView);
object!(KSampler);
object!(KQuerySet);
object!(KPipelineLayout);
object!(KCommandBuffer);

impl BindGroupLayoutInterface for KBindGroupLayout {}
impl BindGroupInterface for KBindGroup {}
impl TextureViewInterface for KTextureView {}
impl SamplerInterface for KSampler {}
impl QuerySetInterface for KQuerySet {}
impl PipelineLayoutInterface for KPipelineLayout {}
impl CommandBufferInterface for KCommandBuffer {}

fn handle_of<T: 'static + fmt::Debug + HasHandle>(object: Option<&T>) -> u64 {
    object.map(|o| o.handle()).unwrap_or(0)
}

trait HasHandle {
    fn handle(&self) -> u64;
}

macro_rules! has_handle {
    ($($name:ident),*) => {
        $(impl HasHandle for $name {
            fn handle(&self) -> u64 {
                self.h.0
            }
        })*
    };
}

has_handle!(
    KBindGroupLayout,
    KBindGroup,
    KTextureView,
    KSampler,
    KQuerySet,
    KPipelineLayout,
    KCommandBuffer,
    KBuffer,
    KTexture,
    KShaderModule,
    KRenderPipeline,
    KComputePipeline
);

fn buffer_h(b: &wgpu::Buffer) -> u64 {
    handle_of(b.as_custom::<KBuffer>())
}
fn dbuffer_h(b: &DispatchBuffer) -> u64 {
    handle_of(b.as_custom::<KBuffer>())
}
fn view_h(v: &wgpu::TextureView) -> u64 {
    handle_of(v.as_custom::<KTextureView>())
}
fn texture_h(t: &wgpu::Texture) -> u64 {
    handle_of(t.as_custom::<KTexture>())
}
fn query_h(q: &wgpu::QuerySet) -> u64 {
    handle_of(q.as_custom::<KQuerySet>())
}
fn dquery_h(q: &DispatchQuerySet) -> u64 {
    handle_of(q.as_custom::<KQuerySet>())
}

// ---------------------------------------------------------------------------
// Conversions, wgpu to krate:gpu

fn label(l: wgpu::Label<'_>) -> Option<String> {
    l.map(String::from)
}

fn fmt_name(f: wgpu::TextureFormat) -> String {
    names::format_name(f)
}

fn buffer_usages(u: wgpu::BufferUsages) -> t::BufferUsages {
    let mut out = t::BufferUsages::empty();
    for (w, k) in [
        (wgpu::BufferUsages::MAP_READ, t::BufferUsages::MAP_READ),
        (wgpu::BufferUsages::MAP_WRITE, t::BufferUsages::MAP_WRITE),
        (wgpu::BufferUsages::COPY_SRC, t::BufferUsages::COPY_SRC),
        (wgpu::BufferUsages::COPY_DST, t::BufferUsages::COPY_DST),
        (wgpu::BufferUsages::INDEX, t::BufferUsages::INDEX),
        (wgpu::BufferUsages::VERTEX, t::BufferUsages::VERTEX),
        (wgpu::BufferUsages::UNIFORM, t::BufferUsages::UNIFORM),
        (wgpu::BufferUsages::STORAGE, t::BufferUsages::STORAGE),
        (wgpu::BufferUsages::INDIRECT, t::BufferUsages::INDIRECT),
        (
            wgpu::BufferUsages::QUERY_RESOLVE,
            t::BufferUsages::QUERY_RESOLVE,
        ),
    ] {
        if u.contains(w) {
            out |= k;
        }
    }
    out
}

const TEXTURE_USAGES: [(wgpu::TextureUsages, t::TextureUsages); 5] = [
    (wgpu::TextureUsages::COPY_SRC, t::TextureUsages::COPY_SRC),
    (wgpu::TextureUsages::COPY_DST, t::TextureUsages::COPY_DST),
    (
        wgpu::TextureUsages::TEXTURE_BINDING,
        t::TextureUsages::TEXTURE_BINDING,
    ),
    (
        wgpu::TextureUsages::STORAGE_BINDING,
        t::TextureUsages::STORAGE_BINDING,
    ),
    (
        wgpu::TextureUsages::RENDER_ATTACHMENT,
        t::TextureUsages::RENDER_ATTACHMENT,
    ),
];

fn texture_usages(u: wgpu::TextureUsages) -> t::TextureUsages {
    let mut out = t::TextureUsages::empty();
    for (w, k) in TEXTURE_USAGES {
        if u.contains(w) {
            out |= k;
        }
    }
    out
}

fn texture_usages_in(u: t::TextureUsages) -> wgpu::TextureUsages {
    let mut out = wgpu::TextureUsages::empty();
    for (w, k) in TEXTURE_USAGES {
        if u.contains(k) {
            out |= w;
        }
    }
    out
}

fn extent(e: wgpu::Extent3d) -> t::Extent3d {
    t::Extent3d {
        width: e.width,
        height: e.height,
        depth_or_array_layers: e.depth_or_array_layers,
    }
}

fn origin(o: wgpu::Origin3d) -> t::Origin3d {
    t::Origin3d {
        x: o.x,
        y: o.y,
        z: o.z,
    }
}

fn aspect(a: wgpu::TextureAspect) -> t::TextureAspect {
    match a {
        wgpu::TextureAspect::StencilOnly => t::TextureAspect::StencilOnly,
        wgpu::TextureAspect::DepthOnly => t::TextureAspect::DepthOnly,
        _ => t::TextureAspect::All,
    }
}

fn view_dimension(d: wgpu::TextureViewDimension) -> t::TextureViewDimension {
    match d {
        wgpu::TextureViewDimension::D1 => t::TextureViewDimension::D1,
        wgpu::TextureViewDimension::D2 => t::TextureViewDimension::D2,
        wgpu::TextureViewDimension::D2Array => t::TextureViewDimension::D2Array,
        wgpu::TextureViewDimension::Cube => t::TextureViewDimension::Cube,
        wgpu::TextureViewDimension::CubeArray => t::TextureViewDimension::CubeArray,
        wgpu::TextureViewDimension::D3 => t::TextureViewDimension::D3,
    }
}

fn address(a: wgpu::AddressMode) -> t::AddressMode {
    match a {
        wgpu::AddressMode::ClampToEdge => t::AddressMode::ClampToEdge,
        wgpu::AddressMode::Repeat => t::AddressMode::Repeat,
        wgpu::AddressMode::MirrorRepeat => t::AddressMode::MirrorRepeat,
        wgpu::AddressMode::ClampToBorder => t::AddressMode::ClampToBorder,
    }
}

fn filter(f: wgpu::FilterMode) -> t::FilterMode {
    match f {
        wgpu::FilterMode::Nearest => t::FilterMode::Nearest,
        wgpu::FilterMode::Linear => t::FilterMode::Linear,
    }
}

#[cfg(not(feature = "wgpu-27"))]
fn mip_filter(f: wgpu::MipmapFilterMode) -> t::FilterMode {
    match f {
        wgpu::MipmapFilterMode::Nearest => t::FilterMode::Nearest,
        wgpu::MipmapFilterMode::Linear => t::FilterMode::Linear,
    }
}

/// wgpu 27 names a mipmap filter with the ordinary filter mode.
#[cfg(feature = "wgpu-27")]
fn mip_filter(f: wgpu::FilterMode) -> t::FilterMode {
    filter(f)
}

fn compare(c: wgpu::CompareFunction) -> t::CompareFunction {
    match c {
        wgpu::CompareFunction::Never => t::CompareFunction::Never,
        wgpu::CompareFunction::Less => t::CompareFunction::Less,
        wgpu::CompareFunction::Equal => t::CompareFunction::Equal,
        wgpu::CompareFunction::LessEqual => t::CompareFunction::LessEqual,
        wgpu::CompareFunction::Greater => t::CompareFunction::Greater,
        wgpu::CompareFunction::NotEqual => t::CompareFunction::NotEqual,
        wgpu::CompareFunction::GreaterEqual => t::CompareFunction::GreaterEqual,
        wgpu::CompareFunction::Always => t::CompareFunction::Always,
    }
}

fn stencil_op(o: wgpu::StencilOperation) -> t::StencilOperation {
    match o {
        wgpu::StencilOperation::Keep => t::StencilOperation::Keep,
        wgpu::StencilOperation::Zero => t::StencilOperation::Zero,
        wgpu::StencilOperation::Replace => t::StencilOperation::Replace,
        wgpu::StencilOperation::Invert => t::StencilOperation::Invert,
        wgpu::StencilOperation::IncrementClamp => t::StencilOperation::IncrementClamp,
        wgpu::StencilOperation::DecrementClamp => t::StencilOperation::DecrementClamp,
        wgpu::StencilOperation::IncrementWrap => t::StencilOperation::IncrementWrap,
        wgpu::StencilOperation::DecrementWrap => t::StencilOperation::DecrementWrap,
    }
}

fn stencil_face(s: &wgpu::StencilFaceState) -> t::StencilFaceState {
    t::StencilFaceState {
        compare: compare(s.compare),
        fail_op: stencil_op(s.fail_op),
        depth_fail_op: stencil_op(s.depth_fail_op),
        pass_op: stencil_op(s.pass_op),
    }
}

fn blend_factor(f: wgpu::BlendFactor) -> t::BlendFactor {
    use wgpu::BlendFactor as B;
    match f {
        B::Zero => t::BlendFactor::Zero,
        B::One => t::BlendFactor::One,
        B::Src => t::BlendFactor::Src,
        B::OneMinusSrc => t::BlendFactor::OneMinusSrc,
        B::SrcAlpha => t::BlendFactor::SrcAlpha,
        B::OneMinusSrcAlpha => t::BlendFactor::OneMinusSrcAlpha,
        B::Dst => t::BlendFactor::Dst,
        B::OneMinusDst => t::BlendFactor::OneMinusDst,
        B::DstAlpha => t::BlendFactor::DstAlpha,
        B::OneMinusDstAlpha => t::BlendFactor::OneMinusDstAlpha,
        B::SrcAlphaSaturated => t::BlendFactor::SrcAlphaSaturated,
        B::Constant => t::BlendFactor::Constant,
        B::OneMinusConstant => t::BlendFactor::OneMinusConstant,
        B::Src1 => t::BlendFactor::Src1,
        B::OneMinusSrc1 => t::BlendFactor::OneMinusSrc1,
        B::Src1Alpha => t::BlendFactor::Src1Alpha,
        B::OneMinusSrc1Alpha => t::BlendFactor::OneMinusSrc1Alpha,
    }
}

fn blend_component(c: &wgpu::BlendComponent) -> t::BlendComponent {
    t::BlendComponent {
        src_factor: blend_factor(c.src_factor),
        dst_factor: blend_factor(c.dst_factor),
        operation: match c.operation {
            wgpu::BlendOperation::Add => t::BlendOperation::Add,
            wgpu::BlendOperation::Subtract => t::BlendOperation::Subtract,
            wgpu::BlendOperation::ReverseSubtract => t::BlendOperation::ReverseSubtract,
            wgpu::BlendOperation::Min => t::BlendOperation::Min,
            wgpu::BlendOperation::Max => t::BlendOperation::Max,
        },
    }
}

fn color_writes(c: wgpu::ColorWrites) -> t::ColorWrites {
    let mut out = t::ColorWrites::empty();
    if c.contains(wgpu::ColorWrites::RED) {
        out |= t::ColorWrites::RED;
    }
    if c.contains(wgpu::ColorWrites::GREEN) {
        out |= t::ColorWrites::GREEN;
    }
    if c.contains(wgpu::ColorWrites::BLUE) {
        out |= t::ColorWrites::BLUE;
    }
    if c.contains(wgpu::ColorWrites::ALPHA) {
        out |= t::ColorWrites::ALPHA;
    }
    out
}

fn color(c: wgpu::Color) -> t::Color {
    t::Color {
        r: c.r,
        g: c.g,
        b: c.b,
        a: c.a,
    }
}

fn store(s: wgpu::StoreOp) -> t::StoreOp {
    match s {
        wgpu::StoreOp::Store => t::StoreOp::Store,
        wgpu::StoreOp::Discard => t::StoreOp::Discard,
    }
}

fn index_format(f: wgpu::IndexFormat) -> t::IndexFormat {
    match f {
        wgpu::IndexFormat::Uint16 => t::IndexFormat::Uint16,
        wgpu::IndexFormat::Uint32 => t::IndexFormat::Uint32,
    }
}

fn stages(s: wgpu::ShaderStages) -> t::ShaderStages {
    let mut out = t::ShaderStages::empty();
    if s.contains(wgpu::ShaderStages::VERTEX) {
        out |= t::ShaderStages::VERTEX;
    }
    if s.contains(wgpu::ShaderStages::FRAGMENT) {
        out |= t::ShaderStages::FRAGMENT;
    }
    if s.contains(wgpu::ShaderStages::COMPUTE) {
        out |= t::ShaderStages::COMPUTE;
    }
    out
}

fn copy_layout(l: wgpu::TexelCopyBufferLayout) -> t::TexelCopyBufferLayout {
    t::TexelCopyBufferLayout {
        offset: l.offset,
        bytes_per_row: l.bytes_per_row,
        rows_per_image: l.rows_per_image,
    }
}

fn texture_copy(c: &wgpu::TexelCopyTextureInfo<'_>) -> t::TexelCopyTextureInfo {
    t::TexelCopyTextureInfo {
        texture: texture_h(c.texture),
        mip_level: c.mip_level,
        origin: origin(c.origin),
        aspect: aspect(c.aspect),
    }
}

fn buffer_copy(c: &wgpu::TexelCopyBufferInfo<'_>) -> t::TexelCopyBufferInfo {
    t::TexelCopyBufferInfo {
        buffer: buffer_h(c.buffer),
        layout: copy_layout(c.layout),
    }
}

fn stage(
    module: &wgpu::ShaderModule,
    entry: Option<&str>,
    options: &wgpu::PipelineCompilationOptions<'_>,
) -> t::ProgrammableStage {
    t::ProgrammableStage {
        module: handle_of(module.as_custom::<KShaderModule>()),
        entry_point: entry.map(String::from),
        constants: options
            .constants
            .iter()
            .map(|(name, value)| t::ConstantEntry {
                name: name.to_string(),
                value: *value,
            })
            .collect(),
    }
}

fn features_from(names: &[String]) -> wgpu::Features {
    let mut out = wgpu::Features::empty();
    for name in names {
        if let Some(f) = names::parse_feature(name) {
            out |= f;
        }
    }
    // Experimental features need an opt-in krate:gpu does not offer.
    out - wgpu::Features::all_experimental_mask()
}

fn limits_from(list: &[t::Limit]) -> wgpu::Limits {
    let mut limits = wgpu::Limits::default();
    for l in list {
        names::set_limit(&mut limits, &l.name, l.value);
    }
    limits
}

// ---------------------------------------------------------------------------
// Instance and adapter

#[derive(Debug)]
struct KInstance;

impl InstanceInterface for KInstance {
    #[cfg(not(feature = "wgpu-27"))]
    fn new(_desc: wgpu::InstanceDescriptor) -> Self {
        KInstance
    }

    #[cfg(feature = "wgpu-27")]
    fn new(_desc: &wgpu::InstanceDescriptor) -> Self {
        KInstance
    }

    unsafe fn create_surface(
        &self,
        target: wgpu::SurfaceTargetUnsafe,
    ) -> Result<DispatchSurface, wgpu::CreateSurfaceError> {
        let wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_window_handle: rwh::RawWindowHandle::Web(web),
            ..
        } = target
        else {
            panic!("wgpu-krate: a surface needs a Krate window (wgpu_krate::surface_target or KrateWindow)");
        };
        let (window, widget) =
            KrateWindow::lookup(web.id).expect("wgpu-krate: an unknown Krate window id");
        let h = match g::create_surface(window, widget) {
            Ok(h) => h,
            Err(e) => panic!("wgpu-krate: no surface on that canvas: {}", error_text(&e)),
        };
        Ok(DispatchSurface::custom(KSurface {
            h: Owned(h),
            device: Mutex::new(None),
        }))
    }

    fn request_adapter(
        &self,
        options: &wgpu::RequestAdapterOptions<'_, '_>,
    ) -> Pin<Box<dyn RequestAdapterFuture>> {
        let result = make_adapter(t::AdapterOptions {
            power_preference: match options.power_preference {
                wgpu::PowerPreference::LowPower => t::PowerPreference::LowPower,
                wgpu::PowerPreference::HighPerformance => t::PowerPreference::HighPerformance,
                _ => t::PowerPreference::None,
            },
            force_fallback_adapter: options.force_fallback_adapter,
            compatible_surface: None,
        });
        Box::pin(ready(result))
    }

    fn poll_all_devices(&self, _force_wait: bool) -> bool {
        run_pending();
        true
    }

    fn wgsl_language_features(&self) -> wgpu::WgslLanguageFeatures {
        wgpu::WgslLanguageFeatures::empty()
    }

    #[cfg(not(feature = "wgpu-27"))]
    fn enumerate_adapters(
        &self,
        _backends: wgpu::Backends,
    ) -> Pin<Box<dyn EnumerateAdapterFuture>> {
        let found = make_adapter(t::AdapterOptions {
            power_preference: t::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        });
        Box::pin(ready(found.into_iter().collect::<Vec<_>>()))
    }
}

fn make_adapter(options: t::AdapterOptions) -> Result<DispatchAdapter, wgpu::RequestAdapterError> {
    let h = g::request_adapter(options).map_err(|_| wgpu::RequestAdapterError::NotFound {
        active_backends: wgpu::Backends::empty(),
        requested_backends: wgpu::Backends::empty(),
        supported_backends: wgpu::Backends::empty(),
        no_fallback_backends: wgpu::Backends::empty(),
        no_adapter_backends: wgpu::Backends::empty(),
        incompatible_surface_backends: wgpu::Backends::empty(),
    })?;
    let info = g::describe_adapter(h).ok();
    let features = g::adapter_features(h).unwrap_or_default();
    let limits = g::adapter_limits(h).unwrap_or_default();
    Ok(DispatchAdapter::custom(KAdapter {
        h: Owned(h),
        info: adapter_info(info),
        features: features_from(&features),
        limits: limits_from(&limits),
    }))
}

fn adapter_info(info: Option<t::AdapterInfo>) -> wgpu::AdapterInfo {
    let info = info.unwrap_or(t::AdapterInfo {
        name: String::from("Krate GPU"),
        vendor: 0,
        device: 0,
        device_type: t::DeviceType::Other,
        backend: String::new(),
        driver: String::new(),
        driver_info: String::new(),
    });
    wgpu::AdapterInfo {
        name: info.name,
        vendor: info.vendor,
        device: info.device,
        device_type: match info.device_type {
            t::DeviceType::IntegratedGpu => wgpu::DeviceType::IntegratedGpu,
            t::DeviceType::DiscreteGpu => wgpu::DeviceType::DiscreteGpu,
            t::DeviceType::VirtualGpu => wgpu::DeviceType::VirtualGpu,
            t::DeviceType::Cpu => wgpu::DeviceType::Cpu,
            t::DeviceType::Other => wgpu::DeviceType::Other,
        },
        #[cfg(not(feature = "wgpu-27"))]
        device_pci_bus_id: String::new(),
        driver: info.driver,
        driver_info: format!("{} through krate:gpu", info.backend),
        // The app talks WebGPU; what is under the host is the host's.
        backend: wgpu::Backend::BrowserWebGpu,
        #[cfg(not(feature = "wgpu-27"))]
        subgroup_min_size: 4,
        #[cfg(not(feature = "wgpu-27"))]
        subgroup_max_size: 128,
        #[cfg(not(feature = "wgpu-27"))]
        transient_saves_memory: false,
    }
}

#[derive(Debug)]
struct KAdapter {
    h: Owned,
    info: wgpu::AdapterInfo,
    features: wgpu::Features,
    limits: wgpu::Limits,
}

impl AdapterInterface for KAdapter {
    fn request_device(
        &self,
        desc: &wgpu::DeviceDescriptor<'_>,
    ) -> Pin<Box<dyn RequestDeviceFuture>> {
        let required_limits = names::limit_pairs(&desc.required_limits)
            .into_iter()
            // A wgpu 27 limit the host's wgpu no longer has (it dropped
            // max-inter-stage-shader-components) is not asked for.
            .filter(|(name, _)| krate_gpu_names::LIMITS.iter().any(|(n, _, _)| n == name))
            .map(|(name, value)| t::Limit {
                name: name.to_string(),
                value,
            })
            .collect();
        let made = g::request_device(
            self.h.0,
            &t::DeviceDescriptor {
                label: label(desc.label),
                required_features: names::feature_names(desc.required_features)
                    .into_iter()
                    .map(String::from)
                    .collect(),
                required_limits,
            },
        );
        let result = match made {
            Ok(h) => {
                let features = g::device_features(h).unwrap_or_default();
                let limits = g::device_limits(h).unwrap_or_default();
                let ctx = Arc::new(DeviceCtx {
                    handle: Owned(h),
                    info: self.info.clone(),
                    features: features_from(&features),
                    limits: limits_from(&limits),
                    scopes: Mutex::new(Vec::new()),
                    handler: Mutex::new(None),
                    lost: Mutex::new(None),
                });
                Ok((
                    DispatchDevice::custom(KDevice { ctx: ctx.clone() }),
                    DispatchQueue::custom(KQueue { ctx }),
                ))
            }
            // wgpu gives a custom backend no way to build a
            // RequestDeviceError, so a refused device ends here, saying why.
            Err(e) => panic!("wgpu-krate: the GPU refused the device: {}", error_text(&e)),
        };
        Box::pin(ready(result))
    }

    fn is_surface_supported(&self, _surface: &DispatchSurface) -> bool {
        true
    }

    fn features(&self) -> wgpu::Features {
        self.features
    }

    fn limits(&self) -> wgpu::Limits {
        self.limits.clone()
    }

    fn downlevel_capabilities(&self) -> wgpu::DownlevelCapabilities {
        wgpu::DownlevelCapabilities::default()
    }

    fn get_info(&self) -> wgpu::AdapterInfo {
        self.info.clone()
    }

    fn get_texture_format_features(
        &self,
        format: wgpu::TextureFormat,
    ) -> wgpu::TextureFormatFeatures {
        let mut flags = wgpu::TextureFormatFeatureFlags::empty();
        let mut allowed = wgpu::TextureUsages::empty();
        if let Ok(f) = g::adapter_format_features(self.h.0, &fmt_name(format)) {
            allowed = texture_usages_in(f.allowed_usages);
            if f.filterable {
                flags |= wgpu::TextureFormatFeatureFlags::FILTERABLE;
            }
            if f.blendable {
                flags |= wgpu::TextureFormatFeatureFlags::BLENDABLE;
            }
            if f.storage_read_write {
                flags |= wgpu::TextureFormatFeatureFlags::STORAGE_READ_WRITE;
            }
            for count in f.sample_counts {
                flags |= match count {
                    2 => wgpu::TextureFormatFeatureFlags::MULTISAMPLE_X2,
                    4 => wgpu::TextureFormatFeatureFlags::MULTISAMPLE_X4,
                    8 => wgpu::TextureFormatFeatureFlags::MULTISAMPLE_X8,
                    16 => wgpu::TextureFormatFeatureFlags::MULTISAMPLE_X16,
                    _ => wgpu::TextureFormatFeatureFlags::empty(),
                };
            }
            if allowed.contains(wgpu::TextureUsages::RENDER_ATTACHMENT) {
                flags |= wgpu::TextureFormatFeatureFlags::MULTISAMPLE_RESOLVE;
            }
            if allowed.contains(wgpu::TextureUsages::STORAGE_BINDING) {
                flags |= wgpu::TextureFormatFeatureFlags::STORAGE_WRITE_ONLY
                    | wgpu::TextureFormatFeatureFlags::STORAGE_READ_ONLY;
            }
        }
        wgpu::TextureFormatFeatures {
            allowed_usages: allowed,
            flags,
        }
    }

    fn get_presentation_timestamp(&self) -> wgpu::PresentationTimestamp {
        wgpu::PresentationTimestamp::INVALID_TIMESTAMP
    }

    #[cfg(not(feature = "wgpu-27"))]
    fn cooperative_matrix_properties(&self) -> Vec<wgpu::wgt::CooperativeMatrixProperties> {
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// Device and queue

#[derive(Debug)]
struct KDevice {
    ctx: Arc<DeviceCtx>,
}

#[derive(Debug)]
struct KShaderModule {
    h: Owned,
    messages: Vec<wgpu::CompilationMessage>,
}

impl ShaderModuleInterface for KShaderModule {
    fn get_compilation_info(&self) -> Pin<Box<dyn ShaderCompilationInfoFuture>> {
        Box::pin(ready(wgpu::CompilationInfo {
            messages: self.messages.clone(),
        }))
    }
}

fn message(
    text: String,
    kind: wgpu::CompilationMessageType,
    line: u32,
    column: u32,
    offset: u32,
    length: u32,
) -> wgpu::CompilationMessage {
    wgpu::CompilationMessage {
        message: text,
        message_type: kind,
        location: (line > 0).then_some(wgpu::SourceLocation {
            line_number: line,
            line_position: column,
            offset,
            length,
        }),
    }
}

/// A naga module (what Bevy composes its shaders into, and what a GLSL or
/// SPIR-V shader parses into) as WGSL source.
fn naga_to_wgsl(module: &naga::Module) -> Result<String, String> {
    let mut module = module.clone();
    lower::lower_for_wgsl(&mut module)?;
    let module = &module;
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(module)
    .map_err(|e| format!("{e:?}"))?;
    naga::back::wgsl::write_string(module, &info, naga::back::wgsl::WriterFlags::empty())
        .map_err(|e| e.to_string())
}

#[derive(Debug)]
struct KRenderPipeline {
    h: Owned,
    ctx: Arc<DeviceCtx>,
}

#[derive(Debug)]
struct KComputePipeline {
    h: Owned,
    ctx: Arc<DeviceCtx>,
}

impl RenderPipelineInterface for KRenderPipeline {
    fn get_bind_group_layout(&self, index: u32) -> DispatchBindGroupLayout {
        let h = self
            .ctx
            .take(g::pipeline_bind_group_layout(self.h.0, index));
        DispatchBindGroupLayout::custom(KBindGroupLayout::new(h))
    }
}

impl ComputePipelineInterface for KComputePipeline {
    fn get_bind_group_layout(&self, index: u32) -> DispatchBindGroupLayout {
        let h = self
            .ctx
            .take(g::pipeline_bind_group_layout(self.h.0, index));
        DispatchBindGroupLayout::custom(KBindGroupLayout::new(h))
    }
}

#[derive(Debug)]
struct KPipelineCache;
impl PipelineCacheInterface for KPipelineCache {
    fn get_data(&self) -> Option<Vec<u8>> {
        None
    }
}

#[derive(Debug)]
struct KExternalTexture;
impl ExternalTextureInterface for KExternalTexture {
    fn destroy(&self) {}
}

#[derive(Debug)]
struct KBlas;
impl BlasInterface for KBlas {
    fn prepare_compact_async(&self, callback: BlasCompactCallback) {
        defer(Box::new(move || callback(Err(wgpu::BlasAsyncError))));
    }
    fn ready_for_compaction(&self) -> bool {
        false
    }
}

#[derive(Debug)]
struct KTlas;
impl TlasInterface for KTlas {}

fn binding_type(ty: &wgpu::BindingType) -> Result<t::BindingType, String> {
    Ok(match ty {
        wgpu::BindingType::Buffer {
            ty,
            has_dynamic_offset,
            min_binding_size,
        } => t::BindingType::Buffer(t::BufferBindingLayout {
            kind: match ty {
                wgpu::BufferBindingType::Uniform => t::BufferBindingType::Uniform,
                wgpu::BufferBindingType::Storage { read_only } => {
                    t::BufferBindingType::Storage(*read_only)
                }
            },
            has_dynamic_offset: *has_dynamic_offset,
            min_binding_size: min_binding_size.map(|s| s.get()).unwrap_or(0),
        }),
        wgpu::BindingType::Sampler(s) => t::BindingType::Sampler(match s {
            wgpu::SamplerBindingType::Filtering => t::SamplerBindingType::Filtering,
            wgpu::SamplerBindingType::NonFiltering => t::SamplerBindingType::NonFiltering,
            wgpu::SamplerBindingType::Comparison => t::SamplerBindingType::Comparison,
        }),
        wgpu::BindingType::Texture {
            sample_type,
            view_dimension: vd,
            multisampled,
        } => t::BindingType::Texture(t::TextureBindingLayout {
            sample_type: match sample_type {
                wgpu::TextureSampleType::Float { filterable } => {
                    t::TextureSampleType::Float(*filterable)
                }
                wgpu::TextureSampleType::Depth => t::TextureSampleType::Depth,
                wgpu::TextureSampleType::Sint => t::TextureSampleType::Sint,
                wgpu::TextureSampleType::Uint => t::TextureSampleType::Uint,
            },
            view_dimension: view_dimension(*vd),
            multisampled: *multisampled,
        }),
        wgpu::BindingType::StorageTexture {
            access,
            format,
            view_dimension: vd,
        } => t::BindingType::StorageTexture(t::StorageTextureBindingLayout {
            access: match access {
                wgpu::StorageTextureAccess::WriteOnly => t::StorageTextureAccess::WriteOnly,
                wgpu::StorageTextureAccess::ReadOnly => t::StorageTextureAccess::ReadOnly,
                wgpu::StorageTextureAccess::ReadWrite => t::StorageTextureAccess::ReadWrite,
                other => return Err(format!("storage texture access {other:?}")),
            },
            format: fmt_name(*format),
            view_dimension: view_dimension(*vd),
        }),
        other => return Err(format!("binding type {other:?}")),
    })
}

fn buffer_binding(b: &wgpu::BufferBinding<'_>) -> t::BufferBinding {
    t::BufferBinding {
        buffer: buffer_h(b.buffer),
        offset: b.offset,
        size: b.size.map(|s| s.get()),
    }
}

impl DeviceInterface for KDevice {
    fn features(&self) -> wgpu::Features {
        self.ctx.features
    }

    fn limits(&self) -> wgpu::Limits {
        self.ctx.limits.clone()
    }

    #[cfg(not(feature = "wgpu-27"))]
    fn adapter_info(&self) -> wgpu::AdapterInfo {
        self.ctx.info.clone()
    }

    fn create_shader_module(
        &self,
        desc: wgpu::ShaderModuleDescriptor<'_>,
        _checks: wgpu::ShaderRuntimeChecks,
    ) -> DispatchShaderModule {
        let code: Result<Cow<'_, str>, String> = match &desc.source {
            wgpu::ShaderSource::Wgsl(code) => Ok(code.clone()),
            wgpu::ShaderSource::Naga(module) => naga_to_wgsl(module).map(Cow::Owned),
            #[cfg(feature = "glsl")]
            wgpu::ShaderSource::Glsl {
                shader,
                stage,
                defines,
            } => {
                let mut options = naga::front::glsl::Options::from(*stage);
                options.defines = defines
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect();
                naga::front::glsl::Frontend::default()
                    .parse(&options, shader)
                    .map_err(|e| e.emit_to_string(shader))
                    .and_then(|m| naga_to_wgsl(&m))
                    .map(Cow::Owned)
            }
            #[cfg(feature = "spirv")]
            wgpu::ShaderSource::SpirV(words) => {
                naga::front::spv::parse_u8_slice(
                    bytemuck_words(words),
                    &naga::front::spv::Options::default(),
                )
                .map_err(|e| e.to_string())
                .and_then(|m| naga_to_wgsl(&m))
                .map(Cow::Owned)
            }
            _ => Err("only WGSL, a naga module, GLSL (feature glsl) or SPIR-V (feature spirv) reaches krate:gpu".to_string()),
        };
        let code = match code {
            Ok(code) => code,
            Err(why) => {
                self.ctx.report(t::GpuError::Validation(why.clone()));
                return DispatchShaderModule::custom(KShaderModule {
                    h: Owned(0),
                    messages: vec![message(
                        why,
                        wgpu::CompilationMessageType::Error,
                        0,
                        0,
                        0,
                        0,
                    )],
                });
            }
        };
        match g::create_shader_module(
            self.ctx.h(),
            &t::ShaderModuleDescriptor {
                label: label(desc.label),
                code: code.into_owned(),
            },
        ) {
            Ok(h) => {
                let messages = g::shader_compilation_info(h)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|m| {
                        let kind = match m.kind.as_str() {
                            "error" => wgpu::CompilationMessageType::Error,
                            "warning" => wgpu::CompilationMessageType::Warning,
                            _ => wgpu::CompilationMessageType::Info,
                        };
                        message(m.message, kind, m.line, m.column, m.offset, m.length)
                    })
                    .collect();
                DispatchShaderModule::custom(KShaderModule {
                    h: Owned(h),
                    messages,
                })
            }
            Err(e) => {
                let text = format!(
                    "shader {:?} refused at line {}, column {}: {}",
                    desc.label.unwrap_or(""),
                    e.line,
                    e.column,
                    e.message
                );
                self.ctx.report(t::GpuError::Validation(text));
                DispatchShaderModule::custom(KShaderModule {
                    h: Owned(0),
                    messages: vec![message(
                        e.message,
                        wgpu::CompilationMessageType::Error,
                        e.line,
                        e.column,
                        e.offset,
                        e.length,
                    )],
                })
            }
        }
    }

    unsafe fn create_shader_module_passthrough(
        &self,
        _desc: &wgpu::ShaderModuleDescriptorPassthrough<'_>,
    ) -> DispatchShaderModule {
        self.ctx.report(t::GpuError::Unsupported(
            "passthrough shaders: krate:gpu takes WGSL only".to_string(),
        ));
        DispatchShaderModule::custom(KShaderModule {
            h: Owned(0),
            messages: Vec::new(),
        })
    }

    fn create_bind_group_layout(
        &self,
        desc: &wgpu::BindGroupLayoutDescriptor<'_>,
    ) -> DispatchBindGroupLayout {
        let mut entries = Vec::with_capacity(desc.entries.len());
        for e in desc.entries {
            match binding_type(&e.ty) {
                Ok(kind) => entries.push(t::BindGroupLayoutEntry {
                    binding: e.binding,
                    visibility: stages(e.visibility),
                    kind,
                    count: e.count.map(|c| c.get()),
                }),
                Err(why) => {
                    self.ctx.report(t::GpuError::Unsupported(why));
                    return DispatchBindGroupLayout::custom(KBindGroupLayout::new(0));
                }
            }
        }
        let h = self.ctx.take(g::create_bind_group_layout(
            self.ctx.h(),
            &t::BindGroupLayoutDescriptor {
                label: label(desc.label),
                entries,
            },
        ));
        DispatchBindGroupLayout::custom(KBindGroupLayout::new(h))
    }

    fn create_bind_group(&self, desc: &wgpu::BindGroupDescriptor<'_>) -> DispatchBindGroup {
        let mut entries = Vec::with_capacity(desc.entries.len());
        for e in desc.entries {
            let resource = match &e.resource {
                wgpu::BindingResource::Buffer(b) => t::BindingResource::Buffer(buffer_binding(b)),
                wgpu::BindingResource::BufferArray(list) => {
                    t::BindingResource::BufferArray(list.iter().map(buffer_binding).collect())
                }
                wgpu::BindingResource::Sampler(s) => {
                    t::BindingResource::Sampler(handle_of(s.as_custom::<KSampler>()))
                }
                wgpu::BindingResource::SamplerArray(list) => t::BindingResource::SamplerArray(
                    list.iter()
                        .map(|s| handle_of(s.as_custom::<KSampler>()))
                        .collect(),
                ),
                wgpu::BindingResource::TextureView(v) => t::BindingResource::TextureView(view_h(v)),
                wgpu::BindingResource::TextureViewArray(list) => {
                    t::BindingResource::TextureViewArray(list.iter().map(|v| view_h(v)).collect())
                }
                other => {
                    self.ctx.report(t::GpuError::Unsupported(format!(
                        "binding resource {other:?}"
                    )));
                    return DispatchBindGroup::custom(KBindGroup::new(0));
                }
            };
            entries.push(t::BindGroupEntry {
                binding: e.binding,
                resource,
            });
        }
        let h = self.ctx.take(g::create_bind_group(
            self.ctx.h(),
            &t::BindGroupDescriptor {
                label: label(desc.label),
                layout: handle_of(desc.layout.as_custom::<KBindGroupLayout>()),
                entries,
            },
        ));
        DispatchBindGroup::custom(KBindGroup::new(h))
    }

    fn create_pipeline_layout(
        &self,
        desc: &wgpu::PipelineLayoutDescriptor<'_>,
    ) -> DispatchPipelineLayout {
        let h = self.ctx.take(g::create_pipeline_layout(
            self.ctx.h(),
            &t::PipelineLayoutDescriptor {
                label: label(desc.label),
                #[cfg(not(feature = "wgpu-27"))]
                bind_group_layouts: desc
                    .bind_group_layouts
                    .iter()
                    .map(|l| l.map(|l| handle_of(l.as_custom::<KBindGroupLayout>())))
                    .collect(),
                #[cfg(not(feature = "wgpu-27"))]
                immediate_size: desc.immediate_size,
                #[cfg(feature = "wgpu-27")]
                bind_group_layouts: desc
                    .bind_group_layouts
                    .iter()
                    .map(|l| Some(handle_of(l.as_custom::<KBindGroupLayout>())))
                    .collect(),
                // wgpu 27's push constant ranges are one block of immediates.
                #[cfg(feature = "wgpu-27")]
                immediate_size: desc
                    .push_constant_ranges
                    .iter()
                    .map(|r| r.range.end)
                    .max()
                    .unwrap_or(0),
            },
        ));
        DispatchPipelineLayout::custom(KPipelineLayout::new(h))
    }

    fn create_render_pipeline(
        &self,
        desc: &wgpu::RenderPipelineDescriptor<'_>,
    ) -> DispatchRenderPipeline {
        let v = &desc.vertex;
        let vertex = t::VertexState {
            stage: stage(v.module, v.entry_point, &v.compilation_options),
            buffers: v
                .buffers
                .iter()
                .map(|b| t::VertexBufferLayout {
                    array_stride: b.array_stride,
                    step_mode: match b.step_mode {
                        wgpu::VertexStepMode::Vertex => t::VertexStepMode::Vertex,
                        wgpu::VertexStepMode::Instance => t::VertexStepMode::Instance,
                    },
                    attributes: b
                        .attributes
                        .iter()
                        .map(|a| t::VertexAttribute {
                            format: names::vertex_format_name(a.format).to_string(),
                            offset: a.offset,
                            shader_location: a.shader_location,
                        })
                        .collect(),
                })
                .collect(),
        };
        let p = &desc.primitive;
        let primitive = t::PrimitiveState {
            topology: match p.topology {
                wgpu::PrimitiveTopology::PointList => t::PrimitiveTopology::PointList,
                wgpu::PrimitiveTopology::LineList => t::PrimitiveTopology::LineList,
                wgpu::PrimitiveTopology::LineStrip => t::PrimitiveTopology::LineStrip,
                wgpu::PrimitiveTopology::TriangleList => t::PrimitiveTopology::TriangleList,
                wgpu::PrimitiveTopology::TriangleStrip => t::PrimitiveTopology::TriangleStrip,
            },
            strip_index_format: p.strip_index_format.map(index_format),
            front_face: match p.front_face {
                wgpu::FrontFace::Ccw => t::FrontFace::Ccw,
                wgpu::FrontFace::Cw => t::FrontFace::Cw,
            },
            cull_mode: match p.cull_mode {
                None => t::CullMode::None,
                Some(wgpu::Face::Front) => t::CullMode::Front,
                Some(wgpu::Face::Back) => t::CullMode::Back,
            },
            unclipped_depth: p.unclipped_depth,
            polygon_mode: match p.polygon_mode {
                wgpu::PolygonMode::Fill => t::PolygonMode::Fill,
                wgpu::PolygonMode::Line => t::PolygonMode::Line,
                wgpu::PolygonMode::Point => t::PolygonMode::Point,
            },
            conservative: p.conservative,
        };
        let depth_stencil = desc.depth_stencil.as_ref().map(|ds| t::DepthStencilState {
            format: fmt_name(ds.format),
            #[cfg(not(feature = "wgpu-27"))]
            depth_write_enabled: ds.depth_write_enabled.unwrap_or(false),
            #[cfg(not(feature = "wgpu-27"))]
            depth_compare: compare(ds.depth_compare.unwrap_or(wgpu::CompareFunction::Always)),
            #[cfg(feature = "wgpu-27")]
            depth_write_enabled: ds.depth_write_enabled,
            #[cfg(feature = "wgpu-27")]
            depth_compare: compare(ds.depth_compare),
            stencil_front: stencil_face(&ds.stencil.front),
            stencil_back: stencil_face(&ds.stencil.back),
            stencil_read_mask: ds.stencil.read_mask,
            stencil_write_mask: ds.stencil.write_mask,
            depth_bias: ds.bias.constant,
            depth_bias_slope_scale: ds.bias.slope_scale,
            depth_bias_clamp: ds.bias.clamp,
        });
        let fragment = desc.fragment.as_ref().map(|f| t::FragmentState {
            stage: stage(f.module, f.entry_point, &f.compilation_options),
            targets: f
                .targets
                .iter()
                .map(|c| {
                    c.as_ref().map(|c| t::ColorTargetState {
                        format: fmt_name(c.format),
                        blend: c.blend.map(|b| t::BlendState {
                            color: blend_component(&b.color),
                            alpha: blend_component(&b.alpha),
                        }),
                        write_mask: color_writes(c.write_mask),
                    })
                })
                .collect(),
        });
        let h = self.ctx.take(g::create_render_pipeline(
            self.ctx.h(),
            &t::RenderPipelineDescriptor {
                label: label(desc.label),
                layout: desc
                    .layout
                    .map(|l| handle_of(l.as_custom::<KPipelineLayout>())),
                vertex,
                primitive,
                depth_stencil,
                multisample: t::MultisampleState {
                    count: desc.multisample.count,
                    mask: desc.multisample.mask,
                    alpha_to_coverage_enabled: desc.multisample.alpha_to_coverage_enabled,
                },
                fragment,
                #[cfg(not(feature = "wgpu-27"))]
                multiview_mask: desc.multiview_mask.map(|m| m.get()),
                // wgpu 27 counts views; the mask has that many low bits.
                #[cfg(feature = "wgpu-27")]
                multiview_mask: desc.multiview.map(|n| (1u32 << n.get().min(31)) - 1),
            },
        ));
        DispatchRenderPipeline::custom(KRenderPipeline {
            h: Owned(h),
            ctx: self.ctx.clone(),
        })
    }

    fn create_mesh_pipeline(
        &self,
        _desc: &wgpu::MeshPipelineDescriptor<'_>,
    ) -> DispatchRenderPipeline {
        self.ctx
            .report(t::GpuError::Unsupported("mesh shaders".to_string()));
        DispatchRenderPipeline::custom(KRenderPipeline {
            h: Owned(0),
            ctx: self.ctx.clone(),
        })
    }

    fn create_compute_pipeline(
        &self,
        desc: &wgpu::ComputePipelineDescriptor<'_>,
    ) -> DispatchComputePipeline {
        let h = self.ctx.take(g::create_compute_pipeline(
            self.ctx.h(),
            &t::ComputePipelineDescriptor {
                label: label(desc.label),
                layout: desc
                    .layout
                    .map(|l| handle_of(l.as_custom::<KPipelineLayout>())),
                stage: stage(desc.module, desc.entry_point, &desc.compilation_options),
            },
        ));
        DispatchComputePipeline::custom(KComputePipeline {
            h: Owned(h),
            ctx: self.ctx.clone(),
        })
    }

    unsafe fn create_pipeline_cache(
        &self,
        _desc: &wgpu::PipelineCacheDescriptor<'_>,
    ) -> DispatchPipelineCache {
        DispatchPipelineCache::custom(KPipelineCache)
    }

    fn create_buffer(&self, desc: &wgpu::BufferDescriptor<'_>) -> DispatchBuffer {
        let h = self.ctx.take(g::create_buffer(
            self.ctx.h(),
            &t::BufferDescriptor {
                label: label(desc.label),
                size: desc.size,
                usage: buffer_usages(desc.usage),
                mapped_at_creation: desc.mapped_at_creation,
            },
        ));
        DispatchBuffer::custom(KBuffer {
            h: Owned(h),
            ctx: self.ctx.clone(),
            mapped: Arc::new(Mutex::new(
                (desc.mapped_at_creation && h != 0).then_some(Mapping { zeroed: true }),
            )),
        })
    }

    fn create_texture(&self, desc: &wgpu::TextureDescriptor<'_>) -> DispatchTexture {
        let h = self.ctx.take(g::create_texture(
            self.ctx.h(),
            &t::TextureDescriptor {
                label: label(desc.label),
                size: extent(desc.size),
                mip_level_count: desc.mip_level_count,
                sample_count: desc.sample_count,
                dimension: match desc.dimension {
                    wgpu::TextureDimension::D1 => t::TextureDimension::D1,
                    wgpu::TextureDimension::D2 => t::TextureDimension::D2,
                    wgpu::TextureDimension::D3 => t::TextureDimension::D3,
                },
                format: fmt_name(desc.format),
                usage: texture_usages(desc.usage),
                view_formats: desc.view_formats.iter().map(|f| fmt_name(*f)).collect(),
            },
        ));
        DispatchTexture::custom(KTexture {
            h: Owned(h),
            ctx: self.ctx.clone(),
        })
    }

    fn create_external_texture(
        &self,
        _desc: &wgpu::ExternalTextureDescriptor<'_>,
        _planes: &[&wgpu::TextureView],
    ) -> DispatchExternalTexture {
        self.ctx
            .report(t::GpuError::Unsupported("external textures".to_string()));
        DispatchExternalTexture::custom(KExternalTexture)
    }

    fn create_blas(
        &self,
        _desc: &wgpu::CreateBlasDescriptor<'_>,
        _sizes: wgpu::BlasGeometrySizeDescriptors,
    ) -> (Option<u64>, DispatchBlas) {
        self.ctx
            .report(t::GpuError::Unsupported("ray tracing".to_string()));
        (None, DispatchBlas::custom(KBlas))
    }

    fn create_tlas(&self, _desc: &wgpu::CreateTlasDescriptor<'_>) -> DispatchTlas {
        self.ctx
            .report(t::GpuError::Unsupported("ray tracing".to_string()));
        DispatchTlas::custom(KTlas)
    }

    fn create_sampler(&self, desc: &wgpu::SamplerDescriptor<'_>) -> DispatchSampler {
        let h = self.ctx.take(g::create_sampler(
            self.ctx.h(),
            &t::SamplerDescriptor {
                label: label(desc.label),
                address_mode_u: address(desc.address_mode_u),
                address_mode_v: address(desc.address_mode_v),
                address_mode_w: address(desc.address_mode_w),
                mag_filter: filter(desc.mag_filter),
                min_filter: filter(desc.min_filter),
                mipmap_filter: mip_filter(desc.mipmap_filter),
                lod_min_clamp: desc.lod_min_clamp,
                lod_max_clamp: desc.lod_max_clamp,
                compare: desc.compare.map(compare),
                anisotropy_clamp: desc.anisotropy_clamp,
                border_color: desc.border_color.map(|b| match b {
                    wgpu::SamplerBorderColor::TransparentBlack => t::BorderColor::TransparentBlack,
                    wgpu::SamplerBorderColor::OpaqueBlack => t::BorderColor::OpaqueBlack,
                    wgpu::SamplerBorderColor::OpaqueWhite => t::BorderColor::OpaqueWhite,
                    wgpu::SamplerBorderColor::Zero => t::BorderColor::Zero,
                }),
            },
        ));
        DispatchSampler::custom(KSampler::new(h))
    }

    fn create_query_set(&self, desc: &wgpu::QuerySetDescriptor<'_>) -> DispatchQuerySet {
        let kind = match desc.ty {
            wgpu::QueryType::Occlusion => t::QueryType::Occlusion,
            wgpu::QueryType::Timestamp => t::QueryType::Timestamp,
            _ => {
                self.ctx.report(t::GpuError::Unsupported(
                    "pipeline statistics queries".to_string(),
                ));
                return DispatchQuerySet::custom(KQuerySet::new(0));
            }
        };
        let h = self.ctx.take(g::create_query_set(
            self.ctx.h(),
            &t::QuerySetDescriptor {
                label: label(desc.label),
                kind,
                count: desc.count,
            },
        ));
        DispatchQuerySet::custom(KQuerySet::new(h))
    }

    fn create_command_encoder(
        &self,
        desc: &wgpu::CommandEncoderDescriptor<'_>,
    ) -> DispatchCommandEncoder {
        let h = self.ctx.take(g::create_command_encoder(
            self.ctx.h(),
            label(desc.label).as_deref(),
        ));
        DispatchCommandEncoder::custom(KCommandEncoder {
            h: Owned(h),
            ctx: self.ctx.clone(),
        })
    }

    fn create_render_bundle_encoder(
        &self,
        _desc: &wgpu::RenderBundleEncoderDescriptor<'_>,
    ) -> DispatchRenderBundleEncoder {
        DispatchRenderBundleEncoder::custom(KRenderBundleEncoder {
            commands: Vec::new(),
        })
    }

    fn set_device_lost_callback(&self, callback: BoxDeviceLostCallback) {
        *self.ctx.lost.lock().unwrap() = Some(callback);
    }

    fn on_uncaptured_error(&self, handler: Arc<dyn wgpu::UncapturedErrorHandler>) {
        *self.ctx.handler.lock().unwrap() = Some(handler);
    }

    #[cfg(not(feature = "wgpu-27"))]
    fn push_error_scope(&self, filter: wgpu::ErrorFilter) -> u32 {
        let mut scopes = self.ctx.scopes.lock().unwrap();
        scopes.push((filter, None));
        (scopes.len() - 1) as u32
    }

    #[cfg(not(feature = "wgpu-27"))]
    fn pop_error_scope(&self, _index: u32) -> Pin<Box<dyn PopErrorScopeFuture>> {
        let error = self.ctx.scopes.lock().unwrap().pop().and_then(|(_, e)| e);
        Box::pin(ready(error))
    }

    #[cfg(feature = "wgpu-27")]
    fn push_error_scope(&self, filter: wgpu::ErrorFilter) {
        self.ctx.scopes.lock().unwrap().push((filter, None));
    }

    #[cfg(feature = "wgpu-27")]
    fn pop_error_scope(&self) -> Pin<Box<dyn PopErrorScopeFuture>> {
        let error = self.ctx.scopes.lock().unwrap().pop().and_then(|(_, e)| e);
        Box::pin(ready(error))
    }

    unsafe fn start_graphics_debugger_capture(&self) {}

    unsafe fn stop_graphics_debugger_capture(&self) {}

    fn poll(
        &self,
        poll_type: wgpu::wgt::PollType<u64>,
    ) -> Result<wgpu::PollStatus, wgpu::PollError> {
        let wait = !matches!(poll_type, wgpu::wgt::PollType::Poll);
        let empty = match g::poll(self.ctx.h(), wait) {
            Ok(empty) => empty,
            Err(e) => {
                self.ctx.report(e);
                true
            }
        };
        run_pending();
        Ok(if wait {
            wgpu::PollStatus::WaitSucceeded
        } else if empty {
            wgpu::PollStatus::QueueEmpty
        } else {
            wgpu::PollStatus::Poll
        })
    }

    fn get_internal_counters(&self) -> wgpu::InternalCounters {
        Default::default()
    }

    fn generate_allocator_report(&self) -> Option<wgpu::AllocatorReport> {
        None
    }

    fn destroy(&self) {
        self.ctx.check(g::destroy_device(self.ctx.h()));
    }
}

#[derive(Debug)]
struct KQueue {
    ctx: Arc<DeviceCtx>,
}

#[derive(Debug)]
struct KQueueWriteBuffer {
    data: Vec<u8>,
}

impl QueueWriteBufferInterface for KQueueWriteBuffer {
    #[cfg(not(feature = "wgpu-27"))]
    fn len(&self) -> usize {
        self.data.len()
    }

    #[cfg(not(feature = "wgpu-27"))]
    unsafe fn write_slice(&mut self) -> wgpu::WriteOnly<'_, [u8]> {
        wgpu::WriteOnly::from_mut(&mut self.data[..])
    }

    #[cfg(feature = "wgpu-27")]
    fn slice(&self) -> &[u8] {
        &self.data
    }

    #[cfg(feature = "wgpu-27")]
    fn slice_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }
}

impl QueueInterface for KQueue {
    fn write_buffer(&self, buffer: &DispatchBuffer, offset: wgpu::BufferAddress, data: &[u8]) {
        self.ctx.check(g::write_buffer(
            self.ctx.h(),
            dbuffer_h(buffer),
            offset,
            data,
        ));
    }

    fn create_staging_buffer(&self, size: wgpu::BufferSize) -> Option<DispatchQueueWriteBuffer> {
        Some(DispatchQueueWriteBuffer::custom(KQueueWriteBuffer {
            data: vec![0; size.get() as usize],
        }))
    }

    fn validate_write_buffer(
        &self,
        _buffer: &DispatchBuffer,
        _offset: wgpu::BufferAddress,
        _size: wgpu::BufferSize,
    ) -> Option<()> {
        Some(())
    }

    fn write_staging_buffer(
        &self,
        buffer: &DispatchBuffer,
        offset: wgpu::BufferAddress,
        staging: &DispatchQueueWriteBuffer,
    ) {
        if let Some(staging) = staging.as_custom::<KQueueWriteBuffer>() {
            self.write_buffer(buffer, offset, &staging.data);
        }
    }

    fn write_texture(
        &self,
        texture: wgpu::TexelCopyTextureInfo<'_>,
        data: &[u8],
        layout: wgpu::TexelCopyBufferLayout,
        size: wgpu::Extent3d,
    ) {
        self.ctx.check(g::write_texture(
            self.ctx.h(),
            texture_copy(&texture),
            data,
            copy_layout(layout),
            extent(size),
        ));
    }

    fn submit(&self, command_buffers: &mut dyn Iterator<Item = DispatchCommandBuffer>) -> u64 {
        let buffers: Vec<DispatchCommandBuffer> = command_buffers.collect();
        let handles: Vec<u64> = buffers
            .iter()
            .map(|b| handle_of(b.as_custom::<KCommandBuffer>()))
            .filter(|h| *h != 0)
            .collect();
        let index = match g::submit(self.ctx.h(), &handles) {
            Ok(index) => index,
            Err(e) => {
                self.ctx.report(e);
                0
            }
        };
        // Submitted buffers are spent on the host; releasing their handles
        // afterwards is a no-op there (handles are never reused).
        drop(buffers);
        run_pending();
        index
    }

    fn get_timestamp_period(&self) -> f32 {
        g::timestamp_period(self.ctx.h()).unwrap_or(1.0)
    }

    fn on_submitted_work_done(&self, callback: BoxSubmittedWorkDoneCallback) {
        defer(callback);
    }

    fn compact_blas(&self, _blas: &DispatchBlas) -> (Option<u64>, DispatchBlas) {
        (None, DispatchBlas::custom(KBlas))
    }
}

// ---------------------------------------------------------------------------
// Buffers and textures

#[derive(Debug, Clone)]
struct Mapping {
    /// The host checks the range and the mode; this side only needs to know
    /// whether there is anything to read back.
    ///
    /// Mapped at creation: the contents are zeros, no need to read them.
    zeroed: bool,
}

#[derive(Debug)]
struct KBuffer {
    h: Owned,
    ctx: Arc<DeviceCtx>,
    mapped: Arc<Mutex<Option<Mapping>>>,
}

#[derive(Debug)]
struct KMappedRange {
    buffer: u64,
    offset: u64,
    data: Vec<u8>,
    modified: bool,
    ctx: Arc<DeviceCtx>,
}

impl BufferMappedRangeInterface for KMappedRange {
    #[cfg(not(feature = "wgpu-27"))]
    fn len(&self) -> usize {
        self.data.len()
    }

    #[cfg(not(feature = "wgpu-27"))]
    unsafe fn read_slice(&self) -> &[u8] {
        &self.data
    }

    #[cfg(not(feature = "wgpu-27"))]
    unsafe fn write_slice(&mut self) -> wgpu::WriteOnly<'_, [u8]> {
        self.modified = true;
        wgpu::WriteOnly::from_mut(&mut self.data[..])
    }

    #[cfg(feature = "wgpu-27")]
    fn slice(&self) -> &[u8] {
        &self.data
    }

    /// wgpu 27 cannot say whether a mutable view was written; assume it was.
    #[cfg(feature = "wgpu-27")]
    fn slice_mut(&mut self) -> &mut [u8] {
        self.modified = true;
        &mut self.data
    }
}

impl Drop for KMappedRange {
    fn drop(&mut self) {
        if self.modified && !self.data.is_empty() {
            self.ctx
                .check(g::write_mapped(self.buffer, self.offset, &self.data));
        }
    }
}

impl BufferInterface for KBuffer {
    fn map_async(
        &self,
        mode: wgpu::MapMode,
        range: Range<wgpu::BufferAddress>,
        callback: BufferMapCallback,
    ) {
        let write = mode == wgpu::MapMode::Write;
        let result = g::begin_map_buffer(
            self.h.0,
            if write {
                t::MapMode::Write
            } else {
                t::MapMode::Read
            },
            range.start,
            range.end - range.start,
        );
        match result {
            // Finished by a later poll or submit, once the GPU is done with
            // the buffer -- as WebGPU's mapAsync is. Waiting here made every
            // map wait for all the GPU's work, so an engine that reads a few
            // bytes back each frame (Bevy) ran its CPU and GPU in turn.
            Ok(()) => PENDING_MAPS.lock().unwrap().push(PendingMap {
                buffer: self.h.0,
                mapped: Arc::clone(&self.mapped),
                callback,
            }),
            Err(e) => {
                eprintln!("wgpu-krate: map_async refused: {}", error_text(&e));
                defer(Box::new(move || callback(Err(wgpu::BufferAsyncError))));
            }
        }
    }

    fn get_mapped_range(&self, sub_range: Range<wgpu::BufferAddress>) -> DispatchBufferMappedRange {
        let len = (sub_range.end - sub_range.start) as usize;
        let mapping = self.mapped.lock().unwrap().clone();
        let data = match &mapping {
            Some(m) if m.zeroed => vec![0; len],
            Some(_) => match g::read_mapped(self.h.0, sub_range.start, len as u64) {
                Ok(bytes) => bytes,
                Err(e) => {
                    self.ctx.report(e);
                    vec![0; len]
                }
            },
            None => vec![0; len],
        };
        DispatchBufferMappedRange::custom(KMappedRange {
            buffer: self.h.0,
            offset: sub_range.start,
            data,
            modified: false,
            ctx: self.ctx.clone(),
        })
    }

    fn unmap(&self) {
        self.mapped.lock().unwrap().take();
        self.ctx.check(g::unmap_buffer(self.h.0));
    }

    fn destroy(&self) {
        self.ctx.check(g::destroy_buffer(self.h.0));
    }
}

#[derive(Debug)]
struct KTexture {
    h: Owned,
    ctx: Arc<DeviceCtx>,
}

impl TextureInterface for KTexture {
    fn create_view(&self, desc: &wgpu::TextureViewDescriptor<'_>) -> DispatchTextureView {
        let h = self.ctx.take(g::create_texture_view(
            self.h.0,
            &t::TextureViewDescriptor {
                label: label(desc.label),
                format: desc.format.map(fmt_name),
                dimension: desc.dimension.map(view_dimension),
                usage: desc.usage.map(texture_usages),
                aspect: aspect(desc.aspect),
                base_mip_level: desc.base_mip_level,
                mip_level_count: desc.mip_level_count,
                base_array_layer: desc.base_array_layer,
                array_layer_count: desc.array_layer_count,
            },
        ));
        DispatchTextureView::custom(KTextureView::new(h))
    }

    fn destroy(&self) {
        self.ctx.check(g::destroy_texture(self.h.0));
    }
}

// ---------------------------------------------------------------------------
// Commands

#[derive(Debug)]
struct KCommandEncoder {
    h: Owned,
    ctx: Arc<DeviceCtx>,
}

#[derive(Debug)]
struct KRenderPass {
    encoder: u64,
    ctx: Arc<DeviceCtx>,
    desc: Option<t::RenderPassDescriptor>,
    commands: Vec<t::RenderCommand>,
}

#[derive(Debug)]
struct KComputePass {
    encoder: u64,
    ctx: Arc<DeviceCtx>,
    desc: Option<t::ComputePassDescriptor>,
    commands: Vec<t::ComputeCommand>,
}

impl Drop for KRenderPass {
    fn drop(&mut self) {
        if let Some(desc) = self.desc.take() {
            self.ctx
                .check(g::encode_render_pass(self.encoder, &desc, &self.commands));
        }
    }
}

impl Drop for KComputePass {
    fn drop(&mut self) {
        if let Some(desc) = self.desc.take() {
            self.ctx
                .check(g::encode_compute_pass(self.encoder, &desc, &self.commands));
        }
    }
}

fn timestamps(
    query_set: &wgpu::QuerySet,
    begin: Option<u32>,
    end: Option<u32>,
) -> t::PassTimestampWrites {
    t::PassTimestampWrites {
        query_set: query_h(query_set),
        beginning_of_pass_write_index: begin,
        end_of_pass_write_index: end,
    }
}

impl CommandEncoderInterface for KCommandEncoder {
    fn copy_buffer_to_buffer(
        &self,
        source: &DispatchBuffer,
        source_offset: wgpu::BufferAddress,
        destination: &DispatchBuffer,
        destination_offset: wgpu::BufferAddress,
        copy_size: Option<wgpu::BufferAddress>,
    ) {
        self.ctx.check(g::copy_buffer_to_buffer(
            self.h.0,
            dbuffer_h(source),
            source_offset,
            dbuffer_h(destination),
            destination_offset,
            copy_size,
        ));
    }

    fn copy_buffer_to_texture(
        &self,
        source: wgpu::TexelCopyBufferInfo<'_>,
        destination: wgpu::TexelCopyTextureInfo<'_>,
        copy_size: wgpu::Extent3d,
    ) {
        self.ctx.check(g::copy_buffer_to_texture(
            self.h.0,
            buffer_copy(&source),
            texture_copy(&destination),
            extent(copy_size),
        ));
    }

    fn copy_texture_to_buffer(
        &self,
        source: wgpu::TexelCopyTextureInfo<'_>,
        destination: wgpu::TexelCopyBufferInfo<'_>,
        copy_size: wgpu::Extent3d,
    ) {
        self.ctx.check(g::copy_texture_to_buffer(
            self.h.0,
            texture_copy(&source),
            buffer_copy(&destination),
            extent(copy_size),
        ));
    }

    fn copy_texture_to_texture(
        &self,
        source: wgpu::TexelCopyTextureInfo<'_>,
        destination: wgpu::TexelCopyTextureInfo<'_>,
        copy_size: wgpu::Extent3d,
    ) {
        self.ctx.check(g::copy_texture_to_texture(
            self.h.0,
            texture_copy(&source),
            texture_copy(&destination),
            extent(copy_size),
        ));
    }

    fn begin_compute_pass(&self, desc: &wgpu::ComputePassDescriptor<'_>) -> DispatchComputePass {
        DispatchComputePass::custom(KComputePass {
            encoder: self.h.0,
            ctx: self.ctx.clone(),
            desc: Some(t::ComputePassDescriptor {
                label: label(desc.label),
                timestamp_writes: desc.timestamp_writes.as_ref().map(|w| {
                    timestamps(
                        w.query_set,
                        w.beginning_of_pass_write_index,
                        w.end_of_pass_write_index,
                    )
                }),
            }),
            commands: Vec::new(),
        })
    }

    fn begin_render_pass(&self, desc: &wgpu::RenderPassDescriptor<'_>) -> DispatchRenderPass {
        let color_attachments = desc
            .color_attachments
            .iter()
            .map(|a| {
                a.as_ref().map(|a| t::RenderPassColorAttachment {
                    view: view_h(a.view),
                    depth_slice: a.depth_slice,
                    resolve_target: a.resolve_target.map(view_h),
                    load: match a.ops.load {
                        wgpu::LoadOp::Clear(c) => t::ColorLoadOp::Clear(color(c)),
                        _ => t::ColorLoadOp::Load,
                    },
                    store: store(a.ops.store),
                })
            })
            .collect();
        let depth_stencil_attachment =
            desc.depth_stencil_attachment
                .as_ref()
                .map(|d| t::RenderPassDepthStencilAttachment {
                    view: view_h(d.view),
                    depth_ops: d.depth_ops.map(|o| t::DepthOps {
                        load: match o.load {
                            wgpu::LoadOp::Clear(v) => t::DepthLoadOp::Clear(v),
                            _ => t::DepthLoadOp::Load,
                        },
                        store: store(o.store),
                    }),
                    stencil_ops: d.stencil_ops.map(|o| t::StencilOps {
                        load: match o.load {
                            wgpu::LoadOp::Clear(v) => t::StencilLoadOp::Clear(v),
                            _ => t::StencilLoadOp::Load,
                        },
                        store: store(o.store),
                    }),
                });
        DispatchRenderPass::custom(KRenderPass {
            encoder: self.h.0,
            ctx: self.ctx.clone(),
            desc: Some(t::RenderPassDescriptor {
                label: label(desc.label),
                color_attachments,
                depth_stencil_attachment,
                timestamp_writes: desc.timestamp_writes.as_ref().map(|w| {
                    timestamps(
                        w.query_set,
                        w.beginning_of_pass_write_index,
                        w.end_of_pass_write_index,
                    )
                }),
                occlusion_query_set: desc.occlusion_query_set.map(query_h),
                #[cfg(not(feature = "wgpu-27"))]
                multiview_mask: desc.multiview_mask.map(|m| m.get()),
                #[cfg(feature = "wgpu-27")]
                multiview_mask: None,
            }),
            commands: Vec::new(),
        })
    }

    fn finish(&mut self) -> DispatchCommandBuffer {
        let h = self.ctx.take(g::finish_encoder(self.h.0, None));
        // The host spent the encoder; nothing is left to release.
        self.h.0 = 0;
        DispatchCommandBuffer::custom(KCommandBuffer::new(h))
    }

    fn clear_texture(&self, _texture: &DispatchTexture, _range: &wgpu::ImageSubresourceRange) {
        self.ctx
            .report(t::GpuError::Unsupported("clear_texture".to_string()));
    }

    fn clear_buffer(
        &self,
        buffer: &DispatchBuffer,
        offset: wgpu::BufferAddress,
        size: Option<wgpu::BufferAddress>,
    ) {
        self.ctx
            .check(g::clear_buffer(self.h.0, dbuffer_h(buffer), offset, size));
    }

    fn insert_debug_marker(&self, _label: &str) {}
    fn push_debug_group(&self, _label: &str) {}
    fn pop_debug_group(&self) {}

    fn write_timestamp(&self, query_set: &DispatchQuerySet, query_index: u32) {
        self.ctx.check(g::write_timestamp(
            self.h.0,
            dquery_h(query_set),
            query_index,
        ));
    }

    fn resolve_query_set(
        &self,
        query_set: &DispatchQuerySet,
        first_query: u32,
        query_count: u32,
        destination: &DispatchBuffer,
        destination_offset: wgpu::BufferAddress,
    ) {
        self.ctx.check(g::resolve_query_set(
            self.h.0,
            dquery_h(query_set),
            first_query,
            query_count,
            dbuffer_h(destination),
            destination_offset,
        ));
    }

    fn mark_acceleration_structures_built<'a>(
        &self,
        _blas: &mut dyn Iterator<Item = &'a wgpu::Blas>,
        _tlas: &mut dyn Iterator<Item = &'a wgpu::Tlas>,
    ) {
    }

    fn build_acceleration_structures<'a>(
        &self,
        _blas: &mut dyn Iterator<Item = &'a wgpu::BlasBuildEntry<'a>>,
        _tlas: &mut dyn Iterator<Item = &'a wgpu::Tlas>,
    ) {
        self.ctx
            .report(t::GpuError::Unsupported("ray tracing".to_string()));
    }

    fn transition_resources<'a>(
        &mut self,
        _buffers: &mut dyn Iterator<Item = wgpu::wgt::BufferTransition<&'a DispatchBuffer>>,
        _textures: &mut dyn Iterator<Item = wgpu::wgt::TextureTransition<&'a DispatchTexture>>,
    ) {
    }
}

fn set_bind_group(
    index: u32,
    group: Option<&DispatchBindGroup>,
    offsets: &[u32],
) -> t::SetBindGroup {
    t::SetBindGroup {
        index,
        group: group.map(|g| handle_of(g.as_custom::<KBindGroup>())),
        offsets: offsets.to_vec(),
    }
}

impl ComputePassInterface for KComputePass {
    fn set_pipeline(&mut self, pipeline: &DispatchComputePipeline) {
        self.commands.push(t::ComputeCommand::SetPipeline(handle_of(
            pipeline.as_custom::<KComputePipeline>(),
        )));
    }

    fn set_bind_group(
        &mut self,
        index: u32,
        group: Option<&DispatchBindGroup>,
        offsets: &[wgpu::DynamicOffset],
    ) {
        self.commands
            .push(t::ComputeCommand::SetBindGroup(set_bind_group(
                index, group, offsets,
            )));
    }

    #[cfg(not(feature = "wgpu-27"))]
    fn set_immediates(&mut self, offset: u32, data: &[u8]) {
        self.commands
            .push(t::ComputeCommand::SetImmediates(t::SetImmediates {
                offset,
                data: data.to_vec(),
            }));
    }

    /// wgpu 27's push constants are what the host calls immediates.
    #[cfg(feature = "wgpu-27")]
    fn set_push_constants(&mut self, offset: u32, data: &[u8]) {
        self.commands
            .push(t::ComputeCommand::SetImmediates(t::SetImmediates {
                offset,
                data: data.to_vec(),
            }));
    }

    #[cfg(feature = "wgpu-27")]
    fn end(&mut self) {
        if let Some(desc) = self.desc.take() {
            self.ctx
                .check(g::encode_compute_pass(self.encoder, &desc, &self.commands));
        }
    }

    fn insert_debug_marker(&mut self, _label: &str) {}
    fn push_debug_group(&mut self, _label: &str) {}
    fn pop_debug_group(&mut self) {}

    fn write_timestamp(&mut self, query_set: &DispatchQuerySet, index: u32) {
        self.commands
            .push(t::ComputeCommand::WriteTimestamp(t::QueryWrite {
                query_set: dquery_h(query_set),
                index,
            }));
    }

    fn begin_pipeline_statistics_query(&mut self, _q: &DispatchQuerySet, _i: u32) {}
    fn end_pipeline_statistics_query(&mut self) {}

    fn dispatch_workgroups(&mut self, x: u32, y: u32, z: u32) {
        self.commands
            .push(t::ComputeCommand::Dispatch(t::Origin3d { x, y, z }));
    }

    fn dispatch_workgroups_indirect(
        &mut self,
        buffer: &DispatchBuffer,
        offset: wgpu::BufferAddress,
    ) {
        self.commands
            .push(t::ComputeCommand::DispatchIndirect(t::Indirect {
                buffer: dbuffer_h(buffer),
                offset,
            }));
    }
}

/// The commands render passes and render bundles share.
trait Recorder {
    fn push(&mut self, command: t::RenderCommand);
}

impl Recorder for KRenderPass {
    fn push(&mut self, command: t::RenderCommand) {
        self.commands.push(command);
    }
}

#[derive(Debug)]
struct KRenderBundleEncoder {
    commands: Vec<t::RenderCommand>,
}

impl Recorder for KRenderBundleEncoder {
    fn push(&mut self, command: t::RenderCommand) {
        self.commands.push(command);
    }
}

#[derive(Debug)]
struct KRenderBundle {
    commands: Vec<t::RenderCommand>,
}

impl RenderBundleInterface for KRenderBundle {}

fn rec_pipeline(r: &mut impl Recorder, pipeline: &DispatchRenderPipeline) {
    r.push(t::RenderCommand::SetPipeline(handle_of(
        pipeline.as_custom::<KRenderPipeline>(),
    )));
}

fn rec_index(
    r: &mut impl Recorder,
    buffer: &DispatchBuffer,
    format: wgpu::IndexFormat,
    offset: wgpu::BufferAddress,
    size: Option<wgpu::BufferSize>,
) {
    r.push(t::RenderCommand::SetIndexBuffer(t::SetIndexBuffer {
        buffer: dbuffer_h(buffer),
        format: index_format(format),
        offset,
        size: size.map(|s| s.get()),
    }));
}

fn rec_vertex(
    r: &mut impl Recorder,
    slot: u32,
    buffer: &DispatchBuffer,
    offset: wgpu::BufferAddress,
    size: Option<wgpu::BufferSize>,
) {
    r.push(t::RenderCommand::SetVertexBuffer(t::SetVertexBuffer {
        slot,
        buffer: dbuffer_h(buffer),
        offset,
        size: size.map(|s| s.get()),
    }));
}

fn rec_draw(r: &mut impl Recorder, vertices: Range<u32>, instances: Range<u32>) {
    r.push(t::RenderCommand::Draw(t::Draw {
        vertex_count: vertices.end.saturating_sub(vertices.start),
        instance_count: instances.end.saturating_sub(instances.start),
        first_vertex: vertices.start,
        first_instance: instances.start,
    }));
}

fn rec_draw_indexed(
    r: &mut impl Recorder,
    indices: Range<u32>,
    base_vertex: i32,
    instances: Range<u32>,
) {
    r.push(t::RenderCommand::DrawIndexed(t::DrawIndexed {
        index_count: indices.end.saturating_sub(indices.start),
        instance_count: instances.end.saturating_sub(instances.start),
        first_index: indices.start,
        base_vertex,
        first_instance: instances.start,
    }));
}

fn indirect(buffer: &DispatchBuffer, offset: wgpu::BufferAddress) -> t::Indirect {
    t::Indirect {
        buffer: dbuffer_h(buffer),
        offset,
    }
}

impl RenderPassInterface for KRenderPass {
    fn set_pipeline(&mut self, pipeline: &DispatchRenderPipeline) {
        rec_pipeline(self, pipeline);
    }

    fn set_bind_group(
        &mut self,
        index: u32,
        group: Option<&DispatchBindGroup>,
        offsets: &[wgpu::DynamicOffset],
    ) {
        self.push(t::RenderCommand::SetBindGroup(set_bind_group(
            index, group, offsets,
        )));
    }

    fn set_index_buffer(
        &mut self,
        buffer: &DispatchBuffer,
        format: wgpu::IndexFormat,
        offset: wgpu::BufferAddress,
        size: Option<wgpu::BufferSize>,
    ) {
        rec_index(self, buffer, format, offset, size);
    }

    fn set_vertex_buffer(
        &mut self,
        slot: u32,
        buffer: &DispatchBuffer,
        offset: wgpu::BufferAddress,
        size: Option<wgpu::BufferSize>,
    ) {
        rec_vertex(self, slot, buffer, offset, size);
    }

    #[cfg(not(feature = "wgpu-27"))]
    fn set_immediates(&mut self, offset: u32, data: &[u8]) {
        self.push(t::RenderCommand::SetImmediates(t::SetImmediates {
            offset,
            data: data.to_vec(),
        }));
    }

    #[cfg(feature = "wgpu-27")]
    fn set_push_constants(&mut self, _stages: wgpu::ShaderStages, offset: u32, data: &[u8]) {
        self.push(t::RenderCommand::SetImmediates(t::SetImmediates {
            offset,
            data: data.to_vec(),
        }));
    }

    #[cfg(feature = "wgpu-27")]
    fn end(&mut self) {
        if let Some(desc) = self.desc.take() {
            self.ctx
                .check(g::encode_render_pass(self.encoder, &desc, &self.commands));
        }
    }

    fn set_blend_constant(&mut self, c: wgpu::Color) {
        self.push(t::RenderCommand::SetBlendConstant(color(c)));
    }

    fn set_scissor_rect(&mut self, x: u32, y: u32, width: u32, height: u32) {
        self.push(t::RenderCommand::SetScissorRect(t::ScissorRect {
            x,
            y,
            width,
            height,
        }));
    }

    fn set_viewport(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        min_depth: f32,
        max_depth: f32,
    ) {
        self.push(t::RenderCommand::SetViewport(t::Viewport {
            x,
            y,
            width,
            height,
            min_depth,
            max_depth,
        }));
    }

    fn set_stencil_reference(&mut self, reference: u32) {
        self.push(t::RenderCommand::SetStencilReference(reference));
    }

    fn draw(&mut self, vertices: Range<u32>, instances: Range<u32>) {
        rec_draw(self, vertices, instances);
    }

    fn draw_indexed(&mut self, indices: Range<u32>, base_vertex: i32, instances: Range<u32>) {
        rec_draw_indexed(self, indices, base_vertex, instances);
    }

    fn draw_mesh_tasks(&mut self, _x: u32, _y: u32, _z: u32) {
        self.ctx
            .report(t::GpuError::Unsupported("mesh shaders".to_string()));
    }

    fn draw_indirect(&mut self, buffer: &DispatchBuffer, offset: wgpu::BufferAddress) {
        self.push(t::RenderCommand::DrawIndirect(indirect(buffer, offset)));
    }

    fn draw_indexed_indirect(&mut self, buffer: &DispatchBuffer, offset: wgpu::BufferAddress) {
        self.push(t::RenderCommand::DrawIndexedIndirect(indirect(
            buffer, offset,
        )));
    }

    fn draw_mesh_tasks_indirect(&mut self, _buffer: &DispatchBuffer, _offset: wgpu::BufferAddress) {
        self.ctx
            .report(t::GpuError::Unsupported("mesh shaders".to_string()));
    }

    fn multi_draw_indirect(
        &mut self,
        buffer: &DispatchBuffer,
        offset: wgpu::BufferAddress,
        count: u32,
    ) {
        self.push(t::RenderCommand::MultiDrawIndirect(t::MultiIndirect {
            buffer: dbuffer_h(buffer),
            offset,
            count,
        }));
    }

    fn multi_draw_indexed_indirect(
        &mut self,
        buffer: &DispatchBuffer,
        offset: wgpu::BufferAddress,
        count: u32,
    ) {
        self.push(t::RenderCommand::MultiDrawIndexedIndirect(
            t::MultiIndirect {
                buffer: dbuffer_h(buffer),
                offset,
                count,
            },
        ));
    }

    fn multi_draw_indirect_count(
        &mut self,
        buffer: &DispatchBuffer,
        offset: wgpu::BufferAddress,
        count_buffer: &DispatchBuffer,
        count_offset: wgpu::BufferAddress,
        max_count: u32,
    ) {
        self.push(t::RenderCommand::MultiDrawIndirectCount(
            t::MultiIndirectCount {
                buffer: dbuffer_h(buffer),
                offset,
                count_buffer: dbuffer_h(count_buffer),
                count_offset,
                max_count,
            },
        ));
    }

    fn multi_draw_mesh_tasks_indirect(
        &mut self,
        _b: &DispatchBuffer,
        _o: wgpu::BufferAddress,
        _c: u32,
    ) {
        self.ctx
            .report(t::GpuError::Unsupported("mesh shaders".to_string()));
    }

    fn multi_draw_indexed_indirect_count(
        &mut self,
        buffer: &DispatchBuffer,
        offset: wgpu::BufferAddress,
        count_buffer: &DispatchBuffer,
        count_offset: wgpu::BufferAddress,
        max_count: u32,
    ) {
        self.push(t::RenderCommand::MultiDrawIndexedIndirectCount(
            t::MultiIndirectCount {
                buffer: dbuffer_h(buffer),
                offset,
                count_buffer: dbuffer_h(count_buffer),
                count_offset,
                max_count,
            },
        ));
    }

    fn multi_draw_mesh_tasks_indirect_count(
        &mut self,
        _b: &DispatchBuffer,
        _o: wgpu::BufferAddress,
        _cb: &DispatchBuffer,
        _co: wgpu::BufferAddress,
        _m: u32,
    ) {
        self.ctx
            .report(t::GpuError::Unsupported("mesh shaders".to_string()));
    }

    fn insert_debug_marker(&mut self, _label: &str) {}
    fn push_debug_group(&mut self, _label: &str) {}
    fn pop_debug_group(&mut self) {}

    fn write_timestamp(&mut self, query_set: &DispatchQuerySet, index: u32) {
        self.push(t::RenderCommand::WriteTimestamp(t::QueryWrite {
            query_set: dquery_h(query_set),
            index,
        }));
    }

    fn begin_occlusion_query(&mut self, index: u32) {
        self.push(t::RenderCommand::BeginOcclusionQuery(index));
    }

    fn end_occlusion_query(&mut self) {
        self.push(t::RenderCommand::EndOcclusionQuery);
    }

    fn begin_pipeline_statistics_query(&mut self, _q: &DispatchQuerySet, _i: u32) {}
    fn end_pipeline_statistics_query(&mut self) {}

    fn execute_bundles(&mut self, bundles: &mut dyn Iterator<Item = &DispatchRenderBundle>) {
        for bundle in bundles {
            if let Some(bundle) = bundle.as_custom::<KRenderBundle>() {
                self.commands.extend(bundle.commands.iter().cloned());
            }
        }
    }
}

impl RenderBundleEncoderInterface for KRenderBundleEncoder {
    fn set_pipeline(&mut self, pipeline: &DispatchRenderPipeline) {
        rec_pipeline(self, pipeline);
    }

    fn set_bind_group(
        &mut self,
        index: u32,
        group: Option<&DispatchBindGroup>,
        offsets: &[wgpu::DynamicOffset],
    ) {
        self.push(t::RenderCommand::SetBindGroup(set_bind_group(
            index, group, offsets,
        )));
    }

    fn set_index_buffer(
        &mut self,
        buffer: &DispatchBuffer,
        format: wgpu::IndexFormat,
        offset: wgpu::BufferAddress,
        size: Option<wgpu::BufferSize>,
    ) {
        rec_index(self, buffer, format, offset, size);
    }

    fn set_vertex_buffer(
        &mut self,
        slot: u32,
        buffer: &DispatchBuffer,
        offset: wgpu::BufferAddress,
        size: Option<wgpu::BufferSize>,
    ) {
        rec_vertex(self, slot, buffer, offset, size);
    }

    #[cfg(not(feature = "wgpu-27"))]
    fn set_immediates(&mut self, offset: u32, data: &[u8]) {
        self.push(t::RenderCommand::SetImmediates(t::SetImmediates {
            offset,
            data: data.to_vec(),
        }));
    }

    #[cfg(feature = "wgpu-27")]
    fn set_push_constants(&mut self, _stages: wgpu::ShaderStages, offset: u32, data: &[u8]) {
        self.push(t::RenderCommand::SetImmediates(t::SetImmediates {
            offset,
            data: data.to_vec(),
        }));
    }

    fn draw(&mut self, vertices: Range<u32>, instances: Range<u32>) {
        rec_draw(self, vertices, instances);
    }

    fn draw_indexed(&mut self, indices: Range<u32>, base_vertex: i32, instances: Range<u32>) {
        rec_draw_indexed(self, indices, base_vertex, instances);
    }

    fn draw_indirect(&mut self, buffer: &DispatchBuffer, offset: wgpu::BufferAddress) {
        self.push(t::RenderCommand::DrawIndirect(indirect(buffer, offset)));
    }

    fn draw_indexed_indirect(&mut self, buffer: &DispatchBuffer, offset: wgpu::BufferAddress) {
        self.push(t::RenderCommand::DrawIndexedIndirect(indirect(
            buffer, offset,
        )));
    }

    fn finish(self, _desc: &wgpu::RenderBundleDescriptor<'_>) -> DispatchRenderBundle {
        DispatchRenderBundle::custom(KRenderBundle {
            commands: self.commands,
        })
    }
}

// ---------------------------------------------------------------------------
// Surfaces

#[derive(Debug)]
struct KSurface {
    h: Owned,
    /// The device the surface was configured for, for its errors.
    device: Mutex<Option<Arc<DeviceCtx>>>,
}

#[derive(Debug)]
struct KSurfaceOutputDetail {
    surface: u64,
    ctx: Option<Arc<DeviceCtx>>,
}

fn present_mode(m: t::PresentMode) -> wgpu::PresentMode {
    match m {
        t::PresentMode::Fifo => wgpu::PresentMode::Fifo,
        t::PresentMode::FifoRelaxed => wgpu::PresentMode::FifoRelaxed,
        t::PresentMode::Immediate => wgpu::PresentMode::Immediate,
        t::PresentMode::Mailbox => wgpu::PresentMode::Mailbox,
    }
}

impl SurfaceInterface for KSurface {
    fn get_capabilities(&self, adapter: &DispatchAdapter) -> wgpu::SurfaceCapabilities {
        let adapter = handle_of_adapter(adapter);
        match g::get_surface_capabilities(self.h.0, adapter) {
            Ok(caps) => wgpu::SurfaceCapabilities {
                formats: caps
                    .formats
                    .iter()
                    .filter_map(|f| names::parse_format(f))
                    .collect(),
                present_modes: caps.present_modes.into_iter().map(present_mode).collect(),
                alpha_modes: vec![wgpu::CompositeAlphaMode::Opaque],
                usages: texture_usages_in(caps.usages),
            },
            Err(e) => {
                eprintln!("wgpu-krate: surface capabilities: {}", error_text(&e));
                wgpu::SurfaceCapabilities::default()
            }
        }
    }

    fn configure(&self, device: &DispatchDevice, config: &wgpu::SurfaceConfiguration) {
        let Some(device) = device.as_custom::<KDevice>() else {
            return;
        };
        *self.device.lock().unwrap() = Some(device.ctx.clone());
        device.ctx.check(g::configure_surface(
            self.h.0,
            device.ctx.h(),
            &t::SurfaceConfiguration {
                usage: texture_usages(config.usage),
                format: fmt_name(config.format),
                width: config.width,
                height: config.height,
                present_mode: match config.present_mode {
                    wgpu::PresentMode::Immediate | wgpu::PresentMode::AutoNoVsync => {
                        t::PresentMode::Immediate
                    }
                    _ => t::PresentMode::Fifo,
                },
                alpha_mode: t::CompositeAlphaMode::Opaque,
                view_formats: config.view_formats.iter().map(|f| fmt_name(*f)).collect(),
                desired_maximum_frame_latency: config.desired_maximum_frame_latency,
            },
        ));
    }

    fn get_current_texture(
        &self,
    ) -> (
        Option<DispatchTexture>,
        wgpu::SurfaceStatus,
        DispatchSurfaceOutputDetail,
    ) {
        let ctx = self.device.lock().unwrap().clone();
        let detail = DispatchSurfaceOutputDetail::custom(KSurfaceOutputDetail {
            surface: self.h.0,
            ctx: ctx.clone(),
        });
        let Some(ctx) = ctx else {
            return (None, wgpu::SurfaceStatus::Outdated, detail);
        };
        match g::current_texture(self.h.0) {
            Ok(frame) => (
                Some(DispatchTexture::custom(KTexture {
                    h: Owned(frame.texture),
                    ctx,
                })),
                if frame.suboptimal {
                    wgpu::SurfaceStatus::Suboptimal
                } else {
                    wgpu::SurfaceStatus::Good
                },
                detail,
            ),
            #[cfg(not(feature = "wgpu-27"))]
            Err(t::SurfaceError::Occluded) => (None, wgpu::SurfaceStatus::Occluded, detail),
            #[cfg(feature = "wgpu-27")]
            Err(t::SurfaceError::Occluded) => (None, wgpu::SurfaceStatus::Timeout, detail),
            Err(t::SurfaceError::Lost(_)) => (None, wgpu::SurfaceStatus::Lost, detail),
            Err(_) => (None, wgpu::SurfaceStatus::Outdated, detail),
        }
    }
}

fn handle_of_adapter(adapter: &DispatchAdapter) -> u64 {
    adapter.as_custom::<KAdapter>().map(|a| a.h.0).unwrap_or(0)
}

impl SurfaceOutputDetailInterface for KSurfaceOutputDetail {
    fn present(&self) {
        if let Err(e) = g::present(self.surface) {
            match &self.ctx {
                Some(ctx) => ctx.report(e),
                None => eprintln!("wgpu-krate: present: {}", error_text(&e)),
            }
        }
        run_pending();
    }

    fn texture_discard(&self) {}
}

/// The size in pixels a surface's canvas is shown at now: configure to it
/// for a sharp picture. None for a surface that is not Krate's.
pub fn pixel_size(surface: &wgpu::Surface<'_>) -> Option<(u32, u32)> {
    let h = surface.as_custom::<KSurface>()?.h.0;
    g::surface_pixel_size(h).ok()
}

/// Run callbacks a wgpu call queued (map_async, on_submitted_work_done).
/// `Device::poll` does this too; an app that never polls can call it.
pub fn run_callbacks() {
    run_pending();
}

#[allow(dead_code)]
fn _assert_send_sync() {
    fn check<T: Send + Sync>() {}
    check::<KInstance>();
    check::<KDevice>();
    check::<KBuffer>();
    let _: Option<Box<dyn Future<Output = ()>>> = None;
}

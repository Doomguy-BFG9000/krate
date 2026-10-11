//! One wgpu device for the whole process, so a 3D scene's texture can be
//! presented without a trip through the CPU.
//!
//! A texture belongs to the device that created it and cannot cross to
//! another. The 3D scene used to make its own device, and the window
//! presenter made a second, so the only way from one to the other was to copy
//! the frame out to the CPU and back up again -- measured at 4,346us at p50
//! on a 1600x900 scene, 26% of a frame budget (K-405).
//!
//! So both take their device from here. It is created once, on first ask, and
//! every later ask gets a clone of the same handle: a `Device` and a `Queue`
//! are reference-counted, so a clone is the same device rather than a second
//! one.
//!
//! Created WITHOUT a surface to be compatible with, because the scene asks
//! first and no window exists yet. On every machine this targets the window
//! surface and an offscreen texture live on the same adapter anyway; when
//! they do not, `PixelPresenter` finds its surface unsupported and falls back
//! to its own device, which is the behaviour that was there before this
//! existed.

use std::sync::OnceLock;

// Reached through vello, like every other wgpu use in this crate, so the
// version can never drift from the one the presenter and the scene compile
// against -- which would put the types on two incompatible wgpu crates and
// make sharing impossible in a way the error message would not explain.
use vello::wgpu::{self, Adapter, Device, Instance, Queue};

/// The process-wide device, or the reason there is not one.
static SHARED: OnceLock<Option<SharedGpu>> = OnceLock::new();

/// A device, its queue, and the adapter they came from.
#[derive(Clone)]
pub struct SharedGpu {
    pub instance: Instance,
    pub adapter: Adapter,
    pub device: Device,
    pub queue: Queue,
}

/// The shared device, creating it on the first call.
///
/// `None` means this machine has no usable GPU -- no adapter, or a software
/// one. A software adapter rasterizes on the CPU behind a driver mask and is
/// SLOWER than Krate's own rasterizer, so accepting one would make the GPU
/// path a downgrade. Both callers declined them separately before; the check
/// lives here now so they cannot disagree.
pub fn shared_gpu() -> Option<SharedGpu> {
    SHARED.get_or_init(create).clone()
}

fn create() -> Option<SharedGpu> {
    let instance = Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .ok()?;
    if adapter.get_info().device_type == wgpu::DeviceType::Cpu {
        return None;
    }
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    Some(SharedGpu {
        instance,
        adapter,
        device,
        queue,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shared_device_is_one_device_however_often_it_is_asked_for() {
        // The whole point: two asks must return the SAME device, or a texture
        // made against one still cannot be presented by the other and K-405
        // is not fixed at all.
        let Some(first) = shared_gpu() else {
            eprintln!("skipping: no usable GPU on this machine");
            return;
        };
        let second = shared_gpu().expect("a second ask, after a first that worked");
        // Proved by USE, not by comparing names.
        //
        // Two devices from one adapter describe themselves identically, so a
        // Debug-string or name comparison passes whether or not they are the
        // same device -- it would have passed on the code this replaces. A
        // texture, though, belongs to exactly one device: creating it on the
        // first and using it in an encoder from the second is a validation
        // error, and NOT getting one is the proof.
        let texture = first.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shared-device proof"),
            size: wgpu::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let failed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let flag = std::sync::Arc::clone(&failed);
            second
                .device
                .on_uncaptured_error(std::sync::Arc::new(move |_| {
                    flag.store(true, std::sync::atomic::Ordering::SeqCst);
                }));
        }
        let mut encoder = second
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        second.queue.submit(Some(encoder.finish()));
        let _ = second.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });
        assert!(
            !failed.load(std::sync::atomic::Ordering::SeqCst),
            "the second ask returned a DIFFERENT device: a texture from the \
             first was rejected by it"
        );
    }
}

/// Frames waiting on the GPU, by widget id.
///
/// The seam between a 3D scene and the window that shows it.
///
/// The adapters are deliberately host-neutral: `krate-adapter-macos` does not
/// depend on wgpu or on the scene crate, and `WidgetPlacement` is a plain
/// record every platform shares. Threading a `wgpu::Texture` through that
/// record would put wgpu in the signature of every adapter on every platform,
/// including the ones with no GPU path at all.
///
/// So the texture travels beside the placement rather than inside it. The
/// runtime publishes the scene's texture here after rendering; the macOS
/// adapter, which already depends on this crate for `PixelPresenter`, takes
/// it and blits. A platform that has not been wired yet simply never looks,
/// and the CPU pixels in the placement are still correct for it.
///
/// Keyed by widget id, which is what both sides already have. One entry per
/// canvas, replaced each frame.
pub mod scene_frames {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use vello::wgpu::Texture;

    static FRAMES: Mutex<Option<BTreeMap<u64, Texture>>> = Mutex::new(None);

    /// Publish the texture holding this widget's latest 3D frame.
    pub fn publish(widget: u64, texture: Texture) {
        if let Ok(mut frames) = FRAMES.lock() {
            frames
                .get_or_insert_with(BTreeMap::new)
                .insert(widget, texture);
        }
    }

    /// Take this widget's latest frame, if one was published.
    ///
    /// Takes rather than borrows, so a stale texture cannot be presented
    /// twice: a frame that was not re-published this frame is not a frame.
    pub fn take(widget: u64) -> Option<Texture> {
        FRAMES.lock().ok()?.as_mut()?.remove(&widget)
    }

    /// Whether anything is able to CONSUME a published frame.
    ///
    /// A publisher has no way to know whether a presenter on the shared device
    /// exists -- the scene runs long before any window does. Without this the
    /// failure mode is silent: the texture is published, nobody takes it, and
    /// the canvas renders black while every 2D overlay above it looks
    /// perfect.
    ///
    /// Set by a presenter when it adopts the shared device. False today,
    /// because the presenter cannot: the shared instance is built without a
    /// display handle and cannot own a window surface (K-407).
    pub fn consumers_exist() -> bool {
        CONSUMERS.load(core::sync::atomic::Ordering::SeqCst) > 0
    }

    /// Record that a presenter on the shared device is able to take frames.
    pub fn add_consumer() {
        CONSUMERS.fetch_add(1, core::sync::atomic::Ordering::SeqCst);
    }

    static CONSUMERS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

    /// Whether a frame is waiting for this widget.
    ///
    /// Asked before taking, because the caller has to decide whether the
    /// canvas has anything to show at all before it builds a view for it.
    pub fn has(widget: u64) -> bool {
        FRAMES
            .lock()
            .ok()
            .and_then(|frames| frames.as_ref().map(|f| f.contains_key(&widget)))
            .unwrap_or(false)
    }

    /// Forget a widget's frame, when its canvas goes away.
    pub fn forget(widget: u64) {
        if let Ok(mut frames) = FRAMES.lock() {
            if let Some(frames) = frames.as_mut() {
                frames.remove(&widget);
            }
        }
    }
}

/// Frames a krate:gpu app drew, by canvas widget id (IC-940).
///
/// The seam for an app's own GPU device, beside `scene_frames` for Krate's 3D
/// scene. An app's device is its own -- the features and limits it asked
/// for -- so a frame travels with the device, queue, adapter and instance it
/// belongs to, and the window's presenter for that canvas is built on that
/// device. The frame never leaves the GPU: no copy down to the CPU and back.
///
/// The runtime publishes only while something can consume (`available`):
/// the macOS window adapter says so when it starts, and retires the path the
/// first time presenting a frame fails, after which the runtime reads frames
/// back to the CPU as before. A platform that never says so never gets one.
pub mod app_frames {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    use vello::wgpu::{Adapter, Device, Instance, Queue, Texture};

    /// One frame and the GPU it lives on.
    #[derive(Clone)]
    pub struct AppFrame {
        pub texture: Texture,
        pub device: Device,
        pub queue: Queue,
        pub adapter: Adapter,
        pub instance: Instance,
    }

    static FRAMES: Mutex<BTreeMap<u64, AppFrame>> = Mutex::new(BTreeMap::new());
    static CONSUMER: AtomicBool = AtomicBool::new(false);
    static RETIRED: AtomicBool = AtomicBool::new(false);

    /// A window presenter can take frames from here (the macOS adapter).
    pub fn add_consumer() {
        CONSUMER.store(true, Ordering::SeqCst);
    }

    /// Presenting a frame from here failed: stop publishing, read back.
    pub fn retire() {
        RETIRED.store(true, Ordering::SeqCst);
    }

    /// Whether publishing a frame will get it on screen. Off when nothing
    /// consumes, after a failure, and with KRATE_GPU_READBACK=1.
    pub fn available() -> bool {
        CONSUMER.load(Ordering::SeqCst)
            && !RETIRED.load(Ordering::SeqCst)
            && std::env::var_os("KRATE_GPU_READBACK").is_none()
            && std::env::var_os("KRATE_CPU_PRESENT").is_none()
    }

    /// Publish a canvas's latest frame, replacing one not yet taken.
    pub fn publish(widget: u64, frame: AppFrame) {
        if let Ok(mut frames) = FRAMES.lock() {
            frames.insert(widget, frame);
        }
    }

    /// Take a canvas's latest frame.
    pub fn take(widget: u64) -> Option<AppFrame> {
        FRAMES.lock().ok()?.remove(&widget)
    }

    /// Whether a frame waits for this canvas.
    pub fn has(widget: u64) -> bool {
        FRAMES
            .lock()
            .map(|frames| frames.contains_key(&widget))
            .unwrap_or(false)
    }

    /// Forget a canvas's frame.
    pub fn forget(widget: u64) {
        if let Ok(mut frames) = FRAMES.lock() {
            frames.remove(&widget);
        }
    }
}

#[cfg(test)]
mod app_frame_tests {
    use super::app_frames;
    use vello::wgpu;

    /// Nothing is published to a platform that cannot show it, and a
    /// failure turns the path off for the rest of the run.
    #[test]
    fn app_frames_go_out_only_while_a_window_can_take_them() {
        assert!(!app_frames::available(), "no consumer yet");
        app_frames::add_consumer();
        assert!(app_frames::available(), "the macOS window says it can");
        app_frames::retire();
        assert!(!app_frames::available(), "retired after a failure");
    }

    /// The direct path's colour: an app's sRGB frame, copied on the GPU to
    /// the same bytes under the non-sRGB format (the runtime's staging) and
    /// blitted to a window-format target (the presenter), arrives with its
    /// bytes unchanged -- what the CPU readback path shows. Sampling the
    /// sRGB texture itself would decode it and the window would be darker.
    #[test]
    fn an_app_frame_reaches_the_window_format_with_its_bytes_unchanged() {
        let Some(gpu) = super::shared_gpu() else {
            eprintln!("skipping: no usable GPU on this machine");
            return;
        };
        let (w, h) = (4u32, 4u32);
        let make = |format, usage| {
            gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let usage = wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::RENDER_ATTACHMENT;
        let drawn = make(wgpu::TextureFormat::Bgra8UnormSrgb, usage);
        let staged = make(wgpu::TextureFormat::Bgra8Unorm, usage);
        let window = make(wgpu::TextureFormat::Bgra8Unorm, usage);
        let pixel = [40u8, 120, 200, 255];
        let bytes: Vec<u8> = pixel
            .iter()
            .copied()
            .cycle()
            .take((w * h * 4) as usize)
            .collect();
        gpu.queue.write_texture(
            drawn.as_image_copy(),
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        let size = wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        };
        let padded = (w * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (padded * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let blitter = wgpu::util::TextureBlitter::new(&gpu.device, wgpu::TextureFormat::Bgra8Unorm);
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.copy_texture_to_texture(drawn.as_image_copy(), staged.as_image_copy(), size);
        blitter.copy(
            &gpu.device,
            &mut encoder,
            &staged.create_view(&Default::default()),
            &window.create_view(&Default::default()),
        );
        encoder.copy_texture_to_buffer(
            window.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(h),
                },
            },
            size,
        );
        gpu.queue.submit([encoder.finish()]);
        buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        let _ = gpu.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });
        let got = buffer.slice(..).get_mapped_range()[..4].to_vec();
        for (g, p) in got.iter().zip(pixel) {
            assert!(
                g.abs_diff(p) <= 1,
                "the window got {got:?} for a frame of {pixel:?}"
            );
        }
    }
}

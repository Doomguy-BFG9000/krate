//! The Android camera backend (IC-918): Camera2 through the player's
//! activity, behind `CameraBackend`. Frames arrive as I420 from Java; the
//! adapter turns them to straight RGBA the right way up for the screen, and
//! a thread of this stream's own moves the newest one into the slot. Policy
//! -- the capability check, the newest-frame rule -- stays in
//! `camera_capture`, as for every platform.
//!
//! Android asks the person too: Krate's grant is not the phone's, so the
//! phone's CAMERA prompt comes up on the first open, and a refusal there is
//! `SystemDenied`, not the Krate sheet's no.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use krate_adapter_android::camera;
use krate_adapter_android::phone::Asked;

use crate::camera_capture::{
    CameraBackend, CameraConfig, CameraError, DeviceInfo, Frame, FrameInfo, FrameSlot,
    PlatformStream,
};

pub struct AndroidCameraBackend;

impl CameraBackend for AndroidCameraBackend {
    fn devices(&self) -> Result<Vec<DeviceInfo>, CameraError> {
        Ok(camera::devices()
            .into_iter()
            .map(|(id, label)| DeviceInfo { id, label })
            .collect())
    }

    fn open(
        &mut self,
        device: &str,
        config: CameraConfig,
        slot: Arc<FrameSlot>,
    ) -> Result<(FrameInfo, Box<dyn PlatformStream>), CameraError> {
        if camera::ask() == Asked::Refused {
            return Err(CameraError::SystemDenied);
        }
        if camera::devices().is_empty() {
            return Err(CameraError::DeviceUnavailable);
        }
        let info = FrameInfo {
            width: config.width,
            height: config.height,
            fps: config.fps,
            format: config.format,
        };
        Ok((
            info,
            Box::new(AndroidStream {
                device: device.to_string(),
                config,
                slot,
                running: Arc::new(AtomicBool::new(false)),
                observed: Arc::new(Mutex::new(None)),
                pump: None,
            }),
        ))
    }
}

struct AndroidStream {
    device: String,
    config: CameraConfig,
    slot: Arc<FrameSlot>,
    running: Arc<AtomicBool>,
    observed: Arc<Mutex<Option<(u32, u32)>>>,
    pump: Option<std::thread::JoinHandle<()>>,
}

impl PlatformStream for AndroidStream {
    fn start(&mut self) -> Result<(), CameraError> {
        if self.running.load(Ordering::Relaxed) {
            return Ok(());
        }
        camera::open(
            &self.device,
            self.config.width,
            self.config.height,
            self.config.fps.max(1),
        )
        .map_err(CameraError::Platform)?;
        self.running.store(true, Ordering::Relaxed);
        let (running, slot, observed) = (
            self.running.clone(),
            self.slot.clone(),
            self.observed.clone(),
        );
        let started = std::time::Instant::now();
        self.pump = std::thread::Builder::new()
            .name("krate-camera-frames".into())
            .spawn(move || {
                while running.load(Ordering::Relaxed) {
                    match camera::frame() {
                        Some((bytes, width, height)) => {
                            if let Ok(mut seen) = observed.lock() {
                                *seen = Some((width, height));
                            }
                            slot.put(Frame {
                                bytes,
                                width,
                                height,
                                elapsed_millis: started.elapsed().as_millis() as u64,
                            });
                        }
                        None => std::thread::sleep(std::time::Duration::from_millis(8)),
                    }
                }
            })
            .ok();
        Ok(())
    }

    fn stop(&mut self) -> Result<(), CameraError> {
        self.running.store(false, Ordering::Relaxed);
        if let Some(pump) = self.pump.take() {
            let _ = pump.join();
        }
        camera::close();
        Ok(())
    }

    fn observed_size(&self) -> Option<(u32, u32)> {
        self.observed.lock().ok().and_then(|seen| *seen)
    }
}

impl Drop for AndroidStream {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

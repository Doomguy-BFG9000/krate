//! cpal's host for Krate apps (IC-932): output streams over krate:audio.
//!
//! A program that plays sound through cpal -- directly, or through kira or
//! rodio -- hears it inside a Krate app unchanged. One device, the person's
//! default output. A stream is a thread of the app's own: it keeps
//! krate:audio's queue a little ahead of the speaker (`playback-queue`),
//! calling the program's data callback for each piece, exactly as a native
//! host's audio thread would. So it needs a threaded app (IC-929); without
//! threads `build_output_stream` answers `StreamConfigNotSupported` rather
//! than hanging.
//!
//! Input (krate:audio capture) is not here yet: there is no default input
//! device, which is what cpal reports on a machine without a microphone.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use krate::bindings::krate::audio::{playback, playback_queue, types as k};

use crate::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    BufferSize, Data, DeviceDescription, DeviceDescriptionBuilder, DeviceId, Error, ErrorKind,
    FrameCount, InputCallbackInfo, OutputCallbackInfo, OutputStreamTimestamp, SampleFormat,
    StreamConfig, StreamInstant, SupportedBufferSize, SupportedStreamConfig,
    SupportedStreamConfigRange,
};

/// The rates and channel counts krate:audio takes (the host's own limits).
const MIN_RATE: u32 = 8_000;
const MAX_RATE: u32 = 192_000;
const MAX_CHANNELS: u16 = 8;
const DEFAULT_RATE: u32 = 48_000;
const DEFAULT_FRAMES: FrameCount = 1024;
const FORMATS: [SampleFormat; 2] = [SampleFormat::F32, SampleFormat::I16];

pub struct Host;

impl Host {
    #[allow(dead_code)]
    pub fn new() -> Result<Self, Error> {
        Ok(Self)
    }
}

pub struct Devices(Option<Device>);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Device;

impl fmt::Display for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Krate audio")
    }
}

impl HostTrait for Host {
    type Devices = Devices;
    type Device = Device;

    fn is_available() -> bool {
        true
    }

    fn devices(&self) -> Result<Self::Devices, Error> {
        Ok(Devices(Some(Device)))
    }

    fn default_input_device(&self) -> Option<Self::Device> {
        None
    }

    fn default_output_device(&self) -> Option<Self::Device> {
        Some(Device)
    }
}

impl Iterator for Devices {
    type Item = Device;

    fn next(&mut self) -> Option<Device> {
        self.0.take()
    }
}

#[derive(Clone)]
pub struct SupportedConfigs(std::vec::IntoIter<SupportedStreamConfigRange>);

impl Iterator for SupportedConfigs {
    type Item = SupportedStreamConfigRange;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }
}

fn buffer_range() -> SupportedBufferSize {
    SupportedBufferSize::Range { min: 64, max: 16_384 }
}

fn unsupported(text: impl Into<String>) -> Error {
    Error::with_message(ErrorKind::UnsupportedConfig, text.into())
}

fn audio_error(e: k::AudioError) -> Error {
    let kind = match e {
        k::AudioError::PermissionDenied => ErrorKind::PermissionDenied,
        k::AudioError::DeviceUnavailable => ErrorKind::DeviceNotAvailable,
        k::AudioError::Unsupported(_) => ErrorKind::UnsupportedConfig,
        _ => ErrorKind::BackendError,
    };
    Error::with_message(kind, format!("krate:audio: {e:?}"))
}

impl DeviceTrait for Device {
    type SupportedInputConfigs = SupportedConfigs;
    type SupportedOutputConfigs = SupportedConfigs;
    type Stream = Stream;

    fn description(&self) -> Result<DeviceDescription, Error> {
        Ok(DeviceDescriptionBuilder::new("Krate audio").build())
    }

    fn id(&self) -> Result<DeviceId, Error> {
        Ok(DeviceId::new(crate::platform::HostId::Krate, "default"))
    }

    fn supported_input_configs(&self) -> Result<SupportedConfigs, Error> {
        Ok(SupportedConfigs(Vec::new().into_iter()))
    }

    fn supported_output_configs(&self) -> Result<SupportedConfigs, Error> {
        let mut all = Vec::new();
        for channels in 1..=MAX_CHANNELS {
            for format in FORMATS {
                all.push(SupportedStreamConfigRange::new(
                    channels,
                    MIN_RATE,
                    MAX_RATE,
                    buffer_range(),
                    format,
                ));
            }
        }
        Ok(SupportedConfigs(all.into_iter()))
    }

    fn default_input_config(&self) -> Result<SupportedStreamConfig, Error> {
        Err(Error::with_message(
            ErrorKind::DeviceNotAvailable,
            "no input device in a Krate app yet".to_string(),
        ))
    }

    fn default_output_config(&self) -> Result<SupportedStreamConfig, Error> {
        Ok(SupportedStreamConfig::new(
            2,
            DEFAULT_RATE,
            buffer_range(),
            SampleFormat::F32,
        ))
    }

    fn build_input_stream_raw<D, E>(
        &self,
        _config: StreamConfig,
        _sample_format: SampleFormat,
        _data_callback: D,
        _error_callback: E,
        _timeout: Option<Duration>,
    ) -> Result<Self::Stream, Error>
    where
        D: FnMut(&Data, &InputCallbackInfo) + Send + 'static,
        E: FnMut(Error) + Send + 'static,
    {
        Err(Error::with_message(
            ErrorKind::DeviceNotAvailable,
            "no input device in a Krate app yet".to_string(),
        ))
    }

    fn build_output_stream_raw<D, E>(
        &self,
        config: StreamConfig,
        sample_format: SampleFormat,
        mut data_callback: D,
        mut error_callback: E,
        _timeout: Option<Duration>,
    ) -> Result<Self::Stream, Error>
    where
        D: FnMut(&mut Data, &OutputCallbackInfo) + Send + 'static,
        E: FnMut(Error) + Send + 'static,
    {
        let format = match sample_format {
            SampleFormat::F32 => k::SampleFormat::Float32,
            SampleFormat::I16 => k::SampleFormat::PcmS16,
            other => return Err(unsupported(format!("krate:audio takes f32 or i16, not {other}"))),
        };
        let frames = match config.buffer_size {
            BufferSize::Fixed(n) if n > 0 => n,
            _ => DEFAULT_FRAMES,
        };
        if !(MIN_RATE..=MAX_RATE).contains(&config.sample_rate)
            || config.channels == 0
            || config.channels > MAX_CHANNELS
        {
            return Err(unsupported(format!(
                "krate:audio takes 1-{MAX_CHANNELS} channels at {MIN_RATE}-{MAX_RATE} Hz"
            )));
        }
        let id = playback::open(k::StreamConfig {
            sample_rate: config.sample_rate,
            channels: config.channels,
            format,
            buffer_frames: frames,
        })
        .map_err(audio_error)?;

        let shared = Arc::new(Shared {
            playing: AtomicBool::new(false),
            alive: AtomicBool::new(true),
            started: Instant::now(),
        });
        let worker = {
            let shared = Arc::clone(&shared);
            let rate = config.sample_rate;
            let channels = config.channels as usize;
            std::thread::Builder::new()
                .name("cpal-krate".into())
                .spawn(move || {
                    feed(
                        id,
                        rate,
                        channels,
                        frames,
                        sample_format,
                        &shared,
                        &mut data_callback,
                        &mut error_callback,
                    )
                })
                .map_err(|e| unsupported(format!("an audio stream needs a thread: {e}")))?
        };
        Ok(Stream {
            id,
            frames,
            shared,
            worker: Mutex::new(Some(worker)),
        })
    }
}

struct Shared {
    playing: AtomicBool,
    alive: AtomicBool,
    started: Instant,
}

impl Shared {
    fn now(&self) -> StreamInstant {
        let t = self.started.elapsed();
        StreamInstant::new(t.as_secs(), t.subsec_nanos())
    }
}

/// The stream's thread: keep krate:audio's queue between one and three
/// buffers ahead of the speaker, filling each buffer from the program.
#[allow(clippy::too_many_arguments)]
fn feed<D, E>(
    id: u64,
    rate: u32,
    channels: usize,
    frames: FrameCount,
    format: SampleFormat,
    shared: &Shared,
    data_callback: &mut D,
    error_callback: &mut E,
) where
    D: FnMut(&mut Data, &OutputCallbackInfo),
    E: FnMut(Error),
{
    let samples = frames as usize * channels;
    let mut f32s = vec![0f32; samples];
    let mut i16s = vec![0i16; samples];
    let mut bytes = Vec::with_capacity(samples * 4);
    // Ahead by at least 20 ms, so a frame's hitch is not heard as a gap.
    let ahead = (frames * 3).max(rate / 50);
    let tick = Duration::from_secs_f64((frames as f64 / rate as f64 / 2.0).clamp(0.002, 0.02));
    while shared.alive.load(Ordering::Acquire) {
        if !shared.playing.load(Ordering::Acquire) {
            std::thread::sleep(tick);
            continue;
        }
        let queued = match playback_queue::queued(id) {
            Ok(q) => q,
            Err(e) => {
                error_callback(audio_error(e));
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        if queued >= ahead {
            std::thread::sleep(tick);
            continue;
        }
        let callback = shared.now();
        let delay = Duration::from_secs_f64(queued as f64 / rate as f64);
        let info = OutputCallbackInfo::new(OutputStreamTimestamp {
            callback,
            playback: callback.checked_add(delay).unwrap_or(callback),
        });
        bytes.clear();
        if format == SampleFormat::F32 {
            // SAFETY: the buffer holds `samples` f32s for the call.
            let mut data = unsafe {
                Data::from_parts(f32s.as_mut_ptr().cast(), samples, SampleFormat::F32)
            };
            data_callback(&mut data, &info);
            for s in &f32s {
                bytes.extend_from_slice(&s.to_le_bytes());
            }
        } else {
            // SAFETY: the buffer holds `samples` i16s for the call.
            let mut data = unsafe {
                Data::from_parts(i16s.as_mut_ptr().cast(), samples, SampleFormat::I16)
            };
            data_callback(&mut data, &info);
            for s in &i16s {
                bytes.extend_from_slice(&s.to_le_bytes());
            }
        }
        let mut sent = 0;
        while sent < bytes.len() && shared.alive.load(Ordering::Acquire) {
            match playback::write(id, &bytes[sent..]) {
                Ok(0) => std::thread::sleep(tick),
                Ok(n) => sent += n as usize,
                Err(e) => {
                    error_callback(audio_error(e));
                    break;
                }
            }
        }
    }
    let _ = playback::stop(id);
}

pub struct Stream {
    id: u64,
    frames: FrameCount,
    shared: Arc<Shared>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

crate::assert_stream_send!(Stream);
crate::assert_stream_sync!(Stream);

impl StreamTrait for Stream {
    fn play(&self) -> Result<(), Error> {
        playback::start(self.id).map_err(audio_error)?;
        self.shared.playing.store(true, Ordering::Release);
        Ok(())
    }

    fn pause(&self) -> Result<(), Error> {
        self.shared.playing.store(false, Ordering::Release);
        playback::stop(self.id).map_err(audio_error)
    }

    fn now(&self) -> StreamInstant {
        self.shared.now()
    }

    fn buffer_size(&self) -> Result<FrameCount, Error> {
        Ok(self.frames)
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.shared.alive.store(false, Ordering::Release);
        if let Some(worker) = self.worker.lock().ok().and_then(|mut w| w.take()) {
            let _ = worker.join();
        }
    }
}

//! The music: every track is a short song made here, from the record and
//! the track's number -- a key, a chord progression, a tempo and a melody
//! -- and played by Krate's audio (`krate:audio/playback`). Each note is
//! rendered to sound once and played by handle after that, so a song costs
//! a few sounds per bar, not a stream of samples per frame.

use alloc::vec;
use alloc::vec::Vec;

use krate::audio::{playback, types as aud};
use krate::math::F32Ext;

const SR: u32 = 22_050;

/// Instruments, by how a note is rendered.
#[derive(Clone, Copy)]
enum Voice {
    Pluck,
    Bass,
    Lead,
    Kick,
    Hat,
}

const VOICES: usize = 5;

/// A tiny repeatable random source, so a track is the same song every time.
struct Lcg(u32);

impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.0 >> 8
    }

    fn pick(&mut self, n: u32) -> u32 {
        self.next() % n.max(1)
    }
}

/// One note's samples, mono signed 16-bit little-endian.
fn render(voice: Voice, midi: i32) -> Vec<u8> {
    let freq = 440.0 * 2f32.powf((midi as f32 - 69.0) / 12.0);
    let (secs, gain) = match voice {
        Voice::Pluck => (0.9, 0.22),
        Voice::Bass => (0.8, 0.34),
        Voice::Lead => (1.1, 0.20),
        Voice::Kick => (0.30, 0.55),
        Voice::Hat => (0.06, 0.10),
    };
    let n = (SR as f32 * secs) as usize;
    let mut out = Vec::with_capacity(n * 2);
    let mut noise = Lcg(0x5eed);
    let tau = core::f32::consts::TAU;
    let mut phase = 0.0f32;
    for i in 0..n {
        let t = i as f32 / SR as f32;
        let attack = (t / 0.008).min(1.0);
        let s = match voice {
            Voice::Pluck => {
                let p = tau * freq * t;
                (p.sin() + 0.5 * (2.0 * p).sin() + 0.25 * (3.0 * p).sin()) * (-t * 5.5).exp()
            }
            Voice::Bass => {
                let p = tau * freq * t;
                (p.sin() + 0.3 * (2.0 * p).sin()) * (-t * 2.8).exp()
            }
            Voice::Lead => {
                // Odd harmonics falling as 1/n^2: a soft, flute-like tone,
                // with a slow vibrato.
                let v = 1.0 + 0.004 * (tau * 5.0 * t).sin();
                let p = tau * freq * v * t;
                (p.sin() - (3.0 * p).sin() / 9.0 + (5.0 * p).sin() / 25.0) * (-t * 2.2).exp()
            }
            Voice::Kick => {
                // A falling pitch, the way a drum's skin settles.
                let f = 50.0 + 90.0 * (-t * 30.0).exp();
                phase += tau * f / SR as f32;
                phase.sin() * (-t * 11.0).exp()
            }
            Voice::Hat => {
                let r = (noise.next() & 0xffff) as f32 / 32768.0 - 1.0;
                r * (-t * 70.0).exp()
            }
        };
        let v = (s * attack * gain * 32767.0).clamp(-32767.0, 32767.0) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// The notes of one song, by sixteenth-note step.
pub struct Song {
    /// Milliseconds per sixteenth note.
    step_ms: f32,
    /// Steps in the whole song.
    pub steps: u32,
    /// (step, voice, midi, gain), sorted by step.
    notes: Vec<(u32, u8, i32, f32)>,
}

impl Song {
    /// The song for `record`'s `track`: always the same one.
    pub fn new(record: usize, track: usize) -> Song {
        let mut rng = Lcg((record as u32 + 1) * 7919 + (track as u32 + 1) * 104_729);
        // Major-key progressions, as scale degrees of the chord roots.
        const PROGRESSIONS: [[u8; 4]; 5] = [[0, 4, 5, 3], [0, 5, 3, 4], [5, 3, 0, 4], [0, 3, 4, 3], [0, 2, 5, 4]];
        const MAJOR: [i32; 7] = [0, 2, 4, 5, 7, 9, 11];
        let root = 50 + rng.pick(10) as i32; // D3 to B3
        let bpm = 84.0 + rng.pick(30) as f32;
        let progression = PROGRESSIONS[rng.pick(PROGRESSIONS.len() as u32) as usize];
        let bars = 24u32;
        let degree = |d: i32| -> i32 {
            let octave = d.div_euclid(7);
            MAJOR[d.rem_euclid(7) as usize] + 12 * octave
        };
        let mut notes = Vec::new();
        // A melody motif of eight eighth-notes, repeated with a variation
        // every other bar: scale degrees, or none for a rest.
        let motif: Vec<Option<i32>> = (0..8)
            .map(|_| if rng.pick(4) == 0 { None } else { Some(rng.pick(8) as i32) })
            .collect();
        for bar in 0..bars {
            let chord = progression[(bar % 4) as usize] as i32;
            let first = bar * 16;
            let intro = bar < 2;
            let outro = bar >= bars - 1;
            // Bass on the one and the three.
            for beat in [0, 8] {
                notes.push((first + beat, Voice::Bass as u8, root - 12 + degree(chord), 1.0));
            }
            // The chord, plucked as an arpeggio of eighths.
            for (i, step) in (0..16).step_by(2).enumerate() {
                let tone = chord + [0, 2, 4, 2][i % 4];
                notes.push((first + step, Voice::Pluck as u8, root + degree(tone), if i % 4 == 0 { 0.9 } else { 0.6 }));
            }
            // Drums once the intro is done.
            if !intro && !outro {
                for beat in [0, 4, 8, 12] {
                    if beat % 8 == 0 {
                        notes.push((first + beat, Voice::Kick as u8, 36, 1.0));
                    }
                    notes.push((first + beat + 2, Voice::Hat as u8, 90, 0.7));
                }
            }
            // The melody over the middle of the song.
            if (4..bars - 2).contains(&bar) {
                for (i, note) in motif.iter().enumerate() {
                    if let Some(d) = note {
                        let lift = if bar % 2 == 1 && i >= 4 { 2 } else { 0 };
                        notes.push((first + i as u32 * 2, Voice::Lead as u8, root + 12 + degree(chord + d + lift), 0.8));
                    }
                }
            }
        }
        notes.sort_by_key(|n| n.0);
        Song {
            step_ms: 60_000.0 / bpm / 4.0,
            steps: bars * 16,
            notes,
        }
    }

    /// The song's length in milliseconds.
    pub fn length_ms(&self) -> u32 {
        (self.steps as f32 * self.step_ms) as u32
    }
}

/// What is playing, and the sounds made so far.
pub struct Player {
    stream: Option<u64>,
    /// sounds[voice][midi]: a loaded sound's handle (handles start at 1);
    /// 0 when not made yet.
    sounds: Vec<Vec<u64>>,
    song: Option<Song>,
    /// Which record and track the song is.
    pub now: Option<(usize, usize)>,
    /// When playback (re)started, and how far into the song that was.
    started_at: u64,
    started_ms: u32,
    /// The next note to play.
    cursor: usize,
    pub playing: bool,
}

impl Player {
    pub fn new() -> Player {
        // One stream for the whole run: opening one costs milliseconds no
        // frame can spare. Headless there is no sound, and that is fine.
        let stream = playback::open(aud::StreamConfig {
            sample_rate: SR,
            channels: 1,
            format: aud::SampleFormat::PcmS16,
            buffer_frames: 1024,
        })
        .ok();
        if let Some(s) = stream {
            let _ = playback::start(s);
        }
        Player {
            stream,
            sounds: vec![vec![0; 128]; VOICES],
            song: None,
            now: None,
            started_at: 0,
            started_ms: 0,
            cursor: 0,
            playing: false,
        }
    }

    fn clock_ms() -> u64 {
        krate::time::monotonic_nanos() / 1_000_000
    }

    /// Start `track` of `record` from the top.
    pub fn play(&mut self, record: usize, track: usize) {
        self.song = Some(Song::new(record, track));
        self.now = Some((record, track));
        self.cursor = 0;
        self.started_ms = 0;
        self.started_at = Self::clock_ms();
        self.playing = true;
    }

    /// Pause, or carry on from where it paused.
    pub fn toggle(&mut self) {
        if self.song.is_none() {
            return;
        }
        if self.playing {
            self.started_ms = self.position_ms();
            self.playing = false;
        } else {
            self.started_at = Self::clock_ms();
            self.playing = true;
        }
    }

    /// Jump to `ms` into the song, playing on from there if it was playing.
    pub fn seek(&mut self, ms: u32) {
        let Some(song) = self.song.as_ref() else {
            return;
        };
        let ms = ms.min(song.length_ms());
        let step = (ms as f32 / song.step_ms) as u32;
        self.cursor = song.notes.partition_point(|n| n.0 < step);
        self.started_ms = ms;
        self.started_at = Self::clock_ms();
    }

    /// Milliseconds into the song.
    pub fn position_ms(&self) -> u32 {
        if !self.playing {
            return self.started_ms;
        }
        self.started_ms + (Self::clock_ms() - self.started_at) as u32
    }

    /// The song's length in milliseconds.
    pub fn length_ms(&self) -> u32 {
        self.song.as_ref().map(Song::length_ms).unwrap_or(0)
    }

    fn sound(&mut self, voice: u8, midi: i32) -> Option<u64> {
        let stream = self.stream?;
        let slot = self.sounds.get_mut(voice as usize)?.get_mut(midi.clamp(0, 127) as usize)?;
        if *slot == 0 {
            let v = match voice {
                0 => Voice::Pluck,
                1 => Voice::Bass,
                2 => Voice::Lead,
                3 => Voice::Kick,
                _ => Voice::Hat,
            };
            *slot = playback::load_sound(stream, &render(v, midi)).ok()?;
        }
        Some(*slot)
    }

    /// Play every note that is due. True when the song just ended.
    pub fn tick(&mut self) -> bool {
        if !self.playing {
            return false;
        }
        let at = self.position_ms() as f32;
        let Some(song) = self.song.as_ref() else {
            return false;
        };
        let step_ms = song.step_ms;
        let ended = at >= song.length_ms() as f32;
        let mut due = Vec::new();
        while let Some(&(step, voice, midi, gain)) = song.notes.get(self.cursor) {
            if step as f32 * step_ms > at {
                break;
            }
            // A note more than a step late is skipped, not played out of
            // time (the app was paused by the system, or a frame stalled).
            if at - step as f32 * step_ms < step_ms * 1.5 {
                due.push((voice, midi, gain));
            }
            self.cursor += 1;
        }
        for (voice, midi, gain) in due {
            if let (Some(stream), Some(sound)) = (self.stream, self.sound(voice, midi)) {
                let _ = playback::play_sound(stream, sound, gain);
            }
        }
        if ended {
            self.playing = false;
        }
        ended
    }
}

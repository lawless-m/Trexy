//! The XY audio player — FIRST-SLICE.md §1 deliverable 4.
//!
//! Oscilloscope music drives the beam far harder than any game will: no
//! blanking, no display list, just a continuous curve at audio rate. It is the
//! sternest test the renderer gets before an emulator arrives.
//!
//! Left channel is x, right is y. A third channel, where one exists, is drive
//! — otherwise drive is a constant the user sets. Playback is clocked by the
//! audio device, and the beam follows that clock rather than the wall clock,
//! because a beam that drifts against the sound it is drawing is the one thing
//! this source exists to get right.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use beam_trace::Sample;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// A decoded XY track: one entry per audio frame.
#[derive(Clone, Debug)]
pub struct XyAudio {
    /// x, y, drive per frame. Drive is 1.0 where the file did not supply one.
    frames: Vec<[f32; 3]>,
    sample_rate: u32,
    has_drive_channel: bool,
}

impl XyAudio {
    /// Decode a WAV. Two channels are x and y; three are x, y and drive.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        let reader =
            hound::WavReader::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::decode(reader).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Decode a stereo XY file alongside a mono drive file, as some
    /// oscilloscope-music releases are distributed.
    pub fn load_pair(xy: impl AsRef<Path>, drive: impl AsRef<Path>) -> Result<Self, String> {
        let mut track = Self::load(xy)?;
        let drive_track = Self::load(drive)?;

        if drive_track.sample_rate != track.sample_rate {
            return Err(format!(
                "the drive file is {} Hz but the XY file is {} Hz",
                drive_track.sample_rate, track.sample_rate
            ));
        }
        for (frame, drive) in track.frames.iter_mut().zip(&drive_track.frames) {
            // A mono file decodes into x; that is the drive envelope.
            frame[2] = drive[0].abs();
        }
        track.frames.truncate(drive_track.frames.len());
        track.has_drive_channel = true;
        Ok(track)
    }

    fn decode<R: std::io::Read>(mut reader: hound::WavReader<R>) -> Result<Self, hound::Error> {
        let spec = reader.spec();
        let channels = spec.channels as usize;

        // Integer samples are scaled by their own full range, so a 16-bit and
        // a float rendering of the same track land on the same deflection.
        let values: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
            hound::SampleFormat::Int => {
                let full = f32::from(i16::MAX);
                let shift = 32 - spec.bits_per_sample;
                reader
                    .samples::<i32>()
                    .map(|value| value.map(|v| (v << shift >> 16) as f32 / full))
                    .collect::<Result<_, _>>()?
            }
        };

        let frames = values
            .chunks(channels.max(1))
            .map(|frame| {
                let x = frame.first().copied().unwrap_or(0.0);
                // A mono file draws a diagonal rather than nothing.
                let y = frame.get(1).copied().unwrap_or(x);
                let drive = frame.get(2).copied().map_or(1.0, f32::abs);
                [x, y, drive]
            })
            .collect();

        Ok(Self {
            frames,
            sample_rate: spec.sample_rate,
            has_drive_channel: channels >= 3,
        })
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn seconds(&self) -> f64 {
        self.frames.len() as f64 / f64::from(self.sample_rate)
    }

    /// True when the file carried its own drive, so the slider is inert.
    pub fn has_drive_channel(&self) -> bool {
        self.has_drive_channel
    }

    pub fn frame(&self, index: usize) -> [f32; 3] {
        self.frames[index % self.frames.len().max(1)]
    }

    /// Trace samples for frames `[from, to)`, timestamped from `epoch`.
    ///
    /// One trace sample per audio frame. TRACE-FORMAT.md §4 wants the
    /// piecewise-linear path within ε of the true one; at audio rate the beam
    /// moves a fraction of ε between frames, so the contract is satisfied
    /// without any adaptive work — the source is already denser than ε needs.
    pub fn samples(&self, from: usize, to: usize, drive: f32, epoch: f64) -> Vec<Sample> {
        if self.frames.is_empty() || to <= from {
            return Vec::new();
        }
        let rate = f64::from(self.sample_rate);
        (from..to)
            .map(|index| {
                let [x, y, envelope] = self.frame(index);
                let level = if self.has_drive_channel {
                    envelope * drive
                } else {
                    drive
                };
                Sample::mono(x, y, level.max(0.0), (epoch + index as f64 / rate) as f32)
            })
            .collect()
    }
}

/// A running cpal stream and the frame counter the beam follows.
pub struct AudioPlayer {
    _stream: cpal::Stream,
    /// Frames handed to the device so far. This is the beam's clock.
    played: Arc<AtomicUsize>,
    /// Drive level, as a slider value in thousandths so it can be atomic.
    drive: Arc<AtomicU64>,
    /// Track frames per device frame, where the two rates differ.
    step: f64,
    track: XyAudio,
}

impl AudioPlayer {
    /// Open the default output and start playing `track` on a loop.
    ///
    /// The stream is stereo x/y — which is what an oscilloscope would be fed —
    /// so a drive channel is not sent to the speakers.
    pub fn start(track: XyAudio, drive: f32) -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or("no default audio output device")?;
        let config = device
            .default_output_config()
            .map_err(|e| format!("no default output config: {e}"))?;

        let channels = config.channels() as usize;
        let device_rate = config.sample_rate();
        let played = Arc::new(AtomicUsize::new(0));
        let source = track.clone();
        let cursor = Arc::clone(&played);

        // The device may not run at the file's rate; step through the track at
        // the ratio between them rather than refusing to play.
        let step = f64::from(track.sample_rate()) / f64::from(device_rate);

        let stream = device
            .build_output_stream(
                config.config(),
                move |output: &mut [f32], _| {
                    for frame in output.chunks_mut(channels.max(1)) {
                        let index = cursor.fetch_add(1, Ordering::Relaxed);
                        let at = (index as f64 * step) as usize;
                        let [x, y, _] = source.frame(at);
                        for (channel, value) in frame.iter_mut().enumerate() {
                            *value = if channel % 2 == 0 { x } else { y };
                        }
                    }
                },
                move |error| eprintln!("audio stream error: {error}"),
                None,
            )
            .map_err(|e| format!("could not build the output stream: {e}"))?;
        stream.play().map_err(|e| format!("could not play: {e}"))?;

        Ok(Self {
            _stream: stream,
            played,
            drive: Arc::new(AtomicU64::new((drive * 1000.0) as u64)),
            step,
            track,
        })
    }

    pub fn track(&self) -> &XyAudio {
        &self.track
    }

    /// Device frames handed over so far.
    pub fn played_frames(&self) -> usize {
        self.played.load(Ordering::Relaxed)
    }

    /// The same, in *track* frames — which is the index the beam is at, and
    /// the only clock the trace timestamps are derived from.
    pub fn played_track_frames(&self) -> usize {
        (self.played_frames() as f64 * self.step) as usize
    }

    pub fn drive(&self) -> f32 {
        self.drive.load(Ordering::Relaxed) as f32 / 1000.0
    }

    pub fn set_drive(&self, drive: f32) {
        self.drive
            .store((drive.max(0.0) * 1000.0) as u64, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_wav(path: &Path, channels: u16, format: hound::SampleFormat, frames: &[Vec<f32>]) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: 48_000,
            bits_per_sample: match format {
                hound::SampleFormat::Float => 32,
                hound::SampleFormat::Int => 16,
            },
            sample_format: format,
        };
        let mut writer = hound::WavWriter::create(path, spec).expect("create wav");
        for frame in frames {
            for value in frame {
                match format {
                    hound::SampleFormat::Float => writer.write_sample(*value).unwrap(),
                    hound::SampleFormat::Int => writer
                        .write_sample((value * f32::from(i16::MAX)).round() as i16)
                        .unwrap(),
                }
            }
        }
        writer.finalize().expect("finalize");
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("beam-sources-{}-{name}", std::process::id()))
    }

    #[test]
    fn stereo_maps_left_to_x_and_right_to_y() {
        let path = scratch("stereo.wav");
        write_wav(
            &path,
            2,
            hound::SampleFormat::Float,
            &[vec![0.5, -0.25], vec![-1.0, 1.0], vec![0.0, 0.75]],
        );
        let track = XyAudio::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(track.len(), 3);
        assert_eq!(track.sample_rate(), 48_000);
        assert!(!track.has_drive_channel());
        assert_eq!(track.frame(0)[0], 0.5);
        assert_eq!(track.frame(0)[1], -0.25);
        assert_eq!(track.frame(1), [-1.0, 1.0, 1.0]);
        assert_eq!(track.frame(2)[1], 0.75);
    }

    #[test]
    fn sixteen_bit_and_float_decode_to_the_same_deflection() {
        let frames = vec![vec![0.5, -0.25], vec![-1.0, 0.75]];
        let float = scratch("f32.wav");
        let int = scratch("i16.wav");
        write_wav(&float, 2, hound::SampleFormat::Float, &frames);
        write_wav(&int, 2, hound::SampleFormat::Int, &frames);

        let a = XyAudio::load(&float).unwrap();
        let b = XyAudio::load(&int).unwrap();
        std::fs::remove_file(&float).ok();
        std::fs::remove_file(&int).ok();

        for index in 0..frames.len() {
            for channel in 0..2 {
                assert!(
                    (a.frame(index)[channel] - b.frame(index)[channel]).abs() < 1e-4,
                    "frame {index} channel {channel}: {:?} vs {:?}",
                    a.frame(index),
                    b.frame(index)
                );
            }
        }
    }

    #[test]
    fn a_third_channel_becomes_drive() {
        let path = scratch("three.wav");
        write_wav(
            &path,
            3,
            hound::SampleFormat::Float,
            &[
                vec![0.5, 0.5, 1.0],
                vec![-0.5, 0.25, 0.0],
                vec![0.0, 0.0, 0.5],
            ],
        );
        let track = XyAudio::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert!(track.has_drive_channel());
        assert_eq!(track.frame(0)[2], 1.0);
        assert_eq!(track.frame(1)[2], 0.0);
        assert_eq!(track.frame(2)[2], 0.5);

        // With a drive channel the slider scales it rather than replacing it,
        // so a blanked frame stays blanked however far the slider is pushed.
        let samples = track.samples(0, 3, 2.0, 0.0);
        assert_eq!(samples[0].drive_r, 2.0);
        assert_eq!(samples[1].drive_r, 0.0);
        assert_eq!(samples[2].drive_r, 1.0);
    }

    #[test]
    fn a_stereo_pair_plus_a_mono_file_routes_the_third_file_to_drive() {
        let xy = scratch("pair-xy.wav");
        let drive = scratch("pair-drive.wav");
        write_wav(
            &xy,
            2,
            hound::SampleFormat::Float,
            &[vec![0.1, 0.2], vec![0.3, 0.4]],
        );
        write_wav(
            &drive,
            1,
            hound::SampleFormat::Float,
            &[vec![0.25], vec![0.75]],
        );

        let track = XyAudio::load_pair(&xy, &drive).unwrap();
        std::fs::remove_file(&xy).ok();
        std::fs::remove_file(&drive).ok();

        assert!(track.has_drive_channel());
        assert_eq!(track.frame(0), [0.1, 0.2, 0.25]);
        assert_eq!(track.frame(1), [0.3, 0.4, 0.75]);
    }

    #[test]
    fn timestamps_advance_at_the_audio_rate_and_strictly_increase() {
        let path = scratch("rate.wav");
        let frames: Vec<Vec<f32>> = (0..64)
            .map(|i| vec![i as f32 / 64.0, -(i as f32) / 64.0])
            .collect();
        write_wav(&path, 2, hound::SampleFormat::Float, &frames);
        let track = XyAudio::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        let samples = track.samples(0, 64, 1.0, 10.0);
        assert_eq!(samples.len(), 64);
        assert!((samples[0].t - 10.0).abs() < 1e-6);

        let period = 1.0 / 48_000.0;
        for (index, pair) in samples.windows(2).enumerate() {
            assert!(pair[1].t > pair[0].t, "sample {index} did not advance");
            assert!(
                ((pair[1].t - pair[0].t) - period).abs() < 1e-6,
                "sample {index} advanced by {}",
                pair[1].t - pair[0].t
            );
        }
        // And the result is a trace the renderer would accept.
        beam_trace::validate(&samples).expect("a valid trace");
    }

    #[test]
    fn without_a_drive_channel_the_slider_sets_the_level() {
        let path = scratch("slider.wav");
        write_wav(
            &path,
            2,
            hound::SampleFormat::Float,
            &[vec![0.0, 0.0], vec![0.5, 0.5]],
        );
        let track = XyAudio::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        let samples = track.samples(0, 2, 0.6, 0.0);
        assert!(samples.iter().all(|s| s.drive_r == 0.6));
    }
}

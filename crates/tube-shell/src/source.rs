//! The live signal source: a producer thread filling the ring buffer.
//!
//! TRACE-FORMAT.md §5 puts the renderer on one side and the producer on the
//! other, and is explicit that the renderer never blocks the producer and
//! never owns the time base. Both hold here: the producer runs on its own
//! thread against the wall clock, and the only contact between them is a
//! mutex held for the length of a push or a copy — never across GPU work.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use beam_sources::{AudioPlayer, Lissajous, PATTERNS, XyAudio};
use beam_trace::{RingBuffer, Sample, TraceHeader};
use std::path::Path;

/// How far ahead of the wall clock the producer is allowed to generate. Enough
/// that a scheduling hiccup does not starve the renderer, short enough that a
/// slider change is felt immediately.
const LOOKAHEAD_SECONDS: f64 = 0.05;

/// Where the producer sleeps when it has run far enough ahead.
const IDLE: Duration = Duration::from_millis(2);

/// Ring capacity in samples. The default covers 200 ms at the worst-case
/// density the renderer targets.
const CAPACITY: usize = beam_trace::DEFAULT_CAPACITY;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The general figure, with everything on a slider.
    Lissajous,
    /// One of the seven acceptance patterns, by index into `PATTERNS`.
    Pattern(usize),
    /// An oscilloscope-music WAV, clocked by the audio device.
    Audio,
    /// A recorded `.btr0`, looped. This is the shader-iteration workflow:
    /// replay plus hot-reload plus the debug views (FIRST-SLICE.md §1
    /// deliverable 6).
    Replay,
}

impl Source {
    pub fn name(self) -> String {
        match self {
            Source::Lissajous => "lissajous".to_owned(),
            Source::Pattern(index) => PATTERNS[index].slug.to_owned(),
            Source::Audio => "xy audio".to_owned(),
            Source::Replay => "replay".to_owned(),
        }
    }
}

/// Everything the panel can change while the source runs.
#[derive(Clone, Debug)]
pub struct Controls {
    pub source: Source,
    pub lissajous: Lissajous,
    /// WAV to play. A second path, if given, is a mono drive envelope.
    pub audio_path: String,
    pub audio_drive_path: String,
    /// Constant drive for files that carry no drive channel.
    pub audio_drive: f32,
    /// The `.btr0` to loop.
    pub replay_path: String,
    /// Bumped when the source changes, so the producer knows to drop whatever
    /// it had queued and start again from the present.
    pub generation: u64,
}

impl Default for Controls {
    fn default() -> Self {
        Self {
            source: Source::Lissajous,
            lissajous: Lissajous::default(),
            audio_path: String::new(),
            audio_drive_path: String::new(),
            audio_drive: 1.0,
            replay_path: String::new(),
            generation: 0,
        }
    }
}

impl Controls {
    /// The next chunk to emit, timestamped from zero, and how long it lasts.
    ///
    /// Chunks are whole units of the program — one refresh frame, or one
    /// closed traversal of the figure — so a parameter change lands on a
    /// boundary rather than tearing a figure in half.
    fn chunk(&self) -> (Vec<Sample>, f32) {
        match self.source {
            // These two are not generated: audio is clocked by the device and
            // replay comes off disk, both handled in the producer.
            Source::Audio | Source::Replay => (Vec::new(), 0.01),
            Source::Pattern(index) => PATTERNS[index].frame(),
            Source::Lissajous => {
                let figure = self.lissajous;
                let period = figure.period();
                let mut pen = beam_sources::Pen::new(figure.at(0.0).0, beam_sources::EPSILON);
                pen.curve(period, |u| figure.at(u * period));
                (pen.into_samples(), period)
            }
        }
    }
}

/// A running producer and the buffer it fills.
pub struct LiveSource {
    ring: Arc<Mutex<RingBuffer>>,
    controls: Arc<Mutex<Controls>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    start: Instant,
    /// Whatever the audio source has to say for itself, for the panel.
    status: Arc<Mutex<String>>,
}

impl LiveSource {
    pub fn spawn() -> Self {
        let ring = Arc::new(Mutex::new(RingBuffer::with_capacity(CAPACITY, 0.0)));
        let controls = Arc::new(Mutex::new(Controls::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(String::new()));
        let start = Instant::now();

        let thread = std::thread::Builder::new()
            .name("beam-source".to_owned())
            .spawn({
                let ring = Arc::clone(&ring);
                let controls = Arc::clone(&controls);
                let stop = Arc::clone(&stop);
                let status = Arc::clone(&status);
                move || produce(&ring, &controls, &stop, &status, start)
            })
            .expect("spawn the producer thread");

        Self {
            ring,
            controls,
            stop,
            thread: Some(thread),
            start,
            status,
        }
    }

    /// Seconds since the source started. This is the renderer's `T_now`, and
    /// the producer's timestamps are against the same origin.
    pub fn elapsed(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    pub fn controls(&self) -> Controls {
        self.controls.lock().expect("controls lock").clone()
    }

    /// What the audio source last reported: what it loaded, or why it could
    /// not.
    pub fn status(&self) -> String {
        self.status.lock().expect("status lock").clone()
    }

    pub fn set_controls(&self, controls: Controls) {
        *self.controls.lock().expect("controls lock") = controls;
    }

    /// Copy out every sample needed to draw the spans in `[from, to]`.
    ///
    /// The copy is a raw move of whole `Sample` records — the same 32-byte
    /// layout that goes to the GPU, with no transform anywhere in between
    /// (FIRST-SLICE.md §5). The lock is held for exactly this memcpy.
    pub fn window(&self, from: f32, to: f32) -> Vec<Sample> {
        let ring = self.ring.lock().expect("ring lock");
        let (a, b) = ring.spans_in(from, to);
        let mut out = Vec::with_capacity(a.len() + b.len());
        out.extend_from_slice(a);
        out.extend_from_slice(b);
        out
    }

    pub fn buffered(&self) -> usize {
        self.ring.lock().expect("ring lock").len()
    }

    /// Dump whatever the ring currently holds to a `.btr0`.
    ///
    /// The write is a raw copy of the sample array plus a header — on-disk is
    /// the in-memory layout by design (TRACE-FORMAT.md §6), so capturing a
    /// regression trace really is a memcpy and a write.
    ///
    /// Timestamps are rebased so the file starts near zero. Session time can
    /// run to minutes, and f32 `t` loses resolution out there; the epoch field
    /// exists precisely so a file need not carry that history.
    pub fn record(&self, path: impl AsRef<Path>, producer: &str) -> Result<usize, String> {
        let ring = self.ring.lock().expect("ring lock");
        record_ring(&ring, path, producer)
    }
}

/// Write a ring buffer's contents as a `.btr0`.
///
/// Timestamps are rebased so the file starts at zero, with the original origin
/// carried in the header's epoch. Session time runs to minutes and f32 `t`
/// loses resolution out there; the epoch field exists precisely so a file need
/// not carry that history (TRACE-FORMAT.md §2).
pub fn record_ring(
    ring: &RingBuffer,
    path: impl AsRef<Path>,
    producer: &str,
) -> Result<usize, String> {
    let samples: Vec<Sample> = (0..ring.len())
        .filter_map(|index| ring.get(index).copied())
        .collect();
    if samples.is_empty() {
        return Err("the ring buffer is empty; nothing to record".to_owned());
    }

    let first = samples[0].t;
    let rebased = rebase(&samples, -first, false);

    let header = TraceHeader {
        epoch: ring.epoch() + f64::from(first),
        epsilon: beam_sources::EPSILON,
        // A live capture has no display list, so no nominal rate to claim.
        nominal_refresh_hz: 0.0,
        producer_id: format!("live/{producer}").chars().take(32).collect(),
    };
    beam_trace::write_file(path, &header, &rebased).map_err(|e| e.to_string())?;
    Ok(rebased.len())
}

/// Offset a whole trace so a repeat of it follows the one before.
///
/// `drop_opening` removes the trace's first sample, which is what makes
/// looping sound: a trace ends where it began, so laying repeat N+1 straight
/// after repeat N would put two samples on the same instant, and `t` must
/// strictly increase within a buffer generation (TRACE-FORMAT.md §2). The
/// opening sample only restates a position the previous repeat already left
/// the beam at, so dropping it loses nothing.
pub fn rebase(trace: &[Sample], offset: f32, drop_opening: bool) -> Vec<Sample> {
    let from = if drop_opening && !trace.is_empty() {
        1
    } else {
        0
    };
    trace[from..]
        .iter()
        .map(|sample| Sample {
            t: sample.t + offset,
            ..*sample
        })
        .collect()
}

impl Drop for LiveSource {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A `.btr0` held open for looping.
struct Loaded {
    path: String,
    samples: Vec<Sample>,
    seconds: f32,
    /// False until the first repeat has been emitted, after which each repeat
    /// drops its opening sample so the joins keep time moving.
    started: bool,
}

/// The audio source's state, which lives on the producer thread because a cpal
/// stream is not `Send` on every platform.
struct Playing {
    player: AudioPlayer,
    /// Trace time at which the stream started.
    epoch: f64,
    /// Track frames already turned into samples.
    cursor: usize,
}

fn produce(
    ring: &Mutex<RingBuffer>,
    controls: &Mutex<Controls>,
    stop: &AtomicBool,
    status: &Mutex<String>,
    start: Instant,
) {
    let mut cursor = 0.0f64;
    let mut generation = u64::MAX;
    let mut last_t = f32::NEG_INFINITY;
    let mut playing: Option<Playing> = None;
    let mut loaded: Option<Loaded> = None;

    while !stop.load(Ordering::Relaxed) {
        let controls = controls.lock().expect("controls lock").clone();
        let now = start.elapsed().as_secs_f64();

        if controls.generation != generation {
            // A new source: abandon anything queued for the future and pick up
            // from the present instant.
            generation = controls.generation;
            cursor = cursor.max(now);
            playing = None;
        }

        let chunk = if controls.source == Source::Audio {
            match audio_chunk(&mut playing, &controls, status, now) {
                Some(chunk) => chunk,
                None => {
                    std::thread::sleep(IDLE);
                    continue;
                }
            }
        } else if controls.source == Source::Replay {
            playing = None;
            if cursor > now + LOOKAHEAD_SECONDS {
                std::thread::sleep(IDLE);
                continue;
            }
            match replay_chunk(&mut loaded, &controls, status, cursor) {
                Some((chunk, seconds)) => {
                    cursor += f64::from(seconds);
                    chunk
                }
                None => {
                    std::thread::sleep(IDLE);
                    continue;
                }
            }
        } else {
            playing = None;
            if cursor > now + LOOKAHEAD_SECONDS {
                std::thread::sleep(IDLE);
                continue;
            }
            let (chunk, seconds) = controls.chunk();
            cursor += f64::from(seconds);
            // Synthetic chunks are timestamped from zero and placed here.
            chunk
                .into_iter()
                .map(|mut sample| {
                    sample.t += (cursor - f64::from(seconds)) as f32;
                    sample
                })
                .collect()
        };

        if chunk.is_empty() {
            std::thread::sleep(IDLE);
            continue;
        }

        let mut ring = ring.lock().expect("ring lock");
        for mut sample in chunk {
            // Chunks abut, so the first sample of one lands on the last of the
            // previous. `t` must strictly increase (TRACE-FORMAT.md §2) and the
            // duplicate carries no new information, so nudge it.
            if sample.t <= last_t {
                sample.t = f32::from_bits(last_t.to_bits() + 1);
            }
            last_t = sample.t;
            ring.push(sample);
        }
    }
}

/// One loop of the replayed trace, placed after whatever came before.
fn replay_chunk(
    loaded: &mut Option<Loaded>,
    controls: &Controls,
    status: &Mutex<String>,
    cursor: f64,
) -> Option<(Vec<Sample>, f32)> {
    let report = |text: String| *status.lock().expect("status lock") = text;

    let path = controls.replay_path.trim();
    if path.is_empty() {
        report("give replay a .btr0 to loop".to_owned());
        return None;
    }
    if loaded.as_ref().is_none_or(|held| held.path != path) {
        match beam_trace::read_file(path) {
            Ok(trace) if trace.samples.len() < 2 => {
                report(format!("{path} has too few samples to replay"));
                return None;
            }
            Ok(trace) => {
                let seconds = trace.samples.last().map_or(0.0, |s| s.t);
                report(format!(
                    "{} samples over {seconds:.3} s from {}",
                    trace.samples.len(),
                    trace.header.producer_id
                ));
                *loaded = Some(Loaded {
                    path: path.to_owned(),
                    samples: trace.samples,
                    seconds,
                    started: false,
                });
            }
            Err(e) => {
                report(e.to_string());
                return None;
            }
        }
    }

    let held = loaded.as_mut()?;
    let chunk = rebase(&held.samples, cursor as f32, held.started);
    held.started = true;
    Some((chunk, held.seconds))
}

/// Samples for the audio frames the device has actually played.
///
/// Timestamps come from the audio clock, never the wall clock: the beam is
/// slaved to the sound rather than racing it. The beam therefore lags the
/// speakers by one producer poll, which is milliseconds — well inside the
/// 30 ms that acceptance pattern 8 allows.
fn audio_chunk(
    playing: &mut Option<Playing>,
    controls: &Controls,
    status: &Mutex<String>,
    now: f64,
) -> Option<Vec<Sample>> {
    let report = |text: String| *status.lock().expect("status lock") = text;

    if playing.is_none() {
        if controls.audio_path.trim().is_empty() {
            report("give the audio source a WAV to play".to_owned());
            return None;
        }
        let track = if controls.audio_drive_path.trim().is_empty() {
            XyAudio::load(&controls.audio_path)
        } else {
            XyAudio::load_pair(&controls.audio_path, &controls.audio_drive_path)
        };
        let track = match track {
            Ok(track) if track.is_empty() => {
                report("that file has no audio in it".to_owned());
                return None;
            }
            Ok(track) => track,
            Err(e) => {
                report(e);
                return None;
            }
        };
        let summary = format!(
            "{:.1} s at {} Hz, drive {}",
            track.seconds(),
            track.sample_rate(),
            if track.has_drive_channel() {
                "from the file"
            } else {
                "from the slider"
            }
        );
        match AudioPlayer::start(track, controls.audio_drive) {
            Ok(player) => {
                report(summary);
                *playing = Some(Playing {
                    player,
                    epoch: now,
                    cursor: 0,
                });
            }
            Err(e) => {
                report(e);
                return None;
            }
        }
    }

    let playing = playing.as_mut()?;
    playing.player.set_drive(controls.audio_drive);

    let played = playing.player.played_track_frames();
    if played <= playing.cursor {
        return Some(Vec::new());
    }
    let samples =
        playing
            .player
            .track()
            .samples(playing.cursor, played, controls.audio_drive, playing.epoch);
    playing.cursor = played;
    Some(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Long enough for the producer to get several chunks ahead of the clock.
    const SETTLE: Duration = Duration::from_millis(150);

    fn at(t: f32) -> Sample {
        Sample::mono(t.sin(), t.cos(), 1.0, t)
    }

    /// Lay a trace end to end, exactly as the producer does one repeat at a
    /// time: the first repeat whole, every later one without its opening
    /// sample.
    fn looped(trace: &[Sample], repeats: usize) -> Vec<Sample> {
        let seconds = trace.last().map_or(0.0, |sample| sample.t);
        (0..repeats)
            .flat_map(|repeat| rebase(trace, repeat as f32 * seconds, repeat > 0))
            .collect()
    }

    #[test]
    fn a_recorded_ring_reloads_byte_identically() {
        let mut ring = RingBuffer::with_capacity(64, 0.0);
        let written: Vec<Sample> = (0..32).map(|i| at(i as f32 * 0.001)).collect();
        ring.extend(&written);

        let path = std::env::temp_dir().join(format!("trexy-record-{}.btr0", std::process::id()));
        let count = record_ring(&ring, &path, "test").expect("record");
        let read = beam_trace::read_file(&path).expect("reload");
        let bytes = std::fs::read(&path).expect("raw bytes");
        std::fs::remove_file(&path).ok();

        assert_eq!(count, written.len());
        // The first sample is already at t = 0, so rebasing is the identity
        // here and the round trip must be exact.
        assert_eq!(read.samples, written);
        assert_eq!(read.header.producer_id, "live/test");
        assert_eq!(
            beam_trace::encode(&read.header, &read.samples).unwrap(),
            bytes,
            "re-encoding the file must reproduce it"
        );
    }

    #[test]
    fn recording_carries_the_original_origin_in_the_epoch() {
        let mut ring = RingBuffer::with_capacity(64, 0.0);
        // A capture taken a while into the session.
        let written: Vec<Sample> = (0..8).map(|i| at(120.0 + i as f32 * 0.001)).collect();
        ring.extend(&written);

        let path = std::env::temp_dir().join(format!("trexy-epoch-{}.btr0", std::process::id()));
        record_ring(&ring, &path, "test").expect("record");
        let read = beam_trace::read_file(&path).expect("reload");
        std::fs::remove_file(&path).ok();

        assert!(
            (read.header.epoch - 120.0).abs() < 1e-3,
            "{}",
            read.header.epoch
        );
        assert_eq!(read.samples[0].t, 0.0, "the file starts at its own zero");
        // And the shape is unchanged: only the origin moved.
        for (index, sample) in read.samples.iter().enumerate() {
            assert_eq!(sample.x, written[index].x);
            assert!((sample.t - (written[index].t - 120.0)).abs() < 1e-6);
        }
    }

    #[test]
    fn looping_a_replay_keeps_time_strictly_increasing() {
        let trace: Vec<Sample> = (0..16).map(|i| at(i as f32 * 0.002)).collect();

        // Three repeats gives two joins, which is where this can go wrong.
        let played = looped(&trace, 3);
        assert_eq!(
            played.len(),
            trace.len() * 3 - 2,
            "each repeat drops its opening"
        );

        let mut previous = f32::NEG_INFINITY;
        for (index, sample) in played.iter().enumerate() {
            assert!(
                sample.t > previous,
                "sample {index} went backwards: {} after {previous}",
                sample.t
            );
            previous = sample.t;
        }

        // And the result is a trace the renderer would accept, joins included.
        beam_trace::validate(&played).expect("a valid looped trace");
    }

    #[test]
    fn the_producer_fills_the_ring_with_a_valid_trace() {
        let source = LiveSource::spawn();
        std::thread::sleep(SETTLE);

        assert!(source.buffered() > 0, "the producer emitted nothing");

        let window = source.window(0.0, source.elapsed() as f32);
        assert!(!window.is_empty());
        // Whatever the renderer pulls out must satisfy the same contract a
        // file does: strictly increasing t, non-negative drive, finite floats.
        beam_trace::validate(&window).expect("the live window is a valid trace");
    }

    #[test]
    fn the_producer_runs_ahead_but_not_away() {
        let source = LiveSource::spawn();
        std::thread::sleep(SETTLE);

        let now = source.elapsed() as f32;
        let latest = source
            .window(0.0, f32::INFINITY)
            .last()
            .map(|s| s.t)
            .expect("samples");
        assert!(latest > now, "the producer is behind the clock");
        assert!(
            f64::from(latest - now) < LOOKAHEAD_SECONDS + 0.5,
            "the producer ran {} s ahead",
            latest - now
        );
    }

    #[test]
    fn switching_source_takes_effect_without_a_restart() {
        let source = LiveSource::spawn();
        std::thread::sleep(SETTLE);

        let mut controls = source.controls();
        controls.source = Source::Pattern(0);
        controls.generation += 1;
        source.set_controls(controls);
        std::thread::sleep(SETTLE);

        assert_eq!(source.controls().source, Source::Pattern(0));
        let window = source.window(0.0, source.elapsed() as f32 + 1.0);
        beam_trace::validate(&window).expect("still a valid trace after the switch");
    }
}

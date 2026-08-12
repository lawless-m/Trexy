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

use beam_sources::{Lissajous, PATTERNS};
use beam_trace::{RingBuffer, Sample};

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
}

impl Source {
    pub fn name(self) -> String {
        match self {
            Source::Lissajous => "lissajous".to_owned(),
            Source::Pattern(index) => PATTERNS[index].slug.to_owned(),
        }
    }
}

/// Everything the panel can change while the source runs.
#[derive(Clone, Copy, Debug)]
pub struct Controls {
    pub source: Source,
    pub lissajous: Lissajous,
    /// Bumped when the source changes, so the producer knows to drop whatever
    /// it had queued and start again from the present.
    pub generation: u64,
}

impl Default for Controls {
    fn default() -> Self {
        Self {
            source: Source::Lissajous,
            lissajous: Lissajous::default(),
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
}

impl LiveSource {
    pub fn spawn() -> Self {
        let ring = Arc::new(Mutex::new(RingBuffer::with_capacity(CAPACITY, 0.0)));
        let controls = Arc::new(Mutex::new(Controls::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let start = Instant::now();

        let thread = std::thread::Builder::new()
            .name("beam-source".to_owned())
            .spawn({
                let ring = Arc::clone(&ring);
                let controls = Arc::clone(&controls);
                let stop = Arc::clone(&stop);
                move || produce(&ring, &controls, &stop, start)
            })
            .expect("spawn the producer thread");

        Self {
            ring,
            controls,
            stop,
            thread: Some(thread),
            start,
        }
    }

    /// Seconds since the source started. This is the renderer's `T_now`, and
    /// the producer's timestamps are against the same origin.
    pub fn elapsed(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    pub fn controls(&self) -> Controls {
        *self.controls.lock().expect("controls lock")
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
}

impl Drop for LiveSource {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn produce(
    ring: &Mutex<RingBuffer>,
    controls: &Mutex<Controls>,
    stop: &AtomicBool,
    start: Instant,
) {
    let mut cursor = 0.0f64;
    let mut generation = u64::MAX;
    let mut last_t = f32::NEG_INFINITY;

    while !stop.load(Ordering::Relaxed) {
        let controls = *controls.lock().expect("controls lock");
        let now = start.elapsed().as_secs_f64();

        if controls.generation != generation {
            // A new source: abandon anything queued for the future and pick up
            // from the present instant.
            generation = controls.generation;
            cursor = cursor.max(now);
        }

        if cursor > now + LOOKAHEAD_SECONDS {
            std::thread::sleep(IDLE);
            continue;
        }

        let (chunk, seconds) = controls.chunk();
        {
            let mut ring = ring.lock().expect("ring lock");
            for sample in chunk {
                let mut sample = sample;
                sample.t += cursor as f32;
                // Chunks abut, so the first sample of one lands on the last of
                // the previous. `t` must strictly increase (TRACE-FORMAT.md §2)
                // and the duplicate carries no new information, so nudge it.
                if sample.t <= last_t {
                    sample.t = f32::from_bits(last_t.to_bits() + 1);
                }
                last_t = sample.t;
                ring.push(sample);
            }
        }
        cursor += f64::from(seconds);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Long enough for the producer to get several chunks ahead of the clock.
    const SETTLE: Duration = Duration::from_millis(150);

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

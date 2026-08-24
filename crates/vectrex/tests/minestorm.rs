//! The whole-machine gate: the BIOS boots and draws.
//!
//! ARCHITECTURE.md §6 step 4 — "MineStorm from BIOS is the integration test".
//! Everything below the CPU is unverified against real hardware, so what this
//! can assert is the *shape* of a Vectrex picture: a beam that moves, a gun
//! that is switched on and off, and the ~50 Hz recal cadence that gives the
//! machine its frame. A trace with those properties is not proof the picture
//! is right, but no picture can be right without them.
//!
//! The ROM is read in place from wherever it was extracted; it is copyrighted
//! and is never copied into this repository.

use beam_rasteriser::{Constants, Rasteriser};
use beam_trace::{flags, Sample, TraceHeader};
use vectrex::frontend::Frontend;
use vectrex::machine::Machine;
use vectrex::memory::{load, rom_dir};

/// Two seconds of Mine Storm, run once and shared: every assertion below is
/// about the same picture, and emulating it three times over proves nothing
/// extra.
fn picture() -> &'static [Sample] {
    static ONCE: std::sync::OnceLock<Vec<Sample>> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| run(2.0))
}

/// Run the machine for `seconds` and collect what the beam did.
fn run(seconds: f64) -> Vec<Sample> {
    let image = rom_dir().join("Mine Storm (1982).vec");
    let rom = load(&image).unwrap_or_else(|e| panic!("{e}"));

    let mut machine = Machine::new(rom, Vec::new());
    let mut frontend = Frontend::new();
    let mut rasteriser = Rasteriser::new(Constants::vectrex(), beam_trace::DEFAULT_EPSILON);
    let mut events = Vec::new();
    let mut out = Vec::new();

    while machine.seconds() < seconds {
        machine.step();
        for (cycle, pins) in &machine.memory.pin_log {
            frontend.cycle(*cycle, *pins, &mut events);
        }
        for event in events.drain(..) {
            rasteriser.push(event, &mut out);
        }
    }
    rasteriser.run_to(machine.seconds(), &mut out);
    out
}

#[test]
fn the_bios_draws_a_vectrex_picture() {
    let samples = picture();
    assert!(samples.len() > 1_000, "only {} samples in two seconds", samples.len());

    // A picture needs both halves: a gun that lights and a gun that is off
    // while the beam repositions.
    let lit = samples.iter().filter(|s| s.drive_r > 0.0).count();
    assert!(lit > 0, "the beam was never lit");
    assert!(lit < samples.len(), "the beam was never blanked");

    // Every position stays inside the tube's own coordinates.
    for s in samples {
        assert!(
            s.x.abs() <= 1.2 && s.y.abs() <= 1.2,
            "beam left the tube at ({}, {})",
            s.x,
            s.y
        );
    }
}

#[test]
fn the_recal_cadence_is_the_frame_rate() {
    let samples = picture();

    // Every break is the integrators being dumped. Most are not frame
    // boundaries: the BIOS re-zeros several times *within* a frame to stop the
    // integrators drifting, so the raw spacing is a couple of milliseconds and
    // says nothing about the refresh. What repeats at the frame rate is the
    // pattern — a long settling gap, then the frame's strokes.
    let breaks: Vec<f64> = samples
        .iter()
        .filter(|s| s.flags & flags::DISCONTINUITY != 0)
        .map(|s| f64::from(s.t))
        .collect();
    assert!(breaks.len() > 20, "only {} breaks in two seconds", breaks.len());

    // The gap before a frame's first stroke is far longer than the ones
    // between its strokes, which is what makes the boundary findable at all.
    const FRAME_GAP: f64 = 0.005;
    let starts: Vec<f64> = breaks
        .windows(2)
        .filter(|w| w[1] - w[0] > FRAME_GAP)
        .map(|w| w[1])
        .collect();
    assert!(starts.len() > 20, "only {} frames in two seconds", starts.len());

    let mut periods: Vec<f64> = starts.windows(2).map(|w| w[1] - w[0]).collect();
    periods.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = periods[periods.len() / 2];
    assert!(
        (0.018..=0.022).contains(&median),
        "frame period {:.2} ms is not the Vectrex's ~50 Hz",
        median * 1000.0
    );
}

#[test]
fn the_trace_survives_being_written_and_read_back() {
    let samples = run(0.25);
    let header = TraceHeader {
        epoch: 0.0,
        epsilon: beam_trace::DEFAULT_EPSILON,
        nominal_refresh_hz: 50.0,
        producer_id: "vectrex/via".to_owned(),
    };

    let path = std::env::temp_dir().join("trexy-minestorm-gate.btr0");
    beam_trace::write_file(&path, &header, &samples).expect("writing the trace");
    // The loader validates loudly: monotonic time, finite floats, no negative
    // drive. Anything the machine produced that a renderer could not eat comes
    // out here.
    let back = beam_trace::read_file(&path).expect("the loader validates what it reads");
    assert_eq!(back.samples.len(), samples.len());
    let _ = std::fs::remove_file(&path);
}


//! Drive the rasteriser from scripted VIA-level events — no CPU.
//!
//! ARCHITECTURE.md §6 step 2: the analogue model is validated on its own,
//! before a 6809 exists, so that when one arrives a wrong picture can only be
//! the CPU's fault. Writes a .btr0 for inspection.
//!
//!   cargo run -p beam-rasteriser --example scripted -- out.btr0 [ideal|vectrex]

use beam_rasteriser::{Constants, Event, Rasteriser};
use beam_trace::{Sample, TraceHeader};

/// One Vectrex-style frame: ZERO the integrators, then walk a square with the
/// beam lit, the way a turtle-style vector machine actually does it.
fn frame(r: &mut Rasteriser, t0: f64, out: &mut Vec<Sample>) -> f64 {
    let mut t = t0;
    let leg = 2.0e-4;

    // ZERO: dump the integrators to centre. Nothing is drawn across the jump.
    r.push(Event::Discontinuity { t, x: 0.0, y: 0.0 }, out);
    r.push(Event::Current { t, drive: [0.0; 3] }, out);

    // Move to a corner blanked, then light the gun and walk the square.
    let legs = [(1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), (0.0, -1.0)];
    r.push(Event::Rate { t, rx: -0.5, ry: -0.5 }, out);
    t += leg;
    r.push(Event::Current { t, drive: [1.0; 3] }, out);
    for (rx, ry) in legs {
        r.push(Event::Rate { t, rx, ry }, out);
        t += leg;
    }
    r.push(Event::Current { t, drive: [0.0; 3] }, out);
    r.push(Event::Rate { t, rx: 0.0, ry: 0.0 }, out);
    t += 2.0e-4;
    r.run_to(t, out);
    t
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().map(String::as_str).unwrap_or("scripted.btr0");
    let constants = match args.get(1).map(String::as_str) {
        Some("ideal") => Constants::ideal(),
        _ => Constants::vectrex(),
    };

    let epsilon = beam_trace::DEFAULT_EPSILON;
    let mut r = Rasteriser::new(constants, epsilon);
    let mut out = Vec::new();
    let mut t = 0.0;
    for _ in 0..4 {
        t = frame(&mut r, t, &mut out);
    }

    let header = TraceHeader {
        epoch: 0.0,
        epsilon,
        nominal_refresh_hz: 0.0,
        producer_id: "rasteriser/scripted".to_owned(),
    };
    match beam_trace::write_file(path, &header, &out) {
        Ok(()) => println!("{} samples over {:.4} s -> {path}", out.len(), t),
        Err(e) => eprintln!("{e}"),
    }
}

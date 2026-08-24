//! Run the BIOS and write what the beam did.
//!
//! ARCHITECTURE.md §6 step 4: with no cartridge fitted the Vectrex BIOS falls
//! into its own built-in game, so the 8 KiB image alone is a complete machine
//! to look at. Nothing here decides what is drawn — the CPU writes to the VIA,
//! the front end reads its pins, and the rasteriser turns those into a beam.
//!
//!   cargo run -p vectrex --example minestorm -- out.btr0 [seconds]

use beam_rasteriser::{Constants, Rasteriser};
use beam_trace::{Sample, TraceHeader};
use vectrex::frontend::Frontend;
use vectrex::machine::Machine;
use vectrex::memory::{load, rom_dir};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().map(String::as_str).unwrap_or("minestorm.btr0");
    let seconds: f64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(5.0);

    let image = rom_dir().join("Mine Storm (1982).vec");
    let rom = match load(&image) {
        Ok(rom) => rom,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    let epsilon = beam_trace::DEFAULT_EPSILON;
    let mut machine = Machine::new(rom, Vec::new());
    let mut frontend = Frontend::new();
    let mut rasteriser = Rasteriser::new(Constants::vectrex(), epsilon);
    let mut events = Vec::new();
    let mut out: Vec<Sample> = Vec::new();

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

    let header = TraceHeader {
        epoch: 0.0,
        epsilon,
        nominal_refresh_hz: 50.0,
        producer_id: "vectrex/via".to_owned(),
    };
    match beam_trace::write_file(path, &header, &out) {
        Ok(()) => println!(
            "{} samples over {:.3} s ({} cycles) -> {path}",
            out.len(),
            machine.seconds(),
            machine.cycles()
        ),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

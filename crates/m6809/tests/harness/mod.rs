//! Running one corpus stem against the core.

use m6809::bus::RecordingBus;
use m6809::moo;
use m6809::{Cpu, Registers};

/// Seed a bus with the test's initial memory, without recording those writes:
/// the machine never performed them.
fn seed(test: &moo::Test) -> RecordingBus {
    let mut bus = RecordingBus::new();
    for (address, value) in &test.initial.ram {
        bus.poke(*address, *value);
    }
    bus
}

fn describe(r: &Registers) -> String {
    format!(
        "pc={:04x} s={:04x} u={:04x} x={:04x} y={:04x} dp={:02x} a={:02x} b={:02x} cc={:02x}",
        r.pc, r.s, r.u, r.x, r.y, r.dp, r.a, r.b, r.cc
    )
}

/// What the corpus says was on the pins, as this core's bus records it.
fn expected(cycle: &moo::Cycle) -> (bool, u16, u8, bool) {
    if cycle.is_internal() {
        // Neither address nor data is valid, so there is nothing to compare
        // but the fact that the cycle happened at all.
        (false, 0, 0, true)
    } else {
        (cycle.is_write(), cycle.address, cycle.data, false)
    }
}

/// Run every test for one stem, panicking on the first divergence with enough
/// detail to find it.
fn check_tests(stem: &str, tests: &[moo::Test]) {
    for (index, test) in tests.iter().enumerate() {
        let mut bus = seed(test);
        let mut cpu = Cpu { regs: test.initial.registers, ..Cpu::new() };
        cpu.step(&mut bus);

        let where_ = format!("{stem}[{index}] ({})", test.name);

        // Bus traffic first: it localises a fault to the cycle it happened on,
        // where a register mismatch only says the end state was wrong.
        for (i, (want, got)) in test.cycles.iter().zip(&bus.cycles).enumerate() {
            assert_eq!(
                expected(want),
                (got.write, got.address, got.value, got.internal),
                "{where_}: cycle {i} differs\n  expected {} {:04x}={:02x}\n  got      {} {:04x}={:02x}",
                String::from_utf8_lossy(&want.status),
                want.address,
                want.data,
                if got.internal { "----" } else if got.write { "-wm-" } else { "r-m-" },
                got.address,
                got.value
            );
        }
        assert_eq!(
            bus.cycles.len(),
            test.cycles.len(),
            "{where_}: {} cycles, expected {}",
            bus.cycles.len(),
            test.cycles.len()
        );

        assert_eq!(
            cpu.regs,
            test.final_state.registers,
            "{where_}: registers differ\n  expected {}\n  got      {}",
            describe(&test.final_state.registers),
            describe(&cpu.regs)
        );

        for (address, value) in &test.final_state.ram {
            assert_eq!(
                bus.peek(*address),
                *value,
                "{where_}: memory at {address:04x} is {:02x}, expected {value:02x}",
                bus.peek(*address)
            );
        }
    }
}

/// The gate: every file in the corpus, cross-checked against its manifest.
///
/// Enumerating the directory rather than listing stems in the source is the
/// point — a list can silently stop covering an opcode that was added to the
/// corpus, and a gate that can be outgrown without saying so is not a gate.
pub fn check_corpus() {
    let dir = moo::corpus_dir();
    let counts = manifest(&dir);
    let stems = match moo::stems() {
        Ok(stems) => stems,
        Err(e) => panic!(
            "{e}\nThe MC6809 verification corpus is not distributed with this \
             project. Clone it to {} , or set M6809_CORPUS.",
            dir.display()
        ),
    };

    let on_disk: Vec<&str> = stems.iter().map(|(stem, _)| stem.as_str()).collect();
    let listed: Vec<&str> = counts.iter().map(|(stem, _)| stem.as_str()).collect();
    assert_eq!(on_disk, listed, "the corpus directory and its manifest disagree");

    let mut total = 0usize;
    for ((stem, path), (_, expected)) in stems.iter().zip(&counts) {
        let tests = moo::read(path).unwrap_or_else(|e| panic!("{stem}: {e}"));
        assert_eq!(
            tests.len(),
            *expected,
            "{stem}: {} tests on disk, manifest says {expected}",
            tests.len()
        );
        check_tests(stem, &tests);
        total += tests.len();
    }
    println!("{total} tests across {} stems verified against the corpus", stems.len());
}

/// The manifest's stems and test counts, in file order.
fn manifest(dir: &std::path::Path) -> Vec<(String, usize)> {
    let path = dir.join("manifest.txt");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e}\nThe MC6809 verification corpus is not distributed with \
             this project. Clone it to {} , or set M6809_CORPUS.",
            path.display(),
            dir.display()
        )
    });
    let mut entries: Vec<(String, usize)> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            // "<stem> <count>", and a page-prefixed stem contains a space.
            let (stem, count) = line.rsplit_once(' ').expect("manifest line");
            (stem.to_string(), count.trim().parse().expect("test count"))
        })
        .collect();
    entries.sort();
    entries
}

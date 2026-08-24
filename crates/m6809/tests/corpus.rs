//! Verify the core against every test in the single-step corpus.
//!
//! The gate for the whole CPU: all 315 opcode stems, all 315,000 tests, each
//! matched on final registers, final memory, cycle count **and** per-cycle bus
//! traffic. The traffic is the part that makes this worth doing: a core can
//! produce the right answer with the wrong cycles, and on a machine whose
//! video timing is the VIA counting those cycles, wrong is wrong.
//!
//! The corpus is read in place from wherever it was cloned — it carries no
//! licence to redistribute, so it is never copied into this repository
//! (ARCHITECTURE.md §5).

mod harness;

#[test]
fn every_opcode_matches_the_corpus() {
    harness::check_corpus();
}

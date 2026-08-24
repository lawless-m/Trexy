//! The verification corpus must decode before it can verify anything.
//!
//! Read in place from wherever it was cloned — it carries no licence to
//! redistribute, so it is never copied into this repository
//! (ARCHITECTURE.md §5).

use m6809::moo;

#[test]
fn every_corpus_file_decodes() {
    let stems = match moo::stems() {
        Ok(stems) => stems,
        Err(e) => panic!("{e}\nSet M6809_CORPUS to the corpus v1 directory."),
    };
    assert!(
        stems.len() > 300,
        "expected the full opcode space, found {} files in {}",
        stems.len(),
        moo::corpus_dir().display()
    );

    let mut total = 0usize;
    for (stem, path) in &stems {
        let tests = moo::read(path).unwrap_or_else(|e| panic!("{stem}: {e}"));
        assert!(!tests.is_empty(), "{stem}: no tests");
        total += tests.len();

        for (i, t) in tests.iter().enumerate() {
            assert!(!t.bytes.is_empty(), "{stem}[{i}]: no instruction bytes");
            assert!(!t.cycles.is_empty(), "{stem}[{i}]: no cycles recorded");

            // Initial and final list the same addresses: the test states what
            // memory it touched, and both ends describe that same window.
            let mut before: Vec<u16> = t.initial.ram.iter().map(|(a, _)| *a).collect();
            let mut after: Vec<u16> = t.final_state.ram.iter().map(|(a, _)| *a).collect();
            before.sort_unstable();
            after.sort_unstable();
            assert_eq!(before, after, "{stem}[{i}]: initial and final RAM differ in extent");

            for c in &t.cycles {
                assert!(
                    c.is_read() || c.is_write() || c.is_internal(),
                    "{stem}[{i}]: unknown cycle status {:?}",
                    String::from_utf8_lossy(&c.status)
                );
            }
        }
    }
    println!("decoded {} tests across {} stems", total, stems.len());
}

#[test]
fn the_manifest_agrees_with_what_is_on_disk() {
    let dir = moo::corpus_dir();
    let manifest = match std::fs::read_to_string(dir.join("manifest.txt")) {
        Ok(text) => text,
        Err(e) => panic!("{}: {e}", dir.join("manifest.txt").display()),
    };
    let mut checked = 0usize;
    for line in manifest.lines().filter(|l| !l.trim().is_empty()) {
        // "<stem> <count>", and a prefixed stem contains a space of its own.
        let (stem, count) = line.rsplit_once(' ').expect("manifest line");
        let expected: usize = count.trim().parse().expect("test count");
        let tests = moo::read(dir.join(format!("{stem}.moo.gz")))
            .unwrap_or_else(|e| panic!("{stem}: {e}"));
        assert_eq!(tests.len(), expected, "{stem}: manifest says {expected}");
        checked += 1;
    }
    assert!(checked > 300, "manifest listed only {checked} stems");
}

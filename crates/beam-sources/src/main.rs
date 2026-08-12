//! Write the seven acceptance fixtures.
//!
//!     cargo run -p beam-sources -- fixtures

use std::path::PathBuf;
use std::process::ExitCode;

use beam_sources::PATTERNS;

fn main() -> ExitCode {
    let Some(out) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: beam-sources <output-directory>");
        return ExitCode::from(2);
    };

    if let Err(e) = std::fs::create_dir_all(&out) {
        eprintln!("{}: {e}", out.display());
        return ExitCode::FAILURE;
    }

    for pattern in PATTERNS {
        let trace = pattern.build();
        let path = out.join(pattern.file_name());
        if let Err(e) = beam_trace::write_file(&path, &trace.header, &trace.samples) {
            eprintln!("{}: {e}", path.display());
            return ExitCode::FAILURE;
        }
        let seconds = trace.samples.last().map_or(0.0, |s| s.t);
        println!(
            "{:<34} {:>7} samples over {seconds:.3} s  ({:.0}/s)  — {}",
            pattern.file_name(),
            trace.samples.len(),
            trace.samples.len() as f32 / seconds,
            pattern.isolates,
        );
    }

    println!("wrote {} fixtures to {}", PATTERNS.len(), out.display());
    ExitCode::SUCCESS
}

//! The native shell: window, UI, source selection, record/replay.

mod app;
mod gpu;
mod shaders;

use std::process::ExitCode;

const USAGE: &str = "\
tube-shell — vector tube renderer

    tube-shell                 open the window
    tube-shell --self-check    probe the GPU headlessly and exit
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        None => match app::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        },
        Some("--self-check") => match gpu::self_check() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        },
        Some("--help" | "-h") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("unknown argument {other}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

//! The native shell: window, UI, source selection, record/replay.

mod app;
mod gpu;
mod headless;
mod shaders;

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
tube-shell — vector tube renderer

    tube-shell                              open the window
    tube-shell --self-check                 probe the GPU headlessly and exit
    tube-shell --headless-debug --out FILE  deposit the debug spans to a PNG
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let result = match args.first().map(String::as_str) {
        None => app::run().map_err(|e| e.to_string()),
        Some("--self-check") => gpu::self_check(),
        Some("--headless-debug") => match parse_out(&args[1..]) {
            Ok(out) => headless::debug(&out),
            Err(e) => Err(e),
        },
        Some("--help" | "-h") => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("unknown argument {other}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn parse_out(rest: &[String]) -> Result<PathBuf, String> {
    match rest {
        [flag, path] if flag == "--out" => Ok(PathBuf::from(path)),
        _ => Err(format!("--headless-debug needs --out FILE\n\n{USAGE}")),
    }
}

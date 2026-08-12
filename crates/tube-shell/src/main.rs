//! The native shell: window, UI, source selection, record/replay.

mod app;
mod gpu;
mod headless;
mod shaders;

use std::path::PathBuf;
use std::process::ExitCode;

use headless::DebugOptions;

const USAGE: &str = "\
tube-shell — vector tube renderer

    tube-shell                              open the window
    tube-shell --self-check                 probe the GPU headlessly and exit
    tube-shell --headless-debug --out FILE [options]

  --debug-splat    select the forbidden point-splat path (reference only)
  --check-beading  measure evenness along a fast stroke; PASS/FAIL
  --sim-ms N       draw, then decay for N ms and check both rates; PASS/FAIL
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let result = match args.first().map(String::as_str) {
        None => app::run().map_err(|e| e.to_string()),
        Some("--self-check") => gpu::self_check(),
        Some("--headless-debug") => {
            parse_debug(&args[1..]).and_then(|(out, options)| headless::debug(&out, options))
        }
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

fn parse_debug(rest: &[String]) -> Result<(PathBuf, DebugOptions), String> {
    let mut out = None;
    let mut options = DebugOptions::default();
    let mut args = rest.iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => {
                out = Some(PathBuf::from(
                    args.next().ok_or("--out needs a path".to_owned())?,
                ));
            }
            "--debug-splat" => options.splat = true,
            "--check-beading" => options.check_beading = true,
            "--sim-ms" => {
                let value = args.next().ok_or("--sim-ms needs a number".to_owned())?;
                options.sim_ms = Some(
                    value
                        .parse()
                        .map_err(|_| format!("--sim-ms {value} is not a number"))?,
                );
            }
            other => return Err(format!("unknown argument {other}\n\n{USAGE}")),
        }
    }

    let out = out.ok_or_else(|| format!("--headless-debug needs --out FILE\n\n{USAGE}"))?;
    Ok((out, options))
}

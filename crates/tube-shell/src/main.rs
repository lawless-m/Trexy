//! The native shell: window, UI, source selection, record/replay.

use std::path::PathBuf;
use std::process::ExitCode;

use tube_renderer::View;
use tube_shell::headless::{self, DebugOptions};
use tube_shell::render::{self, DEFAULT_FRAME_HZ, RenderOptions};
use tube_shell::{app, gpu, regression};

const USAGE: &str = "\
tube-shell — vector tube renderer

    tube-shell                              open the window
    tube-shell --self-check                 probe the GPU headlessly and exit
    tube-shell --headless --trace FILE --out PNG [options]
    tube-shell --headless-debug --out FILE [options]
    tube-shell --write-smoke-trace FILE     write a small .btr0 for smoke tests
    tube-shell --write-profiles DIR         write the shipped tube profiles
    tube-shell --bless                      re-render every blessed regression image

  --headless options
    --sim-seconds S  how much of the trace to replay (default: all of it)
    --frame-hz N     simulated host cadence (default: 60)
    --view NAME      beauty | fast | slow | deposit | energy | samples
    --params FILE    a tube profile as TOML (default: built-in Vectrex)

  --headless-debug options
    --debug-splat    select the forbidden point-splat path (reference only)
    --check-beading  measure evenness along a fast stroke; PASS/FAIL
    --sim-ms N       draw, then decay for N ms and check both rates; PASS/FAIL
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let result = match args.first().map(String::as_str) {
        None => app::run().map_err(|e| e.to_string()),
        Some("--self-check") => gpu::self_check(),
        Some("--headless") => parse_render(&args[1..]).and_then(|o| render::render(&o)),
        Some("--headless-debug") => {
            parse_debug(&args[1..]).and_then(|(out, options)| headless::debug(&out, options))
        }
        Some("--write-smoke-trace") => match args.get(1) {
            Some(path) => write_smoke_trace(Path::new(path)),
            None => Err(format!("--write-smoke-trace needs a path\n\n{USAGE}")),
        },
        Some("--bless") => bless(),
        Some("--write-profiles") => match args.get(1) {
            Some(path) => write_profiles(Path::new(path)),
            None => Err(format!("--write-profiles needs a directory\n\n{USAGE}")),
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

use std::path::Path;

fn write_smoke_trace(path: &Path) -> Result<(), String> {
    let trace = render::smoke_trace();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    beam_trace::write_file(path, &trace.header, &trace.samples)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    println!("wrote {} ({} samples)", path.display(), trace.samples.len());
    Ok(())
}

/// Re-render every regression image from the fixtures.
///
/// A renderer change that alters these is not a failure by itself — but it
/// must be an intended change, blessed deliberately and in the same commit as
/// whatever caused it (FIRST-SLICE.md §5).
fn bless() -> Result<(), String> {
    let params = regression::blessed_params()?;
    for case in regression::CASES {
        let out = case.blessed_path();
        render::render(&case.options(out.clone(), params))?;
        println!("  pattern {} — {}", case.number, case.note);
    }
    println!("blessed {} images", regression::CASES.len());
    Ok(())
}

fn write_profiles(dir: &Path) -> Result<(), String> {
    for profile in [tube_renderer::vectrex_default(), tube_renderer::neutral()] {
        let path = dir.join(format!("{}.toml", profile.name));
        profile.save(&path)?;
        println!("wrote {}", path.display());
    }
    Ok(())
}

fn parse_render(rest: &[String]) -> Result<RenderOptions, String> {
    let mut trace = None;
    let mut out = None;
    let mut sim_seconds = None;
    let mut frame_hz = DEFAULT_FRAME_HZ;
    let mut view = View::default();
    let mut params = tube_renderer::TubeParams::default();
    let mut args = rest.iter();

    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--trace" => trace = Some(PathBuf::from(value()?)),
            "--out" => out = Some(PathBuf::from(value()?)),
            "--sim-seconds" => sim_seconds = Some(parse_number(arg, value()?)?),
            "--frame-hz" => frame_hz = parse_number(arg, value()?)?,
            "--params" => {
                params = tube_renderer::Profile::load(value()?)?.params;
            }
            "--view" => {
                let name = value()?;
                view = View::from_name(name).ok_or_else(|| {
                    let names: Vec<&str> = View::ALL.iter().map(|v| v.name()).collect();
                    format!("unknown view {name}; try one of {}", names.join(", "))
                })?;
            }
            other => return Err(format!("unknown argument {other}\n\n{USAGE}")),
        }
    }

    Ok(RenderOptions {
        trace: trace.ok_or_else(|| format!("--headless needs --trace FILE\n\n{USAGE}"))?,
        out: out.ok_or_else(|| format!("--headless needs --out PNG\n\n{USAGE}"))?,
        sim_seconds,
        frame_hz,
        view,
        params,
    })
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
                options.sim_ms = Some(parse_number("--sim-ms", value)?);
            }
            other => return Err(format!("unknown argument {other}\n\n{USAGE}")),
        }
    }

    let out = out.ok_or_else(|| format!("--headless-debug needs --out FILE\n\n{USAGE}"))?;
    Ok((out, options))
}

fn parse_number(flag: &str, value: &str) -> Result<f64, String> {
    value
        .parse()
        .map_err(|_| format!("{flag} {value} is not a number"))
}

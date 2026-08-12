//! Formal headless rendering: a trace file and parameters in, a PNG out.
//!
//! This is the regression path (FIRST-SLICE.md §1 deliverable 8) and the only
//! way to look at the renderer's output without a window.

use std::path::Path;

use beam_trace::{Sample, Trace, TraceHeader};
use tube_renderer::{DepositMode, Field, FieldShaders, READOUT_PASSES, TubeParams, View};

use crate::gpu;
use crate::shaders::{ShaderLibrary, shader_dir};

/// Rows of display resolution for headless renders.
pub const DISPLAY_HEIGHT: u32 = 512;

/// Host cadence to simulate when none is given. The field must not depend on
/// it — that is acceptance pattern 6 — but something has to drive the loop.
pub const DEFAULT_FRAME_HZ: f64 = 60.0;

pub struct RenderOptions {
    pub trace: std::path::PathBuf,
    pub out: std::path::PathBuf,
    pub sim_seconds: Option<f64>,
    pub frame_hz: f64,
    pub view: View,
    pub params: TubeParams,
}

pub fn render(options: &RenderOptions) -> Result<(), String> {
    let trace = beam_trace::read_file(&options.trace)
        .map_err(|e| format!("{}: {e}", options.trace.display()))?;

    let (device, queue) = gpu::headless_device()?;
    let dir = shader_dir();
    let mut shaders = ShaderLibrary::new();
    shaders.reload_dir(&dir);
    if let Some(error) = shaders.error() {
        return Err(error.to_owned());
    }
    let source = |name: &str| {
        shaders
            .get(name)
            .ok_or_else(|| format!("{}/{name} is missing", dir.display()))
    };

    let mut field = Field::new(
        &device,
        &queue,
        DISPLAY_HEIGHT,
        options.params,
        FieldShaders {
            deposit: source("deposit.wgsl")?,
            splat: source("deposit_splat.wgsl")?,
            resolve: source("deposit_resolve.wgsl")?,
            phosphor: source("phosphor.wgsl")?,
            deposit_total: source("deposit_total.wgsl")?,
            readout: source("readout.wgsl")?,
            blur: source("blur.wgsl")?,
            tonemap: source("tonemap.wgsl")?,
            view: source("view.wgsl")?,
            sample_points: source("sample_points.wgsl")?,
        },
        0.0,
    );
    field.clear(&device, &queue);

    // The trace's own length, unless overridden.
    let duration = options
        .sim_seconds
        .unwrap_or_else(|| trace.samples.last().map_or(0.0, |s| f64::from(s.t)));
    let frames = (duration * options.frame_hz).ceil() as u64;

    let mut substeps = 0;
    for frame in 1..=frames {
        let now = frame as f64 / options.frame_hz;
        substeps += field.advance(
            &device,
            &queue,
            &trace.samples,
            now.min(duration),
            DepositMode::Analytic,
        );
    }
    let timings = field.render(&device, &queue, options.view, &trace.samples);

    let image = field.readout().read_back(&device, &queue);
    let width = field.output_width();
    let height = field.output_height();
    let lit = image.iter().filter(|p| p[0] + p[1] + p[2] > 0.0).count();

    write_png(&options.out, width, height, &image)?;

    println!(
        "{} ({} samples, producer {:?})",
        options.trace.display(),
        trace.samples.len(),
        trace.header.producer_id
    );
    println!(
        "{duration:.3} s at {} Hz: {frames} frames, {substeps} substeps",
        options.frame_hz
    );
    println!(
        "{width}x{height}, view {}: {lit} non-black pixels",
        options.view.name()
    );
    print_timings(&timings);
    println!("wrote {}", options.out.display());

    if lit == 0 {
        return Err("the render is entirely black".to_owned());
    }
    Ok(())
}

fn print_timings(timings: &tube_renderer::Timings) {
    println!(
        "field advance: {:.2} ms wall clock over {} substeps",
        timings.field_advance_micros / 1000.0,
        timings.substeps
    );
    if !timings.gpu_supported {
        println!("readout passes: no timestamp queries on this adapter");
        return;
    }
    let per_pass: Vec<String> = READOUT_PASSES
        .iter()
        .zip(&timings.readout)
        .map(|(label, micros)| format!("{label} {micros:.1}"))
        .collect();
    println!(
        "readout passes (us): {} | total {:.1}",
        per_pass.join(", "),
        timings.readout_total_micros()
    );
}

/// A short trace exercising a slow stroke, a fast stroke, a parked dot and a
/// blanking taper, for smoke-testing the chain without the generator.
pub fn smoke_trace() -> Trace {
    let mut samples = Vec::new();
    let mut push = |s: Sample| samples.push(s);

    // A box, drawn steadily.
    let corners = [
        (-0.5, -0.5),
        (0.5, -0.5),
        (0.5, 0.5),
        (-0.5, 0.5),
        (-0.5, -0.5),
    ];
    for (index, (x, y)) in corners.iter().enumerate() {
        push(Sample::mono(*x, *y, 1.0, index as f32 * 0.004));
    }

    // A blanked hop to the middle, then a parked dot held bright.
    let mut hop = Sample::mono(0.0, 0.0, 0.0, 0.017);
    hop.flags |= beam_trace::flags::DISCONTINUITY;
    push(hop);
    push(Sample::mono(0.0, 0.0, 2.0, 0.019));

    // A stroke whose drive tapers to nothing: no gap, no terminal dot.
    let mut start = Sample::mono(-0.7, -0.8, 1.0, 0.020);
    start.flags |= beam_trace::flags::DISCONTINUITY;
    push(start);
    push(Sample::mono(0.7, -0.8, 0.0, 0.024));

    Trace {
        header: TraceHeader {
            epoch: 0.0,
            epsilon: beam_trace::DEFAULT_EPSILON,
            nominal_refresh_hz: 50.0,
            producer_id: "synthetic/smoke".to_owned(),
        },
        samples,
    }
}

/// Write display values as an sRGB-encoded PNG.
pub fn write_png(path: &Path, width: u32, height: u32, image: &[[f32; 4]]) -> Result<(), String> {
    let pixels: Vec<u8> = image
        .iter()
        .flat_map(|p| [0, 1, 2].map(|c| (srgb_encode(p[c].clamp(0.0, 1.0)) * 255.0).round() as u8))
        .collect();

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    image::RgbImage::from_raw(width, height, pixels)
        .ok_or_else(|| "image does not match the given size".to_owned())?
        .save(path)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// The tonemap emits linear display values; PNG is sRGB, so the encode happens
/// here. On screen the sRGB surface format does the same job.
fn srgb_encode(linear: f32) -> f32 {
    if linear <= 0.003_130_8 {
        12.92 * linear
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

//! Headless debug dumps: deposit hardcoded spans with no window and write the
//! result as a PNG, so the deposition pass is mechanically verifiable.

use std::path::Path;

use beam_trace::{Sample, flags};
use tube_renderer::{Deposit, DepositMode, DepositParams, DepositShaders, TubeProfile};

use crate::gpu;
use crate::shaders::{ShaderLibrary, shader_dir};

/// Rows of *display* resolution; the deposit buffers are `SUPERSAMPLE`× this.
pub const DISPLAY_HEIGHT: u32 = 512;

/// Energy below this is treated as nothing landed here.
const THRESHOLD: f32 = 1e-4;

/// Minimum ratio of dimmest to brightest along a stroke's centreline before we
/// call it beaded. The analytic path is flat to within rounding; a splat path
/// on the same stroke drops to near zero between samples.
const BEADING_LIMIT: f32 = 0.8;

/// What to render.
#[derive(Clone, Copy, Debug, Default)]
pub struct DebugOptions {
    /// Select the forbidden point-splat path (FIRST-SLICE.md §4). Debug only.
    pub splat: bool,
    /// Measure evenness along a fast stroke's centreline and PASS/FAIL on it.
    pub check_beading: bool,
}

/// Two horizontal strokes at the same drive, one swept four times slower than
/// the other. Dwell time is the whole point of the analytic integral, so the
/// slow stroke must come out brighter with no other difference between them.
pub fn hardcoded_spans() -> Vec<Sample> {
    let mut samples = vec![
        // Slow stroke: 4 ms across.
        Sample::mono(-0.6, 0.3, 1.0, 0.000),
        Sample::mono(0.6, 0.3, 1.0, 0.004),
        // Fast stroke: 1 ms across, same drive.
        Sample::mono(-0.6, -0.3, 1.0, 0.005),
        Sample::mono(0.6, -0.3, 1.0, 0.006),
    ];
    // The beam did not sweep from the end of one stroke to the start of the
    // other, so nothing is deposited across that gap.
    samples[2].flags |= flags::DISCONTINUITY;
    samples
}

/// A single fast stroke, sampled the way a producer would sample a straight
/// ramp: sparsely, since TRACE-FORMAT.md §4 only demands enough samples to
/// stay within ε of the true path, and a straight line needs almost none.
///
/// That sparsity is exactly what beads a splat path — the samples here sit
/// roughly 20σ apart — while the analytic path must render it seamlessly,
/// including across the joins between spans.
const BEADING_SAMPLES: usize = 12;
const BEADING_Y: f32 = 0.0;
const BEADING_X0: f32 = -0.7;
const BEADING_X1: f32 = 0.7;

pub fn beading_stroke() -> Vec<Sample> {
    // 0.6 ms end to end: fast, which is where beading shows worst.
    let duration = 0.000_6f32;
    (0..BEADING_SAMPLES)
        .map(|i| {
            let f = i as f32 / (BEADING_SAMPLES - 1) as f32;
            Sample::mono(
                BEADING_X0 + (BEADING_X1 - BEADING_X0) * f,
                BEADING_Y,
                1.0,
                duration * f,
            )
        })
        .collect()
}

pub fn debug(out: &Path, options: DebugOptions) -> Result<(), String> {
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

    let mut deposit = Deposit::new(
        &device,
        DISPLAY_HEIGHT,
        TubeProfile::default(),
        DepositParams::default(),
        DepositShaders {
            deposit: source("deposit.wgsl")?,
            splat: source("deposit_splat.wgsl")?,
            resolve: source("deposit_resolve.wgsl")?,
        },
    );

    let mode = if options.splat {
        DepositMode::Splat
    } else {
        DepositMode::Analytic
    };
    if options.splat {
        println!("DEBUG: point-splat path selected — this is the counter-example");
    }

    if options.check_beading {
        return check_beading(&device, &queue, &mut deposit, out, mode);
    }

    // The splat path exists only to picture beading, so it gets the stroke
    // that beads rather than the dwell-ratio pair.
    let samples = if options.splat {
        beading_stroke()
    } else {
        hardcoded_spans()
    };
    deposit.run(&device, &queue, &samples, mode);
    let field = deposit.read_back(&device, &queue);

    let peak = brightest(&field);
    let lit = field.iter().filter(|t| luminance(t) > THRESHOLD).count();
    write_png(out, deposit.width(), deposit.height(), &field, peak)?;

    println!(
        "deposit {}x{}: peak {peak:.6}, {lit} texels above {THRESHOLD}",
        deposit.width(),
        deposit.height(),
    );
    if options.splat {
        let evenness = evenness(&field, deposit.width(), deposit.height());
        println!("centreline evenness {evenness:.4} — beaded, as intended");
    } else {
        // The two strokes differ only in how long the beam took to draw them,
        // so the brightness ratio is the dwell-time term of the integral alone.
        let split = (deposit.height() / 2 * deposit.width()) as usize;
        let slow = brightest(&field[..split]);
        let fast = brightest(&field[split..]);
        println!(
            "slow stroke {slow:.6} / fast stroke {fast:.6} = {:.2}x",
            slow / fast
        );
    }
    println!("wrote {}", out.display());

    if lit == 0 {
        return Err("nothing was deposited".to_owned());
    }
    Ok(())
}

/// Render one fast stroke and measure how even it is along its centreline.
///
/// Runs both paths: the selected one is asserted on, the other is reported for
/// contrast, because a threshold nothing ever fails is not a check.
fn check_beading(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    deposit: &mut Deposit,
    out: &Path,
    mode: DepositMode,
) -> Result<(), String> {
    let samples = beading_stroke();

    let measure = |deposit: &mut Deposit, mode| {
        deposit.run(device, queue, &samples, mode);
        let field = deposit.read_back(device, queue);
        (evenness(&field, deposit.width(), deposit.height()), field)
    };

    let (analytic, analytic_field) = measure(deposit, DepositMode::Analytic);
    let (splat, splat_field) = measure(deposit, DepositMode::Splat);

    let (ratio, field) = match mode {
        DepositMode::Analytic => (analytic, &analytic_field),
        DepositMode::Splat => (splat, &splat_field),
    };
    let peak = brightest(field);
    write_png(out, deposit.width(), deposit.height(), field, peak)?;

    println!("centreline evenness (min/max along the stroke):");
    println!("  analytic {analytic:.4}");
    println!("  splat    {splat:.4}   (debug reference — expected to bead)");
    println!("wrote {}", out.display());

    if ratio >= BEADING_LIMIT {
        println!("PASS: {ratio:.4} >= {BEADING_LIMIT}, no beading");
        Ok(())
    } else {
        Err(format!(
            "FAIL: {ratio:.4} < {BEADING_LIMIT}, the stroke beads"
        ))
    }
}

/// Ratio of dimmest to brightest along the interior of the known centreline.
///
/// The ends are excluded: the erf terms roll the stroke off there by design,
/// and that rolloff is correct physics, not beading.
fn evenness(field: &[[f32; 4]], width: u32, height: u32) -> f32 {
    const SAMPLES: usize = 400;
    const INTERIOR: f32 = 0.1;

    let mut min = f32::INFINITY;
    let mut max: f32 = 0.0;
    for i in 0..SAMPLES {
        let f = INTERIOR + (1.0 - 2.0 * INTERIOR) * (i as f32 / (SAMPLES - 1) as f32);
        let x = BEADING_X0 + (BEADING_X1 - BEADING_X0) * f;
        let texel_x = (x + 1.0) * width as f32 / 2.0;
        let texel_y = (1.0 - BEADING_Y) * height as f32 / 2.0;
        let value = sample_bilinear(field, width, height, texel_x, texel_y);
        min = min.min(value);
        max = max.max(value);
    }
    if max > 0.0 { min / max } else { 0.0 }
}

/// Bilinear sample of the field, so the measurement is not quantised by the
/// texel grid.
fn sample_bilinear(field: &[[f32; 4]], width: u32, height: u32, x: f32, y: f32) -> f32 {
    let x = (x - 0.5).clamp(0.0, (width - 1) as f32);
    let y = (y - 0.5).clamp(0.0, (height - 1) as f32);
    let (x0, y0) = (x.floor() as u32, y.floor() as u32);
    let (x1, y1) = ((x0 + 1).min(width - 1), (y0 + 1).min(height - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);

    let at = |cx: u32, cy: u32| luminance(&field[(cy * width + cx) as usize]);
    let top = at(x0, y0) * (1.0 - fx) + at(x1, y0) * fx;
    let bottom = at(x0, y1) * (1.0 - fx) + at(x1, y1) * fx;
    top * (1.0 - fy) + bottom * fy
}

fn luminance(texel: &[f32; 4]) -> f32 {
    texel[0].max(texel[1]).max(texel[2])
}

fn brightest(field: &[[f32; 4]]) -> f32 {
    field.iter().map(luminance).fold(0.0f32, f32::max)
}

/// Grayscale, auto-exposed to the peak so the dump is always legible. This is
/// a debug dump, not the tonemapped output.
fn write_png(
    path: &Path,
    width: u32,
    height: u32,
    field: &[[f32; 4]],
    peak: f32,
) -> Result<(), String> {
    let exposure = if peak > 0.0 { 1.0 / peak } else { 0.0 };
    let pixels: Vec<u8> = field
        .iter()
        .map(|texel| {
            let energy = (texel[0] + texel[1] + texel[2]) / 3.0;
            ((energy * exposure).clamp(0.0, 1.0) * 255.0).round() as u8
        })
        .collect();

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    image::GrayImage::from_raw(width, height, pixels)
        .ok_or_else(|| "field does not match the image size".to_owned())?
        .save(path)
        .map_err(|e| format!("{}: {e}", path.display()))
}

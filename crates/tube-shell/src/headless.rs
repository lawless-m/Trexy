//! Headless debug dumps: deposit hardcoded spans with no window and write the
//! result as a PNG, so the deposition pass is mechanically verifiable.

use std::path::{Path, PathBuf};

use beam_trace::{Sample, flags};
use tube_renderer::{
    Component, Deposit, DepositMode, DepositParams, DepositShaders, Field, FieldShaders,
    TubeParams, TubeProfile,
};

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
    /// Deposit the debug spans, then advance this many milliseconds with
    /// nothing further to deposit, and check the two decay rates.
    pub sim_ms: Option<f64>,
}

/// The debug spans end here; everything after is pure decay.
const TRACE_END: f64 = 0.007;

/// The fast component must be all but gone after the simulated interval.
const FAST_MUST_LOSE: f32 = 0.99;
/// The slow component must still be substantially there.
const SLOW_MUST_KEEP: f32 = 0.90;

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

    if let Some(sim_ms) = options.sim_ms {
        return simulate_decay(&device, &queue, &shaders, &dir, out, sim_ms, mode);
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

/// Deposit the debug spans, then let the tube sit dark for `sim_ms` and see
/// what each component has left.
///
/// The two time constants are three orders of magnitude apart, so this is the
/// sharpest possible check that they are genuinely separate buffers with
/// separate decay and not one smeared average: over a single 1.25 ms substep
/// the fast component loses essentially everything while the slow one barely
/// notices.
fn simulate_decay(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    shaders: &ShaderLibrary,
    dir: &Path,
    out: &Path,
    sim_ms: f64,
    mode: DepositMode,
) -> Result<(), String> {
    let source = |name: &str| {
        shaders
            .get(name)
            .ok_or_else(|| format!("{}/{name} is missing", dir.display()))
    };
    let mut field = Field::new(
        device,
        DISPLAY_HEIGHT,
        TubeParams::default(),
        FieldShaders {
            deposit: source("deposit.wgsl")?,
            splat: source("deposit_splat.wgsl")?,
            resolve: source("deposit_resolve.wgsl")?,
            phosphor: source("phosphor.wgsl")?,
            readout: source("readout.wgsl")?,
            blur: source("blur.wgsl")?,
            tonemap: source("tonemap.wgsl")?,
        },
        0.0,
    );
    field.clear(device, queue);

    let samples = hardcoded_spans();
    let drawn = field.advance(device, queue, &samples, TRACE_END, mode);
    let fast_before = brightest(&field.read_back(device, queue, Component::Fast));
    let slow_before = brightest(&field.read_back(device, queue, Component::Slow));

    // Nothing to deposit from here: the field only decays.
    let dark = field.advance(device, queue, &[], TRACE_END + sim_ms / 1000.0, mode);
    let fast = field.read_back(device, queue, Component::Fast);
    let slow = field.read_back(device, queue, Component::Slow);
    let fast_after = brightest(&fast);
    let slow_after = brightest(&slow);

    let combined: Vec<[f32; 4]> = fast
        .iter()
        .zip(slow.iter())
        .map(|(f, s)| std::array::from_fn(|c| f[c] + s[c]))
        .collect();
    let (width, height) = (field.width(), field.height());
    write_png(out, width, height, &combined, brightest(&combined))?;
    write_png(&with_suffix(out, "fast"), width, height, &fast, fast_before)?;
    write_png(&with_suffix(out, "slow"), width, height, &slow, slow_before)?;

    let kept = |after: f32, before: f32| if before > 0.0 { after / before } else { 0.0 };
    let fast_kept = kept(fast_after, fast_before);
    let slow_kept = kept(slow_after, slow_before);

    println!(
        "{drawn} substeps drawing, then {dark} dark ({sim_ms} ms requested, \
         {:.2} ms simulated)",
        dark as f64 * tube_renderer::SUBSTEP_SECONDS * 1000.0
    );
    println!(
        "fast {fast_before:.6} -> {fast_after:.6}  ({:.4}% kept)",
        fast_kept * 100.0
    );
    println!(
        "slow {slow_before:.6} -> {slow_after:.6}  ({:.2}% kept)",
        slow_kept * 100.0
    );
    println!(
        "wrote {}, {}, {}",
        out.display(),
        with_suffix(out, "fast").display(),
        with_suffix(out, "slow").display()
    );

    let mut failures = Vec::new();
    if fast_kept > 1.0 - FAST_MUST_LOSE {
        failures.push(format!(
            "fast kept {:.4}%, must lose at least {:.0}%",
            fast_kept * 100.0,
            FAST_MUST_LOSE * 100.0
        ));
    }
    if slow_kept < SLOW_MUST_KEEP {
        failures.push(format!(
            "slow kept {:.2}%, must keep at least {:.0}%",
            slow_kept * 100.0,
            SLOW_MUST_KEEP * 100.0
        ));
    }
    if failures.is_empty() {
        println!("PASS: two-rate decay behaves as specified");
        Ok(())
    } else {
        Err(format!("FAIL: {}", failures.join("; ")))
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let extension = path.extension().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!("{stem}-{suffix}.{extension}"))
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

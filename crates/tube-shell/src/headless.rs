//! Headless debug dump: deposit hardcoded spans with no window and write the
//! result as a PNG, so the deposition pass is mechanically verifiable.

use std::path::Path;

use beam_trace::{Sample, flags};
use tube_renderer::{Deposit, DepositParams, TubeProfile};

use crate::gpu;
use crate::shaders::{ShaderLibrary, shader_dir};

/// Rows of *display* resolution; the deposit buffers are `SUPERSAMPLE`× this.
pub const DISPLAY_HEIGHT: u32 = 512;

/// Energy below this is treated as nothing landed here.
const THRESHOLD: f32 = 1e-4;

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

pub fn debug(out: &Path) -> Result<(), String> {
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
            .map(str::to_owned)
            .ok_or_else(|| format!("{}/{name} is missing", dir.display()))
    };

    let mut deposit = Deposit::new(
        &device,
        DISPLAY_HEIGHT,
        TubeProfile::default(),
        DepositParams::default(),
        &source("deposit.wgsl")?,
        &source("deposit_resolve.wgsl")?,
    );

    let samples = hardcoded_spans();
    deposit.run(&device, &queue, &samples);
    let field = deposit.read_back(&device, &queue);

    let peak = field
        .iter()
        .map(|texel| texel[0].max(texel[1]).max(texel[2]))
        .fold(0.0f32, f32::max);
    let lit = field
        .iter()
        .filter(|texel| texel[0].max(texel[1]).max(texel[2]) > THRESHOLD)
        .count();

    write_png(out, deposit.width(), deposit.height(), &field, peak)?;

    // The two strokes differ only in how long the beam took to draw them, so
    // the brightness ratio is the dwell-time term of the integral, on its own.
    let split = (deposit.height() / 2 * deposit.width()) as usize;
    let brightest = |rows: &[[f32; 4]]| {
        rows.iter()
            .map(|texel| texel[0].max(texel[1]).max(texel[2]))
            .fold(0.0f32, f32::max)
    };
    let slow = brightest(&field[..split]);
    let fast = brightest(&field[split..]);

    println!(
        "deposit {}x{}: peak {peak:.6}, {lit} texels above {THRESHOLD}",
        deposit.width(),
        deposit.height(),
    );
    println!(
        "slow stroke {slow:.6} / fast stroke {fast:.6} = {:.2}x",
        slow / fast
    );
    println!("wrote {}", out.display());

    if lit == 0 {
        return Err("nothing was deposited".to_owned());
    }
    Ok(())
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

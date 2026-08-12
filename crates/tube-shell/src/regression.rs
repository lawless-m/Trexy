//! The regression suite's shared ground: what gets rendered, at what settings,
//! and how two images are compared.
//!
//! Blessing and checking must agree exactly or the suite is worthless, so both
//! read this one table rather than each carrying its own copy of the numbers.

use std::path::{Path, PathBuf};

use tube_renderer::{DepositMode, Field, FieldShaders, TubeParams, View};

use crate::gpu;
use crate::render::{RenderOptions, Rendered};
use crate::shaders::{ShaderLibrary, shader_dir};

/// The workspace root, so fixtures and profiles resolve the same whether the
/// caller is the CLI (run from anywhere) or an integration test (run with the
/// crate directory as its working directory).
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// Display rows every blessed render uses. Fixed, because a resolution change
/// invalidates every blessed image at once.
pub const DISPLAY_HEIGHT: u32 = 512;

/// Host cadence for blessed renders. The field must not depend on it — that is
/// what [`CASES`] pattern 6 checks — but something has to drive the loop.
pub const FRAME_HZ: f64 = 60.0;

/// Mean absolute per-channel error, as a fraction of full scale, that two
/// renders of the same trace may differ by.
///
/// Non-zero because GPU arithmetic is not bit-reproducible across drivers, and
/// small because anything the renderer actually changes moves it far further
/// than this.
pub const TOLERANCE: f32 = 0.01;

/// One blessed case.
pub struct Case {
    pub number: u8,
    pub fixture: &'static str,
    /// How much of the fixture to replay. Every value here is an exact
    /// multiple of the 1.25 ms substep, so no render lands mid-substep and
    /// none is sensitive to rounding at the boundary.
    pub sim_seconds: f64,
    pub note: &'static str,
}

pub const CASES: [Case; 7] = [
    Case {
        number: 1,
        fixture: "pattern-01-speed-ramp.btr0",
        sim_seconds: 0.5,
        note: "brightness must order inversely with beam speed",
    },
    Case {
        number: 2,
        fixture: "pattern-02-parked-dots.btr0",
        sim_seconds: 0.5,
        note: "dot radius must grow with drive",
    },
    Case {
        number: 3,
        fixture: "pattern-03-square-corner.btr0",
        sim_seconds: 0.5,
        note: "corners stay sharp with a synthetic trace",
    },
    Case {
        number: 4,
        // Ends 50 ms after the second X, a little over one slow time constant,
        // so the tail is in the picture rather than already gone.
        fixture: "pattern-04-flash-decay.btr0",
        sim_seconds: 0.55,
        note: "the decay tail, mid-fade",
    },
    Case {
        number: 5,
        fixture: "pattern-05-blank-taper.btr0",
        sim_seconds: 0.5,
        note: "taper with no gap and no terminal dot",
    },
    Case {
        number: 6,
        fixture: "pattern-06-refresh-beat.btr0",
        sim_seconds: 1.0,
        note: "50 Hz content against a host that is not 50 Hz",
    },
    Case {
        number: 7,
        fixture: "pattern-07-lissajous-torture.btr0",
        sim_seconds: 1.0,
        note: "dense figure at overdrive",
    },
];

impl Case {
    pub fn fixture_path(&self) -> PathBuf {
        workspace_root().join("fixtures").join(self.fixture)
    }

    pub fn blessed_path(&self) -> PathBuf {
        workspace_root()
            .join("fixtures/blessed")
            .join(format!("pattern-{:02}.png", self.number))
    }

    /// The settings a blessed render and a regression render must share.
    pub fn options(&self, out: PathBuf, params: TubeParams) -> RenderOptions {
        RenderOptions {
            trace: self.fixture_path(),
            out,
            sim_seconds: Some(self.sim_seconds),
            frame_hz: FRAME_HZ,
            view: View::Beauty,
            params,
        }
    }
}

/// The profile every blessed render uses.
pub fn blessed_params() -> Result<TubeParams, String> {
    Ok(
        tube_renderer::Profile::load(workspace_root().join("profiles/vectrex-default.toml"))?
            .params,
    )
}

/// Mean absolute per-channel difference between two 8-bit images, as a
/// fraction of full scale.
pub fn difference(a: &[u8], b: &[u8]) -> Result<f32, String> {
    if a.len() != b.len() {
        return Err(format!(
            "images are different sizes: {} and {} bytes",
            a.len(),
            b.len()
        ));
    }
    if a.is_empty() {
        return Err("the images are empty".to_owned());
    }
    let total: u64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| u64::from(x.abs_diff(*y)))
        .sum();
    Ok(total as f32 / (a.len() as f32 * 255.0))
}

/// Load a PNG as 8-bit RGB.
pub fn load_png(path: impl AsRef<Path>) -> Result<(u32, u32, Vec<u8>), String> {
    let path = path.as_ref();
    let image = image::open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .to_rgb8();
    Ok((image.width(), image.height(), image.into_raw()))
}

/// Encode a rendered image the same way [`crate::render::write_png`] does, so
/// a comparison against a blessed file is like for like.
pub fn to_srgb8(rendered: &Rendered) -> Vec<u8> {
    rendered
        .image
        .iter()
        .flat_map(|pixel| {
            [0, 1, 2].map(|channel| {
                (crate::render::srgb_encode(pixel[channel].clamp(0.0, 1.0)) * 255.0).round() as u8
            })
        })
        .collect()
}

/// A headless device and a field built from the shipped shaders, for tests
/// that need to drive the loop themselves rather than render one picture.
pub fn headless_field(
    display_height: u32,
    params: TubeParams,
) -> Result<(wgpu::Device, wgpu::Queue, Field), String> {
    let (device, queue) = gpu::headless_device()?;
    let dir = shader_dir();
    let mut shaders = ShaderLibrary::new();
    shaders.reload_dir(&dir);
    if let Some(error) = shaders.error() {
        return Err(error);
    }
    let source = |name: &str| {
        shaders
            .get(name)
            .ok_or_else(|| format!("{}/{name} is missing", dir.display()))
    };

    let mut field = Field::new(
        &device,
        &queue,
        display_height,
        params,
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
    Ok((device, queue, field))
}

/// Total excitation across both phosphor components — the number that must
/// settle rather than climb.
pub fn field_energy(device: &wgpu::Device, queue: &wgpu::Queue, field: &Field) -> f64 {
    let sum = |component| {
        field
            .read_back(device, queue, component)
            .iter()
            .map(|texel| f64::from(texel[0] + texel[1] + texel[2]))
            .sum::<f64>()
    };
    sum(tube_renderer::Component::Fast) + sum(tube_renderer::Component::Slow)
}

/// Re-export so the tests do not need their own deposit-mode import.
pub const ANALYTIC: DepositMode = DepositMode::Analytic;

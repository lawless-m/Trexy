//! The frame loop — RENDERER.md §2.

use beam_trace::Sample;

use crate::deposit::{Deposit, DepositMode, DepositParams, DepositShaders, TubeProfile};
use crate::phosphor::{Component, Phosphor, PhosphorParams};
use crate::readout::{Readout, ReadoutParams, ReadoutShaders, View};
use crate::substep::{SubstepClock, clip_spans};
use crate::timing::Timings;

/// The most simulated time a single frame will catch up on.
///
/// Beyond this the clock jumps rather than simulating. The ring buffer holds
/// 200 ms because anything older is fully decayed (TRACE-FORMAT.md §5), so
/// there is nothing out there to draw anyway — and without a bound a slow
/// frame becomes a death spiral: more backlog, more substeps, a slower frame,
/// more backlog. A cold start is the common case, the shader compile and
/// window creation having taken a second before the first frame ran.
pub const MAX_CATCHUP_SECONDS: f64 = 0.1;

/// Every parameter the tube model takes, grouped by the pass that owns it.
/// The provenance classes live on the individual structs (ARCHITECTURE.md §4).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct TubeParams {
    /// Deposit resolution as a multiple of the display resolution. A quality
    /// tier, not a physical claim; changing it rebuilds every buffer.
    pub supersample: u32,
    /// Fixed substep duration, seconds. Also structural: the whole
    /// host-independence argument rests on it being constant, not on its value.
    pub substep_seconds: f32,
    pub profile: TubeProfile,
    pub deposit: DepositParams,
    pub phosphor: PhosphorParams,
    pub readout: ReadoutParams,
}

impl Default for TubeParams {
    fn default() -> Self {
        Self {
            supersample: crate::SUPERSAMPLE,
            substep_seconds: crate::SUBSTEP_SECONDS as f32,
            profile: TubeProfile::default(),
            deposit: DepositParams::default(),
            phosphor: PhosphorParams::default(),
            readout: ReadoutParams::default(),
        }
    }
}

impl TubeParams {
    /// Whether two sets differ in a way that needs the buffers rebuilding
    /// rather than merely re-uploading a uniform.
    pub fn needs_rebuild(&self, other: &Self) -> bool {
        self.supersample != other.supersample || self.profile != other.profile
    }
}

/// Every WGSL source the field needs.
pub struct FieldShaders<'a> {
    pub deposit: &'a str,
    pub splat: &'a str,
    pub resolve: &'a str,
    pub phosphor: &'a str,
    pub deposit_total: &'a str,
    pub readout: &'a str,
    pub blur: &'a str,
    pub tonemap: &'a str,
    pub view: &'a str,
    pub sample_points: &'a str,
}

/// Deposition plus the phosphor field it feeds, driven on the fixed substep
/// grid.
/// The whole layer-3 chain: deposition, the phosphor field it feeds, and the
/// readout that turns that field into a picture.
pub struct Field {
    deposit: Deposit,
    phosphor: Phosphor,
    readout: Readout,
    clock: SubstepClock,
    params: TubeParams,
    /// Wall clock across the last advance. Deposition and the phosphor submit
    /// per substep, so they are measured together rather than per pass.
    last_advance_micros: f32,
    last_substeps: usize,
    last_skipped_seconds: f64,
}

impl Field {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        display_height: u32,
        params: TubeParams,
        shaders: FieldShaders<'_>,
        epoch: f64,
    ) -> Self {
        let deposit = Deposit::new(
            device,
            display_height,
            params.supersample,
            params.profile,
            params.deposit,
            DepositShaders {
                deposit: shaders.deposit,
                splat: shaders.splat,
                resolve: shaders.resolve,
            },
        );
        let phosphor = Phosphor::new(
            device,
            deposit.width(),
            deposit.height(),
            params.phosphor,
            deposit.scratch_view(),
            shaders.phosphor,
            shaders.deposit_total,
        );
        let readout = Readout::new(
            device,
            queue,
            deposit.width(),
            deposit.height(),
            params.supersample,
            params.readout,
            &phosphor,
            ReadoutShaders {
                readout: shaders.readout,
                blur: shaders.blur,
                tonemap: shaders.tonemap,
                view: shaders.view,
                sample_points: shaders.sample_points,
            },
        );
        Self {
            deposit,
            phosphor,
            readout,
            clock: SubstepClock::with_dt(epoch, f64::from(params.substep_seconds)),
            params,
            last_advance_micros: 0.0,
            last_substeps: 0,
            last_skipped_seconds: 0.0,
        }
    }

    /// Run the readout chain for `view` and return its timings. `points` is
    /// only read by the sample-point overlay.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: View,
        points: &[Sample],
    ) -> Timings {
        let mut timings = self
            .readout
            .render(device, queue, &self.phosphor, view, points);
        timings.field_advance_micros = self.last_advance_micros;
        timings.substeps = self.last_substeps;
        timings
    }

    pub fn output_view(&self) -> &wgpu::TextureView {
        self.readout.output_view()
    }

    pub fn params(&self) -> TubeParams {
        self.params
    }

    /// Apply parameters that only feed uniforms. Structural changes — see
    /// [`TubeParams::needs_rebuild`] — need the field rebuilding instead.
    pub fn set_params(&mut self, params: TubeParams) {
        self.params = params;
        self.deposit.set_params(params.deposit);
        self.phosphor.set_params(params.phosphor);
        self.readout.set_params(params.readout);
    }

    pub fn readout(&self) -> &Readout {
        &self.readout
    }

    pub fn output_width(&self) -> u32 {
        self.readout.output_width()
    }

    pub fn output_height(&self) -> u32 {
        self.readout.output_height()
    }

    pub fn width(&self) -> u32 {
        self.deposit.width()
    }

    pub fn height(&self) -> u32 {
        self.deposit.height()
    }

    pub fn phosphor(&self) -> &Phosphor {
        &self.phosphor
    }

    pub fn simulated(&self) -> f64 {
        self.clock.simulated()
    }

    /// Simulated time the last advance threw away rather than catching up on.
    /// Non-zero means the renderer could not keep pace, and the panel says so
    /// rather than letting it pass unnoticed.
    pub fn skipped_seconds(&self) -> f64 {
        self.last_skipped_seconds
    }

    /// Advance the field to wall-clock `now`, in whole substeps.
    ///
    /// Each substep deposits only the trace that belongs to its own window and
    /// then decays, so the start and end of a sweep differ by real decay — as
    /// they do on hardware (RENDERER.md §2). Returns the number of substeps run.
    ///
    /// One submission per substep. Batching a whole frame into a single encoder
    /// is the obvious optimisation, but it needs a distinct uniform slot per
    /// substep's spans, and correctness comes first here.
    pub fn advance(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        samples: &[Sample],
        now: f64,
        mode: DepositMode,
    ) -> usize {
        let started = std::time::Instant::now();

        // Drop any backlog beyond what is worth simulating, before working out
        // which substeps to run.
        let behind = now - self.clock.simulated();
        self.last_skipped_seconds = if behind > MAX_CATCHUP_SECONDS {
            self.clock.skip_to(now - MAX_CATCHUP_SECONDS);
            behind - MAX_CATCHUP_SECONDS
        } else {
            0.0
        };

        let substeps = self.clock.advance(now);
        let dt = self.clock.dt();

        for step in &substeps {
            let clipped = clip_spans(samples, step.start, step.end);
            let gain = if clipped.is_empty() {
                // Nothing was drawn in this window; the field only decays.
                0.0
            } else {
                self.deposit.run(device, queue, &clipped, mode);
                1.0
            };
            self.phosphor.step(device, queue, dt, gain);
        }
        self.last_advance_micros = started.elapsed().as_secs_f32() * 1e6;
        self.last_substeps = substeps.len();
        substeps.len()
    }

    /// Zero the phosphor — a cold tube.
    pub fn clear(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        self.phosphor.clear(device, queue);
    }

    pub fn read_back(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        component: Component,
    ) -> Vec<[f32; 4]> {
        self.phosphor.read_back(device, queue, component)
    }
}

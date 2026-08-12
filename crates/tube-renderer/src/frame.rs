//! The frame loop — RENDERER.md §2.

use beam_trace::Sample;

use crate::deposit::{Deposit, DepositMode, DepositParams, DepositShaders, TubeProfile};
use crate::phosphor::{Component, Phosphor, PhosphorParams};
use crate::readout::{Readout, ReadoutParams, ReadoutShaders};
use crate::substep::{SubstepClock, clip_spans};

/// Every parameter the tube model takes, grouped by the pass that owns it.
/// The provenance classes live on the individual structs (ARCHITECTURE.md §4).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TubeParams {
    pub profile: TubeProfile,
    pub deposit: DepositParams,
    pub phosphor: PhosphorParams,
    pub readout: ReadoutParams,
}

/// Every WGSL source the field needs.
pub struct FieldShaders<'a> {
    pub deposit: &'a str,
    pub splat: &'a str,
    pub resolve: &'a str,
    pub phosphor: &'a str,
    pub readout: &'a str,
    pub blur: &'a str,
    pub tonemap: &'a str,
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
}

impl Field {
    pub fn new(
        device: &wgpu::Device,
        display_height: u32,
        params: TubeParams,
        shaders: FieldShaders<'_>,
        epoch: f64,
    ) -> Self {
        let deposit = Deposit::new(
            device,
            display_height,
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
        );
        let readout = Readout::new(
            device,
            deposit.width(),
            deposit.height(),
            crate::SUPERSAMPLE,
            params.readout,
            &phosphor,
            ReadoutShaders {
                readout: shaders.readout,
                blur: shaders.blur,
                tonemap: shaders.tonemap,
            },
        );
        Self {
            deposit,
            phosphor,
            readout,
            clock: SubstepClock::new(epoch),
        }
    }

    /// Run the readout chain and return the final tonemapped image's view.
    pub fn render(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> &wgpu::TextureView {
        self.readout.render(device, queue, &self.phosphor);
        self.readout.output_view()
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

//! The frame loop — RENDERER.md §2.

use beam_trace::Sample;

use crate::deposit::{Deposit, DepositMode, DepositParams, DepositShaders, TubeProfile};
use crate::phosphor::{Component, Phosphor, PhosphorParams};
use crate::substep::{SubstepClock, clip_spans};

/// Every WGSL source the field needs.
pub struct FieldShaders<'a> {
    pub deposit: &'a str,
    pub splat: &'a str,
    pub resolve: &'a str,
    pub phosphor: &'a str,
}

/// Deposition plus the phosphor field it feeds, driven on the fixed substep
/// grid.
pub struct Field {
    deposit: Deposit,
    phosphor: Phosphor,
    clock: SubstepClock,
}

impl Field {
    pub fn new(
        device: &wgpu::Device,
        display_height: u32,
        profile: TubeProfile,
        deposit_params: DepositParams,
        phosphor_params: PhosphorParams,
        shaders: FieldShaders<'_>,
        epoch: f64,
    ) -> Self {
        let deposit = Deposit::new(
            device,
            display_height,
            profile,
            deposit_params,
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
            phosphor_params,
            deposit.scratch_view(),
            shaders.phosphor,
        );
        Self {
            deposit,
            phosphor,
            clock: SubstepClock::new(epoch),
        }
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

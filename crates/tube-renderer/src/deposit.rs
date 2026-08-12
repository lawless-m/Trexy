//! Deposition — RENDERER.md §3.1.
//!
//! Analytic integration of a Gaussian spot along each inter-sample span. The
//! mathematics lives in `shaders/deposit.wgsl`; this module owns the textures,
//! the buffers and the per-span dispatch.

use beam_trace::{Sample, flags};
use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

/// Deposit resolution is this multiple of the display resolution, linearly.
/// The field is bandlimited by spot size, but thin bright strokes alias at 1:1
/// (RENDERER.md §1).
pub const SUPERSAMPLE: u32 = 2;

/// Gaussian support cutoff in σ; must match `CUTOFF_SIGMAS` in deposit.wgsl.
const CUTOFF_SIGMAS: f32 = 4.0;

/// Dynamic uniform offsets must be a multiple of this.
const UNIFORM_ALIGNMENT: u64 = 256;

const WORKGROUP: u32 = 8;

/// Tube face geometry. Aspect belongs to the tube, not the producer and not
/// the window (TRACE-FORMAT.md §1).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct TubeProfile {
    pub aspect_w: f32,
    pub aspect_h: f32,
}

impl TubeProfile {
    /// The Vectrex 9" Samsung tube: portrait, 3:4. Schematic class.
    pub const VECTREX: Self = Self {
        aspect_w: 3.0,
        aspect_h: 4.0,
    };
}

impl Default for TubeProfile {
    fn default() -> Self {
        Self::VECTREX
    }
}

/// Spot-shape parameters. All fitted class (RENDERER.md §4) — starting
/// guesses, to be tuned against the acceptance patterns.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct DepositParams {
    /// σ0 — parked dim dot width, in deflection units.
    pub sigma0: f32,
    /// σ1 — defocus with drive.
    pub sigma1: f32,
    /// γs — growth exponent.
    pub gamma_s: f32,
}

impl Default for DepositParams {
    fn default() -> Self {
        Self {
            sigma0: 0.0015,
            sigma1: 0.0025,
            gamma_s: 0.7,
        }
    }
}

impl DepositParams {
    fn sigma(&self, drive: f32) -> f32 {
        self.sigma0 + self.sigma1 * drive.max(0.0).powf(self.gamma_s)
    }
}

/// WGSL sources for the pass. Passed in rather than embedded so the shell's
/// hot-reload owns them.
pub struct DepositShaders<'a> {
    pub deposit: &'a str,
    pub splat: &'a str,
    pub resolve: &'a str,
}

/// How a span becomes energy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DepositMode {
    /// Closed-form integration along the span. The only production path.
    #[default]
    Analytic,
    /// Debug only: the forbidden point splat, kept as the reference for what
    /// beading looks like (CONTENTS.md, FIRST-SLICE.md §4). Nothing selects
    /// this but an explicit debug flag.
    Splat,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuParams {
    resolution: [u32; 2],
    scale_x: f32,
    scale_y: f32,
    sigma0: f32,
    sigma1: f32,
    gamma_s: f32,
    _pad: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuSpan {
    first: u32,
    origin_x: u32,
    origin_y: u32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuResolution {
    size: [u32; 2],
    _pad: [u32; 2],
}

/// A dispatch's worth of work: which span, and how many workgroups cover its
/// bounding box.
struct SpanDispatch {
    span: GpuSpan,
    groups_x: u32,
    groups_y: u32,
}

/// The deposition pass and its targets.
pub struct Deposit {
    width: u32,
    height: u32,
    params: DepositParams,

    /// rgba16f, `deposit_scratch` in RENDERER.md §1.
    scratch: wgpu::Texture,
    scratch_view: wgpu::TextureView,

    accumulator: wgpu::Buffer,
    params_buffer: wgpu::Buffer,
    spans_buffer: wgpu::Buffer,
    spans_capacity: u64,
    samples_buffer: wgpu::Buffer,
    samples_capacity: u64,

    deposit_layout: wgpu::BindGroupLayout,
    deposit_pipeline: wgpu::ComputePipeline,
    splat_pipeline: wgpu::ComputePipeline,
    resolve_bind_group: wgpu::BindGroup,
    resolve_pipeline: wgpu::ComputePipeline,
}

impl Deposit {
    /// Build the pass for a display of `display_height` rows; the deposit
    /// buffers are [`SUPERSAMPLE`]× that, with the tube's aspect.
    pub fn new(
        device: &wgpu::Device,
        display_height: u32,
        supersample: u32,
        profile: TubeProfile,
        params: DepositParams,
        shaders: DepositShaders<'_>,
    ) -> Self {
        let height = (display_height * supersample.max(1)).max(1);
        let width = ((height as f32 * profile.aspect_w / profile.aspect_h).round() as u32).max(1);

        let scratch = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("deposit_scratch"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let scratch_view = scratch.create_view(&wgpu::TextureViewDescriptor::default());

        let texels = u64::from(width) * u64::from(height);
        let accumulator = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("deposit accumulator"),
            size: texels * 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let params_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("deposit params"),
            contents: bytemuck::bytes_of(&GpuParams {
                resolution: [width, height],
                scale_x: width as f32 / 2.0,
                scale_y: height as f32 / 2.0,
                sigma0: params.sigma0,
                sigma1: params.sigma1,
                gamma_s: params.gamma_s,
                _pad: 0.0,
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let resolution_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("deposit resolution"),
            contents: bytemuck::bytes_of(&GpuResolution {
                size: [width, height],
                _pad: [0, 0],
            }),
            usage: wgpu::BufferUsages::UNIFORM,
        });

        let spans_capacity = UNIFORM_ALIGNMENT;
        let spans_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("deposit spans"),
            size: spans_capacity,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let samples_capacity = (size_of::<Sample>() * 2) as u64;
        let samples_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("trace samples"),
            size: samples_capacity,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let deposit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("deposit"),
            entries: &[
                storage_entry(0, true),
                uniform_entry(1, false),
                uniform_entry(2, true),
                storage_entry(3, false),
            ],
        });
        let deposit_pipeline =
            compute_pipeline(device, "deposit", shaders.deposit, &deposit_layout);
        let splat_pipeline = compute_pipeline(
            device,
            "deposit splat (debug)",
            shaders.splat,
            &deposit_layout,
        );

        let resolve_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("deposit resolve"),
            entries: &[
                storage_entry(0, true),
                uniform_entry(1, false),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba16Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });
        let resolve_pipeline =
            compute_pipeline(device, "deposit resolve", shaders.resolve, &resolve_layout);
        let resolve_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("deposit resolve"),
            layout: &resolve_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: accumulator.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: resolution_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&scratch_view),
                },
            ],
        });

        Self {
            width,
            height,
            params,
            scratch,
            scratch_view,
            accumulator,
            params_buffer,
            spans_buffer,
            spans_capacity,
            samples_buffer,
            samples_capacity,
            deposit_layout,
            deposit_pipeline,
            splat_pipeline,
            resolve_bind_group,
            resolve_pipeline,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Spot parameters feed a uniform, so they change without rebuilding.
    pub fn set_params(&mut self, params: DepositParams) {
        self.params = params;
    }

    pub fn scratch_view(&self) -> &wgpu::TextureView {
        &self.scratch_view
    }

    /// Clear the accumulator and deposit every span of `samples` into it, then
    /// resolve into `deposit_scratch`.
    ///
    /// `mode` is [`DepositMode::Analytic`] for every render that matters;
    /// [`DepositMode::Splat`] is reachable only from a debug flag.
    pub fn run(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        samples: &[Sample],
        mode: DepositMode,
    ) {
        let dispatches = self.plan(samples);
        self.upload(device, queue, samples, &dispatches);
        queue.write_buffer(
            &self.params_buffer,
            0,
            bytemuck::bytes_of(&GpuParams {
                resolution: [self.width, self.height],
                scale_x: self.width as f32 / 2.0,
                scale_y: self.height as f32 / 2.0,
                sigma0: self.params.sigma0,
                sigma1: self.params.sigma1,
                gamma_s: self.params.gamma_s,
                _pad: 0.0,
            }),
        );

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("deposit"),
            layout: &self.deposit_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.samples_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.spans_buffer,
                        offset: 0,
                        size: std::num::NonZeroU64::new(size_of::<GpuSpan>() as u64),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.accumulator.as_entire_binding(),
                },
            ],
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("deposit"),
        });
        encoder.clear_buffer(&self.accumulator, 0, None);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("deposit"),
                timestamp_writes: None,
            });
            pass.set_pipeline(match mode {
                DepositMode::Analytic => &self.deposit_pipeline,
                DepositMode::Splat => &self.splat_pipeline,
            });
            for (index, dispatch) in dispatches.iter().enumerate() {
                let offset = (index as u64 * UNIFORM_ALIGNMENT) as u32;
                pass.set_bind_group(0, &bind_group, &[offset]);
                pass.dispatch_workgroups(dispatch.groups_x, dispatch.groups_y, 1);
            }
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("deposit resolve"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.resolve_pipeline);
            pass.set_bind_group(0, &self.resolve_bind_group, &[]);
            pass.dispatch_workgroups(
                self.width.div_ceil(WORKGROUP),
                self.height.div_ceil(WORKGROUP),
                1,
            );
        }
        queue.submit([encoder.finish()]);
    }

    /// Work out which spans deposit anything and what bounding box each needs.
    fn plan(&self, samples: &[Sample]) -> Vec<SpanDispatch> {
        let mut dispatches = Vec::new();

        for (index, pair) in samples.windows(2).enumerate() {
            let (s0, s1) = (pair[0], pair[1]);

            // The beam was dumped, not swept, so nothing crossed this gap.
            if s1.flags & flags::DISCONTINUITY != 0 {
                continue;
            }
            // Zero-drive spans still advanced the beam; they simply emit
            // nothing (RENDERER.md §3.1).
            let peak = [
                s0.drive_r, s0.drive_g, s0.drive_b, s1.drive_r, s1.drive_g, s1.drive_b,
            ]
            .into_iter()
            .fold(0.0f32, f32::max);
            if peak <= 0.0 || s1.t <= s0.t {
                continue;
            }

            let mean = |s: Sample| (s.drive_r + s.drive_g + s.drive_b) / 3.0;
            let sigma = self.params.sigma((mean(s0) + mean(s1)) * 0.5) * self.height as f32 / 2.0;
            let margin = CUTOFF_SIGMAS * sigma;

            let p0 = self.to_texels(s0.x, s0.y);
            let p1 = self.to_texels(s1.x, s1.y);
            let min_x = (p0.0.min(p1.0) - margin).floor().max(0.0) as u32;
            let min_y = (p0.1.min(p1.1) - margin).floor().max(0.0) as u32;
            let max_x = (p0.0.max(p1.0) + margin)
                .ceil()
                .clamp(0.0, self.width as f32) as u32;
            let max_y = (p0.1.max(p1.1) + margin)
                .ceil()
                .clamp(0.0, self.height as f32) as u32;
            if max_x <= min_x || max_y <= min_y {
                continue;
            }

            dispatches.push(SpanDispatch {
                span: GpuSpan {
                    first: index as u32,
                    origin_x: min_x,
                    origin_y: min_y,
                    _pad: 0,
                },
                groups_x: (max_x - min_x).div_ceil(WORKGROUP),
                groups_y: (max_y - min_y).div_ceil(WORKGROUP),
            });
        }

        dispatches
    }

    fn to_texels(&self, x: f32, y: f32) -> (f32, f32) {
        (
            (x + 1.0) * self.width as f32 / 2.0,
            (1.0 - y) * self.height as f32 / 2.0,
        )
    }

    fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        samples: &[Sample],
        dispatches: &[SpanDispatch],
    ) {
        // A raw copy of the sample array — the 32-byte record is the GPU
        // layout, so there is no transform here by design (FIRST-SLICE.md §5).
        let bytes: &[u8] = bytemuck::cast_slice(samples);
        let needed = (bytes.len() as u64).max(size_of::<Sample>() as u64);
        if needed > self.samples_capacity {
            self.samples_capacity = needed.next_power_of_two();
            self.samples_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("trace samples"),
                size: self.samples_capacity,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !bytes.is_empty() {
            queue.write_buffer(&self.samples_buffer, 0, bytes);
        }

        // One dynamically-offset uniform slot per span. The stride is the
        // alignment, not the record size, so this is sized by spans-per-frame
        // rather than by the whole trace.
        let needed = (dispatches.len().max(1) as u64) * UNIFORM_ALIGNMENT;
        if needed > self.spans_capacity {
            self.spans_capacity = needed.next_power_of_two();
            self.spans_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("deposit spans"),
                size: self.spans_capacity,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        for (index, dispatch) in dispatches.iter().enumerate() {
            queue.write_buffer(
                &self.spans_buffer,
                index as u64 * UNIFORM_ALIGNMENT,
                bytemuck::bytes_of(&dispatch.span),
            );
        }
    }

    /// Copy `deposit_scratch` back to the CPU as linear RGBA.
    pub fn read_back(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Vec<[f32; 4]> {
        crate::readback::read_texture(device, queue, &self.scratch, self.width, self.height)
    }
}

fn storage_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn uniform_entry(binding: u32, has_dynamic_offset: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset,
            min_binding_size: None,
        },
        count: None,
    }
}

fn compute_pipeline(
    device: &wgpu::Device,
    label: &str,
    source: &str,
    layout: &wgpu::BindGroupLayout,
) -> wgpu::ComputePipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spot_grows_with_drive() {
        let params = DepositParams::default();
        assert_eq!(params.sigma(0.0), params.sigma0);
        assert!(params.sigma(1.0) > params.sigma(0.25));
        assert!(params.sigma(4.0) > params.sigma(1.0));
    }
}

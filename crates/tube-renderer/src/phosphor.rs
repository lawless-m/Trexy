//! The phosphor field — RENDERER.md §1 retained state, §3.1 accumulation with
//! saturation, §3.2 two-rate decay.
//!
//! This is the renderer's only frame-to-frame state, exactly mirroring the real
//! device, whose only memory is the phosphor itself.

use bytemuck::{Pod, Zeroable};

use crate::readback::read_texture;

const WORKGROUP: u32 = 8;

/// Which retained buffer to look at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Component {
    Fast,
    Slow,
}

/// Phosphor parameters. All fitted class (RENDERER.md §4).
///
/// The Vectrex tube's phosphor type is not stated in the service manual;
/// standard white TV phosphor (P4-family) is the working assumption, a
/// zinc-sulfide blend whose components decay at different rates — which is the
/// physical basis for there being two buffers at all. The τ values are
/// order-of-magnitude placeholders to be tuned against reference footage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhosphorParams {
    /// E_sat — knee of the hot-spot rolloff.
    pub e_sat: f32,
    /// τf — fast component decay constant, seconds.
    pub tau_fast: f32,
    /// τs — slow component decay constant, seconds.
    pub tau_slow: f32,
    /// Share of deposited energy going to the fast component.
    pub fast_split: f32,
}

impl Default for PhosphorParams {
    fn default() -> Self {
        Self {
            e_sat: 4.0,
            tau_fast: 120e-6,
            tau_slow: 40e-3,
            fast_split: 0.75,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuParams {
    resolution: [u32; 2],
    e_sat: f32,
    fast_split: f32,
    decay_fast: f32,
    decay_slow: f32,
    deposit_gain: f32,
    _pad: f32,
}

/// `phosphor_fast` and `phosphor_slow`, plus the substep update that maintains
/// them.
pub struct Phosphor {
    width: u32,
    height: u32,
    params: PhosphorParams,

    fast: [wgpu::Texture; 2],
    slow: [wgpu::Texture; 2],
    /// Which half of the ping-pong currently holds the live state.
    current: usize,

    params_buffer: wgpu::Buffer,
    pipeline: wgpu::ComputePipeline,
    /// One per ping-pong direction.
    bind_groups: [wgpu::BindGroup; 2],
}

impl Phosphor {
    pub fn new(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        params: PhosphorParams,
        deposit_view: &wgpu::TextureView,
        source: &str,
    ) -> Self {
        // Both buffers are ping-ponged; see the note in phosphor.wgsl for why
        // in-place update is not available in core WebGPU. `phosphor_slow`
        // keeps its rgba32f format regardless — dropping it to 16f to make an
        // in-place path work would be trading away the precision it exists for.
        let fast = std::array::from_fn(|i| {
            make_texture(
                device,
                width,
                height,
                wgpu::TextureFormat::Rgba16Float,
                &format!("phosphor_fast[{i}]"),
            )
        });
        let slow = std::array::from_fn(|i| {
            make_texture(
                device,
                width,
                height,
                wgpu::TextureFormat::Rgba32Float,
                &format!("phosphor_slow[{i}]"),
            )
        });

        let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("phosphor params"),
            size: size_of::<GpuParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("phosphor"),
            entries: &[
                loaded_texture(0),
                loaded_texture(1),
                loaded_texture(2),
                storage_texture(3, wgpu::TextureFormat::Rgba16Float),
                storage_texture(4, wgpu::TextureFormat::Rgba32Float),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let view =
            |texture: &wgpu::Texture| texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_groups = std::array::from_fn(|i| {
            let (from, to) = (i, 1 - i);
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("phosphor"),
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(deposit_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view(&fast[from])),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&view(&slow[from])),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&view(&fast[to])),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(&view(&slow[to])),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: params_buffer.as_entire_binding(),
                    },
                ],
            })
        });

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("phosphor"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("phosphor"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("phosphor"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });

        Self {
            width,
            height,
            params,
            fast,
            slow,
            current: 0,
            params_buffer,
            pipeline,
            bind_groups,
        }
    }

    pub fn params(&self) -> PhosphorParams {
        self.params
    }

    /// The live buffer for a component — what the readout pass samples.
    pub fn texture(&self, component: Component) -> &wgpu::Texture {
        self.phase_texture(component, self.current)
    }

    /// Which half of the ping-pong currently holds the live state. The readout
    /// keeps one bind group per phase rather than rebuilding every frame.
    pub fn phase(&self) -> usize {
        self.current
    }

    pub fn phase_texture(&self, component: Component, phase: usize) -> &wgpu::Texture {
        match component {
            Component::Fast => &self.fast[phase],
            Component::Slow => &self.slow[phase],
        }
    }

    /// Run one substep: accumulate `deposit_scratch` through saturation, then
    /// decay both buffers. `deposit_gain` is 0 when time is advancing with
    /// nothing to deposit.
    pub fn step(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        substep_seconds: f64,
        deposit_gain: f32,
    ) {
        self.write_params(queue, substep_seconds, deposit_gain);

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("phosphor substep"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("phosphor substep"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_groups[self.current], &[]);
            pass.dispatch_workgroups(
                self.width.div_ceil(WORKGROUP),
                self.height.div_ceil(WORKGROUP),
                1,
            );
        }
        queue.submit([encoder.finish()]);
        self.current = 1 - self.current;
    }

    /// Zero both buffers — a cold tube.
    pub fn clear(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("phosphor clear"),
        });
        for texture in self.fast.iter().chain(self.slow.iter()) {
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("phosphor clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &texture.create_view(&wgpu::TextureViewDescriptor::default()),
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        queue.submit([encoder.finish()]);
        self.current = 0;
    }

    pub fn read_back(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        component: Component,
    ) -> Vec<[f32; 4]> {
        read_texture(
            device,
            queue,
            self.texture(component),
            self.width,
            self.height,
        )
    }

    fn write_params(&self, queue: &wgpu::Queue, substep_seconds: f64, deposit_gain: f32) {
        let decay = |tau: f32| (-substep_seconds / f64::from(tau)).exp() as f32;
        queue.write_buffer(
            &self.params_buffer,
            0,
            bytemuck::bytes_of(&GpuParams {
                resolution: [self.width, self.height],
                e_sat: self.params.e_sat,
                fast_split: self.params.fast_split,
                decay_fast: decay(self.params.tau_fast),
                decay_slow: decay(self.params.tau_slow),
                deposit_gain,
                _pad: 0.0,
            }),
        );
    }
}

fn make_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    label: &str,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

/// A texture read with `textureLoad`, so no filtering is needed — which is
/// what makes rgba32f legal to read here at all.
fn loaded_texture(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn storage_texture(binding: u32, format: wgpu::TextureFormat) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decay_factors_match_the_documented_time_constants() {
        let params = PhosphorParams::default();
        let dt = crate::SUBSTEP_SECONDS;
        let fast = (-dt / f64::from(params.tau_fast)).exp();
        let slow = (-dt / f64::from(params.tau_slow)).exp();

        // 1.25 ms is over ten fast time constants: the fast component is gone
        // within a single substep.
        assert!(fast < 1e-4, "fast decay factor was {fast}");
        // And a thirty-second of a slow one: the tail survives many substeps.
        assert!(slow > 0.96, "slow decay factor was {slow}");
    }
}

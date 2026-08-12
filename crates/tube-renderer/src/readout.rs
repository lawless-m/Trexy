//! Readout and optics — RENDERER.md §3.3.
//!
//! Combine the phosphor components, scatter the result through the faceplate,
//! bend it over the tube geometry, put it behind glass, and tonemap once.
//!
//! **Glow is read-out only.** Nothing in this module holds a writable binding
//! to a phosphor buffer — the phosphor textures appear here exactly twice, both
//! times as read-only bindings in the combine pass. Blur in the feedback loop
//! dissolves history into grey mush inside a second and is the classic failure
//! mode of this renderer type (RENDERER.md §2), so the structure forbids it
//! rather than merely the intention.

use bytemuck::{Pod, Zeroable};

use beam_trace::Sample;

use crate::phosphor::{Component, Phosphor};
use crate::readback::read_texture;
use crate::timing::{PassTimer, READOUT_PASSES, Timings};

/// The wide halo is blurred at this fraction of the deposit resolution. A σ of
/// 0.06 face units is affordable only because most of the work happens small.
const HALO_DIVISOR: u32 = 4;

/// Must match `RADIUS` in blur.wgsl.
const BLUR_RADIUS: f32 = 8.0;
/// Taps reach this many σ, matching `INV_SIGMA_TAPS` in blur.wgsl.
const BLUR_REACH_SIGMAS: f32 = 3.0;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Readout, optics and tonemap parameters (RENDERER.md §4).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReadoutParams {
    /// Fast component chromaticity — blue-ish. Fitted.
    pub chroma_fast: [f32; 3],
    /// Slow component chromaticity — yellow-ish. Fitted. The difference is
    /// what makes trails warm as they fade.
    pub chroma_slow: [f32; 3],
    /// Faceplate scatter. Fitted.
    pub glow_tight_sigma: f32,
    /// Long-range haze. Artistic.
    pub glow_halo_sigma: f32,
    pub glow_halo_gain: f32,
    /// Tube-profile pincushion. Fitted.
    pub pincushion: f32,
    /// Radians.
    pub rotation: f32,
    pub overscan: f32,
    /// Glass. Artistic; zero on both is the off switch.
    pub vignette: f32,
    pub reflection: f32,
    /// Artistic.
    pub exposure: f32,
}

impl Default for ReadoutParams {
    fn default() -> Self {
        Self {
            chroma_fast: [0.85, 0.95, 1.0],
            chroma_slow: [1.0, 0.92, 0.70],
            glow_tight_sigma: 0.004,
            glow_halo_sigma: 0.06,
            glow_halo_gain: 0.08,
            pincushion: 0.02,
            rotation: 0.0,
            overscan: 1.0,
            vignette: 0.25,
            reflection: 0.01,
            exposure: 1.0,
        }
    }
}

/// WGSL sources for the readout chain.
pub struct ReadoutShaders<'a> {
    pub readout: &'a str,
    pub blur: &'a str,
    pub tonemap: &'a str,
    pub view: &'a str,
    pub sample_points: &'a str,
}

/// What to put on screen — RENDERER.md §5. First-class, not afterthoughts:
/// the development loop is trace replay plus shader hot-reload plus these.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    /// The full chain, tonemapped.
    #[default]
    Beauty,
    /// Raw `phosphor_fast`, exposure-scaled.
    PhosphorFast,
    /// Raw `phosphor_slow`, exposure-scaled.
    PhosphorSlow,
    /// Every substep's deposition summed, with decay frozen.
    DepositOnly,
    /// Total excitation on a log false-colour ramp.
    FalseColour,
    /// The beauty render with a dot at each trace sample.
    SamplePoints,
}

impl View {
    pub const ALL: [View; 6] = [
        View::Beauty,
        View::PhosphorFast,
        View::PhosphorSlow,
        View::DepositOnly,
        View::FalseColour,
        View::SamplePoints,
    ];

    pub fn name(self) -> &'static str {
        match self {
            View::Beauty => "beauty",
            View::PhosphorFast => "fast",
            View::PhosphorSlow => "slow",
            View::DepositOnly => "deposit",
            View::FalseColour => "energy",
            View::SamplePoints => "samples",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.name() == name)
    }

    /// The mode number view.wgsl switches on. Beauty and SamplePoints do not
    /// use that shader at all.
    fn mode(self) -> u32 {
        match self {
            View::PhosphorFast => 0,
            View::PhosphorSlow => 1,
            View::DepositOnly => 2,
            _ => 3,
        }
    }

    fn uses_view_shader(self) -> bool {
        !matches!(self, View::Beauty | View::SamplePoints)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuChroma {
    fast: [f32; 4],
    slow: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuBlur {
    step: [f32; 2],
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuView {
    mode: u32,
    exposure: f32,
    decades: f32,
    _pad: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuOverlay {
    radius: [f32; 2],
    aspect: f32,
    pincushion: f32,
    rotation: f32,
    overscan: f32,
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuTonemap {
    aspect: f32,
    pincushion: f32,
    rotation: f32,
    overscan: f32,
    halo_gain: f32,
    vignette: f32,
    reflection: f32,
    exposure: f32,
}

/// One axis of one blur: where it writes, what it reads, and how far apart its
/// taps sit in source UV.
struct BlurPass {
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    step: [f32; 2],
}

pub struct Readout {
    width: u32,
    height: u32,
    output_width: u32,
    output_height: u32,
    params: ReadoutParams,

    readout_view: wgpu::TextureView,
    output: wgpu::Texture,
    output_view: wgpu::TextureView,

    chroma_buffer: wgpu::Buffer,
    tonemap_buffer: wgpu::Buffer,

    readout_pipeline: wgpu::RenderPipeline,
    /// One per phosphor ping-pong phase.
    readout_bind_groups: [wgpu::BindGroup; 2],
    blur_pipeline: wgpu::RenderPipeline,
    /// Scatter horizontal, scatter vertical, halo horizontal, halo vertical.
    blurs: [BlurPass; 4],
    tonemap_pipeline: wgpu::RenderPipeline,
    tonemap_bind_group: wgpu::BindGroup,

    view_pipeline: wgpu::RenderPipeline,
    view_bind_groups: [wgpu::BindGroup; 2],
    view_buffer: wgpu::Buffer,

    overlay_pipeline: wgpu::RenderPipeline,
    overlay_layout: wgpu::BindGroupLayout,
    overlay_bind_group: wgpu::BindGroup,
    overlay_buffer: wgpu::Buffer,
    points_buffer: wgpu::Buffer,
    points_capacity: u64,

    timer: Option<PassTimer>,
}

impl Readout {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        supersample: u32,
        params: ReadoutParams,
        phosphor: &Phosphor,
        shaders: ReadoutShaders<'_>,
    ) -> Self {
        let output_width = (width / supersample).max(1);
        let output_height = (height / supersample).max(1);
        let halo_width = (width / HALO_DIVISOR).max(1);
        let halo_height = (height / HALO_DIVISOR).max(1);

        let target = |w: u32, h: u32, label: &str| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let view =
            |texture: &wgpu::Texture| texture.create_view(&wgpu::TextureViewDescriptor::default());

        let readout = target(width, height, "readout");
        let scatter_h = target(width, height, "faceplate scatter (h)");
        let scatter = target(width, height, "faceplate scatter");
        let halo_h = target(halo_width, halo_height, "halo (h)");
        let halo = target(halo_width, halo_height, "halo");
        let output = target(output_width, output_height, "output");

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("readout"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // --- combine -------------------------------------------------------
        let chroma_buffer = uniform_buffer::<GpuChroma>(device, "readout chroma");
        let readout_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("readout"),
            entries: &[loaded_texture(0), loaded_texture(1), uniform_entry(2)],
        });
        let readout_bind_groups = std::array::from_fn(|phase| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("readout"),
                layout: &readout_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view(
                            phosphor.phase_texture(Component::Fast, phase),
                        )),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view(
                            phosphor.phase_texture(Component::Slow, phase),
                        )),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: chroma_buffer.as_entire_binding(),
                    },
                ],
            })
        });
        let readout_pipeline =
            fullscreen_pipeline(device, "readout", shaders.readout, &readout_layout);

        // --- glow ----------------------------------------------------------
        let blur_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blur"),
            entries: &[
                filtered_texture(0),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                uniform_entry(2),
            ],
        });
        let blur_pipeline = fullscreen_pipeline(device, "blur", shaders.blur, &blur_layout);

        let blur_bind_group = |source: &wgpu::Texture, uniform: &wgpu::Buffer| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("blur"),
                layout: &blur_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view(source)),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: uniform.as_entire_binding(),
                    },
                ],
            })
        };

        // σ is in face units — one unit is half the tube height — so it becomes
        // texels with the y scale, and UV per axis from there.
        let sigma_uv = |sigma: f32| {
            let texels = sigma * height as f32 / 2.0;
            [texels / width as f32, texels / height as f32]
        };
        let spacing = BLUR_REACH_SIGMAS / BLUR_RADIUS;
        let tight = sigma_uv(params.glow_tight_sigma);
        let wide = sigma_uv(params.glow_halo_sigma);

        let uniforms: [wgpu::Buffer; 4] =
            std::array::from_fn(|i| uniform_buffer::<GpuBlur>(device, &format!("blur[{i}]")));
        let blurs = [
            BlurPass {
                view: view(&scatter_h),
                bind_group: blur_bind_group(&readout, &uniforms[0]),
                uniform: uniforms[0].clone(),
                step: [tight[0] * spacing, 0.0],
            },
            BlurPass {
                view: view(&scatter),
                bind_group: blur_bind_group(&scatter_h, &uniforms[1]),
                uniform: uniforms[1].clone(),
                step: [0.0, tight[1] * spacing],
            },
            BlurPass {
                view: view(&halo_h),
                bind_group: blur_bind_group(&readout, &uniforms[2]),
                uniform: uniforms[2].clone(),
                step: [wide[0] * spacing, 0.0],
            },
            BlurPass {
                view: view(&halo),
                bind_group: blur_bind_group(&halo_h, &uniforms[3]),
                uniform: uniforms[3].clone(),
                step: [0.0, wide[1] * spacing],
            },
        ];

        // --- geometry, glass, tonemap ---------------------------------------
        let tonemap_buffer = uniform_buffer::<GpuTonemap>(device, "tonemap");
        let tonemap_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tonemap"),
            entries: &[
                filtered_texture(0),
                filtered_texture(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                uniform_entry(3),
            ],
        });
        let tonemap_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tonemap"),
            layout: &tonemap_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view(&scatter)),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view(&halo)),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: tonemap_buffer.as_entire_binding(),
                },
            ],
        });
        let tonemap_pipeline =
            fullscreen_pipeline(device, "tonemap", shaders.tonemap, &tonemap_layout);

        // --- debug views ----------------------------------------------------
        let view_buffer = uniform_buffer::<GpuView>(device, "view");
        let view_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("view"),
            entries: &[
                loaded_texture(0),
                loaded_texture(1),
                loaded_texture(2),
                uniform_entry(3),
            ],
        });
        let view_bind_groups = std::array::from_fn(|phase| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("view"),
                layout: &view_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view(
                            phosphor.phase_texture(Component::Fast, phase),
                        )),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view(
                            phosphor.phase_texture(Component::Slow, phase),
                        )),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&view(
                            phosphor.phase_texture(Component::DepositTotal, phase),
                        )),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: view_buffer.as_entire_binding(),
                    },
                ],
            })
        });
        let view_pipeline = fullscreen_pipeline(device, "view", shaders.view, &view_layout);

        let overlay_buffer = uniform_buffer::<GpuOverlay>(device, "sample points");
        let points_capacity = (size_of::<Sample>() * 64) as u64;
        let points_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sample points"),
            size: points_capacity,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let overlay_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sample points"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let overlay_bind_group =
            overlay_bind(device, &overlay_layout, &points_buffer, &overlay_buffer);
        let overlay_pipeline = fullscreen_pipeline(
            device,
            "sample points",
            shaders.sample_points,
            &overlay_layout,
        );

        Self {
            width,
            height,
            output_width,
            output_height,
            params,
            readout_view: view(&readout),
            output_view: view(&output),
            output,
            chroma_buffer,
            tonemap_buffer,
            readout_pipeline,
            readout_bind_groups,
            blur_pipeline,
            blurs,
            tonemap_pipeline,
            tonemap_bind_group,
            view_pipeline,
            view_bind_groups,
            view_buffer,
            overlay_pipeline,
            overlay_layout,
            overlay_bind_group,
            overlay_buffer,
            points_buffer,
            points_capacity,
            timer: PassTimer::new(device, queue, READOUT_PASSES.len() as u32),
        }
    }

    pub fn output_width(&self) -> u32 {
        self.output_width
    }

    pub fn output_height(&self) -> u32 {
        self.output_height
    }

    pub fn output_view(&self) -> &wgpu::TextureView {
        &self.output_view
    }

    pub fn params(&self) -> ReadoutParams {
        self.params
    }

    /// Run the whole chain: combine, scatter, halo, then either the tonemap or
    /// a debug view, and optionally the sample-point overlay on top.
    ///
    /// Returns per-pass GPU timings when the adapter supports them.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        phosphor: &Phosphor,
        view: View,
        points: &[Sample],
    ) -> Timings {
        self.write_uniforms(queue, view);
        if view == View::SamplePoints {
            self.upload_points(device, queue, points);
        }

        let phase = phosphor.phase();
        let timer = self.timer.as_ref();
        let at = |index: u32| timer.map(|t| (t, index));

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("readout"),
        });

        pass(
            &mut encoder,
            "combine",
            &self.readout_view,
            &self.readout_pipeline,
            &self.readout_bind_groups[phase],
            at(0),
        );
        for (index, blur) in self.blurs.iter().enumerate() {
            pass(
                &mut encoder,
                READOUT_PASSES[1 + index],
                &blur.view,
                &self.blur_pipeline,
                &blur.bind_group,
                at(1 + index as u32),
            );
        }
        if view.uses_view_shader() {
            pass(
                &mut encoder,
                "view",
                &self.output_view,
                &self.view_pipeline,
                &self.view_bind_groups[phase],
                at(5),
            );
        } else {
            pass(
                &mut encoder,
                "tonemap",
                &self.output_view,
                &self.tonemap_pipeline,
                &self.tonemap_bind_group,
                at(5),
            );
        }

        if view == View::SamplePoints && !points.is_empty() {
            // Drawn over the finished image, so the load op keeps it.
            let mut overlay = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sample points"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.output_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            overlay.set_pipeline(&self.overlay_pipeline);
            overlay.set_bind_group(0, &self.overlay_bind_group, &[]);
            overlay.draw(0..6, 0..points.len() as u32);
        }

        if let Some(timer) = timer {
            timer.resolve(&mut encoder);
        }
        queue.submit([encoder.finish()]);

        Timings {
            readout: self
                .timer
                .as_ref()
                .map(|t| t.read(device))
                .unwrap_or_default(),
            gpu_supported: self.timer.is_some(),
            ..Timings::default()
        }
    }

    /// Grow the overlay's storage buffer to fit and upload the samples. The
    /// bind group is rebuilt only when the buffer is replaced.
    fn upload_points(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, points: &[Sample]) {
        let bytes: &[u8] = bytemuck::cast_slice(points);
        let needed = (bytes.len() as u64).max(size_of::<Sample>() as u64);
        if needed > self.points_capacity {
            self.points_capacity = needed.next_power_of_two();
            self.points_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sample points"),
                size: self.points_capacity,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.overlay_bind_group = overlay_bind(
                device,
                &self.overlay_layout,
                &self.points_buffer,
                &self.overlay_buffer,
            );
        }
        if !bytes.is_empty() {
            queue.write_buffer(&self.points_buffer, 0, bytes);
        }
    }

    /// The final image, as display values in 0..1.
    pub fn read_back(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Vec<[f32; 4]> {
        read_texture(
            device,
            queue,
            &self.output,
            self.output_width,
            self.output_height,
        )
    }

    fn write_uniforms(&self, queue: &wgpu::Queue, view: View) {
        let rgb = |c: [f32; 3]| [c[0], c[1], c[2], 0.0];
        queue.write_buffer(
            &self.chroma_buffer,
            0,
            bytemuck::bytes_of(&GpuChroma {
                fast: rgb(self.params.chroma_fast),
                slow: rgb(self.params.chroma_slow),
            }),
        );
        for blur in &self.blurs {
            queue.write_buffer(
                &blur.uniform,
                0,
                bytemuck::bytes_of(&GpuBlur {
                    step: blur.step,
                    _pad: [0.0, 0.0],
                }),
            );
        }
        queue.write_buffer(
            &self.tonemap_buffer,
            0,
            bytemuck::bytes_of(&GpuTonemap {
                aspect: self.width as f32 / self.height as f32,
                pincushion: self.params.pincushion,
                rotation: self.params.rotation,
                overscan: self.params.overscan,
                halo_gain: self.params.glow_halo_gain,
                vignette: self.params.vignette,
                reflection: self.params.reflection,
                exposure: self.params.exposure,
            }),
        );
        queue.write_buffer(
            &self.view_buffer,
            0,
            bytemuck::bytes_of(&GpuView {
                mode: view.mode(),
                exposure: self.params.exposure,
                // Enough range to see a fresh stroke and the tail of an old one
                // in the same picture.
                decades: 6.0,
                _pad: 0.0,
            }),
        );
        // Three output pixels across, in normalised device coordinates.
        let radius = [
            6.0 / self.output_width as f32,
            6.0 / self.output_height as f32,
        ];
        queue.write_buffer(
            &self.overlay_buffer,
            0,
            bytemuck::bytes_of(&GpuOverlay {
                radius,
                aspect: self.width as f32 / self.height as f32,
                pincushion: self.params.pincushion,
                rotation: self.params.rotation,
                overscan: self.params.overscan,
                _pad: [0.0, 0.0],
            }),
        );
    }
}

fn overlay_bind(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    points: &wgpu::Buffer,
    uniform: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("sample points"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: points.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: uniform.as_entire_binding(),
            },
        ],
    })
}

fn pass(
    encoder: &mut wgpu::CommandEncoder,
    label: &str,
    target: &wgpu::TextureView,
    pipeline: &wgpu::RenderPipeline,
    bind_group: &wgpu::BindGroup,
    timer: Option<(&PassTimer, u32)>,
) {
    let mut render = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: timer.map(|(t, index)| t.render_writes(index)),
        occlusion_query_set: None,
        multiview_mask: None,
    });
    render.set_pipeline(pipeline);
    render.set_bind_group(0, bind_group, &[]);
    render.draw(0..3, 0..1);
}

fn uniform_buffer<T>(device: &wgpu::Device, label: &str) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: size_of::<T>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// Read with `textureLoad`, so no filtering is required — which is what makes
/// the rgba32f slow buffer legal to read here.
fn loaded_texture(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn filtered_texture(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn fullscreen_pipeline(
    device: &wgpu::Device,
    label: &str,
    source: &str,
    layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(FORMAT.into())],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

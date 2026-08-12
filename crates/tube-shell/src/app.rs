//! The winit application: window, wgpu surface, egui side panel, WGSL
//! hot-reload, and the debug view selector.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use bytemuck::{Pod, Zeroable};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tube_renderer::{DepositMode, Field, FieldShaders, READOUT_PASSES, Timings, TubeParams, View};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use crate::gpu;
use crate::headless::hardcoded_spans;
use crate::render::DISPLAY_HEIGHT;
use crate::shaders::{ShaderLibrary, shader_dir};

const PRESENT_SHADER: &str = "present.wgsl";

/// The debug spans run to here; the shell replays them once per rebuild until
/// live sources arrive.
const TRACE_END: f64 = 0.007;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PresentUniform {
    fit: [f32; 2],
    exposure: f32,
    _pad: f32,
}

/// Everything rebuilt when a shader changes. Held as a unit so a failed
/// rebuild leaves the previous one running untouched.
struct Rendered {
    field: Field,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
}

struct Gpu {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    adapter_info: wgpu::AdapterInfo,

    sampler: wgpu::Sampler,
    present_buffer: wgpu::Buffer,
    present_layout: wgpu::BindGroupLayout,
    /// `None` until a shader set validates; kept across a bad edit.
    rendered: Option<Rendered>,
    rendered_generation: Option<u64>,
    /// Debug toggle for the forbidden point-splat path (FIRST-SLICE.md §4).
    splat: bool,
    view: View,
    timings: Timings,

    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
}

pub struct App {
    gpu: Option<Gpu>,
    shaders: ShaderLibrary,
    shader_dir: PathBuf,
    /// Held for its lifetime: dropping the watcher stops the notifications.
    _watcher: Option<RecommendedWatcher>,
    changes: Option<Receiver<()>>,
}

/// Open the window and run until it closes.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new();
    event_loop.run_app(&mut app)?;
    Ok(())
}

impl App {
    fn new() -> Self {
        let dir = shader_dir();
        let mut shaders = ShaderLibrary::new();
        shaders.reload_dir(&dir);

        let (watcher, changes) = match watch(&dir) {
            Ok((watcher, rx)) => (Some(watcher), Some(rx)),
            Err(e) => {
                eprintln!("shader hot-reload unavailable: {e}");
                (None, None)
            }
        };

        Self {
            gpu: None,
            shaders,
            shader_dir: dir,
            _watcher: watcher,
            changes,
        }
    }

    /// Drain pending file events and re-read the shader directory. Returns
    /// true if anything at all happened, so the caller can redraw.
    fn poll_shader_changes(&mut self) -> bool {
        let Some(rx) = &self.changes else {
            return false;
        };
        let mut saw_change = false;
        while rx.try_recv().is_ok() {
            saw_change = true;
        }
        if saw_change {
            self.shaders.reload_dir(&self.shader_dir);
        }
        saw_change
    }
}

fn watch(dir: &Path) -> notify::Result<(RecommendedWatcher, Receiver<()>)> {
    let (tx, rx) = channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if event.is_ok() {
            let _ = tx.send(());
        }
    })?;
    watcher.watch(dir, RecursiveMode::NonRecursive)?;
    Ok((watcher, rx))
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        match Gpu::new(event_loop) {
            Ok(mut gpu) => {
                gpu.rebuild(&self.shaders);
                self.gpu = Some(gpu);
            }
            Err(e) => {
                eprintln!("could not start the renderer: {e}");
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(gpu) = &mut self.gpu else { return };

        let response = gpu.egui_state.on_window_event(&gpu.window, &event);
        if response.repaint {
            gpu.window.request_redraw();
        }
        if response.consumed {
            return;
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => gpu.resize(size.width, size.height),
            WindowEvent::RedrawRequested => gpu.redraw(&self.shaders),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        let changed = self.poll_shader_changes();
        if let Some(gpu) = &mut self.gpu {
            if changed {
                gpu.rebuild(&self.shaders);
            }
            gpu.window.request_redraw();
        }
    }
}

impl Gpu {
    fn new(event_loop: &ActiveEventLoop) -> Result<Self, Box<dyn std::error::Error>> {
        let attributes = Window::default_attributes()
            .with_title("Trexy — vector tube renderer")
            .with_inner_size(winit::dpi::LogicalSize::new(900.0, 1000.0));
        let window = Arc::new(event_loop.create_window(attributes)?);

        let instance = gpu::instance();
        let surface = instance.create_surface(window.clone())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            ..Default::default()
        }))?;
        let adapter_info = adapter.get_info();
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("tube-shell"),
                required_features: gpu::timing_features(&adapter),
                ..Default::default()
            }))?;

        let size = window.inner_size();
        let caps = surface.get_capabilities(&adapter);
        // The tonemap emits linear display values; the sRGB surface encodes
        // them. That is the one and only place the boundary is crossed.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            width: size.width.max(1),
            height: size.height.max(1),
            format,
            ..surface
                .get_default_config(&adapter, size.width.max(1), size.height.max(1))
                .ok_or("surface is not supported by this adapter")?
        };
        surface.configure(&device, &config);

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("present"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let present_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("present"),
            size: size_of::<PresentUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let present_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("present"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let egui_ctx = egui::Context::default();
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            None,
            None,
            None,
        );
        let egui_renderer =
            egui_wgpu::Renderer::new(&device, format, egui_wgpu::RendererOptions::default());

        Ok(Self {
            window,
            surface,
            device,
            queue,
            config,
            adapter_info,
            sampler,
            present_buffer,
            present_layout,
            rendered: None,
            rendered_generation: None,
            splat: false,
            view: View::default(),
            timings: Timings::default(),
            egui_ctx,
            egui_state,
            egui_renderer,
        })
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    fn mode(&self) -> DepositMode {
        if self.splat {
            DepositMode::Splat
        } else {
            DepositMode::Analytic
        }
    }

    /// Rebuild the whole chain from the library's installed sources and replay
    /// the debug spans once. If the library has nothing valid, whatever is
    /// already running is left alone — that is "keep the last good pipeline".
    fn rebuild(&mut self, shaders: &ShaderLibrary) {
        if self.rendered_generation == Some(shaders.generation()) {
            return;
        }
        let source = |name: &str| shaders.get(name);
        let Some(present_source) = source(PRESENT_SHADER) else {
            return;
        };
        let names = [
            "deposit.wgsl",
            "deposit_splat.wgsl",
            "deposit_resolve.wgsl",
            "phosphor.wgsl",
            "deposit_total.wgsl",
            "readout.wgsl",
            "blur.wgsl",
            "tonemap.wgsl",
            "view.wgsl",
            "sample_points.wgsl",
        ];
        if names.iter().any(|name| source(name).is_none()) {
            return;
        }

        // Already naga-validated on the CPU, so these compiles are not expected
        // to fail; if the backend disagrees, keep what is running.
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);

        let mut field = Field::new(
            &self.device,
            &self.queue,
            DISPLAY_HEIGHT,
            TubeParams::default(),
            FieldShaders {
                deposit: source("deposit.wgsl").expect("checked"),
                splat: source("deposit_splat.wgsl").expect("checked"),
                resolve: source("deposit_resolve.wgsl").expect("checked"),
                phosphor: source("phosphor.wgsl").expect("checked"),
                deposit_total: source("deposit_total.wgsl").expect("checked"),
                readout: source("readout.wgsl").expect("checked"),
                blur: source("blur.wgsl").expect("checked"),
                tonemap: source("tonemap.wgsl").expect("checked"),
                view: source("view.wgsl").expect("checked"),
                sample_points: source("sample_points.wgsl").expect("checked"),
            },
            0.0,
        );
        field.clear(&self.device, &self.queue);
        field.advance(
            &self.device,
            &self.queue,
            &hardcoded_spans(),
            TRACE_END,
            self.mode(),
        );

        let module = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(PRESENT_SHADER),
                source: wgpu::ShaderSource::Wgsl(present_source.into()),
            });
        let layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("present"),
                bind_group_layouts: &[Some(&self.present_layout)],
                immediate_size: 0,
            });
        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("present"),
                layout: Some(&layout),
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
                    targets: &[Some(self.config.format.into())],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("present"),
            layout: &self.present_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(field.output_view()),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.present_buffer.as_entire_binding(),
                },
            ],
        });

        if let Some(error) = pollster::block_on(scope.pop()) {
            eprintln!("device rejected a validated shader set: {error}");
            return;
        }

        self.rendered = Some(Rendered {
            field,
            pipeline,
            bind_group,
        });
        self.rendered_generation = Some(shaders.generation());
    }

    /// Redraw the field from scratch after a debug toggle changed what it
    /// should contain.
    fn redeposit(&mut self) {
        let mode = self.mode();
        let Some(rendered) = &mut self.rendered else {
            return;
        };
        rendered.field.clear(&self.device, &self.queue);
        rendered.field.advance(
            &self.device,
            &self.queue,
            &hardcoded_spans(),
            TRACE_END,
            mode,
        );
    }

    /// Letterbox the tube face into the window without distorting it.
    fn present_uniform(&self, rendered: &Rendered) -> PresentUniform {
        let tube = rendered.field.output_width() as f32 / rendered.field.output_height() as f32;
        let window = self.config.width as f32 / self.config.height as f32;
        let fit = if window > tube {
            [tube / window, 1.0]
        } else {
            [1.0, window / tube]
        };
        PresentUniform {
            fit,
            // The chain has already tonemapped; this blit only places it.
            exposure: 1.0,
            _pad: 0.0,
        }
    }

    fn redraw(&mut self, shaders: &ShaderLibrary) {
        use wgpu::CurrentSurfaceTexture as Acquired;
        let frame = match self.surface.get_current_texture() {
            Acquired::Success(frame) => frame,
            // Suboptimal still presents; reconfigure and use it this frame.
            Acquired::Suboptimal(frame) => {
                self.surface.configure(&self.device, &self.config);
                frame
            }
            Acquired::Outdated | Acquired::Lost => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            other => {
                eprintln!("dropped a frame: {other:?}");
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // Run the readout chain for the selected view.
        let selected = self.view;
        let uniform = self
            .rendered
            .as_ref()
            .map(|rendered| self.present_uniform(rendered));
        if let Some(uniform) = uniform {
            self.queue
                .write_buffer(&self.present_buffer, 0, bytemuck::bytes_of(&uniform));
        }
        if let Some(rendered) = &mut self.rendered {
            self.timings =
                rendered
                    .field
                    .render(&self.device, &self.queue, selected, &hardcoded_spans());
        }

        let raw_input = self.egui_state.take_egui_input(&self.window);
        let mut splat = self.splat;
        let mut chosen = self.view;
        let output = self.egui_ctx.clone().run_ui(raw_input, |ui| {
            self.panel(ui, shaders, &mut splat, &mut chosen);
        });
        self.view = chosen;
        if splat != self.splat {
            self.splat = splat;
            self.redeposit();
        }
        self.egui_state
            .handle_platform_output(&self.window, output.platform_output);
        let paint_jobs = self
            .egui_ctx
            .tessellate(output.shapes, output.pixels_per_point);
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point: output.pixels_per_point,
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        for (id, deltas) in &output.textures_delta.set {
            for delta in deltas {
                self.egui_renderer
                    .update_texture(&self.device, &self.queue, *id, delta);
            }
        }
        let commands = self.egui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &paint_jobs,
            &screen,
        );

        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("present"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();

            if let Some(rendered) = &self.rendered {
                pass.set_pipeline(&rendered.pipeline);
                pass.set_bind_group(0, &rendered.bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
            self.egui_renderer.render(&mut pass, &paint_jobs, &screen);
        }

        self.queue
            .submit(commands.into_iter().chain([encoder.finish()]));
        self.queue.present(frame);

        for id in &output.textures_delta.free {
            self.egui_renderer.free_texture(id);
        }
    }

    fn panel(&self, ui: &mut egui::Ui, shaders: &ShaderLibrary, splat: &mut bool, view: &mut View) {
        egui::Panel::left("shell").show(ui, |ui| {
            ui.heading("Trexy");
            ui.label(format!(
                "{} ({:?})",
                self.adapter_info.name, self.adapter_info.backend
            ));
            ui.separator();

            ui.label("view");
            for candidate in View::ALL {
                ui.radio_value(view, candidate, candidate.name());
            }
            ui.separator();

            if let Some(rendered) = &self.rendered {
                ui.label(format!(
                    "field {}×{} → {}×{}",
                    rendered.field.width(),
                    rendered.field.height(),
                    rendered.field.output_width(),
                    rendered.field.output_height(),
                ));
            }
            self.timing_labels(ui);
            ui.separator();

            // The splat path is the documented counter-example, never a
            // production route (CONTENTS.md, FIRST-SLICE.md §4).
            ui.checkbox(splat, "debug: point splat");
            if *splat {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 190, 80),
                    "forbidden path — beading reference only",
                );
            }
            ui.separator();

            ui.label(format!("shaders: generation {}", shaders.generation()));
            match shaders.error() {
                Some(error) => {
                    ui.colored_label(egui::Color32::from_rgb(255, 120, 90), "shader error");
                    ui.label(
                        egui::RichText::new(error)
                            .monospace()
                            .color(egui::Color32::from_rgb(255, 160, 140)),
                    );
                    ui.label("last good shader still running");
                }
                None => {
                    ui.label("shaders ok");
                }
            }
        });
    }

    fn timing_labels(&self, ui: &mut egui::Ui) {
        ui.label(format!(
            "advance {:.2} ms, {} substeps",
            self.timings.field_advance_micros / 1000.0,
            self.timings.substeps
        ));
        if !self.timings.gpu_supported {
            ui.label("no timestamp queries on this adapter");
            return;
        }
        for (label, micros) in READOUT_PASSES.iter().zip(&self.timings.readout) {
            ui.label(format!("  {label}: {micros:.1} µs"));
        }
        ui.label(format!(
            "  readout total: {:.1} µs",
            self.timings.readout_total_micros()
        ));
    }
}

//! The winit application: window, wgpu surface, egui side panel, WGSL
//! hot-reload, and the debug view selector.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use bytemuck::{Pod, Zeroable};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tube_renderer::{
    Class, DepositMode, Field, FieldShaders, Profile, READOUT_PASSES, Timings, TubeParams, View,
    registry,
};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::gpu;
use crate::render::DISPLAY_HEIGHT;
use crate::shaders::{ShaderLibrary, shader_dir};
use crate::source::{Controls, LiveSource, Source};

const PRESENT_SHADER: &str = "present.wgsl";

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PresentUniform {
    fit: [f32; 2],
    exposure: f32,
    _pad: f32,
}

/// What the panel may change in a frame. Gathered so the call site does not
/// grow an argument per control.
struct PanelState<'a> {
    splat: &'a mut bool,
    view: &'a mut View,
    params: &'a mut TubeParams,
    status: &'a mut String,
    /// A capture the panel asked for, acted on after the UI closes its
    /// borrows.
    capture: &'a mut Option<Capture>,
}

/// What the capture buttons and keys ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Capture {
    /// Dump the live ring buffer to a `.btr0`.
    Trace,
    /// Write the presented image to a PNG.
    Screenshot,
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
    /// Samples pulled from the ring for the last frame, for the panel.
    window_samples: usize,
    /// Simulated time the last frame threw away rather than catching up on.
    skipped_seconds: f64,
    params: TubeParams,
    /// What the current buffers were built with, so a structural change is
    /// noticed and rebuilt rather than silently ignored.
    built_with: TubeParams,
    profile_status: String,

    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
}

pub struct App {
    gpu: Option<Gpu>,
    source: LiveSource,
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
            source: LiveSource::spawn(),
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
                gpu.rebuild(&self.shaders, &self.source);
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
            WindowEvent::RedrawRequested => gpu.redraw(&self.shaders, &self.source),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                match event.physical_key {
                    PhysicalKey::Code(KeyCode::KeyR) => gpu.record(&self.source),
                    PhysicalKey::Code(KeyCode::KeyS) => gpu.screenshot(),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        let changed = self.poll_shader_changes();
        if let Some(gpu) = &mut self.gpu {
            if changed {
                gpu.rebuild(&self.shaders, &self.source);
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
            window_samples: 0,
            skipped_seconds: 0.0,
            params: TubeParams::default(),
            built_with: TubeParams::default(),
            profile_status: String::new(),
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
    fn rebuild(&mut self, shaders: &ShaderLibrary, live: &LiveSource) {
        let structural = self.params.needs_rebuild(&self.built_with);
        if self.rendered_generation == Some(shaders.generation()) && !structural {
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
            self.params,
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
            // The field picks up from the present, not from the start of the
            // session: a shader edit must not replay the whole history.
            live.elapsed(),
        );
        field.clear(&self.device, &self.queue);

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
        self.built_with = self.params;
    }

    /// Dump the live ring buffer to a timestamped `.btr0`.
    fn record(&mut self, source: &LiveSource) {
        let name = source.controls().source.name().replace(' ', "-");
        let path = PathBuf::from("recordings").join(format!("{}-{}.btr0", name, stamp()));
        self.profile_status = match source.record(&path, &name) {
            Ok(count) => format!("recorded {count} samples to {}", path.display()),
            Err(e) => e,
        };
        println!("{}", self.profile_status);
    }

    /// Write the final presented image to `screenshots/`.
    fn screenshot(&mut self) {
        let Some(rendered) = &self.rendered else {
            return;
        };
        let image = rendered
            .field
            .readout()
            .read_back(&self.device, &self.queue);
        let path = PathBuf::from("screenshots").join(format!("trexy-{}.png", stamp()));
        self.profile_status = match crate::render::write_png(
            &path,
            rendered.field.output_width(),
            rendered.field.output_height(),
            &image,
        ) {
            Ok(()) => format!("wrote {}", path.display()),
            Err(e) => e,
        };
        println!("{}", self.profile_status);
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

    fn redraw(&mut self, shaders: &ShaderLibrary, source: &LiveSource) {
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
        let mode = self.mode();
        let uniform = self
            .rendered
            .as_ref()
            .map(|rendered| self.present_uniform(rendered));
        if let Some(uniform) = uniform {
            self.queue
                .write_buffer(&self.present_buffer, 0, bytemuck::bytes_of(&uniform));
        }
        let params = self.params;
        if let Some(rendered) = &mut self.rendered {
            rendered.field.set_params(params);
            // Ask the ring buffer for everything since the field last caught
            // up, and let the substep loop chop it (TRACE-FORMAT.md §5).
            let now = source.elapsed();
            let window = source.window(rendered.field.simulated() as f32, now as f32);
            rendered
                .field
                .advance(&self.device, &self.queue, &window, now, mode);
            self.timings = rendered
                .field
                .render(&self.device, &self.queue, selected, &window);
            self.window_samples = window.len();
            self.skipped_seconds = rendered.field.skipped_seconds();
        }

        let raw_input = self.egui_state.take_egui_input(&self.window);
        let mut splat = self.splat;
        let mut chosen = self.view;
        let mut params = self.params;
        let mut status = self.profile_status.clone();
        let mut capture = None;
        let output = self.egui_ctx.clone().run_ui(raw_input, |ui| {
            self.panel(
                ui,
                shaders,
                source,
                &mut PanelState {
                    splat: &mut splat,
                    view: &mut chosen,
                    params: &mut params,
                    status: &mut status,
                    capture: &mut capture,
                },
            );
        });
        self.view = chosen;
        self.splat = splat;
        self.params = params;
        self.profile_status = status;
        match capture {
            Some(Capture::Trace) => self.record(source),
            Some(Capture::Screenshot) => self.screenshot(),
            None => {}
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

    fn panel(
        &self,
        ui: &mut egui::Ui,
        shaders: &ShaderLibrary,
        source: &LiveSource,
        state: &mut PanelState<'_>,
    ) {
        let splat = &mut *state.splat;
        let view = &mut *state.view;
        let params = &mut *state.params;
        let status = &mut *state.status;
        let capture = &mut *state.capture;
        egui::Panel::left("shell").show(ui, |ui| {
            ui.heading("Trexy");
            ui.label(format!(
                "{} ({:?})",
                self.adapter_info.name, self.adapter_info.backend
            ));
            ui.separator();

            source_controls(ui, source);
            ui.separator();

            ui.horizontal(|ui| {
                if ui
                    .button("record (R)")
                    .on_hover_text("dump the ring buffer to recordings/")
                    .clicked()
                {
                    *capture = Some(Capture::Trace);
                }
                if ui
                    .button("screenshot (S)")
                    .on_hover_text("write the presented image to screenshots/")
                    .clicked()
                {
                    *capture = Some(Capture::Screenshot);
                }
            });
            ui.separator();

            ui.label("view");
            for candidate in View::ALL {
                ui.radio_value(view, candidate, candidate.name());
            }
            ui.separator();

            ui.label(format!("{} samples this frame", self.window_samples));
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

            parameter_panel(ui, params, status);
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
        if self.skipped_seconds > 0.0 {
            // Falling behind is a real condition, not a detail to hide: the
            // picture is missing whatever happened in the skipped time.
            ui.colored_label(
                egui::Color32::from_rgb(255, 190, 80),
                format!("behind: skipped {:.0} ms", self.skipped_seconds * 1000.0),
            );
        }
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

/// Source selection and the live Lissajous controls.
///
/// Every one of these takes effect on the next chunk the producer emits, with
/// no restart: the producer reads the controls at the top of each iteration
/// (FIRST-SLICE.md §1 deliverable 4).
fn source_controls(ui: &mut egui::Ui, source: &LiveSource) {
    let mut controls = source.controls();
    let before = controls.clone();

    ui.label("source");
    egui::ComboBox::from_id_salt("source")
        .selected_text(controls.source.name())
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut controls.source, Source::Lissajous, "lissajous");
            for (index, pattern) in beam_sources::PATTERNS.iter().enumerate() {
                ui.selectable_value(
                    &mut controls.source,
                    Source::Pattern(index),
                    format!("{}. {}", pattern.number, pattern.slug),
                );
            }
            ui.selectable_value(&mut controls.source, Source::Audio, "xy audio");
            ui.selectable_value(&mut controls.source, Source::Replay, "replay");
        });

    if controls.source == Source::Lissajous {
        let figure = &mut controls.lissajous;
        ui.add(egui::Slider::new(&mut figure.freq_x, 1.0..=13.0).text("freq x"));
        ui.add(egui::Slider::new(&mut figure.freq_y, 1.0..=13.0).text("freq y"));
        ui.add(egui::Slider::new(&mut figure.phase, 0.0..=std::f32::consts::TAU).text("phase"));
        ui.add(egui::Slider::new(&mut figure.drive, 0.0..=4.0).text("drive"));
        ui.add(egui::Slider::new(&mut figure.speed, 1.0..=60.0).text("speed"));
        ui.add(egui::Slider::new(&mut figure.amplitude, 0.1..=1.0).text("amplitude"));
    } else if let Source::Pattern(index) = controls.source {
        ui.label(beam_sources::PATTERNS[index].isolates);
    } else if controls.source == Source::Replay {
        ui.label("trace to loop (.btr0)");
        ui.text_edit_singleline(&mut controls.replay_path);
        if ui.button("load").clicked() {
            controls.generation = controls.generation.wrapping_add(1);
        }
        let status = source.status();
        if !status.is_empty() {
            ui.label(egui::RichText::new(status).small());
        }
    } else if controls.source == Source::Audio {
        ui.label("XY WAV (left → x, right → y)");
        ui.text_edit_singleline(&mut controls.audio_path);
        ui.label("optional mono drive file");
        ui.text_edit_singleline(&mut controls.audio_drive_path);
        ui.add(egui::Slider::new(&mut controls.audio_drive, 0.0..=4.0).text("drive"));
        if ui.button("play").clicked() {
            controls.generation = controls.generation.wrapping_add(1);
        }
        let status = source.status();
        if !status.is_empty() {
            ui.label(egui::RichText::new(status).small());
        }
    }

    ui.label(format!("{} samples buffered", source.buffered()));

    if controls.source != before.source {
        // A different program: tell the producer to drop what it queued.
        controls.generation = controls.generation.wrapping_add(1);
    }
    if !matches_controls(&controls, &before) {
        source.set_controls(controls);
    }
}

fn matches_controls(a: &Controls, b: &Controls) -> bool {
    a.source == b.source
        && a.lissajous == b.lissajous
        && a.generation == b.generation
        && a.audio_path == b.audio_path
        && a.audio_drive_path == b.audio_drive_path
        && a.audio_drive == b.audio_drive
        && a.replay_path == b.replay_path
}

/// Seconds since the Unix epoch, for naming captures. Enough to keep a
/// session's files in order and distinct.
fn stamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Every RENDERER.md §4 parameter, grouped by provenance class.
///
/// The grouping is the point. ARCHITECTURE.md §4 says the split between
/// physics and taste *is* the accuracy claim, and a claim nobody can see is
/// not a claim at all — so the class headings are the primary structure here,
/// not an afterthought in a tooltip.
fn parameter_panel(ui: &mut egui::Ui, params: &mut TubeParams, status: &mut String) {
    ui.label("parameters");
    ui.horizontal(|ui| {
        if ui.button("vectrex-default").clicked() {
            *params = tube_renderer::vectrex_default().params;
            *status = "loaded built-in vectrex-default".to_owned();
        }
        if ui.button("neutral").clicked() {
            *params = tube_renderer::neutral().params;
            *status = "loaded built-in neutral".to_owned();
        }
    });
    ui.horizontal(|ui| {
        if ui.button("load profiles/…").clicked() {
            *status = match Profile::load("profiles/vectrex-default.toml") {
                Ok(profile) => {
                    *params = profile.params;
                    format!("loaded {}", profile.name)
                }
                Err(e) => e,
            };
        }
        if ui.button("save profiles/current.toml").clicked() {
            let profile = Profile::new("current", "saved from the panel", *params);
            *status = match profile.save("profiles/current.toml") {
                Ok(()) => "wrote profiles/current.toml".to_owned(),
                Err(e) => e,
            };
        }
    });
    if !status.is_empty() {
        ui.label(egui::RichText::new(status.as_str()).small());
    }

    let specs = registry();
    for class in Class::ALL {
        let in_class: Vec<_> = specs.iter().filter(|spec| spec.class == class).collect();
        if in_class.is_empty() {
            continue;
        }
        egui::CollapsingHeader::new(format!("{} — {}", class.name(), class.meaning()))
            .default_open(class == Class::Fitted)
            .show(ui, |ui| {
                for spec in in_class {
                    ui.horizontal(|ui| {
                        let mut value = spec.get(params);
                        let slider = egui::Slider::new(&mut value, spec.min..=spec.max)
                            .text(spec.name)
                            .max_decimals(6);
                        if ui.add(slider).changed() {
                            spec.set(params, value);
                        }
                        // Per-parameter reset, so a slider dragged out of
                        // shape does not cost the whole set.
                        let changed = (spec.get(params) - spec.default()).abs() > f32::EPSILON;
                        if ui
                            .add_enabled(changed, egui::Button::new("↺"))
                            .on_hover_text(format!("reset to {}", spec.default()))
                            .clicked()
                        {
                            spec.reset(params);
                        }
                    });
                    ui.label(egui::RichText::new(spec.note).small().weak());
                }
            });
    }
}

//! The winit application: window, wgpu surface, egui side panel, and the
//! file-watcher half of WGSL hot-reload.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use crate::gpu;
use crate::shaders::{ShaderLibrary, shader_dir};

const BACKGROUND_SHADER: &str = "background.wgsl";

struct Gpu {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    adapter_info: wgpu::AdapterInfo,
    /// `None` until a shader validates; kept across a bad edit.
    pipeline: Option<wgpu::RenderPipeline>,
    pipeline_generation: Option<u64>,
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
                gpu.rebuild_pipeline(&self.shaders);
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
                gpu.rebuild_pipeline(&self.shaders);
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
                ..Default::default()
            }))?;

        let size = window.inner_size();
        let caps = surface.get_capabilities(&adapter);
        // All maths is in linear light and tonemapped once at the end
        // (CONTENTS.md), so the surface does the linear→sRGB encode.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            width: size.width.max(1),
            height: size.height.max(1),
            ..surface
                .get_default_config(&adapter, size.width.max(1), size.height.max(1))
                .ok_or("surface is not supported by this adapter")?
        };
        let config = wgpu::SurfaceConfiguration { format, ..config };
        surface.configure(&device, &config);

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
            pipeline: None,
            pipeline_generation: None,
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

    /// Build a pipeline from the library's installed source. If the library
    /// has nothing valid, the existing pipeline is left alone — that is what
    /// "keep the last good pipeline" means in practice.
    fn rebuild_pipeline(&mut self, shaders: &ShaderLibrary) {
        if self.pipeline_generation == Some(shaders.generation()) {
            return;
        }
        let Some(source) = shaders.get(BACKGROUND_SHADER) else {
            return;
        };

        // Already naga-validated on the CPU, so this compile is not expected
        // to fail; if the backend disagrees, keep the last good pipeline.
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(BACKGROUND_SHADER),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("background"),
                layout: None,
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

        if let Some(error) = pollster::block_on(scope.pop()) {
            eprintln!("{BACKGROUND_SHADER}: device rejected a validated shader: {error}");
            return;
        }
        self.pipeline = Some(pipeline);
        self.pipeline_generation = Some(shaders.generation());
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

        let raw_input = self.egui_state.take_egui_input(&self.window);
        let output = self.egui_ctx.clone().run_ui(raw_input, |ui| {
            self.panel(ui, shaders);
        });
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

            if let Some(pipeline) = &self.pipeline {
                pass.set_pipeline(pipeline);
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

    fn panel(&self, ui: &mut egui::Ui, shaders: &ShaderLibrary) {
        egui::Panel::left("shell").show(ui, |ui| {
            ui.heading("Trexy");
            ui.label(format!(
                "{} ({:?})",
                self.adapter_info.name, self.adapter_info.backend
            ));
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
}

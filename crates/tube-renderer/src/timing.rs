//! Per-pass GPU timings — RENDERER.md §5.
//!
//! Timestamp queries where the adapter offers them, and nothing pretending to
//! be them where it does not: if `TIMESTAMP_QUERY` is missing the timings are
//! reported as unavailable rather than silently replaced by wall clock, which
//! would measure submission latency and be read as GPU cost.
//!
//! Reading the results stalls: the resolve buffer is mapped in the same frame
//! that wrote it. That is acceptable for a dev tool and wrong for a hot loop,
//! so timing is off unless asked for.

/// The passes the readout chain reports, in order.
pub const READOUT_PASSES: [&str; 6] = [
    "combine",
    "scatter h",
    "scatter v",
    "halo h",
    "halo v",
    "tonemap",
];

/// What one frame cost.
#[derive(Clone, Debug, Default)]
pub struct Timings {
    /// Microseconds per readout pass, aligned with [`READOUT_PASSES`].
    pub readout: Vec<f32>,
    /// Wall clock across deposition and the phosphor substeps, which submit
    /// per substep and so are measured as a whole rather than per pass.
    pub field_advance_micros: f32,
    pub substeps: usize,
    /// False when the adapter has no timestamp queries.
    pub gpu_supported: bool,
}

impl Timings {
    pub fn readout_total_micros(&self) -> f32 {
        self.readout.iter().sum()
    }
}

/// A query set sized for a fixed number of passes, plus the buffers to get the
/// results back.
pub struct PassTimer {
    query_set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    staging: wgpu::Buffer,
    /// Nanoseconds per timestamp tick.
    period: f32,
    passes: u32,
}

impl PassTimer {
    /// `None` when the device lacks `TIMESTAMP_QUERY`.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, passes: u32) -> Option<Self> {
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        let count = passes * 2;
        let bytes = u64::from(count) * 8;
        Some(Self {
            query_set: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("pass timings"),
                ty: wgpu::QueryType::Timestamp,
                count,
            }),
            resolve: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pass timings resolve"),
                size: bytes,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            staging: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pass timings staging"),
                size: bytes,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
            period: queue.get_timestamp_period(),
            passes,
        })
    }

    pub fn render_writes(&self, pass: u32) -> wgpu::RenderPassTimestampWrites<'_> {
        wgpu::RenderPassTimestampWrites {
            query_set: &self.query_set,
            beginning_of_pass_write_index: Some(pass * 2),
            end_of_pass_write_index: Some(pass * 2 + 1),
        }
    }

    pub fn resolve(&self, encoder: &mut wgpu::CommandEncoder) {
        encoder.resolve_query_set(&self.query_set, 0..self.passes * 2, &self.resolve, 0);
        encoder.copy_buffer_to_buffer(&self.resolve, 0, &self.staging, 0, self.staging.size());
    }

    /// Microseconds per pass. Stalls until the frame has finished on the GPU.
    pub fn read(&self, device: &wgpu::Device) -> Vec<f32> {
        let slice = self.staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .expect("device poll");

        let mapped = slice.get_mapped_range().expect("timings mapped");
        let ticks: Vec<u64> = mapped
            .chunks_exact(8)
            .map(|word| u64::from_le_bytes(word.try_into().expect("8 bytes")))
            .collect();
        drop(mapped);
        self.staging.unmap();

        ticks
            .chunks_exact(2)
            .map(|pair| {
                // Timestamps can come back out of order or equal on some
                // drivers; a negative duration is meaningless, so clamp.
                let span = pair[1].saturating_sub(pair[0]);
                span as f32 * self.period / 1000.0
            })
            .collect()
    }
}

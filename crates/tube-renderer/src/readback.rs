//! Copying float textures back to the CPU, for headless dumps and self-checks.

/// Read an `Rgba16Float` or `Rgba32Float` texture as linear RGBA.
pub(crate) fn read_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> Vec<[f32; 4]> {
    let bytes_per_texel = match texture.format() {
        wgpu::TextureFormat::Rgba16Float => 8u32,
        wgpu::TextureFormat::Rgba32Float => 16u32,
        other => panic!("readback of {other:?} is not implemented"),
    };
    let unpadded = width * bytes_per_texel;
    let padded =
        unpadded.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("texture readback"),
        size: u64::from(padded) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("texture readback"),
    });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &staging,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);

    let slice = staging.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .expect("device poll");

    let mapped = slice.get_mapped_range().expect("readback buffer mapped");
    let mut out = Vec::with_capacity((width * height) as usize);
    for row in 0..height {
        let start = (row * padded) as usize;
        let row_bytes = &mapped[start..start + unpadded as usize];
        for texel in row_bytes.chunks_exact(bytes_per_texel as usize) {
            out.push(match bytes_per_texel {
                8 => {
                    let mut rgba = [0.0f32; 4];
                    for (channel, half) in rgba.iter_mut().zip(texel.chunks_exact(2)) {
                        *channel = f16_to_f32(u16::from_le_bytes([half[0], half[1]]));
                    }
                    rgba
                }
                _ => {
                    let mut rgba = [0.0f32; 4];
                    for (channel, word) in rgba.iter_mut().zip(texel.chunks_exact(4)) {
                        *channel = f32::from_le_bytes([word[0], word[1], word[2], word[3]]);
                    }
                    rgba
                }
            });
        }
    }
    drop(mapped);
    staging.unmap();
    out
}

/// IEEE 754 binary16 to binary32. Readback is the only place the CPU sees
/// half-floats, so this is cheaper than a dependency.
fn f16_to_f32(bits: u16) -> f32 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = ((bits >> 10) & 0x1f) as i32;
    let fraction = (bits & 0x3ff) as f32;
    let magnitude = match exponent {
        0 => fraction * 2f32.powi(-24),
        31 => {
            if fraction == 0.0 {
                f32::INFINITY
            } else {
                f32::NAN
            }
        }
        _ => (1.0 + fraction / 1024.0) * 2f32.powi(exponent - 15),
    };
    sign * magnitude
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_float_decode_matches_known_values() {
        assert_eq!(f16_to_f32(0x0000), 0.0);
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xbc00), -1.0);
        assert_eq!(f16_to_f32(0x4000), 2.0);
        assert_eq!(f16_to_f32(0x3800), 0.5);
        assert!((f16_to_f32(0x3555) - 0.333_251_95).abs() < 1e-6);
    }
}

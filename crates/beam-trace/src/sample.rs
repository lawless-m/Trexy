//! The 32-byte sample record — TRACE-FORMAT.md §2 and §3.

use bytemuck::{Pod, Zeroable};

/// One point on the beam's actual trajectory.
///
/// Layout is normative (TRACE-FORMAT.md §2): `#[repr(C)]`, 32-byte stride,
/// little-endian. The 32 bytes align for GPU storage-buffer array access with
/// no repacking, so a `&[Sample]` **is** the upload source — the ring buffer
/// and the GPU share this layout by construction (FIRST-SLICE.md §5).
///
/// | Offset | Type | Field |
/// |---|---|---|
/// | 0 | f32 | `x` |
/// | 4 | f32 | `y` |
/// | 8 | f32 | `drive_r` |
/// | 12 | f32 | `drive_g` |
/// | 16 | f32 | `drive_b` |
/// | 20 | f32 | `t` |
/// | 24 | u32 | `flags` |
/// | 28 | u32 | `reserved` |
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct Sample {
    /// Normalised deflection, nominally −1..+1, y-up. Overscan is legal.
    pub x: f32,
    /// Normalised deflection, nominally −1..+1, y-up. Overscan is legal.
    pub y: f32,
    /// Radiant drive in linear light, ≥ 0, unclamped. 0 = blanked.
    pub drive_r: f32,
    /// Radiant drive in linear light, ≥ 0, unclamped. 0 = blanked.
    pub drive_g: f32,
    /// Radiant drive in linear light, ≥ 0, unclamped. 0 = blanked.
    pub drive_b: f32,
    /// Seconds since the buffer epoch. Strictly increasing within a buffer.
    pub t: f32,
    /// Bit field, see [`flags`].
    pub flags: u32,
    /// Must write 0; readers ignore.
    pub reserved: u32,
}

/// Sample flag bits — TRACE-FORMAT.md §3. Bits 1–31 are reserved, write 0.
pub mod flags {
    /// This sample is **not** path-continuous with the previous one (e.g. a
    /// Vectrex ZERO integrator dump). The renderer deposits nothing across
    /// the gap.
    pub const DISCONTINUITY: u32 = 1 << 0;
}

impl Sample {
    /// A blanked sample (zero drive) at `(x, y)` at time `t`.
    pub fn blanked(x: f32, y: f32, t: f32) -> Self {
        Self {
            x,
            y,
            t,
            ..Self::default()
        }
    }

    /// A monochrome sample: equal drive on all three channels, which is the
    /// correct encoding for monochrome producers (TRACE-FORMAT.md §1 — the
    /// renderer's phosphor chromaticity supplies the colour).
    pub fn mono(x: f32, y: f32, drive: f32, t: f32) -> Self {
        Self {
            x,
            y,
            drive_r: drive,
            drive_g: drive,
            drive_b: drive,
            t,
            flags: 0,
            reserved: 0,
        }
    }

    /// True if this sample breaks path continuity with its predecessor.
    pub fn is_discontinuity(&self) -> bool {
        self.flags & flags::DISCONTINUITY != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};

    #[test]
    fn sample_layout_matches_spec_section_2() {
        assert_eq!(size_of::<Sample>(), 32, "32-byte stride is normative");
        assert_eq!(align_of::<Sample>(), 4);
        assert_eq!(offset_of!(Sample, x), 0);
        assert_eq!(offset_of!(Sample, y), 4);
        assert_eq!(offset_of!(Sample, drive_r), 8);
        assert_eq!(offset_of!(Sample, drive_g), 12);
        assert_eq!(offset_of!(Sample, drive_b), 16);
        assert_eq!(offset_of!(Sample, t), 20);
        assert_eq!(offset_of!(Sample, flags), 24);
        assert_eq!(offset_of!(Sample, reserved), 28);
    }

    #[test]
    fn sample_slice_casts_to_bytes_without_repacking() {
        let samples = [Sample::mono(0.0, 0.0, 1.0, 0.0); 4];
        let bytes: &[u8] = bytemuck::cast_slice(&samples);
        assert_eq!(bytes.len(), 4 * 32);
    }

    #[test]
    fn discontinuity_flag_is_bit_zero() {
        assert_eq!(flags::DISCONTINUITY, 1);
        let mut s = Sample::mono(0.0, 0.0, 1.0, 0.0);
        assert!(!s.is_discontinuity());
        s.flags |= flags::DISCONTINUITY;
        assert!(s.is_discontinuity());
    }
}

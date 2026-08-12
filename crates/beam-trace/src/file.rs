//! The `.btr0` file format — TRACE-FORMAT.md §6.
//!
//! Header, then raw §2 records tightly packed. On-disk layout **is** the
//! in-memory layout, so capturing a regression trace is a memcpy and a write.

use crate::{Sample, TraceError, validate};
use std::path::Path;

#[cfg(target_endian = "big")]
compile_error!(
    "beam-trace copies sample records verbatim between memory, disk and the GPU; \
     TRACE-FORMAT.md is little-endian throughout, so a big-endian host would need \
     an explicit byte-swap path that does not exist"
);

/// ASCII `BTR0`.
pub const MAGIC: [u8; 4] = *b"BTR0";
/// The only format version this build reads or writes.
pub const VERSION: u32 = 0;
/// Header size in bytes; records begin here.
pub const HEADER_LEN: usize = 64;
/// `producer_id` field width in bytes, NUL-padded UTF-8.
pub const PRODUCER_ID_LEN: usize = 32;
/// Default positional error bound: 1/4096 of full deflection, about one
/// deposit-buffer texel at 2× supersampling (TRACE-FORMAT.md §4).
pub const DEFAULT_EPSILON: f32 = 1.0 / 4096.0;

/// Everything in a `.btr0` header except `sample_count`, which is implied by
/// the records themselves.
#[derive(Clone, Debug, PartialEq)]
pub struct TraceHeader {
    /// Seconds, producer-defined origin. May be 0.
    pub epoch: f64,
    /// The positional bound this trace honours.
    pub epsilon: f32,
    /// Informational; 0 = none/unknown.
    pub nominal_refresh_hz: f32,
    /// E.g. `synthetic/lissajous`, `vectrex/via`. At most 32 bytes of UTF-8.
    pub producer_id: String,
}

impl Default for TraceHeader {
    fn default() -> Self {
        Self {
            epoch: 0.0,
            epsilon: DEFAULT_EPSILON,
            nominal_refresh_hz: 0.0,
            producer_id: String::new(),
        }
    }
}

/// A validated trace: header plus samples.
#[derive(Clone, Debug, PartialEq)]
pub struct Trace {
    pub header: TraceHeader,
    pub samples: Vec<Sample>,
}

/// Serialise a trace. Validates first — this crate does not write a file it
/// would refuse to read back.
pub fn encode(header: &TraceHeader, samples: &[Sample]) -> Result<Vec<u8>, TraceError> {
    let id = header.producer_id.as_bytes();
    if id.len() > PRODUCER_ID_LEN {
        return Err(TraceError::ProducerIdTooLong(id.len()));
    }
    validate(samples)?;

    let mut out = Vec::with_capacity(HEADER_LEN + size_of_val(samples));
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&header.epoch.to_le_bytes());
    out.extend_from_slice(&(samples.len() as u64).to_le_bytes());
    out.extend_from_slice(&header.epsilon.to_le_bytes());
    out.extend_from_slice(&header.nominal_refresh_hz.to_le_bytes());
    out.extend_from_slice(id);
    out.resize(HEADER_LEN, 0);
    out.extend_from_slice(bytemuck::cast_slice(samples));
    Ok(out)
}

/// Parse and validate a trace. A trace that fails validation is rejected, not
/// repaired (TRACE-FORMAT.md §6).
pub fn decode(bytes: &[u8]) -> Result<Trace, TraceError> {
    if bytes.len() < HEADER_LEN {
        return Err(TraceError::LengthMismatch {
            expected: HEADER_LEN,
            actual: bytes.len(),
        });
    }

    let magic: [u8; 4] = bytes[0..4].try_into().expect("4 bytes");
    if magic != MAGIC {
        return Err(TraceError::BadMagic(magic));
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().expect("4 bytes"));
    if version != VERSION {
        return Err(TraceError::UnsupportedVersion(version));
    }

    let epoch = f64::from_le_bytes(bytes[8..16].try_into().expect("8 bytes"));
    let sample_count = u64::from_le_bytes(bytes[16..24].try_into().expect("8 bytes")) as usize;
    let epsilon = f32::from_le_bytes(bytes[24..28].try_into().expect("4 bytes"));
    let nominal_refresh_hz = f32::from_le_bytes(bytes[28..32].try_into().expect("4 bytes"));

    let id = &bytes[32..HEADER_LEN];
    let id = &id[..id.iter().position(|&b| b == 0).unwrap_or(PRODUCER_ID_LEN)];
    let producer_id = str::from_utf8(id)
        .map_err(|_| TraceError::ProducerIdNotUtf8)?
        .to_owned();

    let expected = HEADER_LEN + sample_count * size_of::<Sample>();
    if bytes.len() != expected {
        return Err(TraceError::LengthMismatch {
            expected,
            actual: bytes.len(),
        });
    }

    // Read unaligned: a buffer off the filesystem carries no alignment promise,
    // whereas `Sample` wants 4.
    let samples: Vec<Sample> = bytes[HEADER_LEN..]
        .chunks_exact(size_of::<Sample>())
        .map(bytemuck::pod_read_unaligned)
        .collect();
    validate(&samples)?;

    Ok(Trace {
        header: TraceHeader {
            epoch,
            epsilon,
            nominal_refresh_hz,
            producer_id,
        },
        samples,
    })
}

/// Write a `.btr0` file.
pub fn write_file(
    path: impl AsRef<Path>,
    header: &TraceHeader,
    samples: &[Sample],
) -> Result<(), TraceError> {
    std::fs::write(path, encode(header, samples)?)?;
    Ok(())
}

/// Read and validate a `.btr0` file.
pub fn read_file(path: impl AsRef<Path>) -> Result<Trace, TraceError> {
    decode(&std::fs::read(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flags;

    fn fixture() -> (TraceHeader, Vec<Sample>) {
        let header = TraceHeader {
            epoch: 1234.5,
            epsilon: DEFAULT_EPSILON,
            nominal_refresh_hz: 50.0,
            producer_id: "synthetic/lissajous".to_owned(),
        };
        let mut samples = vec![
            Sample::mono(-1.0, -1.0, 0.0, 0.0),
            Sample::mono(0.0, 0.25, 1.5, 0.001),
            Sample {
                x: 0.5,
                y: -0.5,
                drive_r: 2.0,
                drive_g: 0.5,
                drive_b: 0.0,
                t: 0.002,
                flags: 0,
                reserved: 0,
            },
        ];
        samples[2].flags |= flags::DISCONTINUITY;
        (header, samples)
    }

    #[test]
    fn header_is_64_bytes_and_records_follow_at_the_32_byte_stride() {
        let (header, samples) = fixture();
        let bytes = encode(&header, &samples).unwrap();
        assert_eq!(bytes.len(), HEADER_LEN + samples.len() * 32);
        assert_eq!(&bytes[0..4], b"BTR0");
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 0);
        assert_eq!(
            u64::from_le_bytes(bytes[16..24].try_into().unwrap()),
            samples.len() as u64
        );
        // producer_id is NUL-padded to the full field width.
        assert_eq!(&bytes[32..51], b"synthetic/lissajous");
        assert!(bytes[51..64].iter().all(|&b| b == 0));
    }

    #[test]
    fn round_trip_is_byte_identical() {
        let (header, samples) = fixture();
        let bytes = encode(&header, &samples).unwrap();

        let trace = decode(&bytes).unwrap();
        assert_eq!(trace.header, header);
        assert_eq!(trace.samples, samples);

        let reencoded = encode(&trace.header, &trace.samples).unwrap();
        assert_eq!(reencoded, bytes, "encode∘decode must be the identity");
    }

    #[test]
    fn file_round_trip_is_byte_identical() {
        let (header, samples) = fixture();
        let path = std::env::temp_dir().join(format!("beam-trace-{}.btr0", std::process::id()));

        write_file(&path, &header, &samples).unwrap();
        let trace = read_file(&path).unwrap();
        let on_disk = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();

        assert_eq!(trace.header, header);
        assert_eq!(trace.samples, samples);
        assert_eq!(encode(&trace.header, &trace.samples).unwrap(), on_disk);
    }

    #[test]
    fn rejects_bad_magic() {
        let (header, samples) = fixture();
        let mut bytes = encode(&header, &samples).unwrap();
        bytes[0..4].copy_from_slice(b"BTR1");
        assert!(matches!(decode(&bytes), Err(TraceError::BadMagic(_))));
    }

    #[test]
    fn rejects_unsupported_version() {
        let (header, samples) = fixture();
        let mut bytes = encode(&header, &samples).unwrap();
        bytes[4..8].copy_from_slice(&1u32.to_le_bytes());
        assert!(matches!(
            decode(&bytes),
            Err(TraceError::UnsupportedVersion(1))
        ));
    }

    #[test]
    fn rejects_a_truncated_file() {
        let (header, samples) = fixture();
        let mut bytes = encode(&header, &samples).unwrap();
        bytes.truncate(bytes.len() - 8);
        assert!(matches!(
            decode(&bytes),
            Err(TraceError::LengthMismatch { .. })
        ));
        assert!(matches!(
            decode(&[]),
            Err(TraceError::LengthMismatch { .. })
        ));
    }

    #[test]
    fn rejects_non_monotonic_time_on_load() {
        let (header, mut samples) = fixture();
        samples[2].t = 0.0005;
        // Build the bytes by hand: encode would refuse to write this.
        let mut bytes = encode(&header, &fixture().1).unwrap();
        bytes[HEADER_LEN..].copy_from_slice(bytemuck::cast_slice(&samples));
        assert!(matches!(
            decode(&bytes),
            Err(TraceError::NonMonotonicTime { index: 2, .. })
        ));
    }

    #[test]
    fn rejects_negative_drive_and_nan_on_load() {
        let (header, mut samples) = fixture();
        samples[1].drive_r = -1.0;
        let mut bytes = encode(&header, &fixture().1).unwrap();
        bytes[HEADER_LEN..].copy_from_slice(bytemuck::cast_slice(&samples));
        assert!(matches!(
            decode(&bytes),
            Err(TraceError::NegativeDrive { index: 1, .. })
        ));

        let (header, mut samples) = fixture();
        samples[0].y = f32::NAN;
        let mut bytes = encode(&header, &fixture().1).unwrap();
        bytes[HEADER_LEN..].copy_from_slice(bytemuck::cast_slice(&samples));
        assert!(matches!(
            decode(&bytes),
            Err(TraceError::NonFinite { index: 0, .. })
        ));
    }

    #[test]
    fn encode_refuses_an_oversized_producer_id() {
        let header = TraceHeader {
            producer_id: "x".repeat(33),
            ..TraceHeader::default()
        };
        assert!(matches!(
            encode(&header, &[]),
            Err(TraceError::ProducerIdTooLong(33))
        ));
    }

    #[test]
    fn encode_refuses_to_write_an_invalid_trace() {
        let (header, mut samples) = fixture();
        samples[1].t = 0.0;
        assert!(matches!(
            encode(&header, &samples),
            Err(TraceError::NonMonotonicTime { .. })
        ));
    }
}

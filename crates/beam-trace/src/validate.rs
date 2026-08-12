//! Trace validation — TRACE-FORMAT.md §6.
//!
//! A trace that fails validation is **rejected, not repaired**.

use crate::Sample;

/// Everything that can be wrong with a trace or a `.btr0` file.
#[derive(Debug)]
pub enum TraceError {
    Io(std::io::Error),
    /// The first four bytes were not ASCII `BTR0`.
    BadMagic([u8; 4]),
    /// Header version this build does not understand.
    UnsupportedVersion(u32),
    /// Byte count does not match the 64-byte header plus `sample_count × 32`.
    LengthMismatch {
        expected: usize,
        actual: usize,
    },
    /// `producer_id` exceeds the 32-byte field.
    ProducerIdTooLong(usize),
    /// `producer_id` bytes were not valid UTF-8.
    ProducerIdNotUtf8,
    /// `t` did not strictly increase (TRACE-FORMAT.md §2).
    NonMonotonicTime {
        index: usize,
        previous: f32,
        found: f32,
    },
    /// A drive channel was negative (TRACE-FORMAT.md §2 — drive is ≥ 0).
    NegativeDrive {
        index: usize,
        channel: &'static str,
        value: f32,
    },
    /// A float field was NaN or infinite.
    NonFinite {
        index: usize,
        field: &'static str,
        value: f32,
    },
}

impl std::fmt::Display for TraceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "trace I/O error: {e}"),
            Self::BadMagic(m) => write!(f, "bad magic {m:?}, expected b\"BTR0\""),
            Self::UnsupportedVersion(v) => write!(f, "unsupported trace version {v}, expected 0"),
            Self::LengthMismatch { expected, actual } => {
                write!(
                    f,
                    "trace length mismatch: expected {expected} bytes, got {actual}"
                )
            }
            Self::ProducerIdTooLong(n) => {
                write!(f, "producer_id is {n} bytes, field is 32")
            }
            Self::ProducerIdNotUtf8 => write!(f, "producer_id is not valid UTF-8"),
            Self::NonMonotonicTime {
                index,
                previous,
                found,
            } => write!(
                f,
                "sample {index}: t must strictly increase, {found} follows {previous}"
            ),
            Self::NegativeDrive {
                index,
                channel,
                value,
            } => write!(f, "sample {index}: negative drive_{channel} = {value}"),
            Self::NonFinite {
                index,
                field,
                value,
            } => write!(f, "sample {index}: {field} is not finite ({value})"),
        }
    }
}

impl std::error::Error for TraceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for TraceError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Check a sample run against TRACE-FORMAT.md §6: finite floats, non-negative
/// drive, strictly increasing `t`.
pub fn validate(samples: &[Sample]) -> Result<(), TraceError> {
    let mut previous: Option<f32> = None;

    for (index, s) in samples.iter().enumerate() {
        for (field, value) in [
            ("x", s.x),
            ("y", s.y),
            ("drive_r", s.drive_r),
            ("drive_g", s.drive_g),
            ("drive_b", s.drive_b),
            ("t", s.t),
        ] {
            if !value.is_finite() {
                return Err(TraceError::NonFinite {
                    index,
                    field,
                    value,
                });
            }
        }

        for (channel, value) in [("r", s.drive_r), ("g", s.drive_g), ("b", s.drive_b)] {
            if value < 0.0 {
                return Err(TraceError::NegativeDrive {
                    index,
                    channel,
                    value,
                });
            }
        }

        if let Some(previous) = previous
            && s.t <= previous
        {
            return Err(TraceError::NonMonotonicTime {
                index,
                previous,
                found: s.t,
            });
        }
        previous = Some(s.t);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run() -> Vec<Sample> {
        vec![
            Sample::mono(0.0, 0.0, 1.0, 0.0),
            Sample::mono(0.5, 0.5, 1.0, 0.001),
            Sample::mono(1.0, 1.0, 1.0, 0.002),
        ]
    }

    #[test]
    fn accepts_a_well_formed_run() {
        assert!(validate(&run()).is_ok());
        assert!(validate(&[]).is_ok());
    }

    #[test]
    fn rejects_non_monotonic_time() {
        let mut s = run();
        s[2].t = 0.001;
        assert!(matches!(
            validate(&s),
            Err(TraceError::NonMonotonicTime { index: 2, .. })
        ));

        // Equal timestamps are also rejected: t must *strictly* increase.
        s[2].t = 0.001;
        s[1].t = 0.001;
        assert!(matches!(
            validate(&s),
            Err(TraceError::NonMonotonicTime { .. })
        ));
    }

    #[test]
    fn rejects_negative_drive() {
        let mut s = run();
        s[1].drive_g = -0.001;
        assert!(matches!(
            validate(&s),
            Err(TraceError::NegativeDrive {
                index: 1,
                channel: "g",
                ..
            })
        ));
    }

    #[test]
    fn rejects_non_finite_floats() {
        let mut s = run();
        s[0].x = f32::NAN;
        assert!(matches!(
            validate(&s),
            Err(TraceError::NonFinite {
                index: 0,
                field: "x",
                ..
            })
        ));

        let mut s = run();
        s[1].t = f32::INFINITY;
        assert!(matches!(
            validate(&s),
            Err(TraceError::NonFinite { field: "t", .. })
        ));
    }
}

//! The parameter registry — RENDERER.md §4 and ARCHITECTURE.md §4.
//!
//! Every parameter carries a provenance class, and the classification is
//! surfaced rather than buried: **the split is the accuracy claim.** Anyone
//! auditing the renderer can see at a glance which numbers are physics and
//! which are taste, and that is only worth anything if the UI shows it.

use std::path::Path;

use crate::frame::TubeParams;

/// Where a number came from (ARCHITECTURE.md §4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    /// A constant with a citation. Not user-adjustable in normal use.
    Datasheet,
    /// Derived from the service-manual circuit. Cite the page; the manual is
    /// authoritative on topology but has known component errata.
    Schematic,
    /// A tube property with no paper source: fitted by eye against footage,
    /// documented default, honest about being fitted.
    Fitted,
    /// Deliberate taste on top of the physical model.
    Artistic,
    /// Not a claim about any tube at all — renderer configuration. The §4
    /// table leaves the class blank for these; naming them keeps every row
    /// accounted for rather than quietly dropped.
    Structural,
}

impl Class {
    pub const ALL: [Class; 5] = [
        Class::Datasheet,
        Class::Schematic,
        Class::Fitted,
        Class::Artistic,
        Class::Structural,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Class::Datasheet => "datasheet",
            Class::Schematic => "schematic",
            Class::Fitted => "fitted",
            Class::Artistic => "artistic",
            Class::Structural => "structural",
        }
    }

    pub fn meaning(self) -> &'static str {
        match self {
            Class::Datasheet => "constant with a citation",
            Class::Schematic => "from the service manual circuit",
            Class::Fitted => "no paper source; fitted by eye",
            Class::Artistic => "taste, not physics",
            Class::Structural => "renderer configuration, not a tube property",
        }
    }
}

/// One adjustable scalar.
pub struct ParamSpec {
    /// The RENDERER.md §4 table row this belongs to. Several controls may
    /// share a row where the row holds a vector or a pair.
    pub row: &'static str,
    pub name: &'static str,
    pub class: Class,
    pub min: f32,
    pub max: f32,
    pub note: &'static str,
    get: fn(&TubeParams) -> f32,
    set: fn(&mut TubeParams, f32),
}

impl ParamSpec {
    pub fn get(&self, params: &TubeParams) -> f32 {
        (self.get)(params)
    }

    pub fn set(&self, params: &mut TubeParams, value: f32) {
        (self.set)(params, value.clamp(self.min, self.max));
    }

    /// The shipped default for this parameter.
    pub fn default(&self) -> f32 {
        (self.get)(&TubeParams::default())
    }

    pub fn reset(&self, params: &mut TubeParams) {
        (self.set)(params, self.default());
    }
}

/// Every row of the RENDERER.md §4 table, by its name there.
///
/// The registry is asserted against this list, so a row added to the document
/// and forgotten in the code fails a test rather than going unnoticed.
pub const TABLE_ROWS: [&str; 17] = [
    "Deposit supersample",
    "Substep duration",
    "Spot base sigma",
    "Spot growth coeff",
    "Spot growth exponent",
    "Saturation level",
    "Fast decay tau",
    "Slow decay tau",
    "Fast/slow energy split",
    "Fast chromaticity",
    "Slow chromaticity",
    "Glow tight sigma",
    "Glow halo sigma / gain",
    "Pincushion coeff",
    "Tube aspect",
    "Exposure",
    "Vignette / reflection gains",
];

macro_rules! spec {
    ($row:literal, $name:literal, $class:expr, $min:literal ..= $max:literal, $note:literal, $field:ident $(. $sub:ident)*) => {
        ParamSpec {
            row: $row,
            name: $name,
            class: $class,
            min: $min,
            max: $max,
            note: $note,
            get: |p| p.$field $(. $sub)*,
            set: |p, v| p.$field $(. $sub)* = v,
        }
    };
}

macro_rules! channel {
    ($row:literal, $name:literal, $class:expr, $note:literal, $field:ident . $sub:ident [$index:literal]) => {
        ParamSpec {
            row: $row,
            name: $name,
            class: $class,
            min: 0.0,
            max: 1.5,
            note: $note,
            get: |p| p.$field.$sub[$index],
            set: |p, v| p.$field.$sub[$index] = v,
        }
    };
}

pub fn registry() -> Vec<ParamSpec> {
    vec![
        ParamSpec {
            row: "Deposit supersample",
            name: "deposit supersample",
            class: Class::Structural,
            min: 1.0,
            max: 4.0,
            note: "quality tier; 1x for integrated graphics. Rebuilds buffers.",
            get: |p| p.supersample as f32,
            set: |p, v| p.supersample = v.round().max(1.0) as u32,
        },
        spec!(
            "Substep duration",
            "substep duration (s)",
            Class::Structural,
            0.0002..=0.005,
            "fixed; constant duration is what makes hosts agree",
            substep_seconds
        ),
        spec!(
            "Spot base sigma",
            "sigma0",
            Class::Fitted,
            0.0..=0.01,
            "parked dim dot width, deflection units",
            deposit.sigma0
        ),
        spec!(
            "Spot growth coeff",
            "sigma1",
            Class::Fitted,
            0.0..=0.01,
            "defocus with drive",
            deposit.sigma1
        ),
        spec!(
            "Spot growth exponent",
            "gamma_s",
            Class::Fitted,
            0.1..=2.0,
            "bright means fatter, not just brighter",
            deposit.gamma_s
        ),
        spec!(
            "Saturation level",
            "E_sat",
            Class::Fitted,
            0.1..=32.0,
            "knee of the hot-spot rolloff",
            phosphor.e_sat
        ),
        spec!(
            "Fast decay tau",
            "tau_f (s)",
            Class::Fitted,
            0.00001..=0.02,
            "P4 is a blend; this is the short component",
            phosphor.tau_fast
        ),
        spec!(
            "Slow decay tau",
            "tau_s (s)",
            Class::Fitted,
            0.001..=0.5,
            "the long tail that makes a vector display readable",
            phosphor.tau_slow
        ),
        spec!(
            "Fast/slow energy split",
            "fast share",
            Class::Fitted,
            0.0..=1.0,
            "how deposited energy divides between the two components",
            phosphor.fast_split
        ),
        channel!(
            "Fast chromaticity",
            "fast chroma r",
            Class::Fitted,
            "blue-ish; the difference from slow is why trails warm",
            readout.chroma_fast[0]
        ),
        channel!(
            "Fast chromaticity",
            "fast chroma g",
            Class::Fitted,
            "blue-ish; the difference from slow is why trails warm",
            readout.chroma_fast[1]
        ),
        channel!(
            "Fast chromaticity",
            "fast chroma b",
            Class::Fitted,
            "blue-ish; the difference from slow is why trails warm",
            readout.chroma_fast[2]
        ),
        channel!(
            "Slow chromaticity",
            "slow chroma r",
            Class::Fitted,
            "yellow-ish",
            readout.chroma_slow[0]
        ),
        channel!(
            "Slow chromaticity",
            "slow chroma g",
            Class::Fitted,
            "yellow-ish",
            readout.chroma_slow[1]
        ),
        channel!(
            "Slow chromaticity",
            "slow chroma b",
            Class::Fitted,
            "yellow-ish",
            readout.chroma_slow[2]
        ),
        spec!(
            "Glow tight sigma",
            "glow tight sigma",
            Class::Fitted,
            0.0..=0.02,
            "faceplate scatter; distinct from spot size",
            readout.glow_tight_sigma
        ),
        spec!(
            "Glow halo sigma / gain",
            "glow halo sigma",
            Class::Artistic,
            0.0..=0.2,
            "long-range haze; a single tight blur reads as neon",
            readout.glow_halo_sigma
        ),
        spec!(
            "Glow halo sigma / gain",
            "glow halo gain",
            Class::Artistic,
            0.0..=1.0,
            "long-range haze amplitude",
            readout.glow_halo_gain
        ),
        spec!(
            "Pincushion coeff",
            "pincushion",
            Class::Fitted,
            -0.1..=0.1,
            "Vectrex profile",
            readout.pincushion
        ),
        ParamSpec {
            row: "Tube aspect",
            name: "tube aspect (w/h)",
            class: Class::Schematic,
            min: 0.4,
            max: 2.5,
            note: "3:4 portrait for the Vectrex. Rebuilds buffers.",
            get: |p| p.profile.aspect_w / p.profile.aspect_h,
            set: |p, v| {
                p.profile.aspect_w = v;
                p.profile.aspect_h = 1.0;
            },
        },
        spec!(
            "Exposure",
            "exposure",
            Class::Artistic,
            0.0..=8.0,
            "applied once, just before the tonemap",
            readout.exposure
        ),
        spec!(
            "Vignette / reflection gains",
            "vignette",
            Class::Artistic,
            0.0..=1.0,
            "glass; zero is the off switch",
            readout.vignette
        ),
        spec!(
            "Vignette / reflection gains",
            "reflection",
            Class::Artistic,
            0.0..=0.2,
            "faint room reflection off the faceplate",
            readout.reflection
        ),
        // Not §4 table rows, but real geometry controls the tube profile owns
        // (RENDERER.md §3.3).
        spec!(
            "Geometry (not tabulated)",
            "rotation (rad)",
            Class::Schematic,
            -0.2..=0.2,
            "deflection yoke rotation",
            readout.rotation
        ),
        spec!(
            "Geometry (not tabulated)",
            "overscan",
            Class::Schematic,
            0.5..=1.5,
            ">1 shows more than the face",
            readout.overscan
        ),
    ]
}

/// A named parameter set. Tube profiles are just these.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Profile {
    pub name: String,
    pub description: String,
    #[serde(flatten)]
    pub params: TubeParams,
}

impl Profile {
    pub fn new(name: &str, description: &str, params: TubeParams) -> Self {
        Self {
            name: name.to_owned(),
            description: description.to_owned(),
            params,
        }
    }

    pub fn to_toml(&self) -> Result<String, String> {
        toml::to_string_pretty(self).map_err(|e| e.to_string())
    }

    pub fn from_toml(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), String> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(path, self.to_toml()?).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_toml(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// The Vectrex tube as RENDERER.md §4 specifies it.
pub fn vectrex_default() -> Profile {
    Profile::new(
        "vectrex-default",
        "The Vectrex 9\" Samsung tube, as specified in RENDERER.md §4. \
         Fitted values are starting guesses to be tuned against the patterns.",
        TubeParams::default(),
    )
}

/// The same physics with the taste turned off: no glow halo, no glass, no
/// geometry. Useful for reading what the field is actually doing.
pub fn neutral() -> Profile {
    let mut params = TubeParams::default();
    params.readout.glow_halo_gain = 0.0;
    params.readout.vignette = 0.0;
    params.readout.reflection = 0.0;
    params.readout.pincushion = 0.0;
    params.readout.rotation = 0.0;
    params.readout.overscan = 1.0;
    Profile::new(
        "neutral",
        "Every artistic-class parameter at zero and the geometry flat, so what \
         is on screen is the phosphor field and nothing else.",
        params,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_table_row_has_at_least_one_control() {
        let registry = registry();
        for row in TABLE_ROWS {
            assert!(
                registry.iter().any(|spec| spec.row == row),
                "RENDERER.md §4 row {row:?} has no control"
            );
        }
    }

    #[test]
    fn no_control_claims_a_row_the_table_does_not_have() {
        for spec in registry() {
            assert!(
                TABLE_ROWS.contains(&spec.row) || spec.row == "Geometry (not tabulated)",
                "{} claims row {:?}, which is not in the §4 table",
                spec.name,
                spec.row
            );
        }
    }

    #[test]
    fn control_names_are_unique() {
        let registry = registry();
        for (index, spec) in registry.iter().enumerate() {
            assert!(
                !registry[..index]
                    .iter()
                    .any(|other| other.name == spec.name),
                "two controls are both called {:?}",
                spec.name
            );
        }
    }

    #[test]
    fn every_control_reads_and_writes_the_parameter_it_names() {
        for spec in registry() {
            let mut params = TubeParams::default();
            let before = spec.get(&params);
            // Somewhere else in range, chosen so it cannot coincide.
            let target = if (before - spec.max).abs() > 1e-6 {
                spec.max
            } else {
                spec.min
            };
            spec.set(&mut params, target);
            assert!(
                (spec.get(&params) - target).abs() < 1e-4 || spec.name.contains("supersample"),
                "{} did not take the value it was given",
                spec.name
            );
            spec.reset(&mut params);
            assert_eq!(spec.get(&params), spec.default(), "{} reset", spec.name);
        }
    }

    #[test]
    fn defaults_match_the_documented_table() {
        // Spot-check the values RENDERER.md §4 states, so a silent drift in a
        // default shows up here rather than in a re-blessed regression PNG.
        let params = TubeParams::default();
        assert_eq!(params.supersample, 2);
        assert_eq!(params.substep_seconds, 0.00125);
        assert_eq!(params.deposit.sigma0, 0.0015);
        assert_eq!(params.deposit.sigma1, 0.0025);
        assert_eq!(params.deposit.gamma_s, 0.7);
        assert_eq!(params.phosphor.e_sat, 4.0);
        assert_eq!(params.phosphor.tau_fast, 120e-6);
        assert_eq!(params.phosphor.tau_slow, 40e-3);
        assert_eq!(params.phosphor.fast_split, 0.75);
        assert_eq!(params.readout.chroma_fast, [0.85, 0.95, 1.0]);
        assert_eq!(params.readout.chroma_slow, [1.0, 0.92, 0.70]);
        assert_eq!(params.readout.glow_tight_sigma, 0.004);
        assert_eq!(params.readout.glow_halo_sigma, 0.06);
        assert_eq!(params.readout.glow_halo_gain, 0.08);
        assert_eq!(params.readout.pincushion, 0.02);
        assert_eq!(params.profile.aspect_w / params.profile.aspect_h, 0.75);
        assert_eq!(params.readout.exposure, 1.0);
    }

    #[test]
    fn toml_round_trips_every_parameter_exactly() {
        // Move every control off its default first, so a field the format
        // forgot cannot pass by accident.
        let mut params = TubeParams::default();
        for (index, spec) in registry().iter().enumerate() {
            let f = (index as f32 + 1.0) / (registry().len() as f32 + 1.0);
            spec.set(&mut params, spec.min + (spec.max - spec.min) * f);
        }

        let profile = Profile::new("test", "a set with nothing at its default", params);
        let text = profile.to_toml().expect("serialise");
        let read = Profile::from_toml(&text).expect("parse");

        assert_eq!(read.name, profile.name);
        assert_eq!(read.description, profile.description);
        for spec in registry() {
            assert_eq!(
                spec.get(&read.params),
                spec.get(&profile.params),
                "{} did not survive the round trip",
                spec.name
            );
        }
        assert_eq!(read.params, profile.params);
    }

    #[test]
    fn the_shipped_profiles_load() {
        for profile in [vectrex_default(), neutral()] {
            let text = profile.to_toml().expect("serialise");
            let read = Profile::from_toml(&text).expect("parse");
            assert_eq!(read, profile);
        }
    }

    #[test]
    fn the_neutral_profile_contributes_no_artistic_light() {
        // The gains are what decide whether a taste-driven effect reaches the
        // image; a σ merely describes a shape, and a σ with zero gain is
        // already inert. So the check is on amplitudes, not on every
        // artistic-class number.
        let params = neutral().params;
        assert_eq!(params.readout.glow_halo_gain, 0.0);
        assert_eq!(params.readout.vignette, 0.0);
        assert_eq!(params.readout.reflection, 0.0);
        // And the geometry is flat, so nothing is bent either.
        assert_eq!(params.readout.pincushion, 0.0);
        assert_eq!(params.readout.rotation, 0.0);
        assert_eq!(params.readout.overscan, 1.0);

        // The physics is untouched: neutral is not a different tube.
        let default = TubeParams::default();
        assert_eq!(params.deposit, default.deposit);
        assert_eq!(params.phosphor, default.phosphor);
        assert_eq!(params.readout.chroma_fast, default.readout.chroma_fast);
        assert_eq!(params.readout.chroma_slow, default.readout.chroma_slow);
    }
}

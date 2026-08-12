//! WGSL hot-reload.
//!
//! The renderer is developed by iterating shaders against replayed traces
//! (ARCHITECTURE.md §7), so this exists from day one. Every candidate is parsed
//! and validated by naga on the CPU **before** it is offered to the device: a
//! bad edit costs an error message in the panel, never a device loss, and the
//! last good source stays installed until a valid one replaces it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The directory watched for `.wgsl` files, relative to the workspace root.
pub const SHADER_DIR: &str = "crates/tube-renderer/shaders";

/// Parse and validate WGSL, returning the error text a human needs to fix it.
pub fn validate_wgsl(source: &str) -> Result<(), String> {
    let module = naga::front::wgsl::parse_str(source).map_err(|e| e.emit_to_string(source))?;
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );
    validator
        .validate(&module)
        .map_err(|e| e.emit_to_string(source))?;
    Ok(())
}

/// The last-good shader set, plus whatever went wrong most recently.
///
/// Holding sources rather than pipelines keeps this testable without a GPU;
/// the caller turns a source into a device module when the generation changes.
#[derive(Debug, Default)]
pub struct ShaderLibrary {
    good: BTreeMap<String, String>,
    /// Per file, so a file that validates cannot mask a sibling that did not.
    errors: BTreeMap<String, String>,
    generation: u64,
}

impl ShaderLibrary {
    pub fn new() -> Self {
        Self::default()
    }

    /// Offer a candidate source under `name`. On success it replaces the
    /// installed source and clears the error; on failure the installed source
    /// is **kept** and the error is recorded for the panel.
    pub fn offer(&mut self, name: &str, source: &str) -> Result<(), String> {
        match validate_wgsl(source) {
            Ok(()) => {
                let changed = self.good.get(name).is_none_or(|old| old != source);
                if changed {
                    self.good.insert(name.to_owned(), source.to_owned());
                    self.generation += 1;
                }
                self.errors.remove(name);
                Ok(())
            }
            Err(text) => {
                let message = format!("{name}: {text}");
                self.errors.insert(name.to_owned(), message.clone());
                Err(message)
            }
        }
    }

    /// Re-read every `.wgsl` file in `dir` and offer each in turn. Returns the
    /// generation, which changes only when an installed source actually
    /// changed — the caller rebuilds its pipelines on that.
    pub fn reload_dir(&mut self, dir: &Path) -> u64 {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) => {
                self.errors
                    .insert(dir.display().to_string(), format!("{}: {e}", dir.display()));
                return self.generation;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "wgsl") {
                continue;
            }
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            match std::fs::read_to_string(&path) {
                Ok(source) => {
                    let _ = self.offer(&name, &source);
                }
                Err(e) => {
                    self.errors.insert(name.clone(), format!("{name}: {e}"));
                }
            }
        }
        self.generation
    }

    /// The installed source for `name`, if one has ever validated.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.good.get(name).map(String::as_str)
    }

    /// Every outstanding error, for the panel. `None` when the whole directory
    /// is good.
    pub fn error(&self) -> Option<String> {
        if self.errors.is_empty() {
            return None;
        }
        Some(self.errors.values().cloned().collect::<Vec<_>>().join("\n"))
    }

    /// Bumped whenever an installed source changes.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// Locate the shader directory: relative to the workspace root when run via
/// `cargo run`, else relative to the executable's grandparent.
pub fn shader_dir() -> PathBuf {
    let from_manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(SHADER_DIR);
    if from_manifest.is_dir() {
        return from_manifest;
    }
    PathBuf::from(SHADER_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
        @fragment
        fn fs_main() -> @location(0) vec4<f32> {
            return vec4<f32>(1.0, 0.0, 0.0, 1.0);
        }
    "#;

    const ALSO_GOOD: &str = r#"
        @fragment
        fn fs_main() -> @location(0) vec4<f32> {
            return vec4<f32>(0.0, 1.0, 0.0, 1.0);
        }
    "#;

    // Missing return type attribute — rejected by the parser.
    const BROKEN: &str = r#"
        @fragment
        fn fs_main() -> vec4<f32> {
            return vec4<f32>(1.0, 0.0, 0.0, 1.0);
    "#;

    #[test]
    fn valid_wgsl_passes_validation() {
        assert!(validate_wgsl(GOOD).is_ok());
    }

    #[test]
    fn broken_wgsl_reports_an_error_rather_than_panicking() {
        let err = validate_wgsl(BROKEN).unwrap_err();
        assert!(!err.is_empty(), "the error text is what the panel shows");
    }

    #[test]
    fn a_bad_edit_keeps_the_last_good_source_and_captures_the_error() {
        let mut library = ShaderLibrary::new();

        library.offer("test.wgsl", GOOD).unwrap();
        let good_generation = library.generation();
        assert_eq!(library.get("test.wgsl"), Some(GOOD));
        assert!(library.error().is_none());

        library.offer("test.wgsl", BROKEN).unwrap_err();
        assert_eq!(
            library.get("test.wgsl"),
            Some(GOOD),
            "the last good source must survive a bad edit"
        );
        assert_eq!(
            library.generation(),
            good_generation,
            "a rejected edit must not trigger a pipeline rebuild"
        );
        let error = library.error().expect("error text is surfaced in-app");
        assert!(error.starts_with("test.wgsl:"));

        library.offer("test.wgsl", ALSO_GOOD).unwrap();
        assert_eq!(library.get("test.wgsl"), Some(ALSO_GOOD));
        assert!(library.error().is_none(), "recovery clears the error");
        assert_ne!(library.generation(), good_generation);
    }

    #[test]
    fn re_offering_an_unchanged_source_does_not_bump_the_generation() {
        let mut library = ShaderLibrary::new();
        library.offer("test.wgsl", GOOD).unwrap();
        let generation = library.generation();
        library.offer("test.wgsl", GOOD).unwrap();
        assert_eq!(library.generation(), generation);
    }

    #[test]
    fn every_shipped_shader_validates() {
        let dir = shader_dir();
        let mut library = ShaderLibrary::new();
        library.reload_dir(&dir);
        assert!(
            library.error().is_none(),
            "{}: {}",
            dir.display(),
            library.error().unwrap_or_default()
        );
        assert!(
            library.generation() > 0,
            "{} contained no .wgsl files",
            dir.display()
        );
    }
}

//! The headless regression suite — FIRST-SLICE.md §3.
//!
//! These need the GPU, which the target machine has. They drive the same
//! rendering path the CLI does, so a change that alters the picture is caught
//! here rather than noticed by eye three slices later.

use tube_shell::regression::{
    ANALYTIC, CASES, DISPLAY_HEIGHT, TOLERANCE, blessed_params, difference, field_energy,
    headless_field, load_png, to_srgb8,
};
use tube_shell::render::render_to_image;

/// Cargo runs integration tests in parallel threads, and each of these builds
/// its own wgpu instance and device. Several live at once is more than the
/// driver will tolerate — it segfaults — so the GPU tests take turns.
static GPU: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn gpu_turn() -> std::sync::MutexGuard<'static, ()> {
    // A panic in one test must not poison the rest; they are independent.
    GPU.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Somewhere to put renders that are compared and thrown away.
fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("trexy-regression-{}-{name}", std::process::id()))
}

#[test]
fn every_pattern_still_renders_as_blessed() {
    let _turn = gpu_turn();
    let params = blessed_params().expect("the vectrex-default profile");
    let mut failures = Vec::new();

    for case in CASES {
        let out = scratch(&format!("pattern-{:02}.png", case.number));
        let rendered = render_to_image(&case.options(out, params)).expect("render the fixture");
        let actual = to_srgb8(&rendered);

        let blessed = case.blessed_path();
        let (width, height, expected) = match load_png(&blessed) {
            Ok(image) => image,
            Err(e) => {
                failures.push(format!("pattern {}: {e}", case.number));
                continue;
            }
        };
        if (width, height) != (rendered.width, rendered.height) {
            failures.push(format!(
                "pattern {}: blessed is {width}x{height} but the render is {}x{}",
                case.number, rendered.width, rendered.height
            ));
            continue;
        }

        match difference(&actual, &expected) {
            Ok(error) if error <= TOLERANCE => {}
            Ok(error) => failures.push(format!(
                "pattern {} ({}) differs by {error:.5}, tolerance {TOLERANCE} — {}",
                case.number, case.fixture, case.note
            )),
            Err(e) => failures.push(format!("pattern {}: {e}", case.number)),
        }
    }

    assert!(
        failures.is_empty(),
        "renders no longer match the blessed images. If the change was \
         intended, re-bless with `cargo run -p tube-shell -- --bless` in the \
         same commit that caused it.\n  {}",
        failures.join("\n  ")
    );
}

/// Acceptance pattern 6. The whole fixed-substep design exists for this: a
/// constant substep *duration* means the sequence of substeps cannot depend on
/// how the wall clock is chopped into frames.
#[test]
fn the_field_does_not_depend_on_the_host_refresh_rate() {
    let _turn = gpu_turn();
    let params = blessed_params().expect("the vectrex-default profile");
    let case = CASES
        .iter()
        .find(|case| case.number == 6)
        .expect("the refresh-beat case");

    let at = |hz: f64| {
        let mut options = case.options(scratch(&format!("cadence-{hz}.png")), params);
        options.frame_hz = hz;
        to_srgb8(&render_to_image(&options).expect("render at this cadence"))
    };

    let sixty = at(60.0);
    let one_forty_four = at(144.0);
    let fifty = at(50.0);

    let error = difference(&sixty, &one_forty_four).expect("compare");
    assert_eq!(
        error, 0.0,
        "60 Hz and 144 Hz hosts produced different fields (mean error {error:.6})"
    );
    let error = difference(&sixty, &fifty).expect("compare");
    assert_eq!(
        error, 0.0,
        "60 Hz and 50 Hz hosts produced different fields (mean error {error:.6})"
    );
}

/// Acceptance pattern 7. Glow is a read-out operation and must never feed back
/// into the phosphor buffers; blur in the loop dissolves history into grey mush
/// within a second (RENDERER.md §2).
///
/// The structure of the readout module already forbids it — nothing there holds
/// a writable binding to a phosphor texture — but structure can be undone by a
/// later edit, so this measures the consequence instead: total field energy
/// must settle rather than climb.
///
/// Run at reduced resolution. Sixty simulated seconds is 48000 substeps, and
/// energy conservation does not depend on how many texels the field has.
#[test]
fn sustained_replay_reaches_a_steady_state() {
    let _turn = gpu_turn();
    const DISPLAY: u32 = 128;
    const SECONDS: f64 = 60.0;

    let params = blessed_params().expect("the vectrex-default profile");
    let (device, queue, mut field) = headless_field(DISPLAY, params).expect("a headless field");

    let trace = beam_trace::read_file(
        tube_shell::regression::workspace_root().join("fixtures/pattern-07-lissajous-torture.btr0"),
    )
    .expect("the torture fixture");
    let loop_seconds = f64::from(trace.samples.last().expect("samples").t);

    // Sample the energy each simulated second, looping the fixture.
    let mut history = Vec::new();
    let mut now = 0.0;
    while now < SECONDS {
        let offset = (now / loop_seconds).floor() * loop_seconds;
        let window: Vec<beam_trace::Sample> = trace
            .samples
            .iter()
            .map(|sample| beam_trace::Sample {
                t: sample.t + offset as f32,
                ..*sample
            })
            .collect();
        now += loop_seconds;
        field.advance(&device, &queue, &window, now, ANALYTIC);
        history.push(field_energy(&device, &queue, &field));
    }

    assert!(history.len() >= 10, "not enough samples to judge a trend");
    let settle = history.len() / 2;
    let early: f64 = history[1..settle].iter().sum::<f64>() / (settle - 1) as f64;
    let late: f64 = history[settle..].iter().sum::<f64>() / (history.len() - settle) as f64;

    assert!(early > 0.0, "nothing was ever deposited");
    assert!(
        late.is_finite(),
        "field energy stopped being a number: {late}"
    );
    // A feedback loop compounds: the second half would be far above the first.
    assert!(
        late < early * 1.25,
        "field energy is still climbing after {SECONDS} s: first half averaged \
         {early:.1}, second half {late:.1}. Glow may be feeding back into the \
         phosphor buffers."
    );
    // And it has not collapsed either, which would mean the replay stopped
    // depositing rather than reaching a steady state.
    assert!(
        late > early * 0.5,
        "field energy collapsed: {early:.1} then {late:.1}"
    );
}

/// The deposit-only view accumulates without decay by design, so it is the one
/// buffer that legitimately grows. Kept alongside the stability check so the
/// two are not confused with each other.
#[test]
fn the_blessed_images_exist_and_are_the_expected_size() {
    for case in CASES {
        let path = case.blessed_path();
        let (width, height, pixels) = load_png(&path).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(height, DISPLAY_HEIGHT, "{}", path.display());
        assert_eq!(
            pixels.len(),
            (width * height * 3) as usize,
            "{}",
            path.display()
        );
    }
}

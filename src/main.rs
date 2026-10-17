mod types;
mod ui;

#[cfg(feature = "server")]
mod engine;
#[cfg(feature = "server")]
mod prefix;

use dioxus::prelude::*;

use types::{Outcome, Request};

fn main() {
    dioxus::launch(ui::App);
}

/// Where CSV captures are read from and written to.
///
/// Both server functions take a *file name*, never a path: the client is
/// across a network, and resolving whatever it sends would be an arbitrary
/// read and write of the host. Set `ABCDEMO_DATA_DIR` to move the directory.
#[cfg(feature = "server")]
fn data_dir() -> std::path::PathBuf {
    std::env::var("ABCDEMO_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
}

/// Resolve a client-supplied name inside [`data_dir`], or refuse it.
#[cfg(feature = "server")]
fn in_data_dir(name: &str) -> Result<std::path::PathBuf, ServerFnError> {
    let name = name.trim();
    let bad = name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || name == "."
        || name == "..";
    if bad {
        return Err(ServerFnError::new(format!(
            "`{name}` is not a file name. Give a plain name like `capture.csv`; \
             it is read from and written to the server's data directory."
        )));
    }
    Ok(data_dir().join(name))
}

/// Run a planner and hand back a drawable movement.
///
/// This is the whole server. abcurves needs 1.5 MB of weights off disk and a
/// real clock, so it lives here and the browser only ever sees the result.
#[server]
pub async fn generate(request: Request) -> Result<Outcome, ServerFnError> {
    use std::cell::RefCell;

    // The planner owns its weights, is neither Clone nor Send, and wants
    // &mut self. Keeping one per worker thread means the 1.5 MB checkpoint is
    // parsed once rather than on every click.
    thread_local! {
        static ENGINE: RefCell<engine::Engine> = RefCell::new(engine::Engine::default());
    }
    let mut request = request;
    if let types::SourceSpec::Csv { path } = &request.source {
        // A name the server will not accept is something the person typed,
        // not a server fault, so it belongs in the refusal panel with every
        // other reason a movement cannot be produced.
        match in_data_dir(path) {
            Ok(resolved) => {
                request.source = types::SourceSpec::Csv {
                    path: resolved.to_string_lossy().into_owned(),
                };
            }
            Err(error) => {
                return Ok(Outcome::Refused(types::Refusal {
                    message: error.to_string(),
                    detail: vec![
                        "Captures are read from the server's data directory; set \
                         ABCDEMO_DATA_DIR to move it."
                            .into(),
                    ],
                    eligible_bc: None,
                }));
            }
        }
    }
    Ok(ENGINE.with(|engine| engine.borrow_mut().generate(&request)))
}

/// Write a source back out as CSV, so a generated movement can be reloaded.
#[server]
pub async fn export_csv(request: Request, name: String) -> Result<String, ServerFnError> {
    use prefix::{CsvPrefix, MockPrefix, PrefixSource};
    use types::SourceSpec;

    let destination = in_data_dir(&name)?;
    let source: Box<dyn PrefixSource> = match &request.source {
        SourceSpec::Mock(params) => Box::new(MockPrefix::new(*params)),
        SourceSpec::Csv { path } => {
            let resolved = in_data_dir(path)?;
            Box::new(CsvPrefix::load(resolved).map_err(|error| ServerFnError::new(error.to_string()))?)
        }
    };
    let text = CsvPrefix::render(source.as_ref());
    std::fs::write(&destination, &text)
        .map_err(|error| ServerFnError::new(format!("{}: {error}", destination.display())))?;
    Ok(format!(
        "wrote {} lines to {}",
        text.lines().count(),
        destination.display()
    ))
}

#[cfg(all(test, feature = "server"))]
mod tests {
    use super::*;
    use crate::types::{Mode, MockParams, Settings, SourceSpec};

    fn request(mode: Mode) -> Request {
        Request {
            source: SourceSpec::Mock(MockParams::default()),
            settings: Settings { mode, ..Settings::default() },
        }
    }

    fn plan(request: &Request) -> types::Trajectory {
        match engine::Engine::default().generate(request) {
            Outcome::Planned(path) => *path,
            Outcome::Refused(reason) => panic!("refused: {}", reason.message),
        }
    }

    /// The observed approach and the generated finish meet at B, and nothing
    /// is lost or duplicated at the seam.
    #[test]
    fn static_run_joins_at_b() {
        let path = plan(&request(Mode::Static));
        let b = path.b_index.expect("the static planner hands off at B");
        // The prefix is the movement cut short at B, so it is shorter than the
        // full A->C the hand was aiming at.
        assert!(
            (24..MockParams::default().duration_ms).contains(&b),
            "B at {b} ms should fall inside the aimed movement"
        );
        assert_eq!(path.samples.len(), 1 + b + path.plan_duration_ms);
        assert_eq!(path.heads.len(), 16);
        assert!(path.hit(), "landed {:.2} counts out", path.miss());

        // Positions are a running sum of the deltas, with no jump at the seam.
        for pair in path.samples.windows(2) {
            let step = [
                pair[1].pos[0] - pair[0].pos[0],
                pair[1].pos[1] - pair[0].pos[1],
            ];
            assert!((step[0] - pair[1].delta[0]).abs() < 1e-9);
            assert!((step[1] - pair[1].delta[1]).abs() < 1e-9);
        }
    }

    /// A handoff catches a movement in flight. If the mock decelerated to a
    /// stop at B, `speed_at_B` would be pinned near zero and the planner would
    /// barely react to the approach at all.
    #[test]
    fn the_hand_is_still_moving_at_b() {
        use prefix::{MockPrefix, PrefixSource};
        let source = MockPrefix::new(MockParams { jitter: 0.0, ..MockParams::default() });
        let prefix = source.prefix();
        let speed_at_b = prefix[prefix.len() - 1][0].hypot(prefix[prefix.len() - 1][1]);
        let peak = prefix
            .iter()
            .map(|r| r[0].hypot(r[1]))
            .fold(0.0f32, f32::max);
        assert!(
            speed_at_b > 0.45 * peak,
            "speed at B is {speed_at_b:.3} against a peak of {peak:.3}: the hand has stopped"
        );
    }

    /// Still milliseconds are real data. The generated finish is mostly made
    /// of them, and dropping them would speed the playback up.
    #[test]
    fn still_milliseconds_are_kept() {
        let path = plan(&request(Mode::Static));
        let b = path.b_index.unwrap();
        let still = path.samples[b + 1..]
            .iter()
            .filter(|sample| sample.delta == [0.0, 0.0])
            .count();
        assert!(
            still > path.plan_duration_ms / 4,
            "expected the finish to be mostly stationary, got {still} of {}",
            path.plan_duration_ms
        );
    }

    /// Same seeds, same movement.
    #[test]
    fn runs_are_reproducible() {
        let request = request(Mode::Static);
        let first = plan(&request);
        let second = plan(&request);
        assert_eq!(first.samples.len(), second.samples.len());
        assert_eq!(first.head, second.head);
        for (a, b) in first.samples.iter().zip(&second.samples) {
            assert_eq!(a.pos, b.pos);
        }
    }

    /// Stopping inside the seam floor is refused, in abcurves' own words.
    #[test]
    fn short_continuations_are_refused() {
        let mut request = request(Mode::Static);
        if let SourceSpec::Mock(params) = &mut request.source {
            params.stop_bc = 4.0;
        }
        match engine::Engine::default().generate(&request) {
            Outcome::Refused(reason) => {
                assert!(
                    reason.message.contains("8"),
                    "the refusal should name the floor: {}",
                    reason.message
                );
                assert!(reason.eligible_bc.is_some());
            }
            Outcome::Planned(_) => panic!("a 4 count continuation should be refused"),
        }
    }

    /// A mock run written out and read back is the same movement.
    #[test]
    fn csv_round_trips() {
        use prefix::{CsvPrefix, MockPrefix, PrefixSource};
        let source = MockPrefix::new(MockParams::default());
        let text = CsvPrefix::render(&source);
        let loaded = CsvPrefix::parse(&text, "round trip".into()).expect("parses");

        assert_eq!(loaded.target_delta(), source.target_delta());
        assert_eq!(loaded.radius(), source.radius());
        let (before, after) = (source.prefix(), loaded.prefix());
        assert_eq!(before.len(), after.len());
        for (a, b) in before.iter().zip(&after) {
            // Six decimal places of counts is far finer than a mouse reports.
            assert!((a[0] - b[0]).abs() < 1e-5 && (a[1] - b[1]).abs() < 1e-5);
        }
    }

    /// A client across a network must not be able to name a path. Resolving
    /// whatever it sends would be an arbitrary read and write of the host.
    #[test]
    fn csv_names_cannot_escape_the_data_directory() {
        for bad in [
            "../../etc/passwd",
            "/etc/passwd",
            "sub/dir.csv",
            "back\\slash.csv",
            "..",
            ".",
            "   ",
            "",
        ] {
            assert!(
                in_data_dir(bad).is_err(),
                "`{bad}` should be refused as a file name"
            );
        }
        let ok = in_data_dir("capture.csv").expect("a plain name is fine");
        assert_eq!(ok.file_name().unwrap(), "capture.csv");
        assert_eq!(ok.parent().unwrap(), data_dir());
    }

    /// A file that does not say where C is cannot be replayed.
    #[test]
    fn csv_without_a_target_is_rejected() {
        use prefix::CsvPrefix;
        let error = CsvPrefix::parse("report,1,1\nreport,2,2\n", "x".into()).unwrap_err();
        assert!(error.to_string().contains("target"));

        let error = CsvPrefix::parse("target,10,10\nreport,oops,1\n", "x".into()).unwrap_err();
        assert!(error.to_string().contains("line 2"), "{error}");
    }

    /// The Continuous Planner has no handoff, and starts from standstill.
    #[test]
    fn continuous_has_no_seam() {
        let path = plan(&request(Mode::Continuous));
        assert!(path.b_index.is_none());
        assert!(path.heads.is_empty());
        assert_eq!(path.samples[0].pos, [0.0, 0.0]);
        assert!(path.duration_ms() > 50);
    }

    /// The views have to actually draw the movement: one stroke per
    /// millisecond, the markers, and the handoff.
    #[test]
    fn the_views_draw_every_millisecond() {
        let path = plan(&request(Mode::Static));
        let segments = path.samples.len() - 1;

        let scene = render(path.clone(), false);
        assert!(scene.contains("<svg"), "no svg element");
        assert_eq!(
            scene.matches("<line").count(),
            segments,
            "one stroke per closed millisecond"
        );
        assert!(scene.contains(">B<"), "the handoff is not marked");
        assert!(scene.contains(">C<") && scene.contains(">A<"), "A and C are not marked");
        // The ramp has to actually vary, or the heatmap says nothing.
        assert!(scene.matches("rgb(").count() > 10, "the speed colouring is flat");

        let chart = render(path, true);
        assert!(chart.contains("<polygon"), "no filled speed area");
        assert!(chart.contains("counts/ms"), "the speed axis is unlabelled");
        // Per-millisecond line, three gridlines, the B rule and the playhead.
        assert_eq!(chart.matches("<line").count(), segments + 5);
    }

    /// Dump the precomputed payload for the static web build.
    ///
    /// Each *family* is one prefix plus all sixteen finishes the planner can
    /// draw from it, so the page can make the head strip a real control: the
    /// prefix is shared, and picking a head swaps only the B->C half.
    ///
    /// `cargo test --features server -- --ignored dump_web`
    #[test]
    #[ignore = "writes the static demo payload"]
    fn dump_web() {
        use crate::types::{MockParams, Settings, SourceSpec};

        fn num(value: f64) -> String {
            let rounded = (value * 1000.0).round() / 1000.0;
            if rounded == 0.0 { "0".into() } else { format!("{rounded}") }
        }
        fn esc(text: &str) -> String {
            text.replace('\\', "\\\\").replace('"', "\\\"")
        }
        fn strings(items: &[String]) -> String {
            let joined: Vec<String> =
                items.iter().map(|item| format!("\"{}\"", esc(item))).collect();
            format!("[{}]", joined.join(","))
        }
        fn pairs(deltas: &[[f64; 2]]) -> String {
            let joined: Vec<String> = deltas
                .iter()
                .map(|d| format!("[{},{}]", num(d[0]), num(d[1])))
                .collect();
            format!("[{}]", joined.join(","))
        }

        let mut engine = engine::Engine::default();
        let mut families: Vec<String> = Vec::new();
        let mut loose: Vec<String> = Vec::new();
        let mut refused: Vec<String> = Vec::new();

        // One prefix, all sixteen finishes.
        let mut family = |engine: &mut engine::Engine,
                          group: &str,
                          name: &str,
                          params: MockParams,
                          settings: Settings| {
            let mut finishes: Vec<String> = Vec::new();
            let mut prefix: Option<Vec<[f64; 2]>> = None;
            let mut meta = String::new();

            for head in 0..16 {
                let request = Request {
                    source: SourceSpec::Mock(params),
                    settings: Settings { head: Some(head), ..settings },
                };
                match engine.generate(&request) {
                    Outcome::Planned(path) => {
                        let b = path.b_index.expect("static runs hand off at B");
                        if prefix.is_none() {
                            prefix = Some(
                                path.samples[1..=b].iter().map(|s| s.delta).collect(),
                            );
                            meta = format!(
                                "\"target\":[{},{}],\"radius\":{},\"notes\":{},\
                                 \"profileUs\":{},\"planUs\":{},\
                                 \"shape\":{{\"durationMs\":{},\"curvature\":{},\
                                 \"jitter\":{},\"stopBc\":{}}},\
                                 \"modelSeed\":{},\"plannerSeed\":{},\
                                 \"rendererSeed\":{},\"mockSeed\":{}",
                                num(path.target[0]), num(path.target[1]), num(path.radius),
                                strings(&path.notes[..3.min(path.notes.len())]),
                                path.timing.profile_us, path.timing.plan_us,
                                params.duration_ms, num(params.curvature),
                                num(params.jitter), num(params.stop_bc),
                                settings.model_seed, settings.planner_seed,
                                settings.renderer_seed, params.seed,
                            );
                        }
                        let finish: Vec<[f64; 2]> =
                            path.samples[b + 1..].iter().map(|s| s.delta).collect();
                        finishes.push(format!(
                            "{{\"head\":{head},\"ms\":{},\"deltas\":{}}}",
                            path.plan_duration_ms,
                            pairs(&finish)
                        ));
                    }
                    Outcome::Refused(reason) => {
                        if head == 0 {
                            refused.push(format!(
                                "{{\"group\":\"{}\",\"name\":\"{}\",\"message\":\"{}\",\
                                 \"detail\":{},\"eligible\":{}}}",
                                esc(group), esc(name), esc(&reason.message),
                                strings(&reason.detail),
                                reason.eligible_bc.map_or("null".into(), |(lo, hi)| format!(
                                    "[{},{}]", num(lo), num(hi)
                                ))
                            ));
                        }
                        return;
                    }
                }
            }
            // Which head this seed actually draws, left unpinned.
            let drawn = abcurves::rng::head_from_seed(
                abcurves::static_planner::Draw::seeded(settings.planner_seed).planner_seed,
                16,
            );
            let Some(prefix) = prefix else { return };
            families.push(format!(
                "{{\"group\":\"{}\",\"name\":\"{}\",{meta},\"drawn\":{drawn},\
                 \"prefix\":{},\"finishes\":[{}]}}",
                esc(group), esc(name), pairs(&prefix), finishes.join(",")
            ));
        };

        let base = MockParams::default();

        // Radius is an input to the model, not decoration: it reaches the
        // planner through distance_over_radius, prefix_lateral / radius and
        // speed_at_B / radius, and it moves the eligible |BC| window.
        for (scene, target, radii) in [
            ("166 counts", [140.0f64, 90.0], &[2.0f64, 4.0, 8.0, 12.0, 20.0, 32.0][..]),
            ("400 counts", [400.0, 0.0], &[4.0, 12.0, 24.0, 40.0][..]),
            ("72 counts", [60.0, 40.0], &[2.0, 4.0, 8.0][..]),
        ] {
            for &radius in radii {
                family(&mut engine, scene, &format!("radius {}", num(radius)),
                    MockParams {
                        target_delta: target, radius,
                        stop_bc: types::midpoint_bc(target, radius), ..base
                    },
                    Settings::default());
            }
        }

        // Different approaches to the same target. The planner seed is held
        // fixed, so what moves the sixteen durations is the prefix alone.
        for (name, duration_ms, curvature, jitter, stop_bc) in [
            ("brisk and straight", 90usize, 0.01f64, 0.15f64, 16.0f64),
            ("brisk, wide bow", 90, 0.20, 0.15, 16.0),
            ("slow and straight", 340, 0.01, 0.15, 16.0),
            ("stops early, 40 left", 180, 0.06, 0.35, 40.0),
            ("shaky hand", 180, 0.06, 1.20, 28.0),
        ] {
            family(&mut engine, "Prefix shape", name,
                MockParams { duration_ms, curvature, jitter, stop_bc, seed: 2026, ..base },
                Settings::default());
        }

        // The pool the reroll draws from.
        let mut lcg: u64 = 0x2026_0A17;
        let mut roll = move || {
            lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            lcg >> 33
        };
        let span = |value: u64, lo: f64, hi: f64| lo + (value % 10_000) as f64 / 10_000.0 * (hi - lo);
        let window = types::approx_eligible_bc(base.target_delta, base.radius);
        for index in 0..24 {
            let hand = roll() % 100_000;
            let duration_ms = span(roll(), 70.0, 380.0) as usize;
            let curvature = span(roll(), 0.0, 0.22);
            let jitter = span(roll(), 0.05, 1.2);
            let stop_bc = match window {
                Some((lo, hi)) => span(roll(), lo + 0.08 * (hi - lo), hi - 0.08 * (hi - lo)),
                None => base.stop_bc,
            };
            let planner = roll() % 100_000;
            let renderer = roll() % 100_000;
            let model = if roll() % 2 == 0 { 7 } else { 23 };
            family(&mut engine, "Random draws", &format!("draw {index}"),
                MockParams { seed: hand, duration_ms, curvature, jitter, stop_bc, ..base },
                Settings {
                    model_seed: model, planner_seed: planner,
                    renderer_seed: renderer, ..Settings::default()
                });
        }

        // The Continuous Planner has no handoff and no heads, so it stays a
        // plain trace rather than a family.
        for (name, seed, moving) in [
            ("settles cleanly", 7u64, None),
            ("overshoots and holds", 2026, None),
            ("C moves at 160 ms", 7, Some(160usize)),
            ("C moves, seed 31337", 31337, Some(160)),
        ] {
            let request = Request {
                source: SourceSpec::Mock(base),
                settings: Settings {
                    mode: types::Mode::Continuous, planner_seed: seed,
                    renderer_seed: seed, move_target_at_ms: moving,
                    ..Settings::default()
                },
            };
            if let Outcome::Planned(path) = engine.generate(&request) {
                let deltas: Vec<[f64; 2]> = path.samples[1..].iter().map(|s| s.delta).collect();
                loose.push(format!(
                    "{{\"group\":\"Continuous\",\"name\":\"{}\",\"target\":[{},{}],\
                     \"radius\":{},\"notes\":{},\"renderUs\":{},\"plannerSeed\":{},\
                     \"rendererSeed\":{},\"modelSeed\":{},\"deltas\":{}}}",
                    esc(name), num(path.target[0]), num(path.target[1]), num(path.radius),
                    strings(&path.notes), path.timing.render_us, seed, seed, 7,
                    pairs(&deltas)
                ));
            }
        }

        // The seam refusing, in its own words.
        for (name, stop) in [("stopped 4 counts out", 4.0), ("stopped 60 counts out", 60.0)] {
            family(&mut engine, "Refused", name,
                MockParams { stop_bc: stop, ..base }, Settings::default());
        }

        let json = format!(
            "{{\"families\":[{}],\"loose\":[{}],\"refusals\":[{}]}}",
            families.join(","), loose.join(","), refused.join(",")
        );
        let path = std::env::var("DUMP_PATH").unwrap_or_else(|_| "web-data.json".into());
        std::fs::write(&path, &json).expect("write");
        println!(
            "{} families (x16 finishes), {} loose, {} refusals, {} bytes -> {path}",
            families.len(), loose.len(), refused.len(), json.len()
        );
    }

    /// Dump seam-contract cases with the library's own verdict, so the
    /// static page's copy of the rules can be checked against it.
    #[test]
    #[ignore = "writes the seam case fixtures"]
    fn dump_seam_cases() {
        use crate::prefix::{MockPrefix, PrefixSource};
        use crate::types::MockParams;

        let mut rows: Vec<String> = Vec::new();
        let geometries = [
            ([140.0f64, 90.0], 12.0f64),
            ([140.0, 90.0], 2.0),
            ([140.0, 90.0], 40.0),
            ([400.0, 0.0], 18.0),
            ([60.0, 40.0], 8.0),
            ([30.0, 10.0], 12.0),
        ];
        for (target, radius) in geometries {
            let window = engine::eligible_bc(target, radius);
            for stop in [2.0f64, 6.0, 12.0, 20.0, 30.0, 45.0, 70.0, 120.0] {
                let params = MockParams {
                    target_delta: target, radius, stop_bc: stop, ..MockParams::default()
                };
                let source = MockPrefix::new(params);
                let prefix = source.prefix();
                let movement = abcurves::static_planner::Movement::from_counts(
                    [0.0, 0.0], target, radius, &prefix,
                );
                let ok = movement.handoff().is_ok();
                let pts: Vec<String> = prefix
                    .iter()
                    .map(|r| format!("[{},{}]", r[0], r[1]))
                    .collect();
                rows.push(format!(
                    "{{\"name\":\"AC {:.0} r{:.0} stop {:.0}\",\"target\":[{},{}],\
                     \"radius\":{},\"ok\":{},\"window\":{},\"prefix\":[{}]}}",
                    target[0].hypot(target[1]), radius, stop,
                    target[0], target[1], radius, ok,
                    window.map_or("null".into(), |(l, h)| format!("[{l},{h}]")),
                    pts.join(",")
                ));
            }
        }
        let path = std::env::var("SEAM_PATH").unwrap_or_else(|_| "seam-cases.json".into());
        std::fs::write(&path, format!("[{}]", rows.join(","))).expect("write");
        println!("{} seam cases -> {path}", rows.len());
    }

    /// Render one of the two views to HTML, the way the server does.
    fn render(path: types::Trajectory, chart: bool) -> String {
        use dioxus::prelude::*;

        #[component]
        fn Harness(path: types::Trajectory, chart: bool) -> Element {
            rsx! {
                if chart {
                    ui::speed::SpeedChart { trajectory: path.clone(), cursor: 42.0 }
                } else {
                    ui::scene::Scene { trajectory: path.clone(), cursor: 42.0 }
                }
            }
        }

        let mut dom = VirtualDom::new_with_props(Harness, HarnessProps { path, chart });
        dom.rebuild_in_place();
        dioxus::ssr::render(&dom)
    }

    /// The client-side estimate of the eligible window has to agree with the
    /// planner, or the slider lies about what will be accepted.
    #[test]
    fn eligible_window_estimate_matches_the_planner() {
        for target in [[140.0, 90.0], [400.0, -120.0], [60.0, 20.0], [30.0, 0.0]] {
            for radius in [2.0, 12.0, 40.0] {
                let ours = types::approx_eligible_bc(target, radius);
                let theirs = engine::eligible_bc(target, radius);
                match (ours, theirs) {
                    (Some((a, b)), Some((c, d))) => {
                        assert!(
                            (a - c).abs() < 1e-9 && (b - d).abs() < 1e-9,
                            "{target:?} r{radius}: {ours:?} vs {theirs:?}"
                        );
                    }
                    (None, None) => {}
                    _ => panic!("{target:?} r{radius}: {ours:?} vs {theirs:?}"),
                }
            }
        }
    }
}

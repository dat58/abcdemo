//! Driving abcurves, and turning what it returns into something drawable.

use std::time::Instant;

use abcurves::continuous::{ContinuousPipeline, CountTransform, PipelineOptions};
use abcurves::static_planner::{Draw, Movement, Policy, StaticPipeline};

use crate::prefix::{CsvPrefix, MockPrefix, PrefixSource};
use crate::types::{
    HeadOption, Mode, Outcome, Refusal, Request, Settings, SourceSpec, Timing, Trajectory,
};

/// How many still milliseconds count as "the movement is over".
const SETTLE_MS: usize = 150;


/// Holds the loaded models between runs.
///
/// `StaticPipeline` is neither `Clone` nor `Send`, owns ~1.5 MB of weights and
/// wants `&mut self`, so it is created once and kept.
#[derive(Default)]
pub struct Engine {
    static_pipeline: Option<(u32, StaticPipeline)>,
    load_us: u128,
}

impl Engine {
    /// Load the Static Planner, or hand back the one already loaded.
    ///
    /// Model files resolve themselves: `model_store::default_model_dir` falls
    /// back to abcurves' own `CARGO_MANIFEST_DIR/models`, which is the sibling
    /// checkout, so nothing has to be copied or configured.
    fn static_pipeline(&mut self, model_seed: u32) -> Result<&mut StaticPipeline, Refusal> {
        let stale = self
            .static_pipeline
            .as_ref()
            .is_none_or(|(seed, _)| *seed != model_seed);
        if stale {
            let start = Instant::now();
            let pipeline = StaticPipeline::from_pretrained(model_seed, None, true)
                .map_err(|error| Refusal {
                    message: format!("could not load planner cell {model_seed}: {error}"),
                    detail: vec![
                        "abcurves looks in $ABCURVES_MODEL_DIR, ./models, then its own \
                         checkout's models/."
                            .into(),
                    ],
                    eligible_bc: None,
                })?;
            self.load_us = start.elapsed().as_micros();
            self.static_pipeline = Some((model_seed, pipeline));
        }
        Ok(&mut self
            .static_pipeline
            .as_mut()
            .expect("just populated")
            .1)
    }

    /// Resolve a request into a movement, or into the reason there isn't one.
    pub fn generate(&mut self, request: &Request) -> Outcome {
        let source: Box<dyn PrefixSource> = match &request.source {
            SourceSpec::Mock(params) => Box::new(MockPrefix::new(*params)),
            SourceSpec::Csv { path } => match CsvPrefix::load(path) {
                Ok(csv) => Box::new(csv),
                Err(error) => {
                    return Outcome::Refused(Refusal {
                        message: format!("could not read the prefix CSV: {error}"),
                        detail: vec![
                            "expected rows of `target,x,y`, an optional `radius,r,0`, then \
                             one `report,dx,dy` per closed millisecond."
                                .into(),
                        ],
                        eligible_bc: None,
                    });
                }
            },
        };
        match self.run(source.as_ref(), &request.settings) {
            Ok(trajectory) => Outcome::Planned(Box::new(trajectory)),
            Err(refusal) => Outcome::Refused(refusal),
        }
    }

    fn run(
        &mut self,
        source: &dyn PrefixSource,
        settings: &Settings,
    ) -> Result<Trajectory, Refusal> {
        match settings.mode {
            Mode::Static => self.run_static(source, settings),
            Mode::Continuous => self.run_continuous(source, settings),
        }
    }

    // -----------------------------------------------------------------------
    // Static: A -> B observed, B -> C generated
    // -----------------------------------------------------------------------

    fn run_static(
        &mut self,
        source: &dyn PrefixSource,
        settings: &Settings,
    ) -> Result<Trajectory, Refusal> {
        let prefix = source.prefix();
        let target = source.target_delta();
        let radius = source.radius();
        if prefix.is_empty() {
            return Err(Refusal::plain(
                "the prefix is empty, so there is no movement to continue",
            ));
        }

        let pipeline = self.static_pipeline(settings.model_seed)?;
        let mut timing = Timing::default();

        // The renderer profile describes the device, not the movement, and
        // costs nine times the plan it precedes. In a live system it is built
        // during a quiet moment and reused; here it is one call per run.
        let idle = source.idle_window();
        let start = Instant::now();
        let profile = pipeline
            .prepare_renderer_profile(&idle)
            .map_err(|error| Refusal {
                message: format!("renderer profile: {error}"),
                detail: vec![format!("the idle window was {} reports", idle.len())],
                eligible_bc: None,
            })?;
        timing.profile_us = start.elapsed().as_micros();

        // A is the origin, so C's delta is also its absolute position.
        let movement = Movement::from_counts([0.0, 0.0], target, radius, &prefix);
        let remaining = movement.remaining();
        let bc = remaining[0].hypot(remaining[1]);

        // Validate B before planning. The refusal names the rule it failed and
        // by how much, which is worth showing rather than swallowing.
        movement.handoff().map_err(|error| Refusal {
            message: error.to_string(),
            detail: describe_geometry(target, radius, bc, prefix.len()),
            eligible_bc: eligible_bc(target, radius),
        })?;

        // All sixteen heads cost about one and a half, and knowing the spread
        // is the point: duration is the finishing style, not the distance.
        let start = Instant::now();
        let plans = pipeline.plan_movement_all(&movement).map_err(|error| Refusal {
            message: error.to_string(),
            detail: describe_geometry(target, radius, bc, prefix.len()),
            eligible_bc: eligible_bc(target, radius),
        })?;
        timing.plan_us = start.elapsed().as_micros();

        let heads: Vec<HeadOption> = plans
            .iter()
            .map(|plan| HeadOption {
                head: plan.head,
                duration_ms: plan.duration_ms,
            })
            .collect();

        // A pinned head, or the one this seed draws. Entry k is bit for bit
        // what `continue_movement` returns for `draw.head(k)`, so choosing
        // after the fact loses nothing.
        let draw = Draw::seeded(settings.planner_seed);
        let drawn = match settings.head {
            Some(head) => head.min(plans.len().saturating_sub(1)),
            None => abcurves::rng::head_from_seed(draw.planner_seed, plans.len() as u32),
        };
        let intent = plans
            .into_iter()
            .nth(drawn)
            .expect("plan_movement_all returns one intent per head");
        let plan_duration_ms = intent.duration_ms;

        // Rendering is the half that does not batch: ~18 us a report.
        let start = Instant::now();
        let reports = pipeline
            .begin(&profile, intent, settings.renderer_seed)
            .and_then(|mut stream| stream.render_remaining(pipeline.model()))
            .map_err(|error| Refusal::plain(error.to_string()))?;
        timing.render_us = start.elapsed().as_micros();

        // One timeline: the observed approach, then the generated finish.
        let mut deltas: Vec<[f64; 2]> = prefix
            .iter()
            .map(|report| [f64::from(report[0]), f64::from(report[1])])
            .collect();
        let b_index = deltas.len(); // samples[0] is A, so B lands at prefix.len()
        deltas.extend(
            reports
                .iter()
                .map(|report| [f64::from(report[0]), f64::from(report[1])]),
        );
        let samples = Trajectory::from_deltas(&deltas);
        timing.load_us = self.load_us;

        let moving = reports.iter().filter(|report| *report != &[0, 0]).count();
        let policy = movement
            .resolved_policy()
            .map(|config| format!("{:.0}%", config.threshold * 100.0))
            .unwrap_or_else(|_| "-".into());
        let notes = vec![
            format!("A to B: {} ms observed", prefix.len()),
            format!("B to C: {} reports, {moving} carrying motion", reports.len()),
            format!("|BC| at B: {bc:.2} counts, seam policy {policy}"),
            format!("head {drawn} of {}", heads.len()),
        ];

        Ok(Trajectory {
            samples,
            b_index: Some(b_index),
            target,
            radius,
            head: Some(drawn),
            heads,
            plan_duration_ms,
            timing,
            notes,
            source_label: source.label(),
            eligible_bc: eligible_bc(target, radius),
        })
    }

    // -----------------------------------------------------------------------
    // Continuous: standstill to the target, and settle
    // -----------------------------------------------------------------------

    fn run_continuous(
        &mut self,
        source: &dyn PrefixSource,
        settings: &Settings,
    ) -> Result<Trajectory, Refusal> {
        let target = source.target_delta();
        let radius = source.radius();
        let idle = source.idle_window();

        // Keep counts Y-up internally and flip only when drawing, so both
        // planners share one coordinate convention.
        let transform =
            CountTransform::new(settings.radians_per_count, false).map_err(|error| Refusal {
                message: format!("count transform: {error}"),
                detail: vec!["radians per count has to be positive and finite".into()],
                eligible_bc: None,
            })?;

        let start = Instant::now();
        let options = PipelineOptions::new(transform)
            .seed(settings.planner_seed)
            .renderer_seed(settings.renderer_seed)
            .initial_xy([0.0, 0.0]);
        let mut stream = ContinuousPipeline::load(&idle, options).map_err(|error| Refusal {
            message: format!("could not load the Continuous Planner: {error}"),
            detail: vec![
                "it needs models/continuous/weights.npz plus every file the manifest \
                 declares, including the ONNX graphs it never evaluates."
                    .into(),
            ],
            eligible_bc: None,
        })?;
        let load_us = start.elapsed().as_micros();

        stream
            .update_target(target, 0)
            .map_err(|error| Refusal::plain(error.to_string()))?;

        // A second target, to show the thing the Static Planner cannot do.
        let moved_target = settings.move_target_at_ms.map(|at| {
            let swing = [-target[1] * 0.45, target[0] * 0.45];
            (at, [target[0] + swing[0], target[1] + swing[1]])
        });

        let start = Instant::now();
        let mut deltas: Vec<[f64; 2]> = Vec::new();
        let mut position = [0.0f64; 2];
        let mut goal = target;
        let mut settled = 0usize;
        let mut moving_yet = false;
        for tick in 1..=settings.continuous_budget_ms as i64 {
            if let Some((at, next)) = moved_target
                && tick == at as i64
            {
                stream
                    .update_target(next, tick * 1000)
                    .map_err(|error| Refusal::plain(error.to_string()))?;
                goal = next;
            }
            let advance = stream.advance(tick * 1000).map_err(|error| Refusal {
                message: match error {
                    abcurves::continuous::PipelineError::Contract(inner) => inner.to_string(),
                    abcurves::continuous::PipelineError::Failed(failure) => failure.message.clone(),
                },
                detail: vec![format!("after {tick} ms")],
                eligible_bc: None,
            })?;

            for report in &advance.reports {
                let delta = [f64::from(report[0]), f64::from(report[1])];
                position = [position[0] + delta[0], position[1] + delta[1]];
                deltas.push(delta);
            }

            // Stop once the hand has gone quiet. The planner holds position
            // indefinitely once it is done, and that is true whether it
            // landed or not: some draws overshoot and simply stay there,
            // which is a thing people do. Waiting for `inside` would burn the
            // whole budget on stationary samples for exactly those runs.
            //
            // Only after it has started: the planner takes a human moment to
            // react, and that opening silence is not the end of the movement.
            let still = advance.reports.iter().all(|report| report == &[0, 0]);
            if !still {
                moving_yet = true;
            }
            settled = if still && moving_yet { settled + 1 } else { 0 };
            if settled >= SETTLE_MS {
                break;
            }
        }
        let render_us = start.elapsed().as_micros();

        if deltas.is_empty() {
            return Err(Refusal {
                message: "the Continuous Planner produced no reports".into(),
                detail: vec![format!("budget was {} ms", settings.continuous_budget_ms)],
                eligible_bc: None,
            });
        }

        // Keep a short tail of the hold so the settle reads on the speed
        // chart, but drop the rest of the silence.
        if let Some(last_moving) = deltas.iter().rposition(|delta| delta != &[0.0, 0.0]) {
            deltas.truncate((last_moving + 1 + SETTLE_MS / 3).min(deltas.len()));
        }

        let samples = Trajectory::from_deltas(&deltas);
        let moving = deltas.iter().filter(|delta| *delta != &[0.0, 0.0]).count();
        let gap = (goal[0] - position[0]).hypot(goal[1] - position[1]);
        let mut notes = vec![
            format!("{} reports, {moving} carrying motion", deltas.len()),
            "no handoff: this planner starts from standstill, so there is no B".into(),
        ];
        if gap > radius {
            // Not a failure. The sixteen-way distribution the model learned
            // includes draws that overshoot and stay there.
            notes.push(format!(
                "stopped {gap:.1} counts out, past the {radius:.0} count radius: this draw \
                 overshot and held, which is in the distribution"
            ));
        }
        if let Some((at, next)) = moved_target {
            notes.push(format!(
                "C moved at {at} ms to ({:.0}, {:.0})",
                next[0], next[1]
            ));
        }

        Ok(Trajectory {
            samples,
            b_index: None,
            target: goal,
            radius,
            head: None,
            heads: Vec::new(),
            plan_duration_ms: deltas.len(),
            timing: Timing {
                load_us,
                render_us,
                ..Timing::default()
            },
            notes,
            source_label: source.label(),
            eligible_bc: None,
        })
    }
}

/// The numbers worth seeing next to a refusal.
fn describe_geometry(target: [f64; 2], radius: f64, bc: f64, ticks: usize) -> Vec<String> {
    let distance = target[0].hypot(target[1]);
    let mut detail = vec![
        format!("A to C is {distance:.2} counts, radius {radius:.2}"),
        format!("the hand stopped {bc:.2} counts short, after {ticks} ms"),
    ];
    if let Some((low, high)) = eligible_bc(target, radius) {
        detail.push(format!(
            "|BC| has to land between {low:.2} and {high:.2} counts"
        ));
    } else {
        detail.push(
            "no |BC| is eligible for this geometry: C is too close to A for a handoff".into(),
        );
    }
    detail
}

/// The `|BC|` window abcurves will accept for this geometry, if any.
///
/// This is the authority the UI's client-side estimate defers to.
pub fn eligible_bc(target: [f64; 2], radius: f64) -> Option<(f64, f64)> {
    Movement::eligible_remaining([0.0, 0.0], target, radius, Policy::Auto)
        .map(|window| (*window.start(), *window.end()))
}

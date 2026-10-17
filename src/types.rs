//! Everything that crosses the wire, plus the geometry helpers the views need.
//!
//! abcurves is native-only — it reads 1.5 MB of weights off disk — so it runs
//! on the server and nothing in this module may depend on it. These are the
//! shapes the browser receives and draws.

use serde::{Deserialize, Serialize};

/// The seam contract refuses an A->B longer than this.
pub const MAX_PREFIX_MS: usize = 1500;

/// A hit radius that suits the distances the mock generates by default.
///
/// Small on purpose. Training spans roughly 2 to 90 counts with 2 to 20
/// deliberately oversampled, and the radius is what tells the model how
/// precise it has to be — a tight one is the interesting case.
pub const DEFAULT_RADIUS: f64 = 2.0;

/// Which planner to run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    /// Continue a movement already in progress: A->B observed, B->C generated.
    Static,
    /// Drive to the target from standstill.
    Continuous,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Static => "Static (A→B→C)",
            Self::Continuous => "Continuous",
        }
    }
}

/// The knobs behind the synthetic approach.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MockParams {
    /// C, as a delta from A, in counts.
    pub target_delta: [f64; 2],
    /// Hit radius in counts.
    pub radius: f64,
    /// How long A->B takes.
    pub duration_ms: usize,
    /// How many counts short of C the hand stops. This is `|BC|`, and it is
    /// what decides whether the handoff is eligible at all.
    pub stop_bc: f64,
    /// Peak lateral bow, as a fraction of the A->C distance.
    pub curvature: f64,
    /// Per-millisecond tremor, in counts.
    pub jitter: f64,
    /// Seed for the curvature phase and the tremor.
    pub seed: u64,
}

impl Default for MockParams {
    fn default() -> Self {
        let target_delta = [140.0, 90.0];
        let radius = DEFAULT_RADIUS;
        Self {
            target_delta,
            radius,
            duration_ms: 180,
            stop_bc: midpoint_bc(target_delta, radius),
            curvature: 0.06,
            jitter: 0.35,
            seed: 2026,
        }
    }
}

/// Where the prefix comes from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SourceSpec {
    /// Synthesise one.
    Mock(MockParams),
    /// Read one off disk, with C and the radius inside the file.
    Csv { path: String },
}

impl Default for SourceSpec {
    fn default() -> Self {
        Self::Mock(MockParams::default())
    }
}

/// Everything else the UI can turn.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub mode: Mode,
    /// Which of the two independently trained planner cells: 7 or 23.
    pub model_seed: u32,
    /// Picks the finishing style.
    pub planner_seed: u64,
    /// Picks the renderer's texture.
    pub renderer_seed: u64,
    /// Pin one of the sixteen heads instead of drawing one.
    pub head: Option<usize>,
    /// Device sensitivity, for the Continuous Planner's count transform.
    pub radians_per_count: f64,
    /// Give up after this many milliseconds of continuous movement.
    pub continuous_budget_ms: usize,
    /// Move C mid-flight, to show what the Continuous Planner does with it.
    pub move_target_at_ms: Option<usize>,
}

/// The reference sensitivity, mirrored from
/// `abcurves::continuous::COMMON_RADIANS_PER_COUNT` so the client can show it
/// without linking the crate.
pub const COMMON_RADIANS_PER_COUNT: f64 = 0.0003509487083280618;

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::Static,
            model_seed: 7,
            planner_seed: 2026,
            renderer_seed: 2026,
            head: None,
            radians_per_count: COMMON_RADIANS_PER_COUNT,
            continuous_budget_ms: 1200,
            move_target_at_ms: None,
        }
    }
}

/// One request for a movement.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub source: SourceSpec,
    pub settings: Settings,
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

/// One closed millisecond of movement.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// Milliseconds since A.
    pub t_ms: usize,
    /// Position relative to A, in native counts, Y up.
    pub pos: [f64; 2],
    /// What this millisecond moved.
    pub delta: [f64; 2],
    /// `hypot(delta)`, in counts per millisecond.
    pub speed: f64,
}

impl Sample {
    /// `0.0` at a standstill, `1.0` at the fastest millisecond of the run.
    pub fn heat(&self, max_speed: f64) -> f64 {
        if max_speed <= 1e-9 {
            0.0
        } else {
            (self.speed / max_speed).clamp(0.0, 1.0)
        }
    }
}

/// Where the time went. All in microseconds, measured on the real calls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timing {
    pub load_us: u128,
    pub profile_us: u128,
    pub plan_us: u128,
    pub render_us: u128,
}

/// One finishing style the planner could have drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadOption {
    pub head: usize,
    pub duration_ms: usize,
}

/// A complete movement, ready to draw.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Trajectory {
    pub samples: Vec<Sample>,
    /// Index of B in `samples` — the last observed report. `None` for the
    /// Continuous Planner, which has no handoff.
    pub b_index: Option<usize>,
    /// C, as a delta from A.
    pub target: [f64; 2],
    pub radius: f64,
    /// Which of the sixteen finishing styles was drawn.
    pub head: Option<usize>,
    /// What all sixteen would have cost, when we asked.
    pub heads: Vec<HeadOption>,
    /// Length of the generated part, in milliseconds.
    pub plan_duration_ms: usize,
    pub timing: Timing,
    /// Free-form lines for the stats panel.
    pub notes: Vec<String>,
    /// How the prefix described itself.
    pub source_label: String,
    /// The `|BC|` window this geometry allows, if any.
    pub eligible_bc: Option<(f64, f64)>,
}

/// A refusal, carrying the planner's own words.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Refusal {
    /// abcurves' message, verbatim. It names the rule and by how much.
    pub message: String,
    /// Extra geometry, so the panel can explain the refusal rather than just
    /// repeat it.
    pub detail: Vec<String>,
    /// The `|BC|` window this geometry allows, if any.
    pub eligible_bc: Option<(f64, f64)>,
}

impl Refusal {
    pub fn plain(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            detail: Vec::new(),
            eligible_bc: None,
        }
    }
}

/// What a generate call comes back with.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Outcome {
    Planned(Box<Trajectory>),
    Refused(Refusal),
}

impl Trajectory {
    /// Accumulate per-millisecond deltas into absolute samples.
    pub fn from_deltas(deltas: &[[f64; 2]]) -> Vec<Sample> {
        let mut pos = [0.0f64; 2];
        let mut samples = Vec::with_capacity(deltas.len() + 1);
        samples.push(Sample {
            t_ms: 0,
            pos,
            delta: [0.0, 0.0],
            speed: 0.0,
        });
        for (index, delta) in deltas.iter().enumerate() {
            pos = [pos[0] + delta[0], pos[1] + delta[1]];
            samples.push(Sample {
                t_ms: index + 1,
                pos,
                delta: *delta,
                speed: delta[0].hypot(delta[1]),
            });
        }
        samples
    }

    pub fn duration_ms(&self) -> usize {
        self.samples.last().map_or(0, |sample| sample.t_ms)
    }

    pub fn max_speed(&self) -> f64 {
        self.samples
            .iter()
            .map(|sample| sample.speed)
            .fold(0.0, f64::max)
    }

    /// Where the movement ended, relative to A.
    pub fn end(&self) -> [f64; 2] {
        self.samples.last().map_or([0.0, 0.0], |sample| sample.pos)
    }

    /// How far the end landed from the centre of C.
    pub fn miss(&self) -> f64 {
        let end = self.end();
        (self.target[0] - end[0]).hypot(self.target[1] - end[1])
    }

    pub fn hit(&self) -> bool {
        self.miss() <= self.radius
    }

    /// Is this sample part of the generated finish, rather than the observed
    /// approach?
    pub fn is_generated(&self, index: usize) -> bool {
        self.b_index.is_some_and(|b| index > b)
    }

    /// The bounding box of everything worth showing, in counts:
    /// `(min_x, min_y, max_x, max_y)`.
    pub fn bounds(&self) -> (f64, f64, f64, f64) {
        let mut min = [0.0f64, 0.0];
        let mut max = [0.0f64, 0.0];
        let mut include = |point: [f64; 2]| {
            min[0] = min[0].min(point[0]);
            min[1] = min[1].min(point[1]);
            max[0] = max[0].max(point[0]);
            max[1] = max[1].max(point[1]);
        };
        for sample in &self.samples {
            include(sample.pos);
        }
        // The target disc, not just its centre.
        include([self.target[0] - self.radius, self.target[1] - self.radius]);
        include([self.target[0] + self.radius, self.target[1] + self.radius]);
        (min[0], min[1], max[0], max[1])
    }

    /// The rows for the stats panel.
    pub fn summary(&self) -> Vec<(String, String)> {
        let mut rows = vec![
            ("duration".into(), format!("{} ms", self.duration_ms())),
            (
                "peak speed".into(),
                format!("{:.2} counts/ms", self.max_speed()),
            ),
            (
                "landed".into(),
                format!(
                    "{:.2} counts out ({})",
                    self.miss(),
                    if self.hit() { "hit" } else { "miss" }
                ),
            ),
        ];
        if self.timing.profile_us > 0 {
            rows.push((
                "renderer profile".into(),
                format!("{} µs", self.timing.profile_us),
            ));
        }
        if self.timing.plan_us > 0 {
            rows.push((
                "plan, 16 heads".into(),
                format!("{} µs", self.timing.plan_us),
            ));
        }
        if self.timing.render_us > 0 {
            rows.push(("render".into(), format!("{} µs", self.timing.render_us)));
        }
        rows
    }
}

// ---------------------------------------------------------------------------
// Geometry the client needs without abcurves
// ---------------------------------------------------------------------------

/// The midpoint of the eligible `|BC|` window, used as the mock's default stop.
///
/// This mirrors the seam floor abcurves enforces: `|BC| >= 8`, `|BC| > radius`,
/// `|BC| - radius >= 8` under the 90% policy, and edge progress at B at or
/// above the policy threshold. The server is still the authority — it asks
/// `Movement::eligible_remaining` and returns the real window — but the slider
/// needs a sensible default before the first round trip.
pub fn midpoint_bc(target_delta: [f64; 2], radius: f64) -> f64 {
    match approx_eligible_bc(target_delta, radius) {
        Some((low, high)) => 0.5 * (low + high),
        None => 10.0,
    }
}

/// A client-side estimate of the eligible window, for annotating the slider
/// before the server answers.
pub fn approx_eligible_bc(target_delta: [f64; 2], radius: f64) -> Option<(f64, f64)> {
    let distance = target_delta[0].hypot(target_delta[1]);
    if !distance.is_finite() || distance <= 1e-9 || !radius.is_finite() || radius <= 0.0 {
        return None;
    }
    // Policy::Auto tries the 90% recommendation, then the 80% contract, and
    // takes the union of whichever windows exist.
    let mut best: Option<(f64, f64)> = None;
    for (threshold, edge_margin, max_center) in [(0.9f64, 8.0f64, 0.92f64), (0.8, 0.0, 0.92)] {
        let lower = 8.0f64
            .max(radius + edge_margin.max(f64::EPSILON))
            .max((1.0 - max_center) * distance);
        let upper = distance - threshold * (distance - radius);
        if lower <= upper {
            best = Some(match best {
                Some((low, high)) => (low.min(lower), high.max(upper)),
                None => (lower, upper),
            });
        }
    }
    best
}

/// Maps counts (Y up, origin at A) onto SVG pixels (Y down, origin top-left).
#[derive(Clone, Copy, Debug)]
pub struct View {
    pub scale: f64,
    pub offset: [f64; 2],
}

impl View {
    /// Fit a trajectory into a box, leaving a margin, never magnifying so far
    /// that a short movement fills the screen with tremor.
    pub fn fit(trajectory: &Trajectory, width: f64, height: f64, margin: f64) -> Self {
        let (min_x, min_y, max_x, max_y) = trajectory.bounds();
        let span_x = (max_x - min_x).max(1.0);
        let span_y = (max_y - min_y).max(1.0);
        let scale = ((width - 2.0 * margin) / span_x)
            .min((height - 2.0 * margin) / span_y)
            .clamp(0.01, 24.0);
        let used = [span_x * scale, span_y * scale];
        let offset = [
            margin + (width - 2.0 * margin - used[0]) / 2.0 - min_x * scale,
            margin + (height - 2.0 * margin - used[1]) / 2.0 + max_y * scale,
        ];
        Self { scale, offset }
    }

    /// Counts to pixels. Y is negated: counts go up, SVG goes down.
    pub fn px(&self, point: [f64; 2]) -> [f64; 2] {
        [
            self.offset[0] + point[0] * self.scale,
            self.offset[1] - point[1] * self.scale,
        ]
    }

    pub fn len(&self, counts: f64) -> f64 {
        counts * self.scale
    }
}

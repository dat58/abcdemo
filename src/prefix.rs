//! Where the A->B movement comes from.
//!
//! abcurves' Static Planner continues a movement a hand has already begun. It
//! needs the reports that hand produced, one per closed millisecond, and it
//! needs to know where the target is. Both travel together through
//! [`PrefixSource`], because a recording that carries the movement but not the
//! target it was aimed at cannot be replayed.
//!
//! Two implementations ship: [`MockPrefix`], which synthesises a plausible
//! approach, and [`CsvPrefix`], which reads one off disk. A real 1 kHz capture
//! is the third, and is left to the caller.

use std::fmt;
use std::path::Path;

use abcurves::rng::Pcg64;

use crate::types::{DEFAULT_RADIUS, MAX_PREFIX_MS, MockParams};

/// One per-millisecond displacement delta, in native device counts.
///
/// X is right and **Y is up** — abcurves works in the device's own counts, not
/// in pixels, and not in screen coordinates. A capture that reports Y down has
/// to be negated before it gets here.
pub type Report = [f32; 2];

/// `prepare_renderer_profile` wants exactly this many reports.
pub const IDLE_WINDOW: usize = 256;

/// A source of 1 ms mouse reports describing an A->B movement already made.
///
/// A is always the origin. Everything here is a delta from it, so the demo
/// never carries an absolute screen coordinate: B is `sum(prefix())` and C is
/// [`target_delta`](PrefixSource::target_delta).
pub trait PrefixSource {
    /// Reports read from the mouse every 1 ms, oldest first.
    ///
    /// The last entry is B. Milliseconds in which the mouse did not move are
    /// `[0.0, 0.0]` entries and must be kept: an empty millisecond is real
    /// data, and dropping it shortens the movement the model sees.
    fn prefix(&self) -> Vec<Report>;

    /// C, as a displacement from A, in native counts.
    fn target_delta(&self) -> [f64; 2];

    /// The "counted as a hit" radius, in native counts.
    ///
    /// This is the on-screen target radius divided by pixels per count. The
    /// model reads it through `distance_over_radius`, `prefix_lateral / radius`
    /// and `speed_at_B / radius`, so it is what tells the planner how precise
    /// it has to be.
    fn radius(&self) -> f64 {
        DEFAULT_RADIUS
    }

    /// 256 quiet reports, from before the movement, for the renderer profile.
    ///
    /// This describes the *device*, not the movement, so the default is fine
    /// unless you have a real idle capture from the mouse you are imitating.
    fn idle_window(&self) -> Vec<[i16; 2]> {
        synthetic_idle_window(0x1d1e)
    }

    /// What to call this source in the UI.
    fn label(&self) -> String;
}

/// Format a count: whole numbers plain, anything else at full precision.
fn count(value: f32) -> String {
    if value.fract() == 0.0 {
        format!("{}", value as i64)
    } else {
        format!("{value:.6}")
    }
}

/// A hand resting on a 1 kHz mouse: mostly nothing, with occasional single
/// counts of sensor noise.
///
/// `RendererProfile::prepare` only constrains the length, so this is accepted.
/// It is not as good as a real capture — the renderer learns device texture
/// from this window — but it is honest about being synthetic.
pub fn synthetic_idle_window(seed: u64) -> Vec<[i16; 2]> {
    let mut rng = Rand::new(seed);
    (0..IDLE_WINDOW)
        .map(|_| {
            let mut tick = [0i16; 2];
            for axis in &mut tick {
                // ~8% of milliseconds carry a single count of jitter.
                if rng.unit() < 0.08 {
                    *axis = if rng.unit() < 0.5 { -1 } else { 1 };
                }
            }
            tick
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Random numbers
// ---------------------------------------------------------------------------

/// A thin float-shaped wrapper over abcurves' own PCG64.
///
/// Using the crate's generator keeps the dependency list at one entry and makes
/// every figure in the UI reproducible from the seed shown next to it.
pub struct Rand(Pcg64);

impl Rand {
    pub fn new(seed: u64) -> Self {
        Self(Pcg64::from_seed(u128::from(seed)))
    }

    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        // 53 bits is the whole mantissa; the low bits of a PCG64 draw are as
        // good as the high ones, so take them off the top and shift.
        (self.0.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in `[low, high)`.
    pub fn range(&mut self, low: f64, high: f64) -> f64 {
        low + self.unit() * (high - low)
    }

    /// Standard normal, via Box-Muller. One of the pair is discarded; at these
    /// sample counts the waste is irrelevant and the code stays obvious.
    pub fn normal(&mut self) -> f64 {
        let u1 = self.unit().max(f64::MIN_POSITIVE);
        let u2 = self.unit();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

// ---------------------------------------------------------------------------
// MockPrefix
// ---------------------------------------------------------------------------

/// A synthesised A->B approach.
///
/// The along-track profile is minimum-jerk, which is the standard model of a
/// human aimed movement: smooth acceleration, a single velocity peak near the
/// middle, smooth deceleration. It is monotone by construction, which also
/// keeps it clear of the seam's progress-regression rule.
#[derive(Clone, Debug)]
pub struct MockPrefix {
    pub params: MockParams,
}

impl MockPrefix {
    pub fn new(params: MockParams) -> Self {
        Self { params }
    }
}

impl PrefixSource for MockPrefix {
    fn prefix(&self) -> Vec<Report> {
        let MockParams {
            target_delta,
            duration_ms,
            stop_bc,
            curvature,
            jitter,
            seed,
            ..
        } = self.params;

        let distance = target_delta[0].hypot(target_delta[1]);
        let ticks = duration_ms.max(24);
        if distance <= 1e-9 {
            return vec![[0.0, 0.0]; ticks];
        }

        let along = [target_delta[0] / distance, target_delta[1] / distance];
        let across = [-along[1], along[0]];
        let bow = curvature * distance;
        let mut rng = Rand::new(seed);
        let phase = rng.range(-0.35, 0.35);
        let flip = if rng.unit() < 0.5 { -1.0 } else { 1.0 };

        // The hand is aiming at C, not at B. B is where the movement happened
        // to be when the library took over, so the prefix is this whole
        // movement cut short — and at the cut the hand is still travelling.
        // Decelerating to a stop at B instead would pin `speed_at_B` near
        // zero, and that is one of the features the planner reads hardest.
        let minimum_jerk = |t: f64| t * t * t * (10.0 - 15.0 * t + 6.0 * t * t);
        let point_at = |s: f64| {
            let lateral = flip * bow * (std::f64::consts::PI * (s + phase * s * (1.0 - s))).sin();
            [
                along[0] * s * distance + across[0] * lateral,
                along[1] * s * distance + across[1] * lateral,
            ]
        };

        // Cut once the distance left to C drops to `stop_bc`.
        let cut_progress = (1.0 - stop_bc / distance).clamp(0.0, 1.0);
        let mut cut = ticks;
        for tick in 1..=ticks {
            if minimum_jerk(tick as f64 / ticks as f64) >= cut_progress {
                cut = tick;
                break;
            }
        }
        // The seam wants at least two dozen milliseconds of evidence.
        let cut = cut.clamp(24, MAX_PREFIX_MS);

        // A mouse reports whole counts. The sensor has no sub-count output, so
        // every 1 ms bin is an integer pair and the fractional part of the
        // intended path has to go somewhere: it accumulates as debt and is
        // paid out as a whole count a tick or two later. That is the same
        // thing abcurves' own Renderer does on the way out, and it is what
        // gives a real capture its texture — runs of [0,1] with the
        // occasional [1,2] rather than a smooth ramp.
        //
        // Tremor perturbs the *position*, not the step. Adding it to each
        // delta instead would integrate into a random walk — at 0.35 counts
        // over 126 ticks that is a drift of several counts, which drags B
        // backwards and costs enough edge progress for the seam to refuse the
        // handoff. As a positional wobble it telescopes away instead, leaving
        // the endpoint where the movement aimed it.
        let mut previous = [0.0f64; 2];
        let mut debt = [0.0f64; 2];
        let mut reports: Vec<Report> = Vec::with_capacity(cut);
        for tick in 1..=cut {
            let s = minimum_jerk(tick as f64 / ticks as f64);
            let mut point = point_at(s);
            if jitter > 0.0 {
                point[0] += rng.normal() * jitter;
                point[1] += rng.normal() * jitter;
            }
            let want = [point[0] - previous[0], point[1] - previous[1]];
            previous = point;

            let mut emit = [0.0f32; 2];
            for axis in 0..2 {
                debt[axis] += want[axis];
                let whole = debt[axis].round();
                debt[axis] -= whole;
                emit[axis] = whole as f32;
            }
            reports.push(emit);
        }

        reports
    }

    fn target_delta(&self) -> [f64; 2] {
        self.params.target_delta
    }

    fn radius(&self) -> f64 {
        self.params.radius
    }

    fn idle_window(&self) -> Vec<[i16; 2]> {
        synthetic_idle_window(self.params.seed)
    }

    fn label(&self) -> String {
        format!("mock (seed {})", self.params.seed)
    }
}

// ---------------------------------------------------------------------------
// CsvPrefix
// ---------------------------------------------------------------------------

/// What went wrong reading a prefix CSV.
#[derive(Clone, Debug)]
pub enum CsvError {
    Io(String),
    /// A line that could not be read, with its 1-based number.
    Line(usize, String),
    NoTarget,
    EmptyPrefix,
    TooLong(usize),
}

impl fmt::Display for CsvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) => write!(f, "{message}"),
            Self::Line(line, message) => write!(f, "line {line}: {message}"),
            Self::NoTarget => write!(
                f,
                "no `target` row: the file has to say where C is, as a delta from A"
            ),
            Self::EmptyPrefix => write!(f, "no `report` rows: there is no movement to continue"),
            Self::TooLong(ticks) => write!(
                f,
                "A to B is {ticks} ms, over the {MAX_PREFIX_MS} ms the seam contract allows"
            ),
        }
    }
}

impl std::error::Error for CsvError {}

/// A recorded movement, loaded from disk.
///
/// The format is one typed row per line, so the file carries the target along
/// with the movement:
///
/// ```text
/// kind,x,y
/// target,140.0,90.0
/// radius,12.0,0
/// report,1.42,0.51
/// report,0.00,0.00
/// ```
///
/// Blank lines, `#` comments and a `kind,x,y` header are skipped. `radius` is
/// optional. `report` rows are per-closed-millisecond deltas in native counts,
/// in order, and `0,0` rows matter — see [`PrefixSource::prefix`].
#[derive(Clone, Debug)]
pub struct CsvPrefix {
    pub reports: Vec<Report>,
    pub target: [f64; 2],
    pub radius: f64,
    pub source: String,
}

impl CsvPrefix {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, CsvError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|error| CsvError::Io(format!("cannot read {}: {error}", path.display())))?;
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Self::parse(&text, name)
    }

    pub fn parse(text: &str, source: String) -> Result<Self, CsvError> {
        let mut reports = Vec::new();
        let mut target = None;
        let mut radius = None;

        for (index, raw) in text.lines().enumerate() {
            let number = index + 1;
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let mut fields = line.split(',').map(str::trim);
            let kind = fields.next().unwrap_or("").to_ascii_lowercase();
            if kind == "kind" {
                continue; // header
            }

            let number_at = |slot: Option<&str>, what: &str| -> Result<f64, CsvError> {
                let field = slot.unwrap_or("").trim();
                if field.is_empty() {
                    return Ok(0.0);
                }
                let value: f64 = field.parse().map_err(|_| {
                    CsvError::Line(number, format!("{what} is not a number: `{field}`"))
                })?;
                if !value.is_finite() {
                    return Err(CsvError::Line(number, format!("{what} is not finite")));
                }
                Ok(value)
            };

            let x = number_at(fields.next(), "x")?;
            let y = number_at(fields.next(), "y")?;

            match kind.as_str() {
                "target" => target = Some([x, y]),
                "radius" => radius = Some(x),
                "report" => reports.push([x as f32, y as f32]),
                other => {
                    return Err(CsvError::Line(
                        number,
                        format!("unknown row kind `{other}`; expected target, radius or report"),
                    ));
                }
            }
        }

        let target = target.ok_or(CsvError::NoTarget)?;
        if reports.is_empty() {
            return Err(CsvError::EmptyPrefix);
        }
        if reports.len() > MAX_PREFIX_MS {
            return Err(CsvError::TooLong(reports.len()));
        }
        let radius = radius.filter(|value| *value > 0.0).unwrap_or(DEFAULT_RADIUS);
        Ok(Self {
            reports,
            target,
            radius,
            source,
        })
    }

    /// Render a source back out in the same format, so a mock run can be saved
    /// and reloaded, and so there is a template to record against.
    ///
    pub fn render(source: &dyn PrefixSource) -> String {
        let target = source.target_delta();
        let mut out = String::from("# abcdemo prefix capture\n# C and the radius are deltas in native counts, X right / Y up.\nkind,x,y\n");
        out.push_str(&format!("target,{:.6},{:.6}\n", target[0], target[1]));
        out.push_str(&format!("radius,{:.6},0\n", source.radius()));
        out.push_str("# one row per closed millisecond, including the still ones\n");
        for report in source.prefix() {
            // Whole counts are written as whole numbers: that is what a mouse
            // reports, and a recording should look like what it recorded.
            out.push_str(&format!(
                "report,{},{}\n",
                count(report[0]),
                count(report[1])
            ));
        }
        out
    }
}

impl PrefixSource for CsvPrefix {
    fn prefix(&self) -> Vec<Report> {
        self.reports.clone()
    }

    fn target_delta(&self) -> [f64; 2] {
        self.target
    }

    fn radius(&self) -> f64 {
        self.radius
    }

    fn label(&self) -> String {
        format!("{} ({} ms)", self.source, self.reports.len())
    }
}

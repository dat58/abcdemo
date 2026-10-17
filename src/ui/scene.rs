//! The path itself: A, B, C, and every millisecond between them.

use dioxus::prelude::*;

use crate::types::{Trajectory, View};

pub const WIDTH: f64 = 760.0;
pub const HEIGHT: f64 = 440.0;

/// Blue through green to amber: slow to fast.
///
/// A sequential ramp, so "further along the ramp" reads as "faster" without a
/// legend, and the two ends stay distinguishable in both themes.
pub fn heat_colour(heat: f64) -> String {
    let stops = [
        (0.00, [56.0, 82.0, 148.0]),
        (0.35, [38.0, 139.0, 158.0]),
        (0.65, [86.0, 180.0, 110.0]),
        (0.85, [214.0, 178.0, 70.0]),
        (1.00, [224.0, 118.0, 62.0]),
    ];
    let heat = heat.clamp(0.0, 1.0);
    let mut lower = stops[0];
    let mut upper = stops[stops.len() - 1];
    for pair in stops.windows(2) {
        if heat >= pair[0].0 && heat <= pair[1].0 {
            lower = pair[0];
            upper = pair[1];
            break;
        }
    }
    let span = (upper.0 - lower.0).max(1e-9);
    let t = (heat - lower.0) / span;
    let mix = |a: f64, b: f64| (a + (b - a) * t).round() as u8;
    format!(
        "rgb({},{},{})",
        mix(lower.1[0], upper.1[0]),
        mix(lower.1[1], upper.1[1]),
        mix(lower.1[2], upper.1[2])
    )
}

/// The movement itself, and the markers around it.
///
/// Split out from the playhead so it is memoised on the trajectory alone:
/// otherwise every one of its several hundred strokes would be diffed sixty
/// times a second while the animation runs, to redraw one moving dot.
#[component]
fn PathLayer(trajectory: ReadSignal<Trajectory>) -> Element {
    let path = trajectory.read();
    let view = View::fit(&path, WIDTH, HEIGHT, 48.0);
    let max_speed = path.max_speed();

    let target_px = view.px(path.target);
    let target_r = view.len(path.radius).max(3.0);
    let a_px = view.px([0.0, 0.0]);

    // One stroke per millisecond, coloured by how fast that millisecond was.
    // A few hundred elements is nothing, and it means the speed is legible
    // without playing the animation.
    let segments = path.samples.windows(2).enumerate().map(|(index, pair)| {
        let from = view.px(pair[0].pos);
        let to = view.px(pair[1].pos);
        let generated = path.is_generated(index + 1);
        let colour = heat_colour(pair[1].heat(max_speed));
        rsx! {
            line {
                key: "{index}",
                x1: "{from[0]:.2}", y1: "{from[1]:.2}",
                x2: "{to[0]:.2}", y2: "{to[1]:.2}",
                stroke: "{colour}",
                // The generated finish is drawn heavier than the observed
                // approach, so the handoff reads without hunting for the mark.
                stroke_width: if generated { "4.0" } else { "2.0" },
                stroke_linecap: "round",
                opacity: if generated { "1.0" } else { "0.75" },
            }
        }
    });

    let b_marker = path.b_index.and_then(|b| path.samples.get(b)).map(|sample| {
        let point = view.px(sample.pos);
        rsx! {
            circle {
                cx: "{point[0]:.2}", cy: "{point[1]:.2}", r: "5",
                fill: "none", stroke: "var(--seam)", stroke_width: "2",
            }
            text {
                x: "{point[0] + 10.0:.2}", y: "{point[1] - 8.0:.2}",
                class: "marker", "B"
            }
        }
    });

    rsx! {
        g {
            // The target: the hit disc, then its edge.
            circle {
                cx: "{target_px[0]:.2}", cy: "{target_px[1]:.2}", r: "{target_r:.2}",
                fill: "var(--target-fill)", stroke: "var(--target)", stroke_width: "1.5",
            }
            circle {
                cx: "{target_px[0]:.2}", cy: "{target_px[1]:.2}", r: "2",
                fill: "var(--target)",
            }
            text {
                x: "{target_px[0] + target_r + 8.0:.2}", y: "{target_px[1] + 4.0:.2}",
                class: "marker", "C"
            }

            // A.
            circle { cx: "{a_px[0]:.2}", cy: "{a_px[1]:.2}", r: "4", fill: "var(--origin)" }
            text {
                x: "{a_px[0] + 10.0:.2}", y: "{a_px[1] + 16.0:.2}",
                class: "marker", "A"
            }

            {segments}
            {b_marker}
        }
    }
}

/// The dot and the short tail behind it. This is all that moves.
#[component]
fn Playhead(trajectory: ReadSignal<Trajectory>, cursor: ReadSignal<f64>) -> Element {
    let path = trajectory.read();
    let view = View::fit(&path, WIDTH, HEIGHT, 48.0);

    let index = (cursor() as usize).min(path.samples.len().saturating_sub(1));
    let head = path.samples.get(index).map(|sample| view.px(sample.pos));
    let tail_start = index.saturating_sub(28);
    let tail: String = path.samples[tail_start..=index]
        .iter()
        .map(|sample| {
            let point = view.px(sample.pos);
            format!("{:.2},{:.2}", point[0], point[1])
        })
        .collect::<Vec<_>>()
        .join(" ");

    rsx! {
        g {
            polyline {
                points: "{tail}",
                fill: "none",
                stroke: "var(--cursor)",
                stroke_width: "2",
                opacity: "0.5",
                stroke_linecap: "round",
            }
            if let Some(point) = head {
                circle {
                    cx: "{point[0]:.2}", cy: "{point[1]:.2}", r: "6",
                    fill: "var(--cursor)", stroke: "var(--bg)", stroke_width: "2",
                }
            }
        }
    }
}

#[component]
pub fn Scene(trajectory: ReadSignal<Trajectory>, cursor: ReadSignal<f64>) -> Element {
    rsx! {
        svg {
            class: "scene",
            view_box: "0 0 {WIDTH} {HEIGHT}",
            width: "100%",
            PathLayer { trajectory }
            Playhead { trajectory, cursor }
        }
    }
}

/// The colour ramp, with the speeds it stands for.
#[component]
pub fn SpeedLegend(max_speed: f64) -> Element {
    let swatches = (0..=10).map(|step| {
        let heat = f64::from(step) / 10.0;
        let colour = heat_colour(heat);
        rsx! { span { key: "{step}", class: "swatch", background_color: "{colour}" } }
    });
    rsx! {
        div { class: "legend",
            span { class: "legend-label", "0" }
            {swatches}
            span { class: "legend-label", "{max_speed:.1} counts/ms" }
        }
    }
}

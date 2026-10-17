//! Speed against time, with the handoff marked.
//!
//! The scene shows where the hand went; this shows the shape of the movement —
//! the single velocity peak of an aimed approach, and then whatever the
//! planner chose to do with what was left.

use dioxus::prelude::*;

use crate::types::Trajectory;
use crate::ui::scene::heat_colour;

const WIDTH: f64 = 760.0;
const HEIGHT: f64 = 170.0;
const PAD_L: f64 = 44.0;
const PAD_R: f64 = 12.0;
const PAD_T: f64 = 12.0;
const PAD_B: f64 = 26.0;

/// Everything except the playhead, memoised on the trajectory alone so the
/// per-millisecond line is not re-diffed on every animation frame.
#[component]
fn ChartLayer(trajectory: ReadSignal<Trajectory>) -> Element {
    let path = trajectory.read();
    let duration = path.duration_ms().max(1) as f64;
    let max_speed = path.max_speed().max(1e-6);

    let plot_w = WIDTH - PAD_L - PAD_R;
    let plot_h = HEIGHT - PAD_T - PAD_B;
    let x_of = move |t_ms: f64| PAD_L + (t_ms / duration) * plot_w;
    let y_of = move |speed: f64| PAD_T + plot_h - (speed / max_speed) * plot_h;

    // Filled area, so the still milliseconds read as a floor rather than a gap.
    let area: String = {
        let mut points = vec![format!("{:.2},{:.2}", x_of(0.0), y_of(0.0))];
        points.extend(path.samples.iter().map(|sample| {
            format!("{:.2},{:.2}", x_of(sample.t_ms as f64), y_of(sample.speed))
        }));
        points.push(format!("{:.2},{:.2}", x_of(duration), y_of(0.0)));
        points.join(" ")
    };

    // The line, coloured per segment with the same ramp as the scene, so the
    // two views agree about what "fast" looks like.
    let line = path.samples.windows(2).enumerate().map(|(index, pair)| {
        let colour = heat_colour(pair[1].heat(max_speed));
        rsx! {
            line {
                key: "{index}",
                x1: "{x_of(pair[0].t_ms as f64):.2}", y1: "{y_of(pair[0].speed):.2}",
                x2: "{x_of(pair[1].t_ms as f64):.2}", y2: "{y_of(pair[1].speed):.2}",
                stroke: "{colour}", stroke_width: "2", stroke_linecap: "round",
            }
        }
    });

    let b_rule = path.b_index.map(|b| {
        let x = x_of(b as f64);
        rsx! {
            line {
                x1: "{x:.2}", y1: "{PAD_T}", x2: "{x:.2}", y2: "{PAD_T + plot_h}",
                stroke: "var(--seam)", stroke_width: "1.5", stroke_dasharray: "4 3",
            }
            text { x: "{x + 5.0:.2}", y: "{PAD_T + 11.0}", class: "marker", "B" }
        }
    });

    // Three gridlines is enough to read a value off; more is clutter.
    let grid = (0..=2).map(|step| {
        let fraction = f64::from(step) / 2.0;
        let speed = max_speed * fraction;
        let y = y_of(speed);
        rsx! {
            g { key: "{step}",
                line {
                    x1: "{PAD_L}", y1: "{y:.2}", x2: "{WIDTH - PAD_R}", y2: "{y:.2}",
                    stroke: "var(--grid)", stroke_width: "1",
                }
                text { x: "{PAD_L - 8.0}", y: "{y + 4.0:.2}", class: "tick", "{speed:.1}" }
            }
        }
    });

    rsx! {
        g {
            {grid}
            polygon { points: "{area}", fill: "var(--area)" }
            {line}
            {b_rule}
            text {
                x: "{PAD_L}", y: "{HEIGHT - 8.0}", class: "tick", "0 ms"
            }
            text {
                x: "{WIDTH - PAD_R}", y: "{HEIGHT - 8.0}",
                class: "tick", text_anchor: "end", "{duration:.0} ms"
            }
            text {
                x: "{PAD_L - 8.0}", y: "{PAD_T - 2.0}", class: "tick",
                text_anchor: "end", "counts/ms"
            }
        }
    }
}

/// The vertical rule that tracks playback.
#[component]
fn Playhead(trajectory: ReadSignal<Trajectory>, cursor: ReadSignal<f64>) -> Element {
    let duration = trajectory.read().duration_ms().max(1) as f64;
    let plot_w = WIDTH - PAD_L - PAD_R;
    let x = PAD_L + (cursor().clamp(0.0, duration) / duration) * plot_w;
    rsx! {
        line {
            x1: "{x:.2}", y1: "{PAD_T}",
            x2: "{x:.2}", y2: "{HEIGHT - PAD_B}",
            stroke: "var(--cursor)", stroke_width: "1.5",
        }
    }
}

#[component]
pub fn SpeedChart(trajectory: ReadSignal<Trajectory>, cursor: ReadSignal<f64>) -> Element {
    rsx! {
        svg { class: "chart", view_box: "0 0 {WIDTH} {HEIGHT}", width: "100%",
            ChartLayer { trajectory }
            Playhead { trajectory, cursor }
        }
    }
}

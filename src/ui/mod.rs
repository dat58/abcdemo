//! The browser half.

pub mod controls;
pub mod scene;
pub mod speed;

use std::time::Duration;

use dioxus::prelude::*;

use crate::types::{Outcome, Refusal, Request, SourceSpec, Trajectory};

const CSS: &str = include_str!("../../assets/app.css");

/// Frame period for the playback ticker. The DOM cannot be redrawn at 1 kHz,
/// so the cursor advances by elapsed wall-clock milliseconds instead: a 384 ms
/// movement still takes 384 ms on screen at 1x, drawn over ~23 frames.
const FRAME: Duration = Duration::from_millis(16);

/// Roll a C far enough out that the eligible |BC| window is comfortably
/// non-empty. Mirrors `MockPrefix::random_target`, which the server uses.
pub fn random_target(seed: u64, radius: f64) -> [f64; 2] {
    // A small splitmix step, so this is reproducible and needs no dependency.
    let mut z = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut next = move || {
        z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut x = z;
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        ((x ^ (x >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    };
    let angle = next() * std::f64::consts::TAU;
    let distance = 6.0 * radius + 60.0 + next() * (12.0 * radius + 100.0);
    [distance * angle.cos(), distance * angle.sin()]
}

#[component]
pub fn App() -> Element {
    let mut request = use_signal(Request::default);
    let csv_path = use_signal(|| "sample_prefix.csv".to_string());
    let mut trajectory = use_signal(|| None::<Trajectory>);
    let mut refusal = use_signal(|| None::<Refusal>);
    let mut status = use_signal(|| None::<String>);
    let mut busy = use_signal(|| false);

    // Playback.
    let mut cursor = use_signal(|| 0.0f64);
    let mut playing = use_signal(|| false);
    let mut rate = use_signal(|| 1.0f64);

    let run = move |_: ()| async move {
        busy.set(true);
        status.set(None);
        let outcome = crate::generate(request.peek().clone()).await;
        busy.set(false);
        match outcome {
            Ok(Outcome::Planned(path)) => {
                cursor.set(0.0);
                refusal.set(None);
                trajectory.set(Some(*path));
                playing.set(true);
            }
            Ok(Outcome::Refused(reason)) => {
                trajectory.set(None);
                refusal.set(Some(reason));
                playing.set(false);
            }
            Err(error) => {
                trajectory.set(None);
                refusal.set(Some(Refusal::plain(format!("the server call failed: {error}"))));
                playing.set(false);
            }
        }
    };

    // Draw something on arrival rather than an empty frame.
    use_future(move || async move { run(()).await });

    // The playback ticker. Signals it both reads and writes are read with
    // peek(), so the future never restarts itself.
    use_future(move || async move {
        loop {
            gloo_sleep(FRAME).await;
            if !*playing.peek() {
                continue;
            }
            let Some(total) = trajectory.peek().as_ref().map(|path| path.duration_ms() as f64)
            else {
                continue;
            };
            let next = *cursor.peek() + FRAME.as_secs_f64() * 1000.0 * *rate.peek();
            if next >= total {
                cursor.set(total);
                playing.set(false);
            } else {
                cursor.set(next);
            }
        }
    });

    let duration = trajectory
        .read()
        .as_ref()
        .map_or(0.0, |path| path.duration_ms() as f64);

    rsx! {
        document::Title { "abcurves — human mouse movement" }
        style { dangerous_inner_html: CSS }
        div { class: "app",
            header {
                h1 { "abcurves" }
                p {
                    "Human mouse movement, one millisecond at a time. "
                    span { class: "dim",
                        "The Static Planner continues a movement a hand already began; \
                         the Continuous Planner drives one from standstill."
                    }
                }
            }

            div { class: "layout",
                controls::Controls {
                    request,
                    csv_path,
                    status,
                    on_run: move |_| { spawn(run(())); },
                    on_export: move |_| {
                        spawn(async move {
                            let path = csv_path.peek().clone();
                            let current = request.peek().clone();
                            match crate::export_csv(current, path.clone()).await {
                                Ok(message) => {
                                    status.set(Some(message));
                                    request.write().source = SourceSpec::Csv { path };
                                }
                                Err(error) => status.set(Some(format!("export failed: {error}"))),
                            }
                        });
                    },
                }

                main { class: "stage",
                    if busy() && trajectory.read().is_none() {
                        div { class: "placeholder", "loading the planner…" }
                    }

                    if let Some(reason) = refusal() {
                        div { class: "refusal",
                            h3 { "abcurves refused this handoff" }
                            p { class: "refusal-message", "{reason.message}" }
                            ul {
                                for (index, line) in reason.detail.iter().enumerate() {
                                    li { key: "{index}", "{line}" }
                                }
                            }
                            p { class: "note",
                                "This is the library validating B rather than inventing one. \
                                 A→B already physically happened, so it cannot be adjusted \
                                 after the fact."
                            }
                        }
                    }

                    if let Some(path) = trajectory() {
                        scene::Scene {
                            trajectory: path.clone(),
                            cursor: cursor(),
                        }
                        scene::SpeedLegend { max_speed: path.max_speed() }

                        div { class: "transport",
                            button {
                                class: "primary",
                                onclick: move |_| {
                                    if cursor() >= duration { cursor.set(0.0); }
                                    let next = !playing();
                                    playing.set(next);
                                },
                                if playing() { "Pause" } else { "Play" }
                            }
                            input {
                                class: "scrub",
                                r#type: "range",
                                min: "0", max: "{duration}", step: "1",
                                value: "{cursor()}",
                                oninput: move |event| {
                                    if let Ok(value) = event.value().parse::<f64>() {
                                        cursor.set(value);
                                        playing.set(false);
                                    }
                                },
                            }
                            span { class: "readout", "{cursor():.0} / {duration:.0} ms" }
                            div { class: "rates",
                                for speed in [0.25f64, 0.5, 1.0, 2.0, 4.0] {
                                    button {
                                        key: "{speed}",
                                        class: if (rate() - speed).abs() < 1e-9 { "rate active" } else { "rate" },
                                        onclick: move |_| rate.set(speed),
                                        "{speed}×"
                                    }
                                }
                            }
                        }
                        p { class: "caption",
                            "Playback runs at wall-clock speed: one report per millisecond, \
                             drawn at about 60 frames a second."
                        }

                        speed::SpeedChart { trajectory: path.clone(), cursor: cursor() }

                        Stats {
                            trajectory: path,
                            request,
                            on_run: move |_| { spawn(run(())); },
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn Stats(
    trajectory: ReadSignal<Trajectory>,
    request: Signal<Request>,
    on_run: EventHandler<()>,
) -> Element {
    let path = trajectory.read();
    let drawn = path.head;
    let pinned = request().settings.head;
    // The longest finish sets the bar scale, so the sixteen are comparable.
    let longest = path
        .heads
        .iter()
        .map(|option| option.duration_ms)
        .max()
        .unwrap_or(1)
        .max(1);

    rsx! {
        div { class: "stats",
            section {
                h3 { "This movement" }
                p { class: "dim", "{path.source_label}" }
                dl {
                    for (key, value) in path.summary() {
                        div { key: "{key}",
                            dt { "{key}" }
                            dd { "{value}" }
                        }
                    }
                }
                ul {
                    for (index, note) in path.notes.iter().enumerate() {
                        li { key: "{index}", "{note}" }
                    }
                }
            }

            if !path.heads.is_empty() {
                section {
                    h3 { "The sixteen finishes" }
                    p { class: "note",
                        "The model learned sixteen ways to finish a movement. They all \
                         start at the same B and reach the same target, and differ in how \
                         long they dwell on the way in. Pick one to render it — A→B does \
                         not move, because it already happened."
                    }
                    div { class: "heads",
                        button {
                            class: if pinned.is_none() { "head auto on" } else { "head auto" },
                            onclick: move |_| {
                                request.write().settings.head = None;
                                on_run.call(());
                            },
                            title: "let the planner seed draw a head",
                            "auto"
                        }
                        for option in path.heads.iter() {
                            button {
                                key: "{option.head}",
                                class: if Some(option.head) == drawn { "head on" } else { "head" },
                                title: "head {option.head}: {option.duration_ms} ms of finish",
                                onclick: {
                                    let head = option.head;
                                    move |_| {
                                        request.write().settings.head = Some(head);
                                        on_run.call(());
                                    }
                                },
                                span {
                                    class: "head-bar",
                                    // A style, not a `width` attribute: on a
                                    // span that attribute does nothing.
                                    style: "width:{(option.duration_ms * 100 / longest).max(4)}%",
                                }
                                span { class: "head-n", "{option.head}" }
                                span { class: "head-ms", "{option.duration_ms}" }
                            }
                        }
                    }
                    p { class: "dim",
                        if let Some(head) = pinned {
                            "Head {head} is pinned, so the seed no longer chooses. "
                        } else {
                            "No head is pinned, so planner seed {request().settings.planner_seed} draws one. "
                        }
                        "Always taking the shortest would replace the model's distribution \
                         with its own tail, which is itself a signature."
                    }
                }
            }
        }
    }
}

/// Sleep, in the browser or on the server-rendered pass.
async fn gloo_sleep(duration: Duration) {
    #[cfg(target_arch = "wasm32")]
    gloo_timers::future::sleep(duration).await;
    #[cfg(not(target_arch = "wasm32"))]
    tokio::time::sleep(duration).await;
}

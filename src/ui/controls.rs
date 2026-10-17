//! The parameter panel.

use dioxus::prelude::*;

use crate::types::{
    COMMON_RADIANS_PER_COUNT, MAX_PREFIX_MS, Mode, MockParams, Request, SourceSpec,
    approx_eligible_bc, midpoint_bc,
};

/// A labelled number box that only commits values it can parse.
#[component]
fn Num(
    label: String,
    value: f64,
    step: f64,
    #[props(default = None)] hint: Option<String>,
    on_change: EventHandler<f64>,
) -> Element {
    rsx! {
        label { class: "field",
            span { class: "field-label", "{label}" }
            input {
                r#type: "number",
                step: "{step}",
                value: "{value}",
                oninput: move |event| {
                    // Ignore anything half-typed, so the path does not flicker
                    // while a number is being edited.
                    if let Ok(parsed) = event.value().parse::<f64>()
                        && parsed.is_finite()
                    {
                        on_change.call(parsed);
                    }
                },
            }
            if let Some(hint) = hint {
                span { class: "field-hint", "{hint}" }
            }
        }
    }
}

/// A slider with its value shown, for the things worth scrubbing.
#[component]
fn Slide(
    label: String,
    value: f64,
    min: f64,
    max: f64,
    step: f64,
    #[props(default = None)] hint: Option<String>,
    #[props(default = false)] warn: bool,
    on_change: EventHandler<f64>,
) -> Element {
    rsx! {
        label { class: if warn { "field slider warn" } else { "field slider" },
            span { class: "field-label",
                "{label}"
                span { class: "field-value", "{value:.2}" }
            }
            input {
                r#type: "range",
                min: "{min}", max: "{max}", step: "{step}",
                value: "{value}",
                oninput: move |event| {
                    if let Ok(parsed) = event.value().parse::<f64>() {
                        on_change.call(parsed);
                    }
                },
            }
            if let Some(hint) = hint {
                span { class: "field-hint", "{hint}" }
            }
        }
    }
}

#[component]
pub fn Controls(
    request: Signal<Request>,
    csv_path: Signal<String>,
    status: Signal<Option<String>>,
    on_run: EventHandler<()>,
    on_export: EventHandler<()>,
) -> Element {
    let current = request();
    let mode = current.settings.mode;
    let is_mock = matches!(current.source, SourceSpec::Mock(_));

    let mock = match &current.source {
        SourceSpec::Mock(params) => Some(*params),
        SourceSpec::Csv { .. } => None,
    };

    rsx! {
        div { class: "controls",

            section { class: "group",
                h2 { "Planner" }
                div { class: "tabs",
                    for option in [Mode::Static, Mode::Continuous] {
                        button {
                            key: "{option:?}",
                            class: if mode == option { "tab active" } else { "tab" },
                            onclick: move |_| request.write().settings.mode = option,
                            "{option.label()}"
                        }
                    }
                }
                p { class: "note",
                    match mode {
                        Mode::Static => "Continues a movement a hand already began. \
                            A→B is observed and cannot be chosen after the fact; \
                            B→C is generated.",
                        Mode::Continuous => "Drives to the target from standstill and \
                            settles. There is no handoff, so the prefix is unused here — \
                            that contrast is the point.",
                    }
                }
            }

            section { class: "group",
                h2 { "Prefix source" }
                div { class: "tabs",
                    button {
                        class: if is_mock { "tab active" } else { "tab" },
                        onclick: move |_| {
                            request.write().source = SourceSpec::Mock(MockParams::default());
                        },
                        "Mock"
                    }
                    button {
                        class: if is_mock { "tab" } else { "tab active" },
                        onclick: move |_| {
                            request.write().source = SourceSpec::Csv { path: csv_path() };
                        },
                        "CSV"
                    }
                }

                if let Some(params) = mock {
                    MockFields { request, params }
                } else {
                    label { class: "field",
                        span { class: "field-label", "CSV file name" }
                        input {
                            r#type: "text",
                            value: "{csv_path}",
                            oninput: move |event| {
                                let path = event.value();
                                csv_path.set(path.clone());
                                request.write().source = SourceSpec::Csv { path };
                            },
                        }
                        span { class: "field-hint",
                            "a plain name in the server's data directory. Rows of \
                             target,x,y · radius,r,0 · report,dx,dy — one report per \
                             closed millisecond, zeros included"
                        }
                    }
                }
            }

            section { class: "group",
                h2 { "Draw" }
                Num {
                    label: "planner seed".to_string(),
                    value: current.settings.planner_seed as f64,
                    step: 1.0,
                    hint: Some("picks which of the sixteen finishing styles".to_string()),
                    on_change: move |value: f64| {
                        request.write().settings.planner_seed = value.max(0.0) as u64;
                    },
                }
                Num {
                    label: "renderer seed".to_string(),
                    value: current.settings.renderer_seed as f64,
                    step: 1.0,
                    hint: Some("picks the device texture laid over it".to_string()),
                    on_change: move |value: f64| {
                        request.write().settings.renderer_seed = value.max(0.0) as u64;
                    },
                }
                label { class: "field",
                    span { class: "field-label", "planner cell" }
                    select {
                        value: "{current.settings.model_seed}",
                        onchange: move |event| {
                            if let Ok(seed) = event.value().parse::<u32>() {
                                request.write().settings.model_seed = seed;
                            }
                        },
                        option { value: "7", "seed 7" }
                        option { value: "23", "seed 23" }
                    }
                    span { class: "field-hint", "two independently trained cells" }
                }

                if mode == Mode::Continuous {
                    Num {
                        label: "radians per count".to_string(),
                        value: current.settings.radians_per_count,
                        step: 0.00001,
                        hint: Some(format!("device sensitivity; reference is {COMMON_RADIANS_PER_COUNT:.7}")),
                        on_change: move |value: f64| {
                            if value > 0.0 {
                                request.write().settings.radians_per_count = value;
                            }
                        },
                    }
                    label { class: "field checkbox",
                        input {
                            r#type: "checkbox",
                            checked: current.settings.move_target_at_ms.is_some(),
                            onchange: move |event| {
                                request.write().settings.move_target_at_ms =
                                    event.checked().then_some(160);
                            },
                        }
                        span { "move C mid-flight at 160 ms" }
                    }
                }
            }

            div { class: "actions",
                button { class: "primary", onclick: move |_| on_run.call(()), "Regenerate" }
                button {
                    onclick: move |_| {
                        let mut write = request.write();
                        if let SourceSpec::Mock(params) = &mut write.source {
                            params.seed = params.seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407) >> 16;
                        }
                        drop(write);
                        on_run.call(());
                    },
                    "New seed"
                }
                button { onclick: move |_| on_export.call(()), disabled: !is_mock, "Export CSV" }
            }

            if let Some(message) = status() {
                p { class: "status", "{message}" }
            }
        }
    }
}

/// The mock's own knobs, split out to keep the panel readable.
#[component]
fn MockFields(request: Signal<Request>, params: MockParams) -> Element {
    let window = approx_eligible_bc(params.target_delta, params.radius);
    let high = window.map_or(60.0, |(_, high)| high);
    let eligible = window.is_some_and(|(low, high)| params.stop_bc >= low && params.stop_bc <= high);
    let distance = params.target_delta[0].hypot(params.target_delta[1]);

    rsx! {
        div { class: "pair",
            Num {
                label: "C · dx".to_string(), value: params.target_delta[0], step: 5.0,
                on_change: move |value| edit(request, |p, v| {
                    p.target_delta[0] = v;
                    p.stop_bc = midpoint_bc(p.target_delta, p.radius);
                }, value),
            }
            Num {
                label: "C · dy".to_string(), value: params.target_delta[1], step: 5.0,
                on_change: move |value| edit(request, |p, v| {
                    p.target_delta[1] = v;
                    p.stop_bc = midpoint_bc(p.target_delta, p.radius);
                }, value),
            }
        }
        p { class: "note", "C is a delta from A, in native counts. |AC| = {distance:.1}." }
        button {
            class: "wide",
            onclick: move |_| edit(request, |p, _| {
                let target = crate::ui::random_target(p.seed, p.radius);
                p.target_delta = target;
                p.stop_bc = midpoint_bc(target, p.radius);
                // Move the seed on too, so the button keeps giving new answers.
                p.seed = p.seed.wrapping_mul(6364136223846793005).wrapping_add(1) >> 8;
            }, 0.0),
            "Randomise C"
        }

        Num {
            label: "radius".to_string(), value: params.radius, step: 1.0,
            hint: Some("hit radius in counts: on-screen radius ÷ pixels per count".to_string()),
            on_change: move |value| edit(request, |p, v| {
                if v > 0.0 {
                    p.radius = v;
                    p.stop_bc = midpoint_bc(p.target_delta, p.radius);
                }
            }, value),
        }
        Slide {
            label: "A→B duration (ms)".to_string(),
            value: params.duration_ms as f64,
            min: 24.0, max: 600.0, step: 1.0,
            hint: Some(format!("the seam refuses an A→B over {MAX_PREFIX_MS} ms")),
            on_change: move |value| edit(request, |p, v| p.duration_ms = v as usize, value),
        }
        Slide {
            label: "|BC| at the handoff".to_string(),
            value: params.stop_bc,
            min: 0.0,
            max: (high * 1.6).max(40.0),
            step: 0.25,
            warn: !eligible,
            hint: Some(match window {
                Some((low, high)) => format!(
                    "eligible between {low:.2} and {high:.2} counts{}",
                    if eligible { "" } else { " — this one will be refused" }
                ),
                None => "no |BC| is eligible: C is too close to A for a handoff".to_string(),
            }),
            on_change: move |value| edit(request, |p, v| p.stop_bc = v, value),
        }
        Slide {
            label: "curvature".to_string(), value: params.curvature,
            min: 0.0, max: 0.3, step: 0.005,
            hint: Some("lateral bow, as a fraction of |AC|".to_string()),
            on_change: move |value| edit(request, |p, v| p.curvature = v, value),
        }
        Slide {
            label: "jitter".to_string(), value: params.jitter,
            min: 0.0, max: 2.0, step: 0.05,
            hint: Some("per-millisecond tremor, in counts".to_string()),
            on_change: move |value| edit(request, |p, v| p.jitter = v, value),
        }
        Num {
            label: "mock seed".to_string(), value: params.seed as f64, step: 1.0,
            on_change: move |value| edit(request, |p, v| p.seed = v.max(0.0) as u64, value),
        }
    }
}

/// One place that knows how to reach into the mock parameters.
///
/// A free function rather than a closure: `Signal::write` takes `&mut self`,
/// which would make a closure `FnMut` and so unusable from the `Fn` event
/// handlers. `Signal` is `Copy`, so passing it costs nothing.
fn edit(mut request: Signal<Request>, update: fn(&mut MockParams, f64), value: f64) {
    let mut write = request.write();
    if let SourceSpec::Mock(params) = &mut write.source {
        update(params, value);
    }
}

# abcdemo

A demo of [abcurves-rs](https://github.com/dat58/abcurves-rs): human mouse movement,
generated one millisecond at a time.

The crate has two planners, and this shows both:

- **Static (A→B→C)** continues a movement a hand has *already begun*. You give it A (where
  the movement started), C (the target), the hit radius, and the reports observed so far.
  B is the last report supplied — A→B physically happened, so the library validates that
  point rather than choosing its own — and it generates the B→C finish.
- **Continuous** drives to a target from standstill and settles on it, and can be told the
  target moved mid-flight.

Movement is drawn as a path from A to C with every millisecond coloured by speed, a
speed-against-time chart, and playback at wall-clock speed.

## Running it

```bash
dx serve --platform web --port 8080      # then open http://localhost:8080
```

Needs the [Dioxus CLI](https://dioxuslabs.com/learn/0.7/getting_started) at the same
version as the `dioxus` dependency (`dx --version` must match `0.7.6`), and the
`wasm32-unknown-unknown` target.

abcurves runs on the server, not in the browser: it loads ~1.5 MB of weights from disk,
which the wasm build cannot do. The browser only ever receives a finished trajectory. The
models resolve themselves from the sibling `../abcurves-rs/models` checkout, so there is
nothing to copy or configure — set `ABCURVES_MODEL_DIR` if you keep them elsewhere.

```bash
cargo test --features server              # the engine and both views
```

## In Docker

The crate depends on `../abcurves-rs` by path, so **the build context is the parent
directory**:

```bash
cd ..                                              # the directory holding both repos
docker build -f abcdemo/Dockerfile -t abcdemo .
docker run --rm -p 8080:8080 abcdemo               # then open http://localhost:8080
```

or `docker compose -f abcdemo/compose.yaml up --build`.

Two things the image has to get right, both already handled:

- `IP=0.0.0.0`. Dioxus binds `127.0.0.1` by default, which inside a container means the
  published port reaches nothing.
- `ABCURVES_MODEL_DIR=/app/models`. `model_store::default_model_dir()` falls back to a
  path baked in when *abcurves* was compiled, and that path does not exist in the runtime
  stage. All twelve model files are copied, because the continuous manifest hashes every
  file it declares — including the six ONNX graphs the crate never evaluates.

To build without the sibling checkout, swap the path dependency in `Cargo.toml` for the
commented-out git one; the context can then be `abcdemo/` alone.

## Plugging in a real mouse

The prefix — the A→B reports a hand actually produced — comes through one trait:

```rust
pub trait PrefixSource {
    /// Reports read from the mouse every 1 ms, oldest first; the last one is B.
    fn prefix(&self) -> Vec<[f32; 2]>;

    /// C, as a displacement from A, in native counts.
    fn target_delta(&self) -> [f64; 2];

    fn radius(&self) -> f64 { /* provided */ }
    fn idle_window(&self) -> Vec<[i16; 2]> { /* provided */ }
    fn label(&self) -> String;
}
```

`src/prefix.rs` has two implementations: `MockPrefix`, which synthesises a minimum-jerk
approach, and `CsvPrefix`, which reads one off disk. A real 1 kHz capture is the third,
and is yours to write. Four things to get right, all of which the mock and the loader
already respect:

- **Native counts, not pixels, and Y up.** A capture reporting Y down has to be negated;
  one reporting pixels has to be divided by pixels per count, radius included.
- **Close the bins yourself.** A 1 kHz mouse does not deliver exactly one report per
  millisecond. Accumulate by timestamp into 1 ms bins and emit `[0.0, 0.0]` for a bin that
  received nothing — an empty millisecond is real data, and dropping it shortens the
  movement the model sees.
- **A is where the movement began**, not where you started looking. abcurves'
  `OnsetDetector` finds it causally and points *backwards*, so keep every bin since the
  target appeared.
- **The idle window describes the device, not the movement.** 256 quiet reports from
  before the movement, prepared while nothing is happening — it costs 2.7 ms, nine times
  the plan it precedes, and belongs nowhere near B.

## The CSV format

One typed row per line, so a recording carries the target along with the movement:

```csv
kind,x,y
target,140.0,90.0
radius,12.0,0
report,1.42,0.51
report,0.00,0.00
```

`target` is required and is C as a delta from A. `radius` is optional. `report` rows are
per-millisecond deltas in native counts, in order, zeros included. Blank lines, `#`
comments and the header are skipped. **Export CSV** writes the current mock run in this
format, which is also the easiest way to get a template.

The two server functions take a **file name, not a path** — `capture.csv`, never
`../capture.csv` or `/tmp/capture.csv`. abcurves runs on the server, so resolving whatever
a browser sent would be an arbitrary read and write of the host. Names resolve inside
`ABCDEMO_DATA_DIR` (the working directory by default, `/app/data` in the image), and
anything containing a separator is refused in the panel.

## Worth trying

- **Drag `|BC| at the handoff` out of its eligible window.** The panel annotates the
  window, and outside it abcurves refuses in its own words, naming the rule and by how
  much. Under 8 counts left nothing can be planned at all — that is what the Continuous
  Planner is for. The refusal is part of the demo, not an error.
- **Look at the sixteen finishes.** All sixteen heads are planned for about the price of
  one and a half, and at one fixed geometry they span an order of magnitude — 44 ms to
  364 ms at the default. Duration is the finishing style, not the distance. Always taking
  the shortest would replace the model's distribution with its own tail, which is itself a
  signature.
- **Switch to Continuous with the same target.** Some draws overshoot and simply stay
  there. That is in the distribution, not a bug, and the panel says so rather than hiding
  it.
- **Turn the jitter up.** The tremor rides on the path rather than accumulating, so B
  stays put while the speed chart gets noisy.

## Layout

| Path | What it is |
| --- | --- |
| `src/types.rs` | Everything that crosses the wire. No abcurves — the browser links this. |
| `src/prefix.rs` | The trait, the mock and the CSV loader. Server only. |
| `src/engine.rs` | Driving abcurves, and turning reports into a drawable path. Server only. |
| `src/ui/` | The page: controls, the scene, the speed chart. |
| `src/main.rs` | `launch`, the two server functions, and the tests. |

A note on playback: the DOM cannot be redrawn at 1 kHz, so the cursor advances by elapsed
wall-clock milliseconds at about 60 frames a second. A 364 ms finish still takes 364 ms on
screen at 1×; it is just drawn in ~22 frames rather than 364.

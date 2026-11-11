# vigily web app: design goals

What the web app is for and who uses it. The brand tokens live in [`brand/DESIGN.md`](https://github.com/sayef/vigily/blob/main/brand/DESIGN.md).

**Subject.** vigily is an experiment tracker: training scripts log runs, hyperparameters and metric
curves through a small Python SDK, and `vigily up` serves a web app to find, compare and inspect them.

**Audience.** ML engineers and researchers, at a desk, during and after training. They keep the app open
for hours beside a terminal and an editor, come back to it many times a day, and arrive with a question:
"which learning rate won?", "did run 41 diverge?", "is the sweep still running?".

**The one action.** Pick the runs that matter and compare their curves.

**Mode:** operate. **Surface:** a dense single-page app served by one Rust binary.

**Stack.** React 19, Vite, TypeScript, Tailwind v4 tokens, Radix primitives (shadcn-style), cmdk,
sonner, lucide, uPlot for charts, TanStack Query and Virtual. Embedded into the binary with `build.rs`.

**Existing brand.** None; a new mark and accent. It shares the restraint of my other tools
and none of their names, marks or wording; the warm paper neutrals and ink blue are its own.

**Imagery that truly exists.** The product's own interface and its charts, with synthetic demo data
(`examples/demo.py`). No photography, no illustration.

**Hard constraints.** WCAG 2.2 AA; light and dark both first-class; no CDN fonts or scripts (offline,
air-gapped training clusters, strict CSP); fast with thousands of runs and 100k+ points per series;
desktop first, usable at 400 px.

**Taste:** BOLD 3 / MOTION 2 / DENSITY 8.

- BOLD 3: users compare it with tools they already know; the layout follows desktop-app convention
  (rail, toolbar, table, inspector). Boldness is spent in one place: the chart readout.
- MOTION 2: state changes only (hover, press, open, the crosshair). No entrances.
- DENSITY 8: 32 px table rows, 13 px meta text, numbers right-aligned in tabular figures. Many runs on
  one screen is the point.

## Subject inventory

1. The name: a vigil, the watch kept through the night. Scheduled evaluations run while the team
   sleeps; drift and alerts are what the watch found by morning.
2. Runs: a name, an experiment, a status (running, finished, failed, aborted), a duration.
3. Hyperparameter sweeps: the same experiment, many runs, one or two values changing (`lr`, `batch_size`).
4. Metric series: a name plus a context (`loss` on `train`, `loss` on `val`), stepped, 10 to 1 000 000 points.
5. Steps, epochs and wall-clock time: three x axes for the same curve.
6. Noise: raw curves are jagged; smoothing (EMA) is a daily control.
7. The best value: lowest validation loss, highest accuracy, and the step it happened at.
8. Divergence and NaNs: failed runs that end early, curves that shoot up.
9. Live runs: points arriving while you watch.
10. The terminal: run ids, paths and commands that are copied back into a shell.
11. Long sessions at night: dark mode used as much as light.

## Sections as claims

- **Runs:** "Every run, its settings and its result on one line" → a dense, sortable, filterable table
  with last/min/max metric values next to the parameters that produced them.
- **Metrics:** "Curves from many runs, readable without a rainbow" → a chart grid in neutrals where the
  run under the cursor or pinned takes the accent, and a readout lists every run's value at the cursor,
  sorted.
- **Run:** "Everything one run knows" → overview, parameters, every series, environment, notes and tags.
- **Experiments:** "Where the sweeps are" → experiments with run counts, state and recency.
- **Command palette:** "Anything in two keystrokes" → pages, runs, experiments, metrics, actions.

## Defaults I changed

- A rainbow of run colours is the category default (W&B, MLflow, TensorBoard). I replaced it with
  neutrals plus one accent for focus, line dashes for groups and a sorted readout for identity: with
  30 runs a palette repeats anyway and colour pairs cannot be told apart by 1 in 12 men.
- My first idea for Runs was a metric "dashboard" of KPI tiles above the table. I removed it: the
  tiles were invented vanity numbers, and the table is the content.
- I considered monospace for every number and rejected it: numbers use the text face with tabular
  figures; mono is kept for run ids, keys and commands, which are code.
- I avoided purple: it would read as generic AI tooling and as the tracker this replaces. The accent
  was first a deep night-sea teal; it is now ink blue (fountain-pen blue on paper), never mistaken
  for the amber of a warning.

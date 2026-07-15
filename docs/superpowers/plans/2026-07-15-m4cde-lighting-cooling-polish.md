# M4c/M4d/M4e: Lighting Stage, Cooling Editor, Polish — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The remaining two real screens (Lighting Stage, Cooling) plus the polish pass — completing M4. Per the owner (2026-07-15): full autonomous run, coordinator-verified acceptance (screenshots vs mockups), ONE owner iteration pass at the end instead of per-task glances.

**Architecture:** spec `docs/superpowers/specs/2026-07-14-m4-ui-design.md` §3–4. Frontend is React 19 + Vite 7 (see M4b plan post-mortem). Foundations shipped in M4b: status store (`ui/src/lib/stores/status.ts`), bind flow, IPC commands (`crates/llw-ui/src/commands.rs`), Nocturne tokens (`theme.css`/`components.css`).

**Design reference (binding):** mockups in `.superpowers/brainstorm/48378-1784033233/content/` — `app-shell.html` Stage option A (hero canvas, right rail, preset chips). Token rules as in M4b.

**Key facts for implementers (verified against code 2026-07-15):**
- `llw_effects::render_animation(&EffectSpec, &Geometry, frames: u16) -> (Vec<Vec<[u8;3]>>, u16)` — pure; compile-once-play-frames is exactly the hardware model. `Geometry::Fans{fan_count, leds_per_fan, layout}`, `FanLayout::{UniformRing, SlInf44}`, `led_polar(layout, i, leds_per_fan) -> (angle, radius)`.
- Daemon `Config` (schema v1) uses field-level serde defaults, NO deny_unknown_fields — new defaulted sections are compat both directions. `SlotSpeed::{Percent(u8), Curve(String)}`; `Curve` = named temp→speed bound to a hwmon `SensorSpec`.
- Frame budget on hardware: `clamp(28000/(leds×3), 8, 96)` frames (effects_bridge.rs); preview should use the SAME count per device so what you see is what uploads.
- EffectSpec: kebab-case kinds, speed 1..=5, brightness ≤4, palette ≤8 colors.
- The daemon enforces the 3s RF settle window after uploads; `rgb_in_sync` in Status flips true when readback confirms. Apply UX surfaces this honestly ("applying — RF quiet window").

---

## M4c — Lighting Stage

### Task C1: WASM effects bridge + parity goldens

- [ ] New crate `crates/llw-effects-wasm`: `cdylib`, deps `llw-effects` + `wasm-bindgen` + `serde_json`(or serde-wasm-bindgen). Exports: `render_animation_json(spec_json, geometry_json, frames) -> Uint8Array` (flat `frames×leds×3` bytes + a small JSON header or a second export for the returned u16), and `led_layout_json(geometry_json) -> String` (per-LED `{x,y}` from `led_polar`, unit-circle coords). Keep the API surface tiny and stringly — the boundary is cold.
- [ ] Workspace: exclude from default-members if it breaks native `cargo test --workspace` (wasm-bindgen compiles natively fine, but keep the workspace green either way). `wasm32-unknown-unknown` target + `wasm-pack` installed (document what was installed; passwordless sudo/pacman or rustup/cargo install).
- [ ] Build integration: `npm run build:wasm` script in `ui/package.json` running `wasm-pack build --target web --out-dir ../../llw-ui/ui/src/lib/wasm-pkg` (adjust paths to reality); output dir gitignored; `npm run build` documents the prerequisite (do NOT auto-chain — keep builds predictable).
- [ ] Parity goldens: a native `cargo test -p llw-effects-wasm` (or a small bin) writes `ui/src/lib/wasm-goldens/*.json` fixtures (2–3 specs × SlInf44 geometry, first/middle frames); a vitest test loads the built WASM and compares byte-exact. If loading WASM in vitest (node) needs `--target nodejs` output instead, build both targets or pick the one that serves both vitest and the browser — document the choice.
- [ ] Gates: workspace cargo tests still green; `npm run test` green including parity; `npm run build` green.
- [ ] Commit: `feat(effects): WASM bridge with native parity goldens`

### Task C2: presets in config (daemon, tiny)

- [ ] `crates/llw-daemon/src/config.rs`: `#[serde(default)] pub presets: Vec<Preset>` on `Config`; `Preset { name: String, effect: EffectSpec }`. Daemon treats it as pass-through data (no behavior). Tests: round-trip, absent-field default (follow the existing default-FUNCTIONS pattern notes), pre-existing-config compat test.
- [ ] Gates: `cargo test -p llw-daemon`, clippy clean.
- [ ] Commit: `feat(daemon): presets section in config (pass-through)`

### Task C3: Stage screen — canvas + device picker

- [ ] `ui/src/lib/sections/Lighting.tsx` replaces the silhouette: hero canvas (HTML `<canvas>`, devicePixelRatio-aware) drawing the selected device's LEDs at their real positions (`led_layout_json`), one fan cluster per `fan_count`, laid out horizontally. LED dots with bloom (shadowBlur or layered radial gradients) — the canvas IS the light show; keep chrome minimal.
- [ ] Playback: on spec change (or device change), call WASM `render_animation_json` once (frame count = the device's hardware budget), then requestAnimationFrame playback at the effect's real cadence (period from speed / frames — mirror effects_bridge timing). Pause playback when `document.hidden`.
- [ ] Device picker: pill row above the canvas from status `devices` (name from config when set, else kind). Empty state: no devices → on-theme prompt pointing at Devices section.
- [ ] Store: `ui/src/lib/stores/stage.ts` (framework-free where practical) holding selected device + working EffectSpec (not yet applied); vitest for its reducer logic (spec edits, clamps: speed 1–5, brightness 0–4, palette ≤8).
- [ ] Gates: `npm run test`, `check`, `build` green.
- [ ] Commit: `feat(ui): Lighting stage — live WASM-rendered canvas`

### Task C4: effect rail + Apply flow + preset chips

- [x] Right rail: effect list (the 8 kinds, kebab-case names prettified), palette editor (swatches, add/remove ≤8, native color input styled to theme), speed (1–5 stepper/slider), direction toggle, brightness (0–4). Edits update the working spec → canvas preview updates instantly (local only).
- [x] Apply: explicit button (bloomed accent when dirty) → `invoke('set_effect', {mac, spec})` → "applying — RF quiet window" state (spinner, rail locked) until status shows `rgb_in_sync === true` for that device (timeout 10s → error toast, unlock). Refusals/errors toast verbatim (reuse Task 5's toast system).
- [x] Preset chips under the stage: from config `presets`; click = load into working spec (does NOT auto-apply); "save as preset" (name prompt, in-theme) appends via get_config→mutate→set_config; delete on chip hover (confirm). Errors toast verbatim.
- [x] Vitest: apply-flow state machine (10 tests: dip-then-true, stale-true guard [trusts true only after a false dip or >2 polls], verbatim refusal, 12s timeout, dismiss-orphaning, last-applied tracking) + specFromWire default-filling (2). NOTE: C4 was implemented by the coordinator inline — the implementer agent died on a session limit before writing anything (clean tree verified).
- [x] Gates green (119 vitest, tsc, build). Commit: `feat(ui): effect rail, apply flow, presets`

### Task C5: M4c acceptance (coordinator)

- [ ] Coordinator: headless screenshots of the Stage (static render; playback sanity via short screencast or two spaced screenshots), compare against `app-shell.html` Stage mockup; verify Apply against the LIVE daemon on the real SL-INF (one effect change, watch `rgb_in_sync` cycle false→true, confirm fans' RGB actually changed — coordinator-run, no owner needed). Record results in this plan.
- [ ] Commit: `docs: M4c acceptance`

## M4d — Cooling

### Task D1: sensors IPC (daemon)

- [x] `Request::ListSensors` → enumerate hwmon (`/sys/class/hwmon/hwmon*/temp*_input` + labels + chip names), reply `{sensors: [{chip, label, path_spec, current_c}]}` shaped to be directly usable as config `SensorSpec` references (reuse `sensors.rs` resolve logic in reverse; read each current temp best-effort).
- [x] Extend `StatusData` with per-curve current temp: `curves: [{name, sensor_c: f32|null}]` (cheap — supervisor already reads them for control; expose the last EMA'd value). Keep envelope v1 (additive field).
- [x] Tests: fake hwmon tree fixture (tempdir) → enumeration + shapes; Status extension in existing sim tests.
- [x] Gates: daemon tests + clippy (104 daemon tests). **History note:** D1's changes were accidentally swept into UI commits `47f62d9`/`9db97b2` by coordinator `git add -A` while agents ran in parallel — content verified identical to the implementer's final tested state; no separate feat(daemon) commit exists. Coordinator process fixed: path-scoped adds only. SensorSpec is chip-name+input based; ListSensors emits verbatim-usable specs; Status.curves uses the control-path EMA values (no new polling).

### Task D2: curve editor component

- [x] `ui/src/lib/components/CurveEditor.tsx` + framework-free `curveModel.ts`: points (temp°C x, duty% y) on an SVG grid; drag to move (pointer events), click-empty to add, double-click point to remove (min 2 points); monotonic temp enforcement (clamp/x-order), y clamp 0–100. Live temp cursor: vertical line + current interpolated duty highlight, fed from Status `curves[].sensor_c`.
- [x] Rust `curve.rs` is ground truth for interpolation semantics — mirror it EXACTLY in `curveModel.ts` (read it; likely linear interp with edge clamping) and pin with vitest cases copied from the Rust tests' values.
- [x] Vitest: drag/add/remove invariants, interpolation parity vector (15 tests; SortedCurve::eval mirrored exactly incl. epsilon guard; model speaks duty %, PWM only at the wire).
- [x] Gates green. Commit: `feat(ui): curve editor component`

### Task D3: Cooling screen

- [x] `ui/src/lib/sections/Cooling.tsx` replaces the silhouette: curve list (from config; add/rename/delete curve with confirm), the editor for the selected curve with its sensor picker (dropdown from ListSensors, shows chip+label+current temp), per-device slot assignment matrix (device × slots 1..fan_count → SlotSpeed: fixed % input or curve select), live RPM/PWM per slot from status alongside.
- [x] All writes: get_config → mutate → set_config round-trip; errors toast verbatim; NO debounced auto-save — explicit Save button per the Apply pattern (dirty-state bloom), since SetConfig hits fan control immediately.
- [x] Guardrail: refuse (client-side, with toast) deleting a curve still referenced by any slot.
- [x] Vitest: config mutation helpers (19 tests incl. rename-rewrites-references; SlotSpeed untagged number|string pinned).
- [x] Gates green. Commit: `feat(ui): Cooling screen — curves, sensors, slot assignment` (5a373cb; adds the list_sensors Tauri command)

### Task D4: M4d acceptance (coordinator)

- [x] Coordinator live check PASSED (2026-07-15 16:25): daemon REDEPLOYED first (running binary predates presets/ListSensors/Status.curves; re-acquired link in ~10s, RGB re-asserted). Status.curves live (61.8 °C EMA under compile load, fans tracking at 88). Nudge test: +6 duty on all points → desired_pwm 88→103 (+6% of 255, one control tick); exact-snapshot restore → 88, GetConfig byte-identical. Layout screenshot verified (unreachable variant; full-data walk = E2).
- [x] Commit: `docs: M4d acceptance`

## M4e — Polish + final acceptance

### Task E1: polish pass

- [ ] Sweep with fresh eyes against the mockups: spacing/typography drift, focus rings + full keyboard reachability (sidebar, rail, editors), window min-size behavior (900×600), empty/error states on all screens, toast consistency, no console errors, no re-render churn (React DevTools profile the 1s poll — memoize where it actually matters).
- [ ] Light/perf sanity on the canvas: stage idle CPU acceptable (<5% of a core at 60fps playback; throttle to 30fps if needed).
- [ ] README: UI section (dev + build instructions incl. wasm-pack prerequisite), screenshots (headless captures fine).
- [ ] Gates: full workspace cargo tests + clippy + ui test/check/build all green.
- [ ] Commit: `polish(ui): M4e pass` (+ `docs: README UI section`)

### Task E2: final acceptance package (coordinator → owner)

- [ ] Coordinator assembles the owner iteration pass: current screenshots of all four sections, list of every judgment call taken without a glance (collected from task reports), known punch-list items. Launch `cargo tauri dev` and hand over. Owner iterates; punch list becomes follow-up tasks.
- [ ] Record owner verdicts in this plan; close M4.

---

## Self-review notes

- Compile-once-play-frames makes preview parity structural, not aspirational — the same frames the hardware would loop are what the canvas loops; the parity golden test guards the WASM boundary only.
- Presets ride the existing SetConfig round-trip: no new IPC, daemon stays policy-free about them (spec said "new config section + IPC" — the IPC already exists; recorded as a deviation-by-simplification).
- Coordinator live checks (C5 Apply, D4 curve nudge) touch the real daemon deliberately — they're the acceptance the owner would have done, done carefully (snapshot config first, restore exactly). Implementer agents still NEVER touch hardware or the real socket.
- Curve interpolation parity pinned to Rust test vectors prevents the classic editor-shows-one-thing-daemon-does-another drift.
- Risks: wasm-pack/vitest target mismatch (mitigated: pick target per what vitest can load, document); canvas perf on webkit2gtk (mitigated: fps throttle escape hatch).

# M3: Effect Engine — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The `llw-effects` crate — pure, deterministic rendering of dynamic RGB effects (Ripple first among eight) against abstract device geometry — wired into the daemon so `llw set-effect 0 ripple --colors 0000FF,8800FF` puts a living animation on real hardware as a fire-and-forget onboard upload. Plus the flash-size probe that sets every effect's frame budget.

**Architecture:** Effects are pure functions `(spec, geometry, t) → frame` where `t ∈ [0,1)` is the phase of one animation period. The daemon compiles a spec into F frames at `t = i/F`, uploads once via the existing `upload_rgb`, and the firmware loops it — zero ongoing RF traffic (M1-proven). The UI (M4) will call the same functions for live previews. Determinism → golden-frame tests in CI for every effect × geometry.

**Tech Stack:** pure Rust, no new dependencies (HSV math hand-rolled — one small function, no palette crate).

**Context for the engineer:**
- Spec §3.2/§5 in `docs/superpowers/specs/2026-07-13-lian-li-wireless-design.md`. M3 gate: Ripple + ≥5 more effects onboard on SL-INF (+ Strimer when installed); frame budgets measured.
- **The effect ALGORITHMS are original work** — upstream never rendered effects for wireless devices (that's the gap this project fills). The names come from L-Connect's catalog; the visual definitions below are ours, written for parity-in-spirit. Implement the math as specified; if a definition seems visually wrong, implement it as written and note the concern — visual tuning happens on hardware, not in the plan.
- Live production constraint: `llw-daemon` OWNS the dongles and is IN A SOAK. Tasks 1–6 are pure/daemon-code only (no hardware). Task 7 restarts the daemon (allowed — restarts are normal ops; do NOT stop it for long). Task 8 (probe) briefly stops it, with the owner present.
- Baseline: 95 workspace tests (37 protocol + 58 daemon), zero clippy.

---

## File structure (end state)

```
crates/
├── llw-effects/
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs           # EffectSpec, EffectKind, render_frame/render_animation, catalog()
│       ├── geometry.rs      # Geometry (Fans/Strip), positions, helpers
│       ├── color.rs         # hsv_to_rgb, lerp, scale (pure helpers)
│       └── effects/
│           ├── mod.rs       # dispatch
│           ├── stat.rs      # Static (module named stat: `static` is a keyword)
│           ├── breathing.rs
│           ├── rainbow.rs   # Rainbow + RainbowMorph
│           ├── color_cycle.rs
│           ├── meteor.rs
│           ├── runway.rs
│           └── ripple.rs
├── llw-daemon/              # + effect field in config, SetEffect IPC, engine in rgb path
└── llw-cli/                 # + `llw effects`, `llw set-effect`
```

---

## Shared definitions (used by every task — read first)

**EffectSpec** (also the config/IPC shape):

```rust
pub struct EffectSpec {
    pub kind: EffectKind,          // enum, serde lowercase
    pub colors: Vec<[u8; 3]>,      // palette; effects define usage; empty → effect default
    pub speed: u8,                 // 1..=5, clamped; period_ms = [6000, 4200, 3000, 2100, 1400][speed-1]
    pub direction: Direction,      // Forward | Reverse (reverse mirrors position: p → 1-p)
    pub brightness: u8,            // 0..=4, same scale as StaticColor (×(b/4) post-render)
}
```

**Geometry** (from `DeviceRecord`, built by a `Geometry::of(&DeviceRecord)` constructor):

```rust
pub enum Geometry {
    /// N fans × L LEDs; LED i of fan f has ring angle a = i/L ∈ [0,1) and
    /// chain position c = (f + i/L)/N ∈ [0,1) — effects choose which axis.
    Fans { fan_count: u8, leds_per_fan: u8 },
    /// Flat strip; LED i has position p = i/total ∈ [0,1).
    Strip { total: u16 },
}
```

Frame layout matches the wire format: fans concatenated fan0..fanN, LEDs in ring order; strip linear. `Geometry::len()` = total LEDs.

**Rendering contract:** `render_frame(&spec, &geom, t: f32) -> Vec<[u8;3]>` — pure, deterministic, `t` already direction-adjusted by the caller (`render_animation` applies `t → 1-t` for Reverse where the effect declares itself directional). `render_animation(&spec, &geom, frames: u16) -> (Vec<Vec<[u8;3]>>, u16 /*interval_ms*/)` renders F frames at `t=i/F` and computes `interval_ms = period_ms / F` (clamped to ≥20ms). Frame budget default: **24** (conservative pre-probe; Task 8 raises it).

**Effect definitions** (t = phase; pos = the axis noted; all colors linearly interpolated in RGB; `palette(x)` = piecewise-linear loop through `colors` at x ∈ [0,1)):

| Effect | Axis | Definition |
|--------|------|-----------|
| Static | — | every LED = colors[0] (default white) |
| Breathing | — | uniform `palette(floor stepping: color j = breath j mod len)` × `sin²(πt)`; one breath per period |
| Rainbow | ring angle (Fans) / pos (Strip) | hue = `(axis + t) mod 1`, full S/V — classic moving wheel |
| RainbowMorph | — | uniform hue = t (whole device cycles together) |
| ColorCycle | — | uniform `palette(t)` (smooth fade through the palette) |
| Meteor | chain pos (Fans) / pos (Strip) | head at `h=t`; LED brightness = `exp(-d*12)` where `d = (h - pos) mod 1` (trailing only); color `palette(pos)`; background black |
| Runway | chain pos / pos | marching segments: on if `((pos*6 + t*3) mod 1) < 0.5`; on-color colors[0], off-color colors[1] or black |
| Ripple | ring angle per fan (Fans) / pos (Strip) | a pulse expands from origin 0: wavefront radius `r = t`; LED brightness = `exp(-((dist − r)²)/(2·0.06²))` where dist = shortest ring distance (Fans: within each fan, all fans in phase) or |pos−0.5|·2 (Strip: expands from center to both ends); color `palette(t)`; background black. One ripple per period |

Speed→period and the definitions above are LOCKED for v1 (tuning happens on hardware after the gate, in one place).

---

### Task 1: `llw-effects` crate — geometry, color, spec, engine API

**Files:** `crates/llw-effects/Cargo.toml`, `src/lib.rs`, `src/geometry.rs`, `src/color.rs`, `src/effects/mod.rs` + a `stat.rs` stub so it compiles.

- [ ] Cargo.toml: name llw-effects, workspace version/edition/license, deps: `serde { workspace = true }` only.
- [ ] `geometry.rs`: the enum above + `len()`, `of(&DeviceRecord)`… **no — no llw-protocol dependency**: keep llw-effects dependency-free of protocol types. `Geometry::of` lives in llw-daemon (a 6-line match on DeviceKind: Fans for fan devices with leds_per_fan/fan_count, Strip for `led_count_override` devices). geometry.rs provides `ring_angle(fan_led_index, leds_per_fan)`, `chain_pos(fan, fan_led_index, ...)`, `strip_pos(i, total)` helpers + unit tests (boundaries: first/last LED, single-fan).
- [ ] `color.rs`: `hsv_to_rgb(h: f32, s: f32, v: f32) -> [u8;3]` (standard sextant algorithm), `lerp(a, b, x)`, `scale(c, k)`, `palette(colors, x)` (empty → white; single → constant; else looped piecewise-linear). Tests: hsv at 0/120/240° = pure R/G/B; palette endpoints and wraparound.
- [ ] `lib.rs`: EffectSpec/EffectKind/Direction (serde, lowercase kind names; `EffectKind::all()` for the CLI), `period_ms(speed)`, `render_frame` (dispatch + brightness scaling), `render_animation` (direction handling: Reverse maps t→1−t only for effects where `EffectKind::directional()` is true — Meteor, Runway, Rainbow; interval clamp ≥20ms). Tests: brightness 0 → all black; determinism (same inputs twice → identical frames); interval math (24 frames @ speed 3 → 3000/24 = 125ms).
- [ ] `stat.rs`: Static (all LEDs colors[0], default white [255,255,255]) + test.
- [ ] Verify: `cargo test -p llw-effects` green, clippy clean; commit `feat(effects): crate skeleton — geometry, color math, engine API, Static`.

### Task 2: Breathing + ColorCycle + RainbowMorph (the uniform effects)

**Files:** `src/effects/breathing.rs`, `color_cycle.rs`, `rainbow.rs` (RainbowMorph half), mod.rs dispatch.

- [ ] Implement per the table. Breathing: `v = sin²(π·t)`, color index advances one palette entry per period is NOT expressible within a single period render — v1: single-period animation uses colors[0] only… **Decision (locked):** Breathing renders `palette(t)` × `sin²(π·t)` — with a multi-color palette the color drifts across the breath; with one color it's a clean breath. Document this in the module.
- [ ] Golden tests per effect on `Fans{3,44}` and `Strip{132}`: pin exact RGB values at t=0, t=0.25, t=0.5 for LED 0, LED 65, LED 131 (compute them from the math in the test with independent inline arithmetic — no calling the function to generate its own expectation). Uniformity asserts (all LEDs equal) for the three uniform effects.
- [ ] Commit `feat(effects): breathing, color-cycle, rainbow-morph`.

### Task 3: Rainbow + Meteor + Runway (the positional effects)

**Files:** `src/effects/rainbow.rs` (Rainbow half), `meteor.rs`, `runway.rs`.

- [ ] Implement per the table. Meteor trailing distance: `d = (h - pos).rem_euclid(1.0)` so the tail follows the head. Runway: exactly the formula given.
- [ ] Golden tests: Rainbow — LED at axis=0, t=0 is hue 0 (red); rotation assert (frame at t=0.5 equals frame at t=0 shifted by half the axis). Meteor — head LED is brightest; brightness strictly decreases along the tail (property assert over 10 LEDs); background black beyond ~4 tail widths. Runway — duty cycle 50% (count on-LEDs ≈ half). Direction tests: Reverse mirrors (frame(t) reversed-order equals Forward frame(t) for strip).
- [ ] Commit `feat(effects): rainbow, meteor, runway`.

### Task 4: Ripple (the flagship)

**Files:** `src/effects/ripple.rs`.

- [ ] Implement per the table. Fans: dist = shortest angular distance from LED's ring angle to angle 0 (i.e. `min(a, 1-a)`), all fans in phase (the whole cluster pulses together — v1; per-fan phase offset is a future variant). Strip: `dist = (pos - 0.5).abs() * 2.0` (center-out). Gaussian σ=0.06, wavefront r=t. Brightness additionally fades the pulse as it expands: multiply by `(1-t)` so the ring dies at the edge (one clean pulse per period, no wraparound ghost).
- [ ] Golden tests on both geometries: at t=0.1 the LEDs nearest the origin are brightest; at t=0.9 the origin is near-black and the far edge carries the (faded) front; total frame energy at t=0.95 < t=0.5 (die-out assert); period boundary: frame(t=0) ≈ black except origin flash.
- [ ] Commit `feat(effects): ripple`.

### Task 5: Daemon integration — config, engine in the RGB path

**Files:** `crates/llw-daemon/Cargo.toml` (+ llw-effects dep), `config.rs`, `supervisor.rs`, new `src/effects_bridge.rs`.

- [ ] config.rs: add `#[serde(default)] pub effect: Option<llw_effects::EffectSpec>` to DeviceConfig (the M3 field the schema doc promised; precedence over `color` per the documented rule). validate(): effect.speed 1..=5, brightness ≤4, colors ≤8 entries. Field-level serde defaults where EffectSpec needs them (follow the Task-1-M2b pattern — every new field tolerant of absence).
- [ ] `effects_bridge.rs`: `Geometry::of(kind, fan_count) -> Option<Geometry>` (Fans for fan devices, Strip for led_count_override devices); `compile(spec, record, frame_budget) -> Option<(frames, interval_ms)>` calling render_animation. Unit tests with synthetic records (SL-INF 3-fan → 132-LED frames; Strimer(2) → 132-LED strip frames).
- [ ] supervisor.rs rgb_tick: device's desired RGB = `effect` if Some (compile via bridge, upload frames with its interval) else `color` as today. Drift detection unchanged (expected_fx from upload_rgb's return). Frame budget: new `FRAME_BUDGET: u16 = 24` const with a doc pointing at Task 8. The existing static-color path keeps working (pin: existing sims untouched and green).
- [ ] New sim: config with `effect: ripple` on the SL-INF device → after acquisition+poll, rgb upload fires with >1 frames (assert via a new StepOutcome field or by asserting `uploaded_rgb == 1` and inspecting the Fake TX write count > the single-frame case; simplest honest assert: the FakeIo write count for the upload exceeds 8 packets — multi-frame payloads are bigger). Choose the cleanest and document it.
- [ ] Verify + commit `feat(daemon): effect specs in config, engine wired into RGB path`.

### Task 6: IPC + CLI — `SetEffect`, `llw effects`, `llw set-effect`

**Files:** `ipc.rs`, `supervisor.rs` (answer arm), `llw-cli/src/main.rs`.

- [ ] ipc.rs: `SetEffect { mac: String, effect: llw_effects::EffectSpec }` request variant (+ envelope test for its wire shape).
- [ ] answer(): validate (same rules as config validate), persist to config (same save-then-mutate order as SetColor — validate → mutate cfg → save → reset expected_fx/last_rgb_upload; on save failure, no half-state: clone-mutate-save-swap).
- [ ] llw-cli: `llw effects` (list EffectKind::all() with one-line descriptions), `llw set-effect <index-or-mac> <kind> [--colors RRGGBB,RRGGBB] [--speed 1-5] [--direction forward|reverse] [--brightness 0-4]`. Sends SetEffect over IPC (daemon applies it — no dongle takeover, works DURING the soak). Map index→mac via a Status call.
- [ ] Verify: build, tests, clippy; `llw effects` prints the catalog (safe to run); commit `feat: SetEffect IPC + llw set-effect / llw effects`.

### Task 7: Live fire — first effect on real hardware (owner present, daemon keeps running)

No files. `systemctl --user restart llw-daemon` (picks up the new binary — install it first: `sudo install -Dm755 target/release/llw-daemon /usr/local/bin/llw-daemon`), then:

```bash
./target/release/llw set-effect 0 ripple --colors 0000FF,8800FF --speed 3
./target/release/llw status    # rgb_sync=true within ~5s
```

- [ ] Owner confirms: a blue/purple pulse expanding across the fan rings, looping smoothly, ~3s period. Try `rainbow`, `breathing`, `meteor` similarly. Record impressions (smoothness at 24 frames, color fidelity) in this plan. Soak counters unaffected (uploads are one-shot).

### Task 8: Flash-size probe (owner present; daemon briefly stopped)

The M1-deferred experiment: find the firmware's animation storage ceiling per family; set FRAME_BUDGET from data.

- [ ] `systemctl --user stop llw-daemon` (fans revert — keep it short).
- [ ] Probe with the CLI against the dongle directly: extend `llw set-color`-style direct path with a hidden `llw probe-frames <index> --frames N` subcommand (small addition in this task: renders a Rainbow at N frames via llw-effects, uploads directly, then GetDev-verifies the effect index echo). Binary-search N over 8→16→32→64→96→128 on the SL-INF: success = fx echo matches after 3s of silence. Record the largest passing N and the failure mode of the first failure (no echo? wrong echo? device reset?). Compressed sizes are printed per upload (tinyuz output) — record those too; the TRUE limit may be bytes, not frames (RGB_MAX_COMPRESSED=55,880 is the protocol ceiling).
- [ ] `systemctl --user start llw-daemon` (restores effect from config).
- [ ] Set `FRAME_BUDGET` in supervisor.rs to ~75% of the measured frame ceiling (or keep 24 if the ceiling is lower than 32); commit `feat(daemon): frame budget from measured flash ceiling` + record the numbers in this plan.
- [ ] Strimer: when physically installed, repeat Task 7's set-effect + a 30-second probe spot-check on it, and record. (Not a gate for this plan if the hardware isn't in yet — note the deferral.)

---

## Self-review notes (already applied)

- **Spec coverage:** §3.2 geometry-abstract effects (T1-T4: 8 of the catalog incl. the M3-gate set), §5 compile-to-onboard flow (T5), the probe (T8 — spec's "known unknown"), UI-shared rendering (pure crate — M4 consumes as-is). Full catalog completion remains M5 per spec.
- **Types:** EffectSpec serde shapes shared config/IPC/CLI (single definition in llw-effects); Geometry::of bridges protocol types in the daemon (llw-effects stays dependency-free). rgb_assert's static path untouched — effect and color coexist with documented precedence.
- **Judgment calls:** effect math is locked-for-v1 to keep tuning centralized post-gate; Ripple pulses all fans in phase (variant knobs are M5+ scope); Breathing's palette-drift semantics chosen over per-period color stepping (single-period animations can't express cross-period state); probe uses a hidden CLI subcommand rather than daemon IPC to keep upload-failure blast-radius away from the soak.

---

## Task 7+8 hardware results (2026-07-14)

**Task 7 (live fire):** Ripple PASS (blue/purple pulse looping; first upload lost to ch8 packet loss, drift-restore re-uploaded automatically — self-heal proven in production). Rainbow: correct on inner ring, mirrored on outer-left — cause discovered: SL-INF wiring is 5 segments, not a uniform ring. Meteor used as wiring probe.

**SL-INF 44-LED physical layout (chase-probed, counts user-verified):**
| idx | segment | count | path |
|-----|---------|-------|------|
| 0-7 | inner ring | 8 | full circle clockwise from left-middle |
| 8-17 | outer LEFT arc | 10 | bottom → top |
| 18-25 | LEFT side strip | 8 | bottom → top |
| 26-35 | outer RIGHT arc | 10 | bottom → top |
| 36-43 | RIGHT side strip | 8 | bottom → top |

**Task 8 (flash probe, Rainbow @ 132 LEDs):** 32/64/96 frames PASS; 112/128 FAIL (firmware wipes fx to all-zero — fails safe, drift-detectable). Ceiling ≈ 38-44KB RAW (likely 40KB); protocol max (55.8KB compressed) is NOT the binding limit. Budget decision: byte-based — RAW_BYTE_BUDGET = 28,000 (≈75% of measured floor), frames = min(96, budget/(leds×3)), floor 8. → 132 LEDs: 70 frames; 174-LED Strimer: 53 frames.

**Bonus observation (CORRECTED):** during the probe session, `discover_master` (GET_MAC first-hit) reported channel 2 while GetDev device records — the ground truth — stayed on channel 8 throughout. No hop occurred: this is further evidence that GET_MAC responses carry no operating-channel information, reinforcing the GetDev-only acquisition design.

---

## M3 gate — CLOSED (2026-07-14 afternoon)

**Protocol discovery (session's biggest find): the firmware needs RF SILENCE during its flash commit.** Multi-frame uploads from the daemon never stuck (infinite 5s drift-retry) while identical payloads with 3s post-upload quiet always passed. Fix: 3s post-upload settle window in the supervisor (suppresses polls/PWM/heartbeat; commit d679b59). After the fix: single upload, first-try stick, every time. This likely explains why upstream's PR #93 only tested modest frame counts.

**Layout + budget landed** (d041297, 0862593, 11ee61d): byte-based frame budget (70 frames @ 132 LEDs); SL-INF 5-segment layout map — Rainbow seamless ("seamless and smooth" — owner), radial Ripple with inner radius hardware-tuned 0.4→0.7 ("that's the one" — owner).

**Visual verdicts (owner, on hardware):** Ripple ✓ (radial, tuned), Rainbow ✓ (seamless post-layout-map), Meteor ✓ (also served as the wiring probe), Runway ✓ ("looks sick"), Breathing ✓, Static ✓ (since M2). Color-cycle + RainbowMorph uploaded clean (rgb_sync) but not eyeballed — same verified engine.

**Gate: Ripple + ≥5 effects running onboard on SL-INF — MET.** Deferred: Strimer validation (hardware not yet installed — one `llw set-effect <idx> ripple` when it is); the remaining L-Connect catalog is M5 scope; per-fan ripple phase + tuning knobs are M4-UI territory.

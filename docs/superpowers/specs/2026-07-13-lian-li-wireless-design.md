# lian-li-wireless — Design

**Date:** 2026-07-13
**Status:** Approved design, pre-implementation
**Owner:** Morgan Blem (SlackEight)

## 1. Problem & goal

Lian Li's 2.4GHz wireless ecosystem (UNI FAN SL-INF Wireless, Strimer Wireless, wireless LCD fans, etc.) has no first-party Linux support. The existing open-source option, `sgtaziz/lian-li-linux`, implements the wireless protocol but: (a) its wireless fan control has reliability failures the owner has personally fought for weeks (boot-time acquisition of congested RF channels causing PWM dropout storms; TX-dongle firmware wedges), and (b) its RGB support for wireless devices is limited to solid colors and raw frame uploads — none of L-Connect 3's dynamic effects (Ripple, Wave, Meteor Shower, …) are actually rendered for wireless devices.

**Goal:** a standalone open-source package, `lian-li-wireless`, specialized in the wireless ecosystem, such that Linux users don't feel the lack of L-Connect 3: bulletproof wireless fan speed control, the full dynamic RGB effect suite, and a management UI of L-Connect-3 class or better.

### V1 scope (must-haves)

1. **Bulletproof wireless fan control** — root-cause fixes for channel acquisition and runtime self-healing (§4).
2. **Dynamic RGB effects engine** — the L-Connect effect catalog rendered as onboard animations for SL-INF fans and Strimer Wireless (§5).
3. **Full-parity management UI** — binding, grouping, naming, per-zone effects, fan curves, health telemetry (§6).

### Non-goals for v1 (explicit deferrals)

- **LCD content** (wireless LCD fans render RGB+PWM over RF, but LCD imagery requires a USB tether by hardware design — post-v1).
- **OpenRGB SDK server** (upstream's implementation is MIT and portable when wanted — post-v1).
- **Audio-reactive / non-periodic streamed effects** (e.g. "Voice") — post-v1; v1 covers effects expressible as onboard frame loops.
- **Wired devices of any kind** (wired Strimer Plus, wired hubs) — out of scope permanently; other projects cover them.
- **Multi-dongle support** — single TX/RX pair, matching upstream.

## 2. Feasibility findings (2026-07-13)

- The USB-dongle protocol is substantially reverse-engineered, working, and MIT-licensed in `sgtaziz/lian-li-linux` (35.6K LOC total; ~3.5K LOC of wireless-relevant core). Verified at upstream commit `d262007`.
- **Strimer Wireless is already protocol-supported** upstream (device types 1–9; LED counts 88/116/132/174 by SKU).
- **Onboard animations are proven**: upstream PR #93 (`SetRgbFrames`, merged 2026-07-02) uploads tinyuz-compressed multi-frame RGB blobs that device firmware stores and loops autonomously at a configurable interval, with the dongle idle afterwards.
- All 28 L-Connect effect IDs (incl. Ripple = 14) are enumerated in upstream `lianli-shared/src/rgb.rs`, but for wireless devices no host-side renderer exists — this is the gap this project fills.
- Upstream's maintainer announced a ground-up rewrite (issue #88) with no timeline — reinforcing the decision not to depend on upstream's roadmap.
- OpenRGB and liquidctl have zero wireless Lian Li support; community demand is demonstrated (upstream's issue traffic, distro forum threads).
- L-Connect 3 cannot practically run under Wine (needs raw WinUSB dongle access). Fallback for genuinely unknown protocol corners: Windows VM with USB passthrough. Not needed for v1 scope.
- Owner's hardware (all on hand): SL-INF wireless fans, Strimer Wireless, wireless LCD fans, V1 dongle pair (TX `0416:8040`, RX `0416:8041`).

## 3. Architecture

Decision: **standalone wireless stack** (fork-and-strip), replacing `lianli-daemon` outright. Rationale: the hardest v1 requirement (fan reliability) is daemon-internal work regardless of approach; owning the daemon removes all dependency on upstream's timeline. Alternatives considered and rejected: companion app on upstream's IPC (fan fixes would wait on upstream merges into a codebase mid-rewrite); staged companion→standalone (fixes the worst pain last).

Rust workspace, four units:

### 3.1 `llw-protocol` — pure protocol library

Ported from upstream (MIT, attribution preserved): `lianli-transport`, `lianli-devices/src/wireless/`, wireless-relevant parts of `lianli-shared`. Responsibilities:

- Dongle transport: open/claim TX+RX (V1 `0416:8040/8041`, V2 `1A86:E304/E305`), 64-byte USB packet I/O, reopen-on-failure primitive.
- Channel scan primitive (`USB_CMD_GET_MAC` 0x11 per channel → master MAC + fw version).
- RX `GetDev` polling (`USB_CMD_SEND_RF` 0x10 + 0x01) and parsing of 42-byte device records (MAC, type, fan count, RPM×4, PWM readback×4, effect_index hash, sequence, 0x1C marker).
- RF frame construction (240 bytes = 4×60-byte chunks over USB): PWM frames (`RF_SELECT` 0x12 + `RF_PWM_CMD` 0x10), RGB direct frames (`RF_SET_RGB` 0x20), animation upload (header packet: data_len/frame_count/led_count/interval_ms; tinyuz-compressed payload in 220-byte chunks; header repeated 4× for reliability).
- Bind/unbind, master-clock heartbeat (0x14), `CMD_RESET` (0x11080000), video/LCD mode commands.
- Device-type table: SL-INF (44 LEDs/fan, min duty 11%), SLV3 LED/LCD (40), TLV2 (26), CL/RL120 (24), wireless AIOs (WaterBlock/WaterBlock2), Strimer types 1–9, LC217, Led88, V150.

**No policy**: no loops, no timers, no retries beyond a single transport reopen primitive. Typed errors out. Designed to be publishable as a crate other projects (OpenRGB, liquidctl) can adopt.

### 3.2 `llw-effects` — effect engine

Pure functions: `(EffectSpec, Geometry, frame_index) → Frame` where `EffectSpec = {effect, colors, speed, direction, brightness}` and `Geometry` describes the device's LED layout abstractly (position along a strip, angle around a ring, fan index). One Ripple implementation serves a fan ring and a 24-pin cable.

- Geometry descriptors per family: SL-INF = N fans × 44-LED ring (known physical order); Strimer SKUs = flat strips of 88/116/132/174 LEDs with per-cable lane mapping; AIO pump = 24-LED ring.
- Effect catalog: the L-Connect 28 (Rainbow, Rainbow Morph, Static, Breathing, Runway, Meteor, Color Cycle, Staggered, Tide, Mixing, Door, Render, Ripple, Reflect, Tail Chasing, Paint, Ping Pong, Stack, Cover Cycle, Wave, Racing, Lottery, Intertwine, Meteor Shower, Collide, Electric Current, Kaleidoscope — Voice deferred post-v1), implemented incrementally: Ripple first, ≥5 more by the M3 gate, full catalog complete by M5 (v1 release).
- Output is deterministic for a given (spec, geometry, frame_index) — enables golden-frame tests and identical UI previews.
- No USB, no async, no global state.

### 3.3 `llw-daemon` — system service

The only process touching the dongles. Owns all policy:

- **Device supervision**: discovery, binding state, channel management (§4), link-quality telemetry.
- **Fan control**: temp→PWM curves against hwmon sensors, smoothing/hysteresis, 1s PWM keepalive, per-fan-type minimum duty clamps, CLV1 153–155 quirk filter, motherboard-PWM sync mode passthrough.
- **RGB orchestration**: compiles `EffectSpec`s via `llw-effects` into frame loops, uploads via `llw-protocol` animation path, monitors `effect_index` (FNV-1a of LED state) for firmware drift and re-uploads on mismatch.
- **Heartbeat**: 1 Hz master-clock 0x14 broadcast.
- **IPC server**: Unix domain socket at `$XDG_RUNTIME_DIR/lian-li-wireless.sock`, newline-delimited JSON, **explicit protocol version field in every request/response from day 1**; version mismatch yields a structured error the UI can render ("update daemon/app").
- **Config**: versioned JSON at `~/.config/lian-li-wireless/config.json` (schema version field; migrations on load). Per-device/per-group zone assignments, fan curves, presets. Applied atomically on start and change.

### 3.4 `llw-ui` — Tauri 2 desktop app

Rust side: thin IPC client + direct in-process calls to `llw-effects` for live previews. Frontend: Svelte. Screens in §6.

### 3.5 Packaging

- systemd **user** service (matches upstream's proven model and udev-rule permission scheme), `Restart=on-failure`.
- udev rules for dongle V1/V2 device nodes.
- AUR package first (`lian-li-wireless-git`), with `conflicts=lianli-linux-git` — one daemon per dongle pair. `pkgver()` must track commit count, not just latest tag (avoids the stale-rebuild trap observed with the upstream AUR package).
- License MIT; `NOTICE`/headers attribute `sgtaziz/lian-li-linux` in ported code.

## 4. Reliability model

Designed directly from two empirically confirmed failure modes (owner's production logs, June–July 2026).

### 4.1 Channel acquisition (root-cause fix)

Upstream scans channel 8 first and accepts the first responder — which is how cold boots lock onto congested channel 8. Instead:

1. Scan the channel space for responders (`GET_MAC` per channel).
2. For each responding channel, run a short scoring burst (~2s of GetDev polls), measuring response rate and record validity.
3. Select the best-scoring channel, not the first.
4. **Re-run acquisition after the daemon's own `CMD_RESET`** (Tier 0): reset makes the master hop, so any pre-reset channel choice is stale by definition.

### 4.2 Runtime self-healing (tiered)

- **Drift detection**: continuously mirror commanded PWM (`desired_pwm`) against the PWM readback in device records (same pattern as upstream's `rgb_drifted`, applied to PWM). A readback of `[0,0,0,0]` against a nonzero command is a dropout observation.
- **Tier 1 — re-acquire**: ≥5 dropout observations within 60s (after a 120s post-acquisition grace period) → re-run scored acquisition; 60s cooldown between attempts.
- **Tier 2 — full reconnect**: 2 consecutive failed Tier-1 attempts → close and reopen dongle transports, full re-discovery; 5-minute cooldown.
- Thresholds live in config with these defaults; they are tuning parameters, not constants.

### 4.3 TX-wedge detection (honest failure)

Signature: TX dongle absent from USB, re-enumeration failing with setup-address errors (empirically unrecoverable in software; requires power removal). Daemon detects the signature, emits one desktop notification ("TX dongle wedged — power-cycle required") + persistent UI health alert, and continues quiet periodic re-enumeration attempts in case power returns. No restart thrashing.

### 4.4 Telemetry as a feature

Current channel, per-channel scores from last acquisition, dropout counters, re-acquire/reconnect history, per-device link quality and last-seen — all first-class daemon state served over IPC (Health screen, §6), plus structured `tracing` to journald. No debug-log grepping required to understand system state.

## 5. RGB data flow

1. UI writes an `EffectSpec` for a device/group zone → IPC → daemon.
2. Daemon renders the spec via `llw-effects` into a finite frame loop sized to the device's frame budget (e.g. Ripple @ Strimer PW24: N frames × 132 LEDs × RGB).
3. Frames are concatenated, tinyuz-compressed, uploaded once via the animation path; firmware loops onboard at `interval_ms` with **zero ongoing RF traffic** — effects survive RF congestion, dropouts, and daemon restarts.
4. Daemon computes the expected `effect_index` hash and watches device records; on drift (firmware forgot), it re-uploads.
5. Groups: one spec rendered against each member's geometry with aligned phase, so fans + Strimer animate in sync.

**Known unknown (first hardware experiment, M3):** maximum animation size firmware accepts per device family (flash limit; upstream tested only modest frame counts). Empirically binary-search it; it sets per-effect frame budgets (smoothness). Until measured, effects render conservatively (≤32 frames).

Static/solid modes use the same path with a 1-frame loop. Per-LED direct streaming (`RF_SET_RGB` immediate) remains in `llw-protocol` for post-v1 reactive effects and UI "identify device" flashes.

## 6. UI

Tauri 2 + Svelte. Four screens:

- **Lighting** (centerpiece): effect browser with live animated previews (rendered by the real `llw-effects` engine in-process); device/group canvas depicting actual hardware (fan rings, Strimer cable lanes); per-zone assignment; color/speed/direction/brightness controls; named presets.
- **Cooling**: drag-point fan-curve editor (temp→PWM), hwmon sensor picker, live RPM/PWM per fan, profiles.
- **Devices**: bind/unbind with L-Connect-style bind lock (prevent neighbor rebinding), grouping, renaming, firmware/link info per device.
- **Health**: channel + link telemetry, dropout/re-acquire history, dongle status, actionable alerts (TX-wedge banner).

The detailed visual design (layout, theme, motion) gets a dedicated design pass at M4 with mockups; this spec pins screens, capabilities, and the quality bar (L-Connect-3 class or better; mac-grade polish), not pixels.

## 7. Error handling

- `llw-protocol`: typed errors, no autonomous retries (single reopen primitive exposed, invoked by daemon policy).
- `llw-daemon`: owns all retry/recovery (transport reopen-and-retry per operation, §4 tiers above it). Never crashes on device weirdness: malformed device records are logged and skipped; unknown device types surface as "unsupported device" entries over IPC rather than disappearing.
- `llw-ui`: degrades gracefully when the daemon is unreachable — cached read-only state + reconnect banner; never a blank window. All user-visible errors are plain-language with a suggested action.

## 8. Testing

- **`llw-effects`**: golden-frame snapshot tests, every effect × representative geometries (SL-INF ring, each Strimer SKU, AIO ring). Deterministic; CI.
- **`llw-protocol`**: byte-exact encoding tests against known-good frames derived from upstream's implementation; `FakeTransport` driving discovery/parsing through scripted responses including truncated/garbage records. CI.
- **Daemon control loop**: simulation against a scripted fake device — channel congestion, dropout storms, TX disappearance — asserting tier thresholds, grace periods, and cooldowns fire correctly. Owner's recorded misbehave logs become fixtures. CI.
- **Hardware validation**: written manual test plan (bind/unbind each family, effect upload per family, dropout injection by RF interference/unplug, boot-cycle acquisition) executed before each release.

## 9. Milestones

| # | Deliverable | Acceptance |
|---|-------------|------------|
| M1 | Protocol port + CLI proof | Fans spin at commanded PWM; one static color on fans + Strimer; on owner's machine |
| M2 | Reliability core | Survives owner's cold-boot scenario for one week with zero manual restarts; watchdog script retired |
| M3 | Effect engine + animation-size probe | Ripple (and ≥5 further effects) running onboard on SL-INF + Strimer; measured frame budgets documented |
| M4 | UI | All four screens functional against live daemon; dedicated visual-design pass done |
| M5 | Full effect catalog + packaging + docs | All §3.2 v1 effects implemented; AUR package installs cleanly on a fresh CachyOS/Arch system; README device matrix + setup guide |

Each milestone is independently useful; M2 alone supersedes the current watchdog stopgap.

## 10. Risks

- **Animation flash limit smaller than hoped** → effects fall back to fewer frames (coarser motion) or post-v1 streaming path. Mitigated by measuring at M3 before UI promises smoothness.
- **Upstream rewrite lands with new protocol knowledge** → `llw-protocol` is the single integration point; port discoveries there. We lose nothing by their progress.
- **V2 dongle (1A86) untestable** (owner has V1 only) → keep V2 IDs and code paths from upstream port, mark "community-tested" in README, solicit testers.
- **Wireless LCD users expect LCD support** → README states the RF/USB split clearly; LCD is a labeled post-v1 milestone.

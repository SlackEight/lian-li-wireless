# M4a: Bind/Unbind + Air Inventory — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Device onboarding without any other software: `llw bind <mac>` (and the IPC the M4b UI will call) binds an unbound wireless device to our dongle, persists it to device flash, and auto-configures it with safe defaults — plus the supervisor's "air inventory" that sees every device on the air, not just configured ones.

**Architecture:** Same discipline as always — pure frame builders (byte-tested), Dongle I/O methods, ALL policy (convergence loop, refusal rules, auto-config) in the supervisor as step()-driven state so nothing blocks the control loop. IPC replies immediately; convergence is observed via Status.

**Reference:** upstream bind.rs at `git -C ~/lian-li-linux-pr show d262007:crates/lianli-devices/src/wireless/bind.rs` (165 lines — the implementer reads it; the semantics are restated exactly below). Spec: `docs/superpowers/specs/2026-07-14-m4-ui-design.md` §5.

**Baseline:** 218 tests (37 protocol + 79 daemon + 102 effects), clippy --all-targets clean. Live constraint: llw-daemon owns the dongles (soak in progress) — Tasks 1–5 are code-only; Task 6 is CLI hardware validation with the owner, via IPC (daemon keeps running — bind is now a daemon capability, no takeover).

---

## Protocol facts (from upstream, restated — the implementer verifies against the reference)

- **Bind frame** = the PWM frame layout (`rf[0]=0x12, rf[1]=0x10`) with re-addressed fields: `rf[2..8]=device_mac`, `rf[8..14]=TARGET master mac` (ours when binding, zeros when unbinding), `rf[14]=target_rx`, `rf[15]=master_channel`, `rf[16]=target_rx` (yes — the seq byte carries target_rx here, upstream line ~94), `rf[17..21]=device's CURRENT reported PWM` (keeps fans steady through the handover). USB packets addressed to the device's CURRENT channel + CURRENT rx_type.
- **Burst:** the frame is sent 6× with 30ms gaps.
- **Convergence:** GetDev until the device reports `master_mac==target && rx_type==target_rx` (bind) or is absent-or-zero-master (unbind); upstream deadline 5s, poll cadence 150ms; timeout is logged but NOT an error (firmware sometimes converges late).
- **SaveConfig frame:** `rf[0]=0x12, rf[1]=0x15`, `rf[2..8]=ff×6`, `rf[8..14]=master_mac`, `rf[14]=0xFF`; sent as USB packets with `packet[2]=master_ch, packet[3]=0xFF`; the WHOLE 4-chunk send repeated 3× with 200ms gaps. Persists bindings to device flash.
- **Unused RX selection:** lowest rx in 1..=14 not used by any device bound to our master; fallback 1.

---

### Task 1: Pure frame builders (`llw-protocol/src/frames.rs`)

- [ ] Add `bind_frame(device_mac, target_master, target_rx, master_channel, current_pwm) -> RfFrame` — implemented AS a call to `pwm_frame(device_mac, target_master, target_rx, master_channel, target_rx, current_pwm)`? NO — write it as its own function that documents the field reuse (`/// The bind command reuses the PWM frame layout with re-addressed fields (upstream bind.rs): the master-mac field carries the TARGET master and both rx and seq carry the target endpoint.`) and internally may delegate to pwm_frame with the right arguments — implementer's call; bytes are what matter.
- [ ] Add `save_config_frame(master_mac) -> RfFrame` per the facts above. Add `RF_SAVE_CONFIG: u8 = 0x15` to consts.rs.
- [ ] Byte tests (independent arithmetic, both builders): bind_frame full-layout pin incl. rf[16]==target_rx and pwm passthrough; unbind variant (zero master, rx 0); save_config_frame pins ff-dst/0xFF-rx/master placement + zero padding.
- [ ] Verify + commit: `feat(protocol): bind + save-config frame builders`

### Task 2: Dongle methods + unused-rx helper

- [ ] `llw-protocol/src/dongle.rs`: `send_bind_burst(&mut self, frame: &RfFrame, channel: u8, rx_type: u8)` — 6× send_rf_frame with 30ms gaps; `send_save_config(&mut self, master_mac: &[u8;6], master_channel: u8)` — builds the frame, sends the 4-chunk set 3× with 200ms gaps and `packet[3]=0xFF` (NOTE: this is send_rf_frame with rx_type=0xFF — reuse it; the 200ms inter-set gap is the only new mechanic).
- [ ] `llw-protocol/src/record.rs`: pure `pub fn unused_rx(records: &[DeviceRecord], our_master: &[u8; 6]) -> u8` (lowest 1..=14 not used by our-master-bound records; fallback 1) + tests (empty air → 1, holes filled, all-taken → 1, foreign devices ignored).
- [ ] FakeIo tests: burst writes exactly 24 USB packets (6×4) with correct headers; save_config writes 12 packets with rx 0xFF.
- [ ] Verify + commit: `feat(protocol): bind burst + save-config dongle ops, unused-rx helper`

### Task 3: Supervisor air inventory

- [ ] The supervisor currently drops records for unconfigured devices in `ingest_records`. Add `air: HashMap<[u8;6], AirEntry>` where `AirEntry { record: DeviceRecord, last_seen: Instant, bond: Bond }`, `enum Bond { Ours, Foreign, Unbound }` classified per record.master_mac vs `link.master_mac` (zeros → Unbound). Every ingest updates it; entries expire after 30s unseen (prune in poll step).
- [ ] `StatusData` grows `pub air: Vec<AirDeviceStatus>` (`mac, kind, bond, channel, rpm, fan_count, last_seen_s`). Existing `devices` list (configured runtime) unchanged. `llw status` prints an "on air" section for non-Ours entries when present.
- [ ] Sims: foreign + unbound records appear in air with right bond; expiry works (record stops appearing → gone after 30s of steps); configured-device flow untouched (existing sims green unmodified).
- [ ] Verify + commit: `feat(daemon): air inventory — every device on the air, classified`

### Task 4: Bind/unbind as supervisor policy + IPC

- [ ] IPC: `Bind { mac: String }`, `Unbind { mac: String }` request variants + envelope tests. Replies are immediate: `ok({"state":"started"})` or a refusal error.
- [ ] Refusal policy (the "bind lock") in answer(): Bind refused if — mac unknown to the air inventory ("not visible on air"), bond==Foreign ("bound to another controller — unbind it there first"), bond==Ours ("already bound"), a bind/unbind op already pending. Unbind refused unless bond==Ours.
- [ ] Pending-op state machine on the supervisor: `pending_bind: Option<BindOp>` where `BindOp { mac, target_master: [u8;6], target_rx: u8, deadline: Instant, save_sent: bool }`. On accepted Bind: compute target_rx via `unused_rx` over air records, build bind_frame from the air record's CURRENT channel/rx/pwm, `send_bind_burst`, set deadline now+5s. Each step() (in the normal poll phase — polls must keep running, so the settle-window does NOT engage here) checks the air record: converged (Ours + rx matches) → `send_save_config`, engage the RF settle window (rename `rgb_settle_until` → `rf_settle_until`, one mechanical rename commit-wide), auto-add DeviceConfig (slots: all four = first configured curve's name if any curve exists, else Percent(40); color None; effect None) + save config + build DeviceRuntime → clear pending. Deadline passed → re-burst ONCE more (upstream loops; we bound it: max 2 bursts total) then mark failed; failure is visible in Status (`pending` field: `{"op":"bind","mac":..,"state":"converging|failed"}` — clear failed state after 30s).
- [ ] Unbind mirror: burst with zero master/rx 0, converge on absent-or-zero-master, save_config, remove DeviceConfig + runtime + save.
- [ ] Sims (FakeIo scripts): successful bind end-to-end (burst bytes → scripted records flip to Ours → save-config bytes → config entry appears with curve default); bind timeout → failed state, no config entry; foreign refusal; unbind end-to-end removes config; concurrent-op refusal.
- [ ] Verify + commit: `feat(daemon): bind/unbind policy + IPC (refusal rules, convergence via step polls)`

### Task 5: CLI

- [ ] `llw bind <MAC>` / `llw unbind <MAC>` — IPC calls; on `started`, poll Status every 500ms up to 8s printing the pending state, then final verdict (bound/failed/still-converging). `llw status` air section from Task 3 verified in help/dry runs only.
- [ ] Verify + commit: `feat(cli): bind/unbind via daemon IPC`

### Task 6: Hardware validation (owner present; daemon stays up)

The SL-INF is bound and must stay that way; the honest test needs an unbound device. **If the Strimer is installed by now:** `llw status` (should show it on air as Unbound) → `llw bind <strimer-mac>` → verify convergence, config entry with 40%/curve default, SaveConfig persistence (power-cycle PSU later → still bound) → then `llw set-effect <strimer> ripple` = closes M3's deferred Strimer validation in the same session. **If not installed:** validate the refusal paths only (bind an already-bound mac → refused; bind a bogus mac → refused) and mark the live-bind test deferred-to-Strimer-install in this plan. Do NOT unbind the SL-INF as a test — recovery depends on the bind path we're testing.

- [ ] Record results in this plan + commit.

---

## Self-review notes (already applied)

- Convergence never blocks step(): burst is ~180ms inline (acceptable, same order as an RGB upload), everything else rides the normal poll cadence. The settle window engages only AFTER SaveConfig (flash write), mirroring the M3 discovery — and is deliberately NOT engaged during convergence polling (we need the polls).
- The rename rgb_settle_until→rf_settle_until touches the M3 settle sim — mechanical rename, sim semantics unchanged.
- Types: AirEntry/Bond (T3) consumed by T4's policy + StatusData; unused_rx (T2) takes records from the air inventory (T4 passes `air.values().map(|e| &e.record)` — collect as needed).
- Judgment pre-made: bind targets get ALL FOUR slots set (extra slots are zeroed by apply_pwm_constraints at send time anyway); auto-config uses the FIRST curve by Vec order (deterministic).

---

## Task 6 results (2026-07-14 evening)

**Refusal paths — validated LIVE against the running daemon:** `bind 02:8b:...` (bound SL-INF) → "already bound", exit 1 ✓; `bind aa:bb:...:99` (not on air) → "not visible on air", exit 1 ✓; `bind not-a-mac` → client-side format error ✓. Daemon healthy throughout (soak uninterrupted; ripple restored post-restart).

**Live bind test — DEFERRED to Strimer install** (the only honest unbound target; unbinding the SL-INF as a test was ruled out by plan). When the Strimer is physically installed: `llw status` (expect it on air, Unbound) → `llw bind <mac>` → verify convergence + auto-config + flash persistence (later PSU cycle) → `llw set-effect <idx> ripple` closes M3's deferred Strimer validation in the same five minutes.

M4a otherwise complete: 245 tests, clippy --all-targets clean, review loop included one Critical catch (re-burst bond re-check) now byte-sim-pinned.

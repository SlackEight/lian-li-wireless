# M4f: Link/Unlink via UI + per-set speed source (MB vs software) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal (owner request, 2026-07-20):** L-Connect parity for (1) linking/unlinking fan sets entirely from the UI — including the newly-discovered **Ours-but-unconfigured** case (a set already RF-paired to our master but absent from config: invisible in the UI and refused by the bind lock today; enrolled by hand via SetConfig), and (2) toggling a fan set between **motherboard-controlled** and **software-controlled** speed.

**Protocol findings (verified against upstream + live hardware 2026-07-20):**
- Upstream `fan_controller.rs:201-217`: wireless hardware MB-sync = sending PWM sentinel **[6,6,6,6]**; `fan_type.rs:85 supports_hw_mobo_sync()` whitelists SLV3 only.
- Live observation (owner's new single-fan SL-INF): a set receiving **no host PWM at all** follows its motherboard header when the wire is connected (readback 0, RPM tracks the header). The set receiving keepalives obeys RF. So "MB mode" may simply be traffic-absence + wire. Coordinator's live validation (Task 3) decides which send policy ships: sentinel, silence, or sentinel-then-silence.
- Bind lock refuses `Bind` on an Ours device ("already bound") — correct for FOREIGN protection, wrong for Ours-unconfigured adoption.

---

### Task 1: daemon — MbSync slots, speed_source in Status, adopt-on-Bind

- [ ] `config.rs`: `SlotSpeed` gains `MbSync`, serialized as the string `"mb"` (untagged order: number → `"mb"` → curve name; `validate()` refuses a curve named `"mb"`). Compat tests per house pattern.
- [ ] `fan.rs`/`supervisor.rs` policy for a device whose ACTIVE slots (first `fan_count`) are ALL `MbSync`: send the sentinel frame `[6,6,6,6]` on entering the mode and then at a slow keepalive (every 5s — cheap, explicit, and covers the case where the sentinel is what the firmware wants; Task 3 may amend to full silence). Mixed Percent/Curve+MbSync slots: treat the MbSync slots as Percent(0) and log a WARN once (per-slot MB isn't offered by the UI; config-level flexibility only).
- [ ] Reliability semantics in MB mode: NO dropout observations, NO surge tracking, NO drift-triggered sends for that device (readback/RPM are wire-driven; BIOS ramps would false-positive everything). DropoutFilter/SurgeTracker simply aren't fed (or fed with `commanded=false`).
- [ ] `ipc.rs` StatusData.DeviceStatus gains `speed_source: String` ("software" | "motherboard", derived from the device's slots; additive field).
- [ ] Adopt-on-Bind: `Bind{mac}` where the air inventory classifies the target **Ours** and the mac is NOT in config → synchronous config enrollment (mac, name None, slots `[0;4]`→ actually enroll with all-MbSync slots if the set is wire-connected... we cannot know — enroll with `Percent(0)` slots [uncommanded] and let the owner pick a mode in the UI; no RF traffic, no SaveConfig), reply ok. Unchanged: Unbound → full RF bind flow; Foreign → refusal.
- [ ] Unbind on an Ours-unconfigured... not reachable (unlink only shows for configured). Unbind for a configured MB-mode device: unchanged RF unbind (it releases the pairing; the wire keeps spinning the fans — note this in the UI copy of Task 2).
- [ ] Tests: slot serde round-trip incl. `"mb"`; validate refuses curve named "mb"; sim: all-MbSync device sends sentinel then no PWM churn, no dropout observations on zero readback, no surges on RPM ramps; adopt-on-Bind sim (air Ours + unconfigured → Bind → config gains entry, no RF frames sent); Status carries speed_source.
- [ ] Gates: `cargo test -p llw-daemon` green, workspace clippy zero warnings.
- [ ] Commit: `feat(daemon): mb-sync speed source + adopt-on-bind for paired-unconfigured sets`

### Task 2: UI — Link/Unlink everywhere + speed-source toggle

- [ ] Devices screen air section: currently filters `bond !== 'Ours'`. New rule: show rows for non-Ours (as today) AND for `bond === 'Ours' && mac ∉ configured` — label the state "paired, not linked", bloomed **Link** button (same `bind` command; the daemon now adopts). Foreign row unchanged.
- [ ] Copy sweep: user-facing "Bind/Unbind" becomes **Link/Unlink** (buttons, dialogs, toasts, converging labels; protocol/code names unchanged). Unlink dialog for an MB-mode device notes the fans keep spinning under motherboard control.
- [ ] Device card gains a **Speed** control: segmented "Software / Motherboard" from `speed_source`; switching to Motherboard sets all active slots to `"mb"`; switching to Software restores `Percent(40)` on active slots (with a toast pointing at Cooling to refine); both via the config round-trip, applied immediately with a success toast.
- [ ] Cooling screen: MB-mode devices' slot rows render "motherboard controlled" (selects disabled, RPM still live); switching modes lives on the Devices card only.
- [ ] Health screen: MB-mode devices show "MB" in the desired/readback columns instead of misleading percentages (`speed_source` from status).
- [ ] Store types: `DeviceStatus.speed_source?: string` (additive-optional).
- [ ] Vitest: air-row visibility logic (Ours-unconfigured shows Link, Ours-configured hidden, Foreign dimmed) as a pure helper + tests; speed-source config mutation helpers (all-slots-mb, restore-software) + tests.
- [ ] Gates: `npm run test`, `check`, `build` green.
- [ ] Commit: `feat(ui): link/unlink parity + per-set speed source toggle`

### Task 3: coordinator — live validation + deploy (owner's new fan is the test subject)

- [x] Deploy daemon. **ALL PASSED (2026-07-20):** New fan (49:8b:62:62:32:e1, wire-connected, currently uncommanded): (a) set Software/40% → confirm RF takes over (readback 102, steady RPM ≈40% decoupled from BIOS curve); (b) toggle Motherboard → confirm sentinel/silence hands speed back to the wire (RPM re-tracks header); (c) confirm zero dropout/surge accumulation across 5 min in MB mode; (d) Link flow end-to-end: unlink the new fan via UI, watch it reappear as "paired, not linked", Link it back. Record which send policy the firmware actually honors (sentinel vs silence) and amend Task 1's policy if needed.
- [x] Results: (a) Software 40% → RF took over in ~4s (readback 102, steady ~860). (b) **Sentinel FALSIFIED for SL-INF**: [6,6,6,6] is accepted as a literal PWM write and keeps the host session alive — the master held stale RF speed indefinitely; policy amended to TOTAL PWM SILENCE, after which the wire took over (RPM tracked the BIOS curve smoothly; the readback register cosmetically holds the last RF write — Health shows "MB" regardless). (c) Zero dropouts/surges/stalls throughout MB-mode observation. (d) Adoption round-trip PASSED (config-remove → adoptable → Bind adopts; first attempt refused "radio settling" because SetConfig's RGB re-assert opens the settle window — the UI's auto-retry covers this, scripts must too). (e) **Live RF unbind+bind PASSED** — M4a's deferred validation: first unbind attempt failed convergence cleanly (ch8 noise ate the burst; no state damage), retry converged in 1s; re-bind converged in 1s and auto-enrolled with the first curve per the M4a design. Post-bind slots restored to MB.
- [x] Commit: `docs: M4f live validation`

### Task 4: acceptance

- [ ] Owner walk: link/unlink + both toggle directions from the UI. Record verdicts; update README feature list.

---

## Self-review notes

- The bind lock's Foreign protection is untouched — adoption only triggers on Ours+unconfigured, which cannot collide with someone else's network by construction.
- MB mode silences the reliability machinery per device rather than globally — the software-controlled cluster keeps full watchdog coverage.
- The sentinel-vs-silence uncertainty is isolated to one constant + one send-policy branch; Task 3 resolves it empirically before the owner ever touches the toggle.
- `"mb"` as a reserved curve name is validated at config load AND SetConfig, so the Cooling screen can't create the collision.

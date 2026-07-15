# M4b: Tauri Shell + Health + Devices — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The `llw-ui` desktop app exists, opens on the owner's KDE Wayland desktop wearing Nocturne Charged, and its Health + Devices screens work against the live daemon — including bind/unbind from the UI. (Lighting/Cooling are M4c/M4d; they appear as styled placeholder sections.)

**Architecture:** Spec `docs/superpowers/specs/2026-07-14-m4-ui-design.md` §3. `crates/llw-ui` = the Tauri 2 Rust crate (thin IPC client exposed as commands, zero policy); `crates/llw-ui/ui/` = Svelte 5 + Vite + TypeScript frontend. One-shot socket connections per request (same as the CLI). Status polled at 1s while the window is visible.

**Design reference (binding):** the owner-approved mockups in `.superpowers/brainstorm/48378-1784033233/content/` — `visual-style-v2.html` (Nocturne Charged card = the design token source), `app-shell.html` option A (section sidebar). Tokens: bg `#0b0b0f`, surface `#141419`, hairline `rgba(255,255,255,0.07)`, text `#f2f2f5`/`#a9a9b3`/`#6e6e78`, accent = device RGB with bloom (`box-shadow` glows in the device's colors), system font stack, 14px base. The ONLY saturated color on screen comes from device RGB or state (green `#7ee2a8` for healthy, amber for converging, red for failed/wedge).

**Risk to burn down FIRST:** Tauri 2 / webkit2gtk on KDE Wayland — Task 1 ends with a window on the owner's screen before anything else is built.

**Baseline:** 245 tests, clippy --all-targets clean. Daemon running (soak). npm/node availability unknown — Task 1 checks and installs (passwordless sudo; pacman).

---

### Task 1: System deps + scaffold + Wayland smoke (owner glances at a window)

- [ ] Check/install prerequisites: `node`/`npm` (pacman: `nodejs npm` if missing), Tauri 2 system libs per Tauri's Arch docs (`webkit2gtk-4.1 base-devel curl wget file openssl gtk3 librsvg`— install only what's missing; document what was installed), `cargo install tauri-cli --locked` if `cargo tauri` absent (or use `npm create tauri-app` scaffolding equivalents manually — prefer manual scaffold for control, see next).
- [ ] Scaffold MANUALLY (no create-tauri-app template noise): `crates/llw-ui/` Rust crate (workspace member via existing glob; `tauri = "2"`, `tauri-build = "2"`, `serde`, `serde_json` deps; `tauri.conf.json` with `productName: "llw"`, identifier `dev.slackeight.llw`, window 1100×720 min 900×600, `frontendDist: "ui/dist"`, `devUrl: "http://localhost:5173"`); `crates/llw-ui/ui/` Vite + Svelte 5 + TS (`npm create vite@latest` svelte-ts template, or manual package.json — smallest working). A `main.rs` with `tauri::Builder::default().run(...)` and one `#[tauri::command] fn ping() -> &'static str { "pong" }`. Frontend: black page, "llw" wordmark, calls ping and shows the reply.
- [ ] Workspace hygiene: `cargo build` for the whole workspace must still work (tauri deps are heavy — first build is minutes); frontend `npm install` + `npm run build`; `.gitignore` gains `crates/llw-ui/ui/node_modules` + `ui/dist`.
- [ ] **Owner smoke:** `cargo tauri dev` (or `cargo run -p llw-ui` with built frontend) — the owner confirms a window opens on KDE Wayland showing the wordmark + pong. Record any Wayland quirks (decorations, scaling) in this plan.
- [ ] Commit: `feat(ui): Tauri 2 + Svelte 5 scaffold, Wayland smoke passed`

### Task 2: Nocturne Charged foundation + shell

- [ ] `ui/src/lib/theme.css`: design tokens as CSS custom properties (colors above, spacing scale, radius 14px cards / 6-8px controls, bloom shadow mixins as utility classes `.bloom` parameterized by `--glow-color`). Base styles: dark bg, font stack, focus rings (accessible but on-theme).
- [ ] Shell per mockup A: fixed 200px sidebar (wordmark top; Lighting/Cooling/Devices/Health items with icons — inline SVG, no icon lib; active item gets the subtle purple-tinted pill from the mockup), content area. Svelte 5 runes state for active section; no router lib.
- [ ] Sections: Health + Devices real (empty shells for now); Lighting/Cooling render an on-theme placeholder ("arrives in M4c/M4d" with a dimmed preview silhouette).
- [ ] Daemon-unreachable banner component (slides down, amber, "daemon unreachable — retrying"; content dims but stays rendered) — wired in Task 4.
- [ ] Vitest setup + first store test scaffold. Frontend `npm run build` + `npm run test` green.
- [x] **Owner glance:** shell up in dev mode, clicks the four sections, confirms it reads as the mockups' language. — PASSED 2026-07-15 ("Yup, everything looks right"), after the React port; see post-mortem below.
- [x] Commit: `feat(ui): Nocturne Charged theme + section shell` (3b616ed, includes the React port)

### Task 3: IPC client (Rust) + Tauri commands

- [x] `crates/llw-ui/src/ipc.rs`: one-shot request fn (connect `$XDG_RUNTIME_DIR/llw-daemon.sock`, send envelope v1 line, read reply line, parse `ResponseEnvelope`-shaped JSON) — mirror the CLI's proven code; typed error for unreachable-socket (front end distinguishes "daemon down" from "request failed").
- [x] Commands: `status()`, `bind(mac)`, `unbind(mac)`, `set_effect(mac, spec)`, `set_color(mac, rgb, brightness)`, `get_config()`, `set_config(json)` — all returning `Result<serde_json::Value, String>` (the daemon's error strings pass through verbatim; the UI renders them).
- [x] Tests: spawn a real `UnixListener` in-test serving canned envelope replies → each command round-trips; unreachable socket → the typed error. (`cargo test -p llw-ui`.)
- [x] Commit: `feat(ui): daemon IPC client + Tauri commands` — 14 tests; commands split into src/commands.rs with path-injected `*_at` twins; unreachable errors carry the stable prefix `daemon unreachable` (frontend matches on it in Tasks 4/5); garbled replies classify as unreachable

### Task 4: Status store + Health screen (live data)

- [x] `ui/src/lib/stores/status.ts` (React port: framework-free store + useSyncExternalStore hook): poll `status` every 1s while `document.visibilityState === 'visible'` (pause hidden); exposes typed state (mirror StatusData incl. air + pending + reliability + link), `daemonReachable` flag driving the banner; exponential backoff to 5s while unreachable.
- [x] Health screen per spec: link card (master mac, channel, wedge banner if tx_wedged — red, prominent), reliability card (dropouts/tier1/tier2/streak with quiet numerals, sparkline optional-skip), per-device sync cards (rgb_sync badge green/amber, dropout streak, desired vs readback PWM, RPM). All state colors per the token rules.
- [x] Vitest: store polling/backoff logic (mock invoke), visibility pause. — 12 store tests, injected timers/visibility, no jsdom. Coordinator fixes: PWM bytes (0–255) now render as % (were shown raw with a % suffix). Unreachable banner + dim verified by headless screenshot (browser dev = no Tauri bridge = natural daemon-down).
- [ ] **Owner glance:** Health screen live against the running daemon.
- [ ] Commit: `feat(ui): status store + Health screen`

### Task 5: Devices screen + bind/unbind UI

- [x] Configured devices: cards in the mockup's device-card style (name/mac/kind, fan count, RPM ring visual — static ring with RGB conic fill from current effect colors if available in config, else neutral; rename inline-edit → `set_config` with the device's `name`), unbind action behind a confirm dialog ("removes it from config and releases it on air").
- [x] Air section: non-Ours entries as rows — Unbound get a glowing **Bind** button (the ONE call-to-action with bloom); Foreign shown dimmed with "bound to another controller" and a disabled button (tooltip explains). Bind click → `bind(mac)` → progress states from `pending` in the status store (converging spinner → bound ✓ transitions the row into the configured list) — the daemon's refusal strings surface as inline toasts verbatim (incl. the auto-retried settling case: the Rust command does NOT retry; the UI shows "radio settling — retrying…" and re-invokes up to 3× 2s like the CLI).
- [x] Vitest: bind-flow state machine in the store (14 tests; settling marker matched to the CLI's; 15s convergence timeout) (started → converging → success/failed paths against mocked status sequences).
- [ ] **Owner session (live):** DEFERRED to the final iteration pass / Strimer install (owner 2026-07-15: complete everything, single pass at the end) — Devices screen against the real daemon — refusal toast on binding a bogus/bound mac; if the Strimer is installed by now, THE live bind happens here from the UI (closing M4a Task 6's deferral + M3's Strimer validation via a follow-up set-effect from the CLI or a temporary button). Record results.
- [x] Commit: `feat(ui): Devices screen — bind/unbind with live convergence`

### Task 6: Acceptance + record

- [ ] Owner walks all four sections (two live, two placeholders) in dev mode; visual check against the mockup files; note the punch list for M4e polish rather than perfecting now. Record results + screenshots (owner-provided or skip) in this plan; update README with a UI section (dev instructions only — packaging is M5).
- [ ] Commit: `docs: M4b acceptance results`

---

## Self-review notes (already applied)

- Wayland risk fronted (Task 1 gate); heavy Tauri first-build noted; npm/node presence checked not assumed.
- The UI never implements policy: refusal strings pass through verbatim; the settling retry mirrors the CLI's documented UX.
- StatusData is consumed as JSON (no shared-crate type coupling for the frontend; the Rust command layer passes Values through) — the envelope version gate protects shape drift.
- Placeholders for M4c/M4d keep the shell honest without blocking on the big screens.
- Owner-glance gates are cheap (they're at the PC and enjoy visual checkpoints — workstyle memory) and match the "hardware/visual" interaction rule.

---

## Task 1 Wayland smoke — PASSED (2026-07-14 night, with one quirk)

First launch crashed: `Gdk-Message: Error 71 (Protocol error) dispatching to Wayland display` — webkit2gtk's DMABUF renderer vs NVIDIA on Wayland. Fix: `WEBKIT_DISABLE_DMABUF_RENDERER=1`, now set programmatically in main.rs (Wayland-only, respects user override). Also fixed: scaffold missed beforeDevCommand (Vite wasn't auto-started). With both fixes the window opens and renders correctly on KDE Plasma 6 Wayland — owner confirmed ("I see llw and pong"). Decorations/scaling: no issues reported.

## Task 2 — post-mortem + framework switch to React (2026-07-14 night)

Owner glance FAILED on first pass: sidebar didn't reach the bottom, Health rendered below it. Diagnosis (headless-chromium screenshots + curl): in dev mode, vite-plugin-svelte served the **raw .svelte source as App.svelte's CSS module** (`?svelte&type=style&lang.css` returned the file verbatim), so the shell's scoped styles were dropped — production builds were unaffected. Reproduced on both vite 6 + plugin 5 and vite 8 + plugin 7. Trigger: importing `theme.css` from inside App.svelte's `<script>` block; moving the import to the entry file fixed dev rendering (verified by screenshot).

The owner then asked (twice) for a mainstream framework. Decision: **switch the frontend to React 19 + Vite 7** while the surface is small (shell + placeholders, one logic module, 6 tests). Port kept everything byte-comparable: theme.css untouched, component styles consolidated verbatim into `src/lib/components.css`, markup to TSX, sections.ts/tests unchanged. Verified: `npm run build` + `vitest` 6/6 + `tsc --noEmit` green; dev-server screenshot pixel-matches the intended shell. All remaining M4 tasks build on React.

## Task 6 — coordinator acceptance walk (2026-07-15, owner walk deferred to M4e-E2)

Owner directive 2026-07-15: complete the whole UI autonomously, single owner iteration pass at the end. Task 6's owner walk therefore folds into M4e-E2; this is the coordinator's walk.

All four sections screenshot-verified via headless chromium against the dev server (browser = no Tauri bridge, so this also exercises the unreachable banner + dim on every section): shell/sidebar correct, Health cards, Devices waiting-state, Lighting/Cooling silhouettes all on-theme. Health was additionally verified LIVE against the real daemon at the Task 4 owner glance ("Looks good"). Added `?section=<name>` deep-link for tooling/screenshots. README gained the UI section. Gates at close: 36 vitest, tsc clean, build clean, workspace Rust suites green (259 + 3 presets tests).

Punch list carried to M4e: none yet from layout; live-data Devices walk + live bind = E2/Strimer.

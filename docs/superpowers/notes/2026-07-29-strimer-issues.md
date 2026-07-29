# Strimer RGB issues — symptom report + hypotheses (deferred; UI redesign first)

**Owner report (2026-07-29):** both Strimers bound and lighting, but: they take *forever* to switch
lighting; only *some* effects work; one accepted meteor, the other only accepts static colour.

**Hardware:** `06:ed:07:f5:17:e1` and `be:48:64:88:19:e1`, both `Strimer Wireless`, both Ours +
configured. Config at report time: `06:…` effect=static, `be:…` effect=meteor. Both rgb_in_sync=true.

## Hypotheses, cheapest first (test in this order)

1. **Subtype LED-count mismatch → oversized payload → fail-safe.** `device_kind.rs led_count_override()`
   maps device_type 1/2/3/9 → 116/132/174/88 LEDs. If the two Strimers are different subtypes, the
   larger one (174) may exceed a firmware ceiling and fail safe to a flat/zeroed effect — matching
   "only accepts static colour". M3 measured the SL-INF flash ceiling at ~38-44 KB raw (96 frames x
   132 LEDs PASS, 112 FAIL); the Strimer's ceiling is UNMEASURED. Check each device's actual
   device_type + resulting frame budget `clamp(28000/(leds*3), 8, 96)` (174 LEDs -> 53 frames).
2. **Geometry semantics, not failure.** A Strimer is a linear strip: `Geometry::Strip`. Radial
   effects (notably **ripple**, which is radius-driven) degenerate on a strip where every LED shares
   one radius - the whole strip pulses uniformly, which reads as "doesn't work" rather than an error.
   Directional effects (meteor/runway/rainbow) sweep along the strip and look right. This alone could
   explain "only some effects work" with zero bugs involved.
3. **Upload duration + settle serialisation** explains "forever to switch": 116-174 LEDs x up to 53
   frames is a much larger payload than a 44-LED fan, and every upload is followed by the mandatory
   ~3 s RF-silence flash commit, serialised across devices.
4. **UI blind spot (confirmed, ours):** `stage.ts geometryForKind()` returns **null** for Strimer
   deliberately, because Status exposes only the display-name string which drops the subtype - so the
   UI cannot preview or reason about Strimers at all. Fix = expose device_type/led count in Status.
   This is on the redesign's critical path anyway.

## Test plan (when we get to it)
Read each Strimer's device_type from its GetDev record; compute its budget; apply the same effect to
both and compare; try one directional and one radial effect on each; time each apply end to end;
watch for the zeroed-effect fail-safe signature in the effect-index echo.

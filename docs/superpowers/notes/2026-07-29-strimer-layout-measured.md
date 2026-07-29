# Strimer LED layout — MEASURED (2026-07-29)

Settled by lighting patterns and asking the owner what they saw, the same way
the SL-INF 44-LED layout was derived on 2026-07-14. Tool:
`llw-protocol/examples/strimer-probe.rs`.

## Result

**A Strimer is a 6 x N GRID, not a strip.**

- **6 logical channels**, each driving **2 adjacent visible light guides** — so the
  cable shows 12 threads but only ever had 6 addressable ones.
- LEDs run **along** the cable within a channel, N positions per channel.
- **Channel 1 is the bottom-most band**; channel index increases upward.
- **Index 0 is at the FAR end** (away from the connector); indices increase
  toward the connector.

| device | mac | device_type | LEDs | layout |
|---|---|---|---|---|
| 24-pin | 06:ed:07:f5:17:e1 | 2 | 132 | 6 x 22 |
| GPU    | be:48:64:88:19:e1 | 3 | 174 | 6 x 29 |

**This dissolves the "174 is not divisible by 12" problem** that made us doubt the
LED count: it was never 12 channels. 132 = 6x22 and 174 = 6x29 both divide cleanly.
`led_count_override`'s totals are correct; only our idea of the arrangement was wrong.

## Evidence
1. `blocks a 11` — twelve 11-LED colour runs. Owner saw six bands, each TWO guides
   wide, each band changing colour once about halfway along. Two blocks per band x
   six bands = twelve blocks: a block is half a channel, so a channel is 22 LEDs
   and spans two guides. Colour order matched 6x22 strand-major exactly, and only
   with channels ordered bottom-up and indices running far-end-to-connector.
2. `pixel a 0` — predicted "far end, bottom band, both guides of that band" before
   running. Owner confirmed bottom, far end. (Partially obscured: that section of
   their 24-pin loops behind the motherboard tray.)

## Why this matters more than it looks
`effects_bridge` compiles Strimer effects against `Geometry::Strip` — one line of
N LEDs. On a real 6xN grid a strip-rendered meteor runs the full length of channel
1, jumps back to the start for channel 2, and so on: what you SEE is each band
lighting in turn, not anything travelling along the cable. That is very likely the
whole of "only some effects work on the strimers" — the effects are fine, the
geometry is wrong. Ripple has the same problem in reverse: every LED shares one
radius on a Strip, so it degenerates to a uniform pulse.

**Fix:** give `llw-effects` a `StrimerGrid { channels: 6, per: N }` layout with
per-LED (x = position along cable, y = channel) and let the existing 2D effect
maths do the rest — exactly what turned SL-INF from "mostly right" into "seamless
and smooth" in M3. Then meteor/runway sweep along the cable, rainbow can run across
the six channels, and ripple can radiate from the connector.

## Second finding, same session: uploads need verifying, not firing
The first `blocks` upload SILENTLY DID NOTHING — the device kept showing the old
frame, no error. `06:ed` is the flaky one (it failed at every gap in the settle
experiment, and it is the device the owner reported "only accepts static colour").
Adding upload -> poll the echoed `effect_index` -> resend until it matches made the
probe reliable immediately. The daemon's RGB path fires once and assumes success;
it needs the same read-back-and-retry. This, not any payload ceiling, is the most
likely cause of "the effect will not change".

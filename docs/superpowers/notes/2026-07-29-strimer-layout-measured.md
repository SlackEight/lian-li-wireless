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


---

## Corroborated against published sources + OpenRGB (same day)

The owner rightly asked whether this was already documented. It is, and it agrees:

- **Lian Li's own product page**: effects can be set "on each channel **(2 light strips)**" —
  the channel-drives-two-guides model, stated in marketing copy all along.
- **Retail specs**, 24-pin: "**6 channels** for individual lighting control".
- **SRGBmods teardown**: the ATX 24-pin Strimer carries **6 data lines** (the dual-8-pin GPU
  cable has 4; the triple-8-pin has 6).
- **OpenRGB `LianLiStrimerLConnectController`** states it in code:
  `STRIMERLCONNECT_STRIP_COUNT = 12`, split as 6 zones x 20 LEDs named "24 Pin ATX Strip 0..5"
  and 6 zones x 27 LEDs named "8 Pin GPU Strip 0..5", all `ZONE_TYPE_LINEAR`.
  Also `BRIGHTNESS_MAX = 4` — the same 0-4 brightness index our RF protocol uses, so that
  scale is a Lian Li house convention rather than a wireless quirk.

### The counts genuinely differ between wired and wireless — ours are right
Wired L-Connect: 120 (6x20) and 162 (6x27). Our wireless devices: 132 (6x22) and 174 (6x29).
Exactly +2 LEDs per channel on both. **Verified on hardware**: lighting only indices 162..173
on the wireless GPU cable lit the last channel's final stretch, from the connector back about
40% of its length. Those LEDs exist. `led_count_override` is correct; the wireless variants
simply carry two more LEDs per strip than the wired ones.

### Firmware VALIDATES frame length — over-length uploads are silently refused
Sending 186 LEDs to the 174-LED device failed four consecutive verified attempts; 174 was
accepted immediately. 186 LEDs is 558 bytes against a 55,880-byte ceiling, so this is a
length check, not a payload limit.

**Consequence, and the best explanation yet for "this Strimer only accepts static colour":**
if our LED count for a device is ever too high, EVERY animation upload is refused, forever,
with no error — while a static colour set through another path still lands. A device whose
count we get wrong looks exactly like a device that "won't take effects".

Worth building: a startup count-probe that walks the frame length down until the device
accepts, measuring the true LED count with no user involvement.

### Index direction, confirmed on both cables
Index 0 sits at the FAR end; indices increase toward the connector. The highest-numbered
channel is the outermost guide pair (right-most on the GPU cable as mounted).

# The 3s settle window is ~3x more than the firmware needs (measured 2026-07-29)

Owner challenged the ~4.5s-per-device apply cost ("L-Connect certainly isn't that slow. Are we not
being overly generous?"). They were right. Measured on the live 5-device rig with
`llw-protocol/examples/settle-probe.rs`, daemon stopped.

## Method
Upload a DISTINCT payload to every device back to back with a fixed inter-upload gap, then poll GetDev
until every device echoes the `effect_index` we computed for it. All five matching = that gap suffices.

## Results

Single-frame static payloads (~14 B):

| gap | result | whole rig confirmed |
|---|---|---|
| 0 ms | fail (4/5) | — |
| 150 ms | fail (4/5) | — |
| **400 ms** | **pass** | **2.63 s** |
| 1000 ms | pass | 5.63 s |
| 3000 ms | pass | 15.63 s |

Full animations at each device's real budget (~27 KB each):

| gap | result | whole rig confirmed |
|---|---|---|
| 0 ms | fail (2 missed) | — |
| 150 ms | fail (1 missed) | — |
| 400 ms | fail (1 missed) | — |
| **1000 ms** | **pass** | **6.25 s** |
| 3000 ms | pass | 16.25 s |

## Conclusions
1. **Whole-rig apply is ~6.25 s for animations and ~2.6 s for static, not ~22 s.** The current design
   holds ALL RF traffic quiet for 3 s after EVERY upload (`RGB_SETTLE`, global). Replace with per-device
   pacing of ~1 s and pipeline the uploads: transmit device B while device A commits. One radio on one
   channel means transmissions cannot literally overlap, but the COMMIT WINDOWS can, and that is where
   the 3.5x came from.
2. 400 ms missed only one device — with retry-on-miss the animation case likely lands near 3 s.
3. Caveat: measured with the daemon stopped, so no competing GetDev polls. Real-world figure will be
   slightly worse until the RGB-only strip removes PWM traffic; a batch should also pause polling.
4. **Over-budget uploads fail SILENTLY.** Forcing 70 frames on the 174-LED Strimer (36.5 KB, over the
   28 KB budget) failed at every gap including 3 s, always the same device — while the same device
   passed at its correct 53-frame budget. This is the most likely mechanism behind the owner's
   "one Strimer only accepts static colour" report: something asked it for more than it can hold and it
   fell back rather than erroring. See 2026-07-29-strimer-issues.md hypothesis 1 — now strongly supported.
5. Both Strimers DO accept full animations at their correct budgets. The hardware is fine.

## Design consequence
The BACKPLATE spec's elaborate apply-latency machinery (per-device 3.000 s countdown rings as a
centrepiece, "make the wait beautiful") is over-built for a 6 s operation. Keep the honest per-device
progress and the confirm pulse; drop the ceremony.

Bug found en route: `Dongle::discover_master()` returned a master MAC that does not exist
(`bc:59:e4:e5:66:e4` vs the real `e5:ba:f0:72:ab:3c`) — GET_MAC answers on every channel and first-hit
discovery picked up junk. Every probe and tool should derive master+channel from GetDev device records
instead, as the daemon's acquisition already does.

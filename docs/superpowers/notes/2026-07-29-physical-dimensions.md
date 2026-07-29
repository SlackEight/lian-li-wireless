# Physical reference dimensions for the 3D rig view

Sourced 2026-07-29 for accurate PLACEMENT. The render is deliberately stylised; the geometry is not.

## Chassis — Phanteks NV5 (our reference mid-tower)
- External: **239 (W) x 477 (D) x 528 (H) mm**
- Fan mounts: **3x120 roof · 3x120 side · 1x120 rear · 1x120 floor** (8 positions)
- Radiators: 360 roof · 360 side · 120 rear; top panel has a dedicated **63 mm** radiator space
- Max GPU length **440 mm**; max CPU air cooler **180 mm**
- Motherboard: E-ATX up to **280 mm** wide, ATX, mATX, ITX
- Note: the NV5 is a showcase case — glass front + left corner, side intake rather than front intake.
  Our builder stays generic and keeps a front mount option for other cases.
Sources: phanteks.com/product/nv5-black, newegg NV5 MK2 listing, kitguru NV5 review.

## Fans — Lian Li UNI FAN SL-INF 120 (the owner's fans)
- **120 x 122.1 x 25 mm** per module. The 122.1 mm height is the interlocking UNI FAN frame — modules
  daisy-chain edge to edge, so a 3-fan set spans **360 mm** and mounts as one unit.
- 44 addressable LEDs per fan in five segments (measured 2026-07-14): inner ring 8 (r 0.70),
  outer arcs 10 + 10 (r 1.00), side strips 8 + 8 (x = +/-1.15).
Source: lian-li.com/product/uni-fan-sl-inf-wireless, coolerguys SL-INF120 listing.

## GPU — the owner runs an RTX 5090
- Founders Edition: **304 x 137 x 48 mm**, 2-slot.
- AIB partner 5090s are substantially larger — commonly ~330-360 mm long and 3-4 slots (~60-80 mm).
  For a generic builder a sensible default is ~330 x 140 mm at 3 slots, user-adjustable later.
Source: techpowerup RTX 5090 FE review, nvidia marketplace listing.

## Motherboard — standard ATX
- **305 x 244 mm**. Tower-mounted the 305 mm edge is VERTICAL and 244 mm runs front-to-back.
- Rear I/O cluster sits at the TOP of the rear edge; I/O shield aperture is 158.75 x 44.45 mm.
- Expansion-slot pitch is **20.32 mm** (0.8 in) — exact, from the PCI/PCIe bracket standard.
  Primary x16 slot centreline sits roughly 45 mm below the board's top edge.

## Handedness (the bug this note exists to prevent)
Viewed from the FRONT of a standard ATX tower: rear I/O and the motherboard tray are on the **RIGHT**,
the main glass viewing panel is on the **LEFT**, and the cable channel is behind the tray between the
tray and the right side panel. (Equivalently: viewed from the REAR, the I/O is on the left.)

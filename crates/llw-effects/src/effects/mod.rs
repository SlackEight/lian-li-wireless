pub mod stat;
pub mod breathing;
pub mod color_cycle;
pub mod rainbow;
pub mod meteor;
pub mod runway;

use crate::{EffectSpec, EffectKind};
use crate::geometry::Geometry;

/// Dispatch a render call to the appropriate effect module.
///
/// Unimplemented effects (Task 4 — Ripple) fall back to Static so every
/// EffectKind is usable end-to-end at every commit.
pub fn dispatch(spec: &EffectSpec, geom: &Geometry, t: f32) -> Vec<[u8; 3]> {
    match spec.kind {
        EffectKind::Static       => stat::render(spec, geom, t),
        EffectKind::Breathing    => breathing::render(spec, geom, t),
        EffectKind::ColorCycle   => color_cycle::render(spec, geom, t),
        EffectKind::RainbowMorph => rainbow::render_morph(spec, geom, t),
        EffectKind::Rainbow      => rainbow::render_rainbow(spec, geom, t),
        EffectKind::Meteor       => meteor::render(spec, geom, t),
        EffectKind::Runway       => runway::render(spec, geom, t),
        // Task 4: replace with ripple::render(spec, geom, t)
        EffectKind::Ripple       => stat::render(spec, geom, t),
    }
}

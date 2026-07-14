pub mod stat;
pub mod breathing;
pub mod color_cycle;
pub mod rainbow;

use crate::{EffectSpec, EffectKind};
use crate::geometry::Geometry;

/// Dispatch a render call to the appropriate effect module.
///
/// Unimplemented effects (Tasks 3-4) fall back to Static so every EffectKind
/// is usable end-to-end at every commit.
pub fn dispatch(spec: &EffectSpec, geom: &Geometry, t: f32) -> Vec<[u8; 3]> {
    match spec.kind {
        EffectKind::Static      => stat::render(spec, geom, t),
        EffectKind::Breathing   => breathing::render(spec, geom, t),
        EffectKind::ColorCycle  => color_cycle::render(spec, geom, t),
        EffectKind::RainbowMorph => rainbow::render_morph(spec, geom, t),
        // Tasks 3-4: replace each arm with its own module call.
        EffectKind::Rainbow
        | EffectKind::Meteor
        | EffectKind::Runway
        | EffectKind::Ripple => stat::render(spec, geom, t),
    }
}

pub mod stat;

use crate::{EffectSpec, EffectKind};
use crate::geometry::Geometry;

/// Dispatch a render call to the appropriate effect module.
///
/// Unimplemented effects (Tasks 2-4) fall back to Static so every EffectKind
/// is usable end-to-end at every commit.
pub fn dispatch(spec: &EffectSpec, geom: &Geometry, t: f32) -> Vec<[u8; 3]> {
    match spec.kind {
        EffectKind::Static => stat::render(spec, geom, t),
        // Tasks 2-4: replace each arm with its own module call.
        EffectKind::Breathing
        | EffectKind::ColorCycle
        | EffectKind::RainbowMorph
        | EffectKind::Rainbow
        | EffectKind::Meteor
        | EffectKind::Runway
        | EffectKind::Ripple => stat::render(spec, geom, t),
    }
}

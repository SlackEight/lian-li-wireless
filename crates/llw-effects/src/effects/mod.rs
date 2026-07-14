pub mod stat;
pub mod breathing;
pub mod color_cycle;
pub mod rainbow;
pub mod meteor;
pub mod runway;
pub mod ripple;

use crate::{EffectSpec, EffectKind};
use crate::geometry::Geometry;

/// Dispatch a render call to the appropriate effect module.
///
/// Every `EffectKind` now renders its own algorithm — no fallback stubs remain.
pub fn dispatch(spec: &EffectSpec, geom: &Geometry, t: f32) -> Vec<[u8; 3]> {
    match spec.kind {
        EffectKind::Static       => stat::render(spec, geom, t),
        EffectKind::Breathing    => breathing::render(spec, geom, t),
        EffectKind::ColorCycle   => color_cycle::render(spec, geom, t),
        EffectKind::RainbowMorph => rainbow::render_morph(spec, geom, t),
        EffectKind::Rainbow      => rainbow::render_rainbow(spec, geom, t),
        EffectKind::Meteor       => meteor::render(spec, geom, t),
        EffectKind::Runway       => runway::render(spec, geom, t),
        EffectKind::Ripple       => ripple::render(spec, geom, t),
    }
}

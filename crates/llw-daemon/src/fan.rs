//! Per-tick fan decisions (pure): config slots → desired PWM bytes,
//! and the keepalive/send policy ported from upstream's fan_speed.rs
//! (policy lives here, not in llw-protocol — by design).

use crate::config::{DeviceConfig, SlotSpeed};
use crate::curve::percent_to_pwm;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Resolve a device's 4 slots to raw PWM bytes given each curve's current
/// speed %. Constraints (min duty etc.) are applied later by
/// `llw_protocol::frames::apply_pwm_constraints`.
pub fn resolve_slots(dev: &DeviceConfig, curve_pct: &HashMap<String, f32>) -> [u8; 4] {
    let mut pwm = [0u8; 4];
    for (i, slot) in dev.slots.iter().enumerate() {
        pwm[i] = match slot {
            SlotSpeed::Percent(pct) => percent_to_pwm(*pct as f32),
            // Only reached in MIXED configs (mb_mode false): per-slot MB sync
            // is not supported, so the slot resolves as off. The supervisor
            // WARNs once per config application (build_runtimes).
            SlotSpeed::MbSync => 0,
            SlotSpeed::Curve(name) => {
                percent_to_pwm(curve_pct.get(name).copied().unwrap_or(0.0))
            }
        };
    }
    pwm
}

/// Device-level speed source (M4f): a device is motherboard-synced iff at
/// least one slot is MbSync and NO slot commands a software speed
/// (Percent > 0 or a curve). Trailing Percent(0) padding does not break MB
/// mode; a mixed config is software mode (its MbSync slots resolve as 0).
pub fn mb_mode(dev: &DeviceConfig) -> bool {
    let any_mb = dev.slots.iter().any(|s| matches!(s, SlotSpeed::MbSync));
    let any_soft = dev.slots.iter().any(|s| {
        matches!(s, SlotSpeed::Percent(p) if *p > 0) || matches!(s, SlotSpeed::Curve(_))
    });
    any_mb && !any_soft
}

/// Upstream send rule: transmit when any slot drifted (|desired − readback| > 5,
/// or desired ≤ 10 and readback differs at all), or when the keepalive interval
/// elapsed. Firmware reverts to hardware default without periodic traffic.
pub fn should_send(
    desired: &[u8; 4],
    readback: &[u8; 4],
    last_sent: Option<Instant>,
    now: Instant,
    keepalive: Duration,
) -> bool {
    let drifted = desired.iter().zip(readback.iter()).any(|(d, r)| {
        d.abs_diff(*r) > 5 || (*d <= 10 && *r != *d)
    });
    let keepalive_due = last_sent.is_none_or(|t| now.duration_since(t) >= keepalive);
    drifted || keepalive_due
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DeviceConfig;

    fn dev(slots: [SlotSpeed; 4]) -> DeviceConfig {
        DeviceConfig { mac: "02:8b:51:62:32:e1".into(), name: None, slots, color: None, effect: None }
    }

    #[test]
    fn resolves_curves_constants_and_off() {
        let d = dev([
            SlotSpeed::Curve("cpu".into()),
            SlotSpeed::Curve("cpu".into()),
            SlotSpeed::Percent(100),
            SlotSpeed::Percent(0),
        ]);
        let mut pct = HashMap::new();
        pct.insert("cpu".to_string(), 34.0);
        assert_eq!(resolve_slots(&d, &pct), [86, 86, 255, 0]);
    }

    #[test]
    fn mb_slots_resolve_to_zero_in_mixed_configs() {
        let d = dev([
            SlotSpeed::MbSync,
            SlotSpeed::Percent(100),
            SlotSpeed::Percent(0),
            SlotSpeed::Percent(0),
        ]);
        assert_eq!(resolve_slots(&d, &HashMap::new()), [0, 255, 0, 0]);
    }

    #[test]
    fn mb_mode_detection() {
        assert!(mb_mode(&dev([
            SlotSpeed::MbSync,
            SlotSpeed::MbSync,
            SlotSpeed::MbSync,
            SlotSpeed::MbSync,
        ])));
        assert!(
            mb_mode(&dev([
                SlotSpeed::MbSync,
                SlotSpeed::MbSync,
                SlotSpeed::MbSync,
                SlotSpeed::Percent(0),
            ])),
            "trailing Percent(0) padding must not break MB mode"
        );
        assert!(
            !mb_mode(&dev([
                SlotSpeed::MbSync,
                SlotSpeed::Percent(40),
                SlotSpeed::Percent(0),
                SlotSpeed::Percent(0),
            ])),
            "a software-commanded percent slot forces software mode"
        );
        assert!(
            !mb_mode(&dev([
                SlotSpeed::MbSync,
                SlotSpeed::Curve("cpu".into()),
                SlotSpeed::Percent(0),
                SlotSpeed::Percent(0),
            ])),
            "a curve slot forces software mode"
        );
        assert!(!mb_mode(&dev([
            SlotSpeed::Percent(40),
            SlotSpeed::Percent(40),
            SlotSpeed::Percent(40),
            SlotSpeed::Percent(0),
        ])));
        assert!(
            !mb_mode(&dev([
                SlotSpeed::Percent(0),
                SlotSpeed::Percent(0),
                SlotSpeed::Percent(0),
                SlotSpeed::Percent(0),
            ])),
            "an all-off device is uncommanded, not MB-synced"
        );
    }

    #[test]
    fn unknown_curve_resolves_to_zero() {
        let d = dev([
            SlotSpeed::Curve("gone".into()),
            SlotSpeed::Percent(0),
            SlotSpeed::Percent(0),
            SlotSpeed::Percent(0),
        ]);
        assert_eq!(resolve_slots(&d, &HashMap::new()), [0, 0, 0, 0]);
    }

    #[test]
    fn send_policy() {
        let t0 = Instant::now();
        let ka = Duration::from_secs(1);
        // matched readback, keepalive not due → no send
        assert!(!should_send(&[86; 4], &[86; 4], Some(t0), t0 + Duration::from_millis(500), ka));
        // keepalive due → send even when matched
        assert!(should_send(&[86; 4], &[86; 4], Some(t0), t0 + ka, ka));
        // never sent → send
        assert!(should_send(&[86; 4], &[86; 4], None, t0, ka));
        // dropout signature [0,0,0,0] → drifted → send immediately
        assert!(should_send(&[86, 86, 86, 0], &[0, 0, 0, 0], Some(t0), t0, ka));
        // small drift within ±5 tolerated (readback jitter)
        assert!(!should_send(&[86; 4], &[84; 4], Some(t0), t0, ka));
        // low-PWM strictness: desired ≤ 10 must match exactly
        assert!(should_send(&[8, 0, 0, 0], &[10, 0, 0, 0], Some(t0), t0, ka));
    }
}

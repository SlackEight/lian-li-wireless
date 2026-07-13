//! Temp→speed curve interpolation and PWM hysteresis (pure).
//! Semantics match lianli-daemon (validated live: 34% → PWM 86).

/// A curve with points sorted by temperature (sorted once at construction —
/// upstream re-sorts every evaluation).
pub struct SortedCurve {
    points: Vec<(f32, f32)>,
}

impl SortedCurve {
    pub fn new(mut points: Vec<(f32, f32)>) -> Self {
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        Self { points }
    }

    /// Speed % for a temperature. Empty/single-point curves → 50 / the point's
    /// speed; below min → min speed; above max → max speed; else linear.
    pub fn eval(&self, temp: f32) -> f32 {
        match self.points.len() {
            0 => return 50.0,
            1 => return self.points[0].1,
            _ => {}
        }
        let first = self.points[0];
        let last = *self.points.last().unwrap();
        if temp <= first.0 {
            return first.1;
        }
        if temp >= last.0 {
            return last.1;
        }
        for w in self.points.windows(2) {
            let (t1, s1) = w[0];
            let (t2, s2) = w[1];
            if temp >= t1 && temp <= t2 {
                if (t2 - t1).abs() < f32::EPSILON {
                    return s1;
                }
                let ratio = (temp - t1) / (t2 - t1);
                return s1 + ratio * (s2 - s1);
            }
        }
        last.1
    }
}

/// Speed % → PWM byte (upstream: `(pct * 2.55) as u8`).
pub fn percent_to_pwm(pct: f32) -> u8 {
    (pct * 2.55) as u8
}

/// Hold the last PWM while BOTH the PWM delta and the temp delta are below
/// their thresholds (prevents chatter around curve breakpoints).
#[derive(Default)]
pub struct Hysteresis {
    last_temp: Option<f32>,
    last_pwm: Option<u8>,
}

impl Hysteresis {
    pub fn apply(&mut self, temp: f32, target_pwm: u8, ht: f32, hp: u8) -> u8 {
        if let (Some(lt), Some(lp)) = (self.last_temp, self.last_pwm) {
            let pwm_delta = target_pwm.abs_diff(lp);
            let temp_delta = (temp - lt).abs();
            if pwm_delta < hp && temp_delta < ht {
                return lp; // hold; do not update anchors
            }
        }
        self.last_temp = Some(temp);
        self.last_pwm = Some(target_pwm);
        target_pwm
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner's real curve-1, stored unsorted exactly as in their config.
    fn owner_curve() -> SortedCurve {
        SortedCurve::new(vec![
            (29.0, 30.0),
            (52.0, 34.0),
            (69.0, 35.0),
            (89.0, 37.0),
            (40.0, 34.0),
            (78.0, 35.0),
        ])
    }

    #[test]
    fn owner_curve_live_anchor() {
        // Observed on real hardware: temp 41.3 °C → 34% → PWM 86.
        let c = owner_curve();
        let pct = c.eval(41.3); // between (40,34) and (52,34) → 34
        assert!((pct - 34.0).abs() < 0.001);
        assert_eq!(percent_to_pwm(pct), 86);
    }

    #[test]
    fn interpolation_boundaries() {
        let c = owner_curve();
        assert!((c.eval(10.0) - 30.0).abs() < 0.001); // below min → min speed
        assert!((c.eval(95.0) - 37.0).abs() < 0.001); // above max → max speed
        // midpoint of (29,30)-(40,34): temp 34.5 → 30 + 0.5*4 = 32
        assert!((c.eval(34.5) - 32.0).abs() < 0.001);
    }

    #[test]
    fn degenerate_curves() {
        assert!((SortedCurve::new(vec![]).eval(50.0) - 50.0).abs() < 0.001);
        assert!((SortedCurve::new(vec![(40.0, 25.0)]).eval(99.0) - 25.0).abs() < 0.001);
    }

    #[test]
    fn hysteresis_holds_then_releases() {
        let mut h = Hysteresis::default();
        assert_eq!(h.apply(40.0, 86, 1.0, 5), 86); // first: adopt
        assert_eq!(h.apply(40.4, 88, 1.0, 5), 86); // both deltas small: hold
        assert_eq!(h.apply(40.4, 92, 1.0, 5), 92); // pwm delta ≥ 5: release
        assert_eq!(h.apply(45.0, 93, 1.0, 5), 93); // temp delta ≥ 1.0: release
    }
}

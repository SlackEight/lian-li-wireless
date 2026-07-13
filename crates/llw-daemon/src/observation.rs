//! Persistence filter turning raw GetDev readbacks into dropout observations
//! (M2a experiment: single-poll blips are healthy-channel background noise;
//! only consecutive-poll readback loss while commanded is link trouble).

/// Per-device streak tracker. Feed it every GetDev poll result.
#[derive(Debug, Default)]
pub struct DropoutFilter {
    streak: u32,
}

impl DropoutFilter {
    /// `commanded`: we have nonzero desired PWM for at least one active slot.
    /// `readback_zero`: every active fan slot read back 0.
    /// Returns true when THIS poll should be reported as a dropout
    /// observation (i.e. streak has reached `threshold`).
    pub fn observe(&mut self, commanded: bool, readback_zero: bool, threshold: u32) -> bool {
        if commanded && readback_zero {
            self.streak = self.streak.saturating_add(1);
            self.streak >= threshold.max(1)
        } else {
            self.streak = 0;
            false
        }
    }

    pub fn streak(&self) -> u32 {
        self.streak
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_poll_blips_are_filtered() {
        // Replay of the experiment's healthy run 1: singles at 51.6s/65.2s
        // and one 3-poll burst — with threshold 2, only the burst's polls
        // 2 and 3 count (2 observations in the whole run).
        let mut f = DropoutFilter::default();
        let mut observations = 0;
        // single blip
        if f.observe(true, true, 2) { observations += 1; }
        if f.observe(true, false, 2) { observations += 1; }
        // 3-poll burst
        if f.observe(true, true, 2) { observations += 1; }
        if f.observe(true, true, 2) { observations += 1; }
        if f.observe(true, true, 2) { observations += 1; }
        if f.observe(true, false, 2) { observations += 1; }
        // another single
        if f.observe(true, true, 2) { observations += 1; }
        if f.observe(true, false, 2) { observations += 1; }
        assert_eq!(observations, 2);
    }

    #[test]
    fn sustained_loss_accumulates_fast() {
        // June-style sustained loss: every poll past the threshold reports.
        let mut f = DropoutFilter::default();
        let count = (0..10).filter(|_| f.observe(true, true, 2)).count();
        assert_eq!(count, 9); // polls 2..=10
        assert_eq!(f.streak(), 10);
    }

    #[test]
    fn uncommanded_never_observes() {
        let mut f = DropoutFilter::default();
        assert!(!f.observe(false, true, 2));
        assert!(!f.observe(false, true, 2));
        assert_eq!(f.streak(), 0);
    }

    #[test]
    fn recovery_resets_streak() {
        let mut f = DropoutFilter::default();
        f.observe(true, true, 2);
        f.observe(true, true, 2);
        assert_eq!(f.streak(), 2);
        f.observe(true, false, 2);
        assert_eq!(f.streak(), 0);
    }
}

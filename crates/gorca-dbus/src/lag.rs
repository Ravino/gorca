//! D-2 lag detection with hysteresis.

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ShedEvent {
    /// Backlog crossed the watermark: app actors move their noisiest non-focus apps to
    /// `Flooded` and per-app event deregistration (D-5) is applied.
    Start,
    /// Backlog fell below half the watermark.
    End,
}

/// Feed it the `SIOCINQ` sample after each batch. Pure.
#[derive(Copy, Clone, Debug)]
pub struct LagDetector {
    watermark: usize,
    shedding: bool,
}

impl LagDetector {
    pub fn new(watermark: usize) -> Self {
        Self {
            watermark,
            shedding: false,
        }
    }

    pub fn is_shedding(&self) -> bool {
        self.shedding
    }

    pub fn observe(&mut self, backlog: usize) -> Option<ShedEvent> {
        if !self.shedding && backlog >= self.watermark {
            self.shedding = true;
            Some(ShedEvent::Start)
        } else if self.shedding && backlog < self.watermark / 2 {
            self.shedding = false;
            Some(ShedEvent::End)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hysteresis_prevents_flapping() {
        let mut d = LagDetector::new(1000);
        assert_eq!(d.observe(999), None);
        assert_eq!(d.observe(1000), Some(ShedEvent::Start));
        assert_eq!(d.observe(5000), None);
        assert_eq!(d.observe(600), None, "still above half the watermark");
        assert!(d.is_shedding());
        assert_eq!(d.observe(499), Some(ShedEvent::End));
        assert_eq!(d.observe(0), None);
    }
}

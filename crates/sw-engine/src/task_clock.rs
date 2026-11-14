use uuid::Uuid;

const SECOND_US: i128 = 1_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FrameKey {
    pub generation: Uuid,
    pub ordinal: u64,
    pub source_us: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Claim {
    pub frame: FrameKey,
    pub base_slot: Option<u64>,
}

#[derive(Default)]
pub(crate) struct TaskClock {
    origin: Option<FrameKey>,
    claimed: Option<u64>,
    accepted: Option<u64>,
    last_frame: Option<FrameKey>,
    in_flight: Option<Claim>,
}

impl TaskClock {
    pub fn eligible(&self, frame: FrameKey) -> bool {
        self.in_flight.is_none()
            && self.last_frame.is_none_or(|last| {
                last.generation != frame.generation
                    || (last.ordinal < frame.ordinal && last.source_us < frame.source_us)
            })
    }

    pub fn due(&self, frame: FrameKey, fps: u8) -> Option<u64> {
        if !self.eligible(frame) {
            return None;
        }
        self.elapsed(frame).map_or(Some(0), |elapsed| {
            let slot = u64::try_from((elapsed * i128::from(fps)).div_euclid(SECOND_US)).ok()?;
            self.claimed.is_none_or(|old| slot > old).then_some(slot)
        })
    }

    pub fn remainder(&self, frame: FrameKey, fps: u8) -> i128 {
        self.elapsed(frame).map_or(0, |elapsed| {
            (elapsed * i128::from(fps)).rem_euclid(SECOND_US) / i128::from(fps)
        })
    }

    pub fn until_base_us(&self, frame: FrameKey, fps: u8) -> u64 {
        let Some(elapsed) = self.elapsed(frame) else {
            return 0;
        };
        let next = i128::from(self.claimed.unwrap_or(0)) + 1;
        let remaining = next * SECOND_US - elapsed * i128::from(fps);
        u64::try_from(remaining.max(0).div_euclid(i128::from(fps))).unwrap_or(u64::MAX)
    }

    fn elapsed(&self, frame: FrameKey) -> Option<i128> {
        let anchor = self
            .origin
            .filter(|anchor| anchor.generation == frame.generation)?;
        Some(i128::from(frame.source_us) - i128::from(anchor.source_us))
    }

    /// Returns the number of base slots skipped before this dispatch.
    pub fn claim(&mut self, claim: Claim) -> Option<u64> {
        if !self.eligible(claim.frame) {
            return None;
        }
        if self
            .origin
            .is_some_and(|origin| origin.generation != claim.frame.generation)
        {
            *self = Self::default();
        }
        let missed = if let Some(slot) = claim.base_slot {
            let missed = self.claimed.map_or(slot, |previous| {
                slot.saturating_sub(previous.saturating_add(1))
            });
            self.origin.get_or_insert(claim.frame);
            self.claimed = Some(slot);
            missed
        } else {
            0
        };
        self.last_frame = Some(claim.frame);
        self.in_flight = Some(claim);
        Some(missed)
    }

    /// A rejected base claim adds one miss. Extra work leaves both slots unchanged.
    pub fn finish(&mut self, claim: Claim, accepted: bool) -> Option<u64> {
        if !self.matches(claim) {
            return None;
        }
        self.in_flight = None;
        let Some(slot) = claim.base_slot else {
            return Some(0);
        };
        if !accepted {
            return Some(1);
        }
        debug_assert!(self.accepted.is_none_or(|previous| slot > previous));
        self.accepted = Some(slot);
        Some(0)
    }

    pub fn matches(&self, claim: Claim) -> bool {
        self.in_flight == Some(claim)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_claims_reject_stale_replies_without_losing_new_work() {
        const SEEDS: u64 = 256;
        const STEPS: u64 = 400;
        for seed in 1..=SEEDS {
            let mut random = seed;
            let mut clock = TaskClock::default();
            let mut generation = 1u128;
            let mut ordinal = 0u64;
            let mut source_us = 0i64;
            let mut previous = None;
            for step in 0..STEPS {
                random = random
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                let control = random >> 32;
                if control.is_multiple_of(19) {
                    generation += 1;
                    ordinal = 0;
                    source_us = -1_000;
                } else {
                    ordinal += 1;
                    source_us += 30_000 + (control % 190_000) as i64;
                }
                let frame = FrameKey {
                    generation: Uuid::from_u128((u128::from(seed) << 64) | generation),
                    ordinal,
                    source_us,
                };
                let base_slot = clock.due(frame, 5);
                let claim = Claim { frame, base_slot };
                assert!(clock.claim(claim).is_some(), "seed {seed} step {step}");
                assert!(!clock.eligible(frame), "seed {seed} step {step}");
                if let Some(stale) = previous {
                    assert!(
                        clock.finish(stale, true).is_none(),
                        "seed {seed} step {step}"
                    );
                    assert!(clock.matches(claim), "seed {seed} step {step}");
                }
                let accepted = !control.is_multiple_of(4);
                let misses = clock.finish(claim, accepted);
                assert_eq!(
                    misses,
                    Some(u64::from(base_slot.is_some() && !accepted)),
                    "seed {seed} step {step}"
                );
                assert!(
                    clock.finish(claim, true).is_none(),
                    "seed {seed} step {step}"
                );
                assert!(clock.due(frame, 5).is_none(), "seed {seed} step {step}");
                previous = Some(claim);
            }
        }
        println!(
            "{{\"scenario\":\"task_claim_fencing\",\"first_seed\":1,\"last_seed\":{SEEDS},\"transitions\":{}}}",
            SEEDS * STEPS
        );
    }

    fn frame(ordinal: u64, source_us: i64) -> FrameKey {
        FrameKey {
            generation: Uuid::nil(),
            ordinal,
            source_us,
        }
    }

    #[test]
    fn exact_sixty_seconds_keep_fire_phase_at_fifteen_and_ntsc_rates() {
        for (num, den) in [(15u64, 1u64), (30_000, 1001), (3, 1)] {
            let mut clock = TaskClock::default();
            let mut claims = Vec::new();
            let frames = (60 * num).div_ceil(den);
            for ordinal in 0..frames {
                let pts = (ordinal * 1_000_000 * den).div_ceil(num) as i64;
                let frame = frame(ordinal, pts);
                if let Some(slot) = clock.due(frame, 2) {
                    let claim = Claim {
                        frame,
                        base_slot: Some(slot),
                    };
                    assert_eq!(clock.claim(claim), Some(0));
                    assert!(clock.due(frame, 2).is_none());
                    assert_eq!(clock.finish(claim, true), Some(0));
                    assert!(clock.due(frame, 2).is_none());
                    claims.push(ordinal);
                }
            }
            assert_eq!(claims.len(), 120, "{num}/{den} fps");
            if num == 15 {
                assert!(
                    claims
                        .windows(2)
                        .all(|pair| matches!(pair[1] - pair[0], 7 | 8))
                );
            }
        }
    }

    #[test]
    fn extra_samples_skips_and_generation_changes_do_not_shift_base_phase() {
        let mut clock = TaskClock::default();
        let first = Claim {
            frame: frame(0, -100_000),
            base_slot: Some(0),
        };
        assert_eq!(clock.claim(first), Some(0));
        assert_eq!(clock.finish(first, true), Some(0));
        let extra = Claim {
            frame: frame(1, 233_300),
            base_slot: None,
        };
        assert_eq!(clock.claim(extra), Some(0));
        assert_eq!(clock.finish(extra, true), Some(0));
        assert_eq!(clock.due(frame(2, 399_900), 2), None);
        assert_eq!(clock.due(frame(3, 400_300), 2), Some(1));
        let timed_out = Claim {
            frame: frame(3, 400_300),
            base_slot: Some(1),
        };
        assert_eq!(clock.claim(timed_out), Some(0));
        assert_eq!(clock.finish(timed_out, false), Some(1));
        assert_eq!(clock.due(frame(4, 500_000), 2), None);
        let next = Claim {
            frame: frame(5, 1_420_000),
            base_slot: Some(3),
        };
        assert_eq!(clock.due(next.frame, 2), Some(3));
        assert_eq!(clock.claim(next), Some(1));
        assert_eq!(clock.finish(timed_out, true), None);
        assert_eq!(clock.finish(next, true), Some(0));
        assert_eq!(clock.due(frame(6, 1_420_000), 2), None);
        let generation = FrameKey {
            generation: Uuid::new_v4(),
            ..frame(1, 0)
        };
        let restart = Claim {
            frame: generation,
            base_slot: Some(0),
        };
        assert_eq!(clock.due(generation, 2), Some(0));
        assert_eq!(clock.claim(restart), Some(0));
        assert_eq!(clock.finish(restart, true), Some(0));
    }
}

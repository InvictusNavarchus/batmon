//! Shared debounce latch for alerts that require a sustained condition.
//!
//! Two families need one: the heat-soak warning and the thermal anomaly
//! detector. Both watch a temperature that spikes constantly under normal use,
//! so a single qualifying sample means nothing — only several consecutive ones
//! describe a real thermal problem.

/// What one sample says about a debounced condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observation {
    /// The condition holds; advance toward firing.
    Qualifying,
    /// The condition is comfortably false; drop the latch and the count.
    Clearing,
    /// Neither — inside a deadband, or the reading was unavailable. The count
    /// resets so a streak must be consecutive, but an existing latch survives.
    Neutral,
}

/// A latch that only trips after a run of consecutive qualifying samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Debounced {
    #[default]
    Clear,
    /// A partial streak; the count is how many consecutive samples have qualified.
    Arming(u32),
    Fired,
}

impl Debounced {
    /// Feed one observation, returning the new state and whether to announce.
    ///
    /// Announcing happens exactly once per arming, on the transition into
    /// [`Debounced::Fired`]. Further qualifying samples keep the latch without
    /// re-announcing.
    ///
    /// The distinction that makes this worth sharing is [`Observation::Neutral`]:
    /// it must reset a partial streak — so that spikes separated by a lull do
    /// not accumulate into a false trip — while leaving a latch that has already
    /// fired alone, so that a reading dithering inside its deadband does not
    /// re-announce on every excursion.
    #[must_use]
    pub fn observe(self, observation: Observation, required: u32) -> (Self, bool) {
        match observation {
            Observation::Clearing => (Self::Clear, false),
            Observation::Neutral => match self {
                Self::Fired => (Self::Fired, false),
                Self::Clear | Self::Arming(_) => (Self::Clear, false),
            },
            Observation::Qualifying => match self {
                Self::Fired => (Self::Fired, false),
                Self::Clear => Self::arm(1, required),
                Self::Arming(count) => Self::arm(count.saturating_add(1), required),
            },
        }
    }

    fn arm(count: u32, required: u32) -> (Self, bool) {
        if count >= required {
            (Self::Fired, true)
        } else {
            (Self::Arming(count), false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed a run of observations, returning how many times it announced.
    fn run(observations: &[Observation], required: u32) -> (Debounced, usize) {
        let mut state = Debounced::default();
        let mut announced = 0;
        for observation in observations {
            let (next, fired) = state.observe(*observation, required);
            state = next;
            announced += usize::from(fired);
        }
        (state, announced)
    }

    #[test]
    fn a_full_streak_announces_exactly_once() {
        use Observation::Qualifying;
        let (state, announced) = run(&[Qualifying, Qualifying, Qualifying], 3);

        assert_eq!(state, Debounced::Fired);
        assert_eq!(announced, 1);
    }

    #[test]
    fn a_short_streak_stays_quiet() {
        use Observation::Qualifying;
        let (state, announced) = run(&[Qualifying, Qualifying], 3);

        assert_eq!(state, Debounced::Arming(2));
        assert_eq!(announced, 0);
    }

    #[test]
    fn holding_the_condition_does_not_re_announce() {
        use Observation::Qualifying;
        let (_, announced) = run(&[Qualifying; 10], 3);

        assert_eq!(announced, 1);
    }

    #[test]
    fn a_neutral_sample_breaks_a_partial_streak() {
        // Two spikes, a lull, then a third spike must not add up to three.
        use Observation::{Neutral, Qualifying};
        let (state, announced) = run(&[Qualifying, Qualifying, Neutral, Qualifying], 3);

        assert_eq!(state, Debounced::Arming(1));
        assert_eq!(announced, 0);
    }

    #[test]
    fn a_neutral_sample_leaves_a_fired_latch_alone() {
        // The deadband case: a reading dithering just under the threshold must
        // not re-arm and re-announce on its next excursion above it.
        use Observation::{Neutral, Qualifying};
        let (state, announced) = run(
            &[Qualifying, Qualifying, Qualifying, Neutral, Qualifying],
            3,
        );

        assert_eq!(state, Debounced::Fired);
        assert_eq!(announced, 1);
    }

    #[test]
    fn a_clearing_sample_drops_the_latch_and_allows_a_second_announcement() {
        use Observation::{Clearing, Qualifying};
        let (state, announced) = run(
            &[
                Qualifying, Qualifying, Qualifying, // fires
                Clearing,   // fully re-arms
                Qualifying, Qualifying, Qualifying, // fires again
            ],
            3,
        );

        assert_eq!(state, Debounced::Fired);
        assert_eq!(announced, 2);
    }

    #[test]
    fn a_required_count_of_one_fires_immediately() {
        let (state, announced) = run(&[Observation::Qualifying], 1);

        assert_eq!(state, Debounced::Fired);
        assert_eq!(announced, 1);
    }

    #[test]
    fn the_streak_counter_saturates_rather_than_wrapping() {
        // A machine that sits over the threshold for weeks must not overflow
        // back into a partial streak and re-announce.
        let (state, fired) = Debounced::Arming(u32::MAX).observe(Observation::Qualifying, 3);

        assert_eq!(state, Debounced::Fired);
        assert!(fired);
    }
}

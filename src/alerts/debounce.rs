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
mod tests;

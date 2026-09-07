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

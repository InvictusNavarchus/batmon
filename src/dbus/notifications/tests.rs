use super::*;

fn notification(family: AlertFamily) -> Notification {
    Notification {
        family,
        title: "Low Battery".to_owned(),
        body: "20% remaining".to_owned(),
        urgency: Urgency::Normal,
        icon: "battery-caution",
    }
}

#[test]
fn a_family_with_no_history_asks_for_a_new_bubble() {
    let ids = ReplacementIds::default();
    assert_eq!(ids.previous(AlertFamily::Charge), NO_REPLACEMENT);
}

#[test]
fn a_recorded_identifier_is_offered_back_for_the_same_family() {
    let mut ids = ReplacementIds::default();
    ids.record(AlertFamily::Charge, 42);

    assert_eq!(ids.previous(AlertFamily::Charge), 42);
}

#[test]
fn families_replace_their_own_bubbles_and_not_each_others() {
    // A thermal alert must not overwrite a low-battery bubble.
    let mut ids = ReplacementIds::default();
    ids.record(AlertFamily::Charge, 42);
    ids.record(AlertFamily::BatteryTemp, 43);

    assert_eq!(ids.previous(AlertFamily::Charge), 42);
    assert_eq!(ids.previous(AlertFamily::BatteryTemp), 43);
    assert_eq!(ids.previous(AlertFamily::Health), NO_REPLACEMENT);
}

#[test]
fn a_later_identifier_supersedes_the_earlier_one() {
    let mut ids = ReplacementIds::default();
    ids.record(AlertFamily::Charge, 42);
    ids.record(AlertFamily::Charge, 77);

    assert_eq!(ids.previous(AlertFamily::Charge), 77);
}

#[test]
fn a_server_that_returns_no_handle_leaves_nothing_to_replace() {
    // Zero is not a bubble; storing it would ask to replace bubble zero.
    let mut ids = ReplacementIds::default();
    ids.record(AlertFamily::Charge, 42);
    ids.record(AlertFamily::Charge, NO_REPLACEMENT);

    assert_eq!(ids.previous(AlertFamily::Charge), NO_REPLACEMENT);
}

#[test]
fn urgency_maps_onto_the_freedesktop_hint_values() {
    // What notify-send -u encoded positionally, sent as a typed hint.
    assert_eq!(Urgency::Low.as_hint(), 0);
    assert_eq!(Urgency::Normal.as_hint(), 1);
    assert_eq!(Urgency::Critical.as_hint(), 2);
}

/// A notifier in the state a headless machine produces.
///
/// Constructed directly rather than through `connect()`, which would bind to
/// whatever session bus happens to be running and — on a desktop — deliver
/// real notifications during `cargo test` while never reaching the branch
/// the test claims to cover.
fn disconnected() -> DesktopNotifier {
    DesktopNotifier {
        server: None,
        replacements: ReplacementIds::default(),
        // Freshly attempted, so delivery inside a test will not reach for
        // the real session bus during the reconnect window.
        last_attempt: Instant::now(),
    }
}

#[test]
fn delivering_without_a_notification_server_does_not_panic() {
    // Headless machines and daemons started before a graphical session must
    // still record; the journal is the durable half.
    let mut notifier = disconnected();
    notifier.deliver(&notification(AlertFamily::Charge));
    notifier.deliver(&notification(AlertFamily::BatteryTemp));
}

#[test]
fn a_disconnected_notifier_does_not_retry_within_the_reconnect_window() {
    // Both that the retry is bounded, and — since the real session bus on a
    // developer machine would answer — that `cargo test` cannot deliver
    // notifications to somebody's desktop.
    let mut notifier = disconnected();
    for _ in 0..20 {
        notifier.deliver(&notification(AlertFamily::Charge));
    }

    assert!(
        notifier.server.is_none(),
        "delivery reached for the real session bus inside the retry window"
    );
}

#[test]
fn a_stale_attempt_makes_the_next_delivery_retry() {
    // The property that matters: a notifier that failed at startup does try
    // again, so a desktop appearing later restores alerts without a restart.
    let mut notifier = disconnected();
    notifier.last_attempt = Instant::now()
        .checked_sub(RECONNECT_INTERVAL + Duration::from_secs(1))
        .expect("the process has not been running since the epoch");

    notifier.deliver(&notification(AlertFamily::Charge));

    assert!(
        notifier.last_attempt.elapsed() < RECONNECT_INTERVAL,
        "the attempt timestamp was not refreshed, so retries would be unbounded"
    );
}

#[test]
fn a_disconnected_notifier_records_no_replacement_ids() {
    // Nothing was delivered, so there is no bubble to replace next time.
    // Storing an id here would ask a future server to replace something
    // that never existed.
    let mut notifier = disconnected();
    notifier.deliver(&notification(AlertFamily::Charge));

    assert_eq!(
        notifier.replacements.previous(AlertFamily::Charge),
        NO_REPLACEMENT
    );
}

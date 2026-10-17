use super::*;

#[test]
fn only_readings_whose_time_has_come_are_due() {
    let t0 = Instant::now();
    let next = [t0, t0 + Duration::from_secs(1), t0];
    assert_eq!(due(&next, t0), vec![0, 2]);
}

#[test]
fn a_reading_on_schedule_is_next_due_one_period_later() {
    let t0 = Instant::now();
    let next = advance(t0, Duration::from_secs(2), t0 + Duration::from_millis(100));
    assert_eq!(next, t0 + Duration::from_secs(2));
}

#[test]
fn a_late_reading_waits_a_full_period_instead_of_catching_up() {
    // Arrange — a slow device made this read 5 s late on a 1 s period
    let t0 = Instant::now();
    let now = t0 + Duration::from_secs(5);
    // Act + Assert
    assert_eq!(
        advance(t0, Duration::from_secs(1), now),
        now + Duration::from_secs(1)
    );
}

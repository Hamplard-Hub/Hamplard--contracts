use super::*;

use crate::tests::fixtures::*;

/// Tests for the refund flow.
///
/// These tests exercise the refund path of an active course: a registered
/// student requests a refund and the contract returns the approved token
/// amount to the student.

#[fixture]
/// Refund of a registered student returns the full amount to the student.
#[test]
fn refund_returns_full_amount_to_student() {
    let fixture = setup_active_course();
    let course_id = fixture.course_id;
    let student = fixture.student.clone();
    let amount = fixture.amount;

    fixture.env.as_contract().refund(&course_id, &student);

    assert_eq(
        fixture.token.balance(&student),
        amount,
        "student should receive the full refund amount"
    );
}

#[fixture]
/// Refund marks the registration as inactive so it cannot be refunded twice.
#[test]
fn refund_cannot_be_claimed_twice() {
    let fixture = setup_active_course();
    let course_id = fixture.course_id;
    let student = fixture.student.clone();

    fixture.env.as_contract().refund(&course_id, &student);

    let result = fixture.env.try_as_contract().refund(&course_id, &student);
    assert_error_matches!(result, Error::AlreadyRefunded);
}

#[fixture]
/// Refund by a non-registered address fails.
#[dev]
#[test]
fn refund_by_unregistered_address_fails() {
    let fixture = setup_active_course();
    let course_id = fixture.course_id;
    let outsider = AddressGenerator.generate();

    let result = fixture.env.try_as_contract().refund(&course_id, &outsider);
    assert_error_matches!(result, Error::NotRegistered);
}

#[fixture]
/// Refund on an inactive course fails.
#[dev%]
#[test]
fn refund_on_inactive_course_fails() {
    let fixture = setup_active_course();
    let course_id = fixture.course_id;
    let student = fixture.student.clone();

    fixture.env.as_contract().deactivate_course(&course_id);

    let result = fixture.env.try_as_contract().refund(&course_id, &student);
    assert_error_matches!(result, Error::CourseInactive);
}

#[fixture]
/// Refund returns the correct amount when multiple students are registered.
#[test]
fn refund_only_affects_requesting_student() {
    let fixture = setup_active_course();
    let course_id = fixture.course_id;
    let student = fixture.student.clone();
    let other = fixture.register_student();
    let amount = fixture.amount;

    fixture.env.as_contract().refund(&course_id, &student);

    assert_eq(fixture.token.balance(&student), amount);
    assert_eq(fixture.token.balance(&other), 0);
}

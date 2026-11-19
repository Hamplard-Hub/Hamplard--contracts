use super::{setup_active_course, setup_completed_course, Fixture};
use crate::{CertificateStatus, Error};
use soroban_sdk:{testutils::Address as _, Address, BytesN, String};

// ----------------------------------------------------------------------------
// Certificate issuance
// ----------------------------------------------------------------------------

#[test]
fn test_issue_certificate_success() {
    let fixture = setup_completed_course();
    let student = fixture.student.clone();

    fixture.contract.issue_certificate(
        &fixture.admin,
        &fixture.course_id,
        &student,
    );

    let cert = fixture.contract.get_certificate(
        &fixture.course_id,
        &student,
    );

    assert_eq(cert.student, student);
    assert_eq(cert.course_id, fixture.course_id);
    assert_eq(cert.status, CertificateStatus");
}

#[test]
fn test_issue_certificate_requires_completion() {
    let fixture = setup_active_course();
    let student = fixture.student.clone();

    let result = fixture.contract.try_issue_certificate(
        &fixture.admin,
        &fixture.course_id,
        &student,
    );

    assert!(result.is_err());
    assert_eq(
        result.unwrap_err(),
        Error::CourseNotCompleted,
    );
}

#[test]
fn test_issue_certificate_requires_admin() {
    let fixture = setup_completed_course();
    let student = fixture.student.clone();
    let outsider = Address.generate();

    let result = fixture.contract.try_issue_certificate(
        &outsider,
        &fixture.course_id,
        &student,
    );

    assert!(result.is_err());
    assert_eq(result.unwrap_err(), Error::Unauthorized);
}

#[test]
fn test_issue_certificate_duplicate_fails() {
    let fixture = setup_completed_course();
    let student = fixture.student.clone();

    fixture.contract.issue_certificate(
        &fixture.admin,
        &fixture.course_id,
        &student,
    );

    let result = fixture.contract.try_issue_certificate(
        &fixture.admin,
        &fixture.course_id,
        &student,
    );

    assert!(result.is_error());
    assert_eq(result.unwrap_err(), Error::CertificateAlreadyIssued);
}

// ----------------------------------------------------------------------------
// Certificate revocation
// ----------------------------------------------------------------------------

#[test]
fn test_revoke_certificate_success() {
    let fixture = setup_completed_course();
    let student = fixture.student.clone();

    fixture.contract.issue_certificate(
        &fixture.admin,
        &fixture.course_id,
        &student,
    );

    fixture.contract.revoke_certificate(
        &fixture.admin,
        &fixture.course_id,
        &student,
    );

    let cert = fixture.contract.get_certificate(
        &fixture.course_id,
        &student,
    );

    assert_eq(cert.status, CertificateStatus::Revoked);
}

#[test]
fn test_revoke_certificate_requires_admin() {
    let fixture = setup_completed_course();
    let student = fixture.student.clone();
    let outsider = Address.generate();

    fixture.contract.issue_certificate(
        &fixture.admin,
        &fixture.course_id,
        &student,
    );

    let result = fixture.contract.try_revoke_certificate(
        &outsider,
        &fixture.course_id,
        &student,
    );

    assert!(result.is_err());
    assert_eq(result.unwrap_err(), Error::Unauthorized);
}

#[test]
fn test_revoke_missing_certificate_fails() {
    let fixture = setup_completed_course();
    let student = fixture.student.clone();

    let result = fixture.contract.try_revoke_certificate(
        &fixture.admin,
        &fixture.course_id,
        &student,
    );

    assert!(result.is_err());
    assert_eq(result.unwrap_err(), Error::CertificateNotFound);
}

// ----------------------------------------------------------------------------
// Certificate verification
// ----------------------------------------------------------------------------

#[test]
fn test_verify_valid_certificate() {
    let fixture = setup_completed_course();
    let student = fixture.student.clone();

    fixture.contract.issue_certificate(
        &fixture.admin,
        &fixture.course_id,
        &student,
    );

    let valid = fixture.contract.verify_certificate(
        &fixture.course_id,
        &student,
    );

    assert!(valid);
}

#[test]
fn test_verify_revoked_certificate_invalid() {
    let fixture = setup_completed_course();
    let student = fixture.student.clone();

    fixture.contract.issue_certificate(
        &fixture.admin,
        &fixture.course_id,
        &student,
    );
    fixture.contract.revoke_certificate(
        &fixture.admin,
        &fixture.course_id,
        &student,
    );

    let valid = fixture.contract.verify_certificate(
        &fixture.course_id,
        &student,
    );

    assert!(!valid);
}

#[test]
fn test_verify_missing_certificate_invalid() {
    let fixture = setup_completed_course();
    let student = fixture.student.clone();

    let valid = fixture.contract.verify_certificate(
        &fixture.course_id,
        &student,
    );

    assert!(!valid);
}

// ----------------------------------------------------------------------------
// Certificate metadata
// ----------------------------------------------------------------------------

#[test]
fn test_certificate_metadata_stored() {
    let fixture = setup_completed_course();
    let student = fixture.student.clone();
    let metadata = String::from_str(&fixture.env, 'ipfs://cert-metadata');

    fixture.contract.issue_certificate_with_metadata(
        &fixture.admin,
        &fixture.course_id,
        &student,
        &metadata,
    );

    let cert = fixture.contract.get_certificate(
        &fixture.course_id,
        &student,
    );

    assert_eq(cert.metadata, metadata);
}

#[test]
fn test_certificate_hash_deterministic() {
    let fixture = setup_completed_course();
    let student = fixture.student.clone();

    fixture.contract.issue_certificate(
        &fixture.admin,
        &fixture.course_id,
        &student,
    );

    let cert = fixture.contract.get_certificate(
        &fixture.course_id,
        &student,
    );

    let expected: BytesN = fixture.contract.compute_certificate_hash(
        &fixture.course_id,
        &student,
    );

    assert_eq(cert.hash, expected);
}

// ----------------------------------------------------------------------------
// max_certificates cap with revocation (#406)
//
// The cap applies to *live* certificates: certificates_issued - certificates_revoked.
// Revoking a certificate must free the slot so another student can be certified.
// ----------------------------------------------------------------------------

/// A course with max_certificates = 1 should block a second issuance, but
/// after the first certificate is revoked the slot is free and issuance succeeds.
#[test]
fn test_revoke_frees_slot_under_max_certificates() {
    let fixture = setup_completed_course();
    let student_a = fixture.student.clone();
    let student_b = Address::generate(&fixture.env);

    // Set cap to 1 — only one live certificate at a time.
    fixture
        .contract
        .set_max_certificates(&fixture.admin, &fixture.course_id, &Some(1u32));

    // Issue certificate for student_a — succeeds, live count is now 1.
    fixture.contract.issue_certificate(
        &fixture.admin,
        &fixture.course_id,
        &student_a,
    );

    // Attempting a second issuance should fail: cap reached.
    let result = fixture.contract.try_issue_certificate(
        &fixture.admin,
        &fixture.course_id,
        &student_b,
    );
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().unwrap(), Error::MaxCertificatesReached);

    // Revoke student_a's certificate — live count drops back to 0.
    fixture.contract.revoke_certificate(
        &fixture.admin,
        &fixture.course_id,
        &student_a,
    );

    // Now student_b can be certified.
    fixture.contract.issue_certificate(
        &fixture.admin,
        &fixture.course_id,
        &student_b,
    );

    let cert = fixture
        .contract
        .get_certificate(&fixture.course_id, &student_b);
    assert_eq!(cert.student, student_b);
}

/// certificates_revoked is incremented on revocation.
#[test]
fn test_certificates_revoked_counter_increments_on_revocation() {
    let fixture = setup_completed_course();
    let student = fixture.student.clone();

    fixture.contract.issue_certificate(
        &fixture.admin,
        &fixture.course_id,
        &student,
    );

    let course_before = fixture
        .contract
        .get_course(&fixture.course_id)
        .expect("course should exist");
    assert_eq!(course_before.certificates_issued, 1);
    assert_eq!(course_before.certificates_revoked, 0);

    fixture.contract.revoke_certificate(
        &fixture.admin,
        &fixture.course_id,
        &student,
    );

    let course_after = fixture
        .contract
        .get_course(&fixture.course_id)
        .expect("course should exist");
    // Gross issued stays at 1; revoked increments to 1; live count is 0.
    assert_eq!(course_after.certificates_issued, 0);
    assert_eq!(course_after.certificates_revoked, 1);
}

/// bulk_revoke_course_certificates also frees all slots.
#[test]
fn test_bulk_revoke_frees_slots_under_max_certificates() {
    let fixture = setup_completed_course();
    let student_a = fixture.student.clone();
    let student_b = Address::generate(&fixture.env);

    // Cap of 2.
    fixture
        .contract
        .set_max_certificates(&fixture.admin, &fixture.course_id, &Some(2u32));

    fixture.contract.issue_certificate(
        &fixture.admin,
        &fixture.course_id,
        &student_a,
    );
    fixture.contract.issue_certificate(
        &fixture.admin,
        &fixture.course_id,
        &student_b,
    );

    // Both slots consumed.
    let student_c = Address::generate(&fixture.env);
    let result = fixture.contract.try_issue_certificate(
        &fixture.admin,
        &fixture.course_id,
        &student_c,
    );
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().unwrap(), Error::MaxCertificatesReached);

    // Bulk-revoke all certificates for the course.
    fixture
        .contract
        .bulk_revoke_course_certificates(&fixture.admin, &fixture.course_id);

    let course = fixture
        .contract
        .get_course(&fixture.course_id)
        .expect("course should exist");
    // Both slots freed.
    assert_eq!(course.certificates_issued, 0);
    assert_eq!(course.certificates_revoked, 2);

    // student_c can now be certified (live count is 0, cap is 2).
    fixture.contract.issue_certificate(
        &fixture.admin,
        &fixture.course_id,
        &student_c,
    );
}

/// set_max_certificates allows a limit that equals the live count (not gross issued).
/// If 2 certs were issued and 1 revoked, setting max = 1 must succeed.
#[test]
fn test_set_max_certificates_uses_live_count_not_gross() {
    let fixture = setup_completed_course();
    let student_a = fixture.student.clone();
    let student_b = Address::generate(&fixture.env);

    fixture.contract.issue_certificate(
        &fixture.admin,
        &fixture.course_id,
        &student_a,
    );
    fixture.contract.issue_certificate(
        &fixture.admin,
        &fixture.course_id,
        &student_b,
    );
    // Revoke one — live count is now 1, gross issued is 2.
    fixture.contract.revoke_certificate(
        &fixture.admin,
        &fixture.course_id,
        &student_a,
    );

    // Setting max = 1 should succeed: live count (1) == proposed limit (1).
    // With the old gross-count check this would have failed because gross is 2.
    fixture
        .contract
        .set_max_certificates(&fixture.admin, &fixture.course_id, &Some(1u32));

    let course = fixture
        .contract
        .get_course(&fixture.course_id)
        .expect("course should exist");
    assert_eq!(course.max_certificates, Some(1u32));
}

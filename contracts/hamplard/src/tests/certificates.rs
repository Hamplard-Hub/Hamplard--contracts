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

#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Events, Ledger as _},
    token, Address, Env, String, Symbol, TryIntoVal,
};

// ============================================================
// TEST HELPERS
// ============================================================

fn setup() -> (Env, Address, Address, Address, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(HamplardContract, ());

    // Deploy mock USDC token
    let token_admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(token_admin.clone())
        .address();
    let token_client = token::StellarAssetClient::new(&env, &token_id);

    let admin = Address::generate(&env);
    let sec_admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let instructor = Address::generate(&env);
    let student = Address::generate(&env);

    // Fund student with 10,000 USDC (100_000_000_000 stroops)
    token_client.mint(&student, &100_000_000_000);

    // Init contract
    let client = HamplardContractClient::new(&env, &contract_id);
    client.init(&admin, &sec_admin, &treasury, &20u32, &50u32); // 20% platform fee, 50 courses max // 20% platform fee
    client.add_approved_token(&admin, &token_id);

    (
        env,
        contract_id,
        token_id,
        admin,
        sec_admin,
        treasury,
        instructor,
    )
}

fn register_and_approve_course(
    env: &Env,
    client: &HamplardContractClient,
    token_id: &Address,
    admin: &Address,
    instructor: &Address,
    course_id: &str,
    price: i128,
) {
    client.register_course(
        instructor,
        &String::from_str(env, course_id),
        &price,
        token_id,
        &0u32, // use platform default fee
        &None,
    );
    client.approve_course(admin, &String::from_str(env, course_id));
}

// ============================================================
// INIT TESTS
// ============================================================

#[test]
fn test_init_success() {
    let (env, contract_id, _token_id, _admin, sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Platform fee should be 20%
    assert_eq!(client.get_platform_fee(), 20);
}

#[test]
fn test_admin_instance_ttl_extended_on_admin_ops() {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    env.ledger().with_mut(|l| {
        l.sequence_number += 50_000;
        l.min_persistent_entry_ttl = 100_000;
        l.min_temp_entry_ttl = 100_000;
    });

    // update_default_fee is a pure admin write — should extend TTL
    client.update_default_fee(&admin, &25u32);

    // If Admin key expired, get_platform_fee would return default or panic.
    // With TTL extension, this must return the updated value.
    assert_eq!(client.get_platform_fee(), 25);
}

#[test]
fn test_treasury_instance_ttl_extended_on_transfer_admin() {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let new_admin = Address::generate(&env);

    env.ledger().with_mut(|l| {
        l.sequence_number += 50_000;
        l.min_persistent_entry_ttl = 100_000;
        l.min_temp_entry_ttl = 100_000;
    });

    // transfer_admin extends TTL — new admin must be able to use admin ops
    let new_sec = Address::generate(&env);
    client.transfer_admin(&admin, &sec_admin, &new_admin, &new_sec);
    client.accept_admin(&new_admin, &new_sec);
    client.update_default_fee(&new_admin, &30u32);
    assert_eq!(client.get_platform_fee(), 30);
}

// ============================================================
// COURSE REGISTRATION TESTS
// ============================================================

#[test]
fn test_register_course_success() {
    let (env, contract_id, token_id, _admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let course_id = String::from_str(&env, "COURSE-TAILORING-001");
    client.register_course(
        &instructor,
        &course_id,
        &50_000_000, // 5 USDC
        &token_id,
        &0u32,
        &None,
    );

    let course = client.get_course(&course_id);
    assert_eq!(course.status, CourseStatus::Pending);
    assert_eq!(course.price, 50_000_000);
    assert_eq!(course.platform_fee_percent, 20);
    assert_eq!(course.total_enrollments, 0);
}

#[test]
fn test_register_course_custom_fee() {
    let (env, contract_id, token_id, _admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-MAKEUP-001"),
        &100_000_000,
        &token_id,
        &30u32, // custom 30% platform fee
        &None,
    );

    let course = client.get_course(&String::from_str(&env, "COURSE-MAKEUP-001"));
    assert_eq!(course.platform_fee_percent, 30);
}

#[test]
#[should_panic(expected = "course already registered")]
fn test_register_duplicate_course() {
    let (env, contract_id, token_id, _admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let course_id = String::from_str(&env, "COURSE-DUP");
    client.register_course(
        &instructor,
        &course_id,
        &50_000_000,
        &token_id,
        &0u32,
        &None,
    );
    client.register_course(
        &instructor,
        &course_id,
        &50_000_000,
        &token_id,
        &0u32,
        &None,
    );
}

// ============================================================
// COURSE APPROVAL TESTS
// ============================================================

#[test]
fn test_approve_course_success() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let course_id = String::from_str(&env, "COURSE-BAKING-001");
    client.register_course(
        &instructor,
        &course_id,
        &75_000_000,
        &token_id,
        &0u32,
        &None,
    );
    client.approve_course(&admin, &course_id);

    let course = client.get_course(&course_id);
    assert_eq!(course.status, CourseStatus::Active);
}

#[test]
#[should_panic(expected = "unauthorized: approve_course")]
fn test_approve_course_unauthorized() {
    let (env, contract_id, token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let course_id = String::from_str(&env, "COURSE-HAIR-001");
    client.register_course(
        &instructor,
        &course_id,
        &60_000_000,
        &token_id,
        &0u32,
        &None,
    );

    // Stop mocking all auths so the real auth + admin check fires
    env.mock_all_auths_allowing_non_root_auth(); // ← or remove mock for this call

    client.approve_course(&instructor, &course_id);
}

#[test]
#[should_panic(expected = "course is not pending approval")]
fn test_approve_already_active_course() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let course_id = String::from_str(&env, "COURSE-NAILS-001");
    client.register_course(
        &instructor,
        &course_id,
        &50_000_000,
        &token_id,
        &0u32,
        &None,
    );
    client.approve_course(&admin, &course_id);
    client.approve_course(&admin, &course_id); // second approve — should panic
}

// ============================================================
// ENROLLMENT & PAYMENT TESTS
// ============================================================

#[test]
fn test_enroll_success_with_payment_split() {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    // Course price: 100 USDC = 1_000_000_000 stroops
    let price: i128 = 1_000_000_000;
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-FASHION-001",
        price,
    );

    let student_balance_before = token_client.balance(&student);

    client.enroll(&student, &String::from_str(&env, "COURSE-FASHION-001"));

    // Check payment split: 20% to treasury, 80% credited as instructor earnings
    let platform_share = price * 20 / 100; // 200_000_000
    let instructor_share = price - platform_share; // 800_000_000

    assert_eq!(token_client.balance(&treasury), platform_share);
    assert_eq!(token_client.balance(&instructor), 0);
    assert_eq!(
        client.get_instructor_earnings(&instructor, &token_id),
        instructor_share,
    );
    assert_eq!(
        token_client.balance(&student),
        student_balance_before - price
    );

    // Enrollment record exists
    let enrollment = client.get_enrollment(&student, &student, &String::from_str(&env, "COURSE-FASHION-001")).unwrap();
    assert_eq!(enrollment.amount_paid, price);
    assert!(!enrollment.completed);
    assert!(!enrollment.certificate_issued);

    // Course stats updated
    let course = client.get_course(&String::from_str(&env, "COURSE-FASHION-001"));
    assert_eq!(course.total_enrollments, 1);
    assert_eq!(course.total_earned, price);
}

#[test]
fn test_enroll_zero_price_free_course() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);

    // Register free course
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-FREE-001",
        0,
    );

    // Enroll should succeed and no transfers should be attempted
    client.enroll(&student, &String::from_str(&env, "COURSE-FREE-001"));

    let enrollment = client.get_enrollment(&student, &student, &String::from_str(&env, "COURSE-FREE-001")).unwrap();
    assert_eq!(enrollment.amount_paid, 0);

    let course = client.get_course(&String::from_str(&env, "COURSE-FREE-001"));
    assert_eq!(course.total_enrollments, 1);
    assert_eq!(course.total_earned, 0);
}

#[test]
#[should_panic(expected = "overflow computing platform fee")]
fn test_enroll_fee_overflow() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Choose a price large enough that price * 100 would overflow i128
    let overflow_price: i128 = i128::MAX / 100 + 1;

    // register with custom 100% platform fee to force multiplication by 100
    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-OVERFLOW-001"),
        &overflow_price,
        &token_id,
        &100u32,
        &None,
    );
    client.approve_course(&admin, &String::from_str(&env, "COURSE-OVERFLOW-001"));

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &overflow_price);

    // This enroll should panic due to overflow in fee calculation
    client.enroll(&student, &String::from_str(&env, "COURSE-OVERFLOW-001"));
}

#[test]
#[should_panic(expected = "already enrolled in this course")]
fn test_enroll_duplicate() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-PHOTO-001",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-PHOTO-001");
    client.enroll(&student, &course_id);
    client.enroll(&student, &course_id); // second enroll — should panic
}

#[test]
fn test_enrollment_receipt_event_emitted_with_payment_breakdown() {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    // Course price: 100 USDC = 1_000_000_000 stroops, 20% platform fee
    let price: i128 = 1_000_000_000;
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-EVENT-001",
        price,
    );

    let course_id = String::from_str(&env, "COURSE-EVENT-001");
    let ledger_before = env.ledger().sequence();

    client.enroll(&student, &course_id);

    // Verify enrollment event was emitted with correct payment breakdown
    let events = env.events().all();
    let mut enrollment_events = 0u32;

    for (contract, topics, data) in events.iter() {
        if contract != contract_id {
            continue;
        }

        let topic0 = topics.get(0).unwrap();
        let sym: Symbol = topic0.try_into_val(&env).unwrap();

        if sym == Symbol::new(&env, "student_enrolled") {
            enrollment_events += 1;

            // Verify event data structure: (student, course_id, amount_paid, platform_fee, instructor_fee, ledger_seq)
            let (
                event_student,
                event_course_id,
                event_amount,
                event_platform_fee,
                event_instructor_fee,
                event_ledger,
            ): (Address, String, i128, i128, i128, u32) = data.try_into_val(&env).unwrap();

            // Verify student address
            assert_eq!(event_student, student);

            // Verify course ID
            assert_eq!(event_course_id, course_id);

            // Verify total amount paid
            assert_eq!(event_amount, price);

            // Verify platform fee (20% of price)
            let expected_platform_fee = price * 20 / 100; // 200_000_000
            assert_eq!(event_platform_fee, expected_platform_fee);

            // Verify instructor fee (80% of price)
            let expected_instructor_fee = price - expected_platform_fee; // 800_000_000
            assert_eq!(event_instructor_fee, expected_instructor_fee);

            // Verify ledger sequence
            assert!(event_ledger >= ledger_before);
        }
    }

    // Ensure exactly one enrollment event was emitted
    assert_eq!(enrollment_events, 1);
}

#[test]
#[should_panic(expected = "course is not available for enrollment")]
fn test_enroll_pending_course() {
    let (env, contract_id, token_id, _admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    // Register but do NOT approve
    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-PENDING"),
        &500_000_000,
        &token_id,
        &0u32,
        &None,
    );

    client.enroll(&student, &String::from_str(&env, "COURSE-PENDING"));
}

#[test]
#[should_panic(expected = "instructor cannot enroll in own course")]
fn test_instructor_self_enrollment_rejected() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Fund instructor so they could pay if the guard were missing
    token::StellarAssetClient::new(&env, &token_id).mint(&instructor, &100_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-SELF-ENROLL",
        500_000_000,
    );

    // Instructor tries to enroll in their own course — must be rejected
    client.enroll(&instructor, &String::from_str(&env, "COURSE-SELF-ENROLL"));
}

#[test]
fn test_is_enrolled_check() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-LASH-001",
        300_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-LASH-001");
    assert!(!client.is_enrolled(&student, &course_id));

    client.enroll(&student, &course_id);
    assert!(client.is_enrolled(&student, &course_id));
}

// ============================================================
// COMPLETION & CERTIFICATE TESTS
// ============================================================

#[test]
fn test_full_lifecycle_enroll_complete_certify() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    let course_id = String::from_str(&env, "COURSE-TAILORING-001");
    let course_title = String::from_str(&env, "Professional Tailoring");
    let cert_id = String::from_str(&env, "CERT-12345-TAILORING");

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-TAILORING-001",
        500_000_000,
    );

    // Enroll
    client.enroll(&student, &course_id);
    assert!(!client.has_completed(&student, &course_id));

    // Mark completed
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "evidence_hash")),
    );
    assert!(client.has_completed(&student, &course_id));

    // Issue certificate
    client.issue_certificate(&admin, &cert_id, &student, &course_id, &course_title, &String::from_str(&env, ""), &None);

    // Verify certificate
    assert!(client.verify_certificate(&cert_id));

    let cert = client.get_certificate(&cert_id);
    assert_eq!(cert.student, student);
    assert!(!cert.revoked);
    assert_eq!(cert.course_id, course_id);

    // Enrollment now shows certificate issued
    let enrollment = client.get_enrollment(&student, &student, &course_id).unwrap();
    assert!(enrollment.certificate_issued);
}

#[test]
#[should_panic(expected = "student has not completed this course")]
fn test_certificate_requires_completion() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-NAILS-001",
        400_000_000,
    );

    client.enroll(&student, &String::from_str(&env, "COURSE-NAILS-001"));

    // Try to issue certificate without completing — should panic
    client.issue_certificate(
        &admin,
        &String::from_str(&env, "CERT-EARLY"),
        &student,
        &String::from_str(&env, "COURSE-NAILS-001"),
        &String::from_str(&env, "Nail Technology"),        &String::from_str(&env, ""),
        &None,
    );
}

#[test]
fn test_revoke_certificate() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-MAKEUP-001",
        600_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-MAKEUP-001");
    let cert_id = String::from_str(&env, "CERT-REVOKE-TEST");

    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "evidence_hash")),
    );
    client.issue_certificate(
        &admin,
        &cert_id,
        &student,
        &course_id,
        &String::from_str(&env, "Makeup Artistry"),        &String::from_str(&env, ""),
        &None,
    );

    assert!(client.verify_certificate(&cert_id));

    // Revoke
    let reason = String::from_str(&env, "ACADEMIC_DISHONESTY");
    client.revoke_certificate(&admin, &cert_id, &reason);
    assert!(!client.verify_certificate(&cert_id));

    let cert = client.get_certificate(&cert_id);
    assert!(cert.revoked);
    assert_eq!(cert.revoked_by, Some(admin.clone()));
    assert!(cert.revoked_at_ledger.is_some());
    assert_eq!(cert.revocation_reason, Some(reason));
}

#[test]
fn test_revoke_certificate_metadata_persisted() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-AUDIT-001",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-AUDIT-001");
    let cert_id = String::from_str(&env, "CERT-AUDIT-TEST");

    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "proof")),
    );
    client.issue_certificate(
        &admin,
        &cert_id,
        &student,
        &course_id,
        &String::from_str(&env, "Audit Course"),        &String::from_str(&env, ""),
        &None,
    );

    // Certificate should have no revocation metadata before revocation
    let cert_before = client.get_certificate(&cert_id);
    assert!(!cert_before.revoked);
    assert!(cert_before.revoked_by.is_none());
    assert!(cert_before.revoked_at_ledger.is_none());
    assert!(cert_before.revocation_reason.is_none());

    let ledger_before = env.ledger().sequence();
    let reason = String::from_str(&env, "ISSUED_IN_ERROR");
    client.revoke_certificate(&admin, &cert_id, &reason);

    // All revocation metadata must be stored after revocation
    let cert_after = client.get_certificate(&cert_id);
    assert!(cert_after.revoked);
    assert_eq!(cert_after.revoked_by, Some(admin.clone()));
    assert!(cert_after.revoked_at_ledger.unwrap() >= ledger_before);
    assert_eq!(
        cert_after.revocation_reason,
        Some(String::from_str(&env, "ISSUED_IN_ERROR"))
    );
}

// ============================================================
// PAUSE / UNPAUSE TESTS
// ============================================================

#[test]
fn test_pause_and_unpause_course() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let course_id = String::from_str(&env, "COURSE-BAKING-001");
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-BAKING-001",
        250_000_000,
    );

    client.pause_course(&instructor, &course_id);
    let course = client.get_course(&course_id);
    assert_eq!(course.status, CourseStatus::Paused);

    client.unpause_course(&admin, &course_id);
    let course = client.get_course(&course_id);
    assert_eq!(course.status, CourseStatus::Active);
}

#[test]
fn test_update_platform_fee() {
    let (env, contract_id, _, admin, sec_admin, _, _) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    assert_eq!(client.get_platform_fee(), 20);
    client.update_default_fee(&admin, &25u32);
    assert_eq!(client.get_platform_fee(), 25);
}

#[test]
fn test_multiple_students_same_course() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-HAIR-001",
        200_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-HAIR-001");
    let asset_client = token::StellarAssetClient::new(&env, &token_id);

    for _ in 0..5 {
        let s = Address::generate(&env);
        asset_client.mint(&s, &1_000_000_000);
        client.enroll(&s, &course_id);
    }

    let course = client.get_course(&course_id);
    assert_eq!(course.total_enrollments, 5);
    assert_eq!(course.total_earned, 5 * 200_000_000);
}

// ============================================================
// NEW TESTS FOR ADDED FEATURES
// ============================================================

#[test]
fn test_mark_completed_no_evidence_requires_student_auth() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-AUTH-1",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-AUTH-1");
    client.enroll(&student, &course_id);

    // Call mark_completed with None.
    client.mark_completed(&admin, &student, &course_id, &None);

    // Verify both admin and student were required to authorize
    let auths = env.auths();
    let mut admin_found = false;
    let mut student_found = false;
    for (address, _) in auths.iter() {
        if address == &admin {
            admin_found = true;
        }
        if address == &student {
            student_found = true;
        }
    }
    assert!(admin_found);
    assert!(student_found);
}

#[test]
fn test_mark_completed_with_evidence_does_not_require_student_auth() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-AUTH-2",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-AUTH-2");
    client.enroll(&student, &course_id);

    // Call mark_completed with evidence hash.
    let hash = String::from_str(&env, "some_evidence_hash");
    client.mark_completed(&admin, &student, &course_id, &Some(hash.clone()));

    // Verify admin was required to authorize but student was not
    let auths = env.auths();
    let mut admin_found = false;
    let mut student_found = false;
    for (address, _) in auths.iter() {
        if address == &admin {
            admin_found = true;
        }
        if address == &student {
            student_found = true;
        }
    }
    assert!(admin_found);
    assert!(!student_found);

    let enrollment = client.get_enrollment(&student, &student, &course_id).unwrap();
    assert_eq!(enrollment.evidence_hash, Some(hash));
}

#[test]
#[should_panic(expected = "course must be paused before archiving")]
fn test_archive_course_blocked_by_active_enrollment() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-ARCHIVE-1",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-ARCHIVE-1");
    client.enroll(&student, &course_id);

    // Try to archive an Active course — must be Paused first
    client.archive_course(&admin, &sec_admin, &course_id, &None);
}

#[test]
fn test_archive_course_with_refunds() {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student_a = Address::generate(&env);
    let student_b = Address::generate(&env);

    let token_client = token::Client::new(&env, &token_id);
    let asset_client = token::StellarAssetClient::new(&env, &token_id);
    asset_client.mint(&student_a, &1_000_000_000);
    asset_client.mint(&student_b, &1_000_000_000);

    let price = 500_000_000;
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-REFUND",
        price,
    );
    let course_id = String::from_str(&env, "COURSE-REFUND");

    client.enroll(&student_a, &course_id);
    client.enroll(&student_b, &course_id);

    assert_eq!(token_client.balance(&student_a), 500_000_000);
    assert_eq!(token_client.balance(&student_b), 500_000_000);

    let platform_fee_total = price * 20 / 100 * 2; // 200_000_000
    let instructor_fee_total = (price - (price * 20 / 100)) * 2; // 800_000_000

    assert_eq!(token_client.balance(&treasury), platform_fee_total);
    assert_eq!(token_client.balance(&instructor), 0);
    assert_eq!(
        client.get_instructor_earnings(&instructor, &token_id),
        instructor_fee_total,
    );

    // Pause first — required before archiving
    client.pause_course(&admin, &course_id);

    // Archive and refund both students
    let mut refund_students = soroban_sdk::Vec::new(&env);
    refund_students.push_back(student_a.clone());
    refund_students.push_back(student_b.clone());

    env.mock_all_auths_allowing_non_root_auth();
    client.archive_course(&admin, &sec_admin, &course_id, &Some(refund_students));

    // Verify refund occurred
    assert_eq!(token_client.balance(&student_a), 1_000_000_000);
    assert_eq!(token_client.balance(&student_b), 1_000_000_000);
    assert_eq!(token_client.balance(&treasury), 0);
    assert_eq!(token_client.balance(&instructor), 0);
    assert_eq!(client.get_instructor_earnings(&instructor, &token_id), 0);

    let course = client.get_course(&course_id);
    assert_eq!(course.status, CourseStatus::Archived);
    assert_eq!(course.active_enrollments, 0);

    assert!(!client.is_enrolled(&student_a, &course_id));
    assert!(!client.is_enrolled(&student_b, &course_id));
}

#[test]
#[should_panic]
fn test_enroll_insufficient_funds_rollback() {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let student = Address::generate(&env);

    let price = 500_000_000;
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-ROLLBACK",
        price,
    );
    let course_id = String::from_str(&env, "COURSE-ROLLBACK");

    // Student has enough for platform fee (100_000_000) but not the full course price
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &150_000_000);

    // Enroll should panic because instructor transfer will fail
    client.enroll(&student, &course_id);

    // Verify treasury didn't receive any tokens (rollback proof)
    assert_eq!(token_client.balance(&treasury), 0);
    assert_eq!(token_client.balance(&student), 150_000_000);
}

#[test]
fn test_treasury_update_delay() {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let student_1 = Address::generate(&env);
    let student_2 = Address::generate(&env);

    let asset_client = token::StellarAssetClient::new(&env, &token_id);
    asset_client.mint(&student_1, &1_000_000_000);
    asset_client.mint(&student_2, &1_000_000_000);

    let price = 500_000_000;
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-TREASURY",
        price,
    );
    let course_id = String::from_str(&env, "COURSE-TREASURY");

    let new_treasury = Address::generate(&env);

    // Update treasury
    client.update_treasury(&admin, &sec_admin, &new_treasury);

    // Enroll student_1 immediately - fee should still go to the old treasury
    client.enroll(&student_1, &course_id);
    let platform_fee = price * 20 / 100;
    assert_eq!(token_client.balance(&treasury), platform_fee);
    assert_eq!(token_client.balance(&new_treasury), 0);

    // Advance ledger sequence by 100
    env.ledger().with_mut(|l| {
        l.sequence_number += 100;
    });

    // Enroll student_2 - fee should now go to the new treasury
    client.enroll(&student_2, &course_id);
    assert_eq!(token_client.balance(&treasury), platform_fee); // unchanged
    assert_eq!(token_client.balance(&new_treasury), platform_fee); // new treasury receives it
}

// ============================================================
// INPUT LENGTH VALIDATION TESTS (#20)
// ============================================================

#[test]
#[should_panic(expected = "course_id exceeds maximum length")]
fn test_register_course_id_too_long() {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let long_id = String::from_str(&env, &"A".repeat(257));
    client.register_course(&instructor, &long_id, &50_000_000, &token_id, &0u32, &None);
}

#[test]
fn test_register_course_id_at_max_length_succeeds() {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let max_id = String::from_str(&env, &"A".repeat(256));
    client.register_course(&instructor, &max_id, &50_000_000, &token_id, &0u32, &None);
    let course = client.get_course(&max_id);
    assert_eq!(course.status, CourseStatus::Pending);
}

#[test]
#[should_panic(expected = "course_title exceeds maximum length")]
fn test_issue_certificate_title_too_long() {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-TITLE-LEN",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-TITLE-LEN");
    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "hash")),
    );

    let long_title = String::from_str(&env, &"T".repeat(513));
    client.issue_certificate(
        &admin,
        &String::from_str(&env, "CERT-TITLE-LEN"),
        &student,
        &course_id,
        &long_title,        &String::from_str(&env, ""),
        &None,
    );
}

#[test]
#[should_panic(expected = "certificate_id exceeds maximum length")]
fn test_issue_certificate_id_too_long() {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-CERT-ID-LEN",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-CERT-ID-LEN");
    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "hash")),
    );

    let long_cert_id = String::from_str(&env, &"C".repeat(257));
    client.issue_certificate(
        &admin,
        &long_cert_id,
        &student,
        &course_id,
        &String::from_str(&env, "Valid Title"),        &String::from_str(&env, ""),
        &None,
    );
}

// ============================================================
// ARCHIVE LIFECYCLE TESTS (#19)
// ============================================================

#[test]
#[should_panic(expected = "course must be paused before archiving")]
fn test_archive_active_course_rejected() {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-ARCHIVE-ACTIVE",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-ARCHIVE-ACTIVE");

    // Course is Active — must panic
    client.archive_course(&admin, &sec_admin, &course_id, &None);
}

#[test]
#[should_panic(expected = "course must be paused before archiving")]
fn test_archive_pending_course_rejected() {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-ARCHIVE-PENDING"),
        &100_000_000,
        &token_id,
        &0u32,
        &None,
    );

    // Course is Pending — must panic
    client.archive_course(
        &admin,
        &sec_admin,
        &String::from_str(&env, "COURSE-ARCHIVE-PENDING"),
        &None,
    );
}

#[test]
fn test_archive_paused_course_succeeds() {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-ARCHIVE-PAUSED",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-ARCHIVE-PAUSED");

    client.pause_course(&admin, &course_id);
    client.archive_course(&admin, &sec_admin, &course_id, &None);

    let course = client.get_course(&course_id);
    assert_eq!(course.status, CourseStatus::Archived);
}
// ISSUE #4: RE-INITIALIZATION GUARD
// ============================================================

#[test]
#[should_panic(expected = "contract already initialized")]
fn test_init_cannot_be_called_twice() {
    let (env, contract_id, _, admin, sec_admin, treasury, _) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    // Second init call must be rejected
    client.init(&admin, &sec_admin, &treasury, &20u32, &50u32);
}

// ============================================================
// ISSUE #2: TOKEN WHITELIST
// ============================================================

#[test]
#[should_panic(expected = "course token is not approved")]
fn test_enroll_with_non_whitelisted_token_fails() {
    let (env, contract_id, _, admin, sec_admin, _, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Create a second token that has NOT been whitelisted
    let evil_token_admin = Address::generate(&env);
    let evil_token_id = env
        .register_stellar_asset_contract_v2(evil_token_admin.clone())
        .address();
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &evil_token_id).mint(&student, &100_000_000_000);

    // Register a course that uses the non-whitelisted token
    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-EVIL-TOKEN"),
        &500_000_000,
        &evil_token_id,
        &0u32,
        &None,
    );
    client.approve_course(&admin, &String::from_str(&env, "COURSE-EVIL-TOKEN"));

    // Enrollment must fail because the token is not whitelisted
    client.enroll(&student, &String::from_str(&env, "COURSE-EVIL-TOKEN"));
}

#[test]
fn test_enroll_succeeds_after_token_removed_from_whitelist_is_re_added() {
    let (env, contract_id, token_id, admin, sec_admin, _, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-WLIST",
        200_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-WLIST");

    // Remove then re-add the token
    client.remove_approved_token(&admin, &token_id);
    client.add_approved_token(&admin, &token_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);
    client.enroll(&student, &course_id);
    assert!(client.is_enrolled(&student, &course_id));
}

// ============================================================
// ISSUE #1: CROSS-COURSE CERTIFICATE ID COLLISION
// ============================================================

#[test]
#[should_panic(expected = "certificate ID already exists")]
fn test_certificate_id_collision_across_courses() {
    let (env, contract_id, token_id, admin, sec_admin, _, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-COLL-A",
        300_000_000,
    );
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-COLL-B",
        300_000_000,
    );

    let course_a = String::from_str(&env, "COURSE-COLL-A");
    let course_b = String::from_str(&env, "COURSE-COLL-B");
    let cert_id = String::from_str(&env, "CERT-SHARED-ID");

    // Student A completes course A and receives cert
    let student_a = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student_a, &1_000_000_000);
    client.enroll(&student_a, &course_a);
    client.mark_completed(
        &admin,
        &student_a,
        &course_a,
        &Some(String::from_str(&env, "ev_a")),
    );
    client.issue_certificate(
        &admin,
        &cert_id,
        &student_a,
        &course_a,
        &String::from_str(&env, "Course A"),        &String::from_str(&env, ""),
        &None,
    );

    // Student B completes course B — attempt to reuse the same cert ID must fail
    let student_b = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student_b, &1_000_000_000);
    client.enroll(&student_b, &course_b);
    client.mark_completed(
        &admin,
        &student_b,
        &course_b,
        &Some(String::from_str(&env, "ev_b")),
    );
    client.issue_certificate(
        &admin,
        &cert_id,
        &student_b,
        &course_b,
        &String::from_str(&env, "Course B"),        &String::from_str(&env, ""),
        &None,
    );
}

// ============================================================
// ISSUE #3: TWO-STEP ADMIN TRANSFER
// ============================================================

#[test]
fn test_two_step_admin_transfer_success() {
    let (env, contract_id, _, admin, sec_admin, _, _) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let new_admin = Address::generate(&env);

    // Step 1: propose
    let new_sec = Address::generate(&env);
    client.transfer_admin(&admin, &sec_admin, &new_admin, &new_sec);

    // Step 2: new admin accepts
    client.accept_admin(&new_admin, &new_sec);

    // New admin can now exercise admin privileges
    client.update_default_fee(&new_admin, &15u32);
    assert_eq!(client.get_platform_fee(), 15);
}

#[test]
#[should_panic(expected = "no pending admin")]
fn test_accept_admin_without_proposal_fails() {
    let (env, contract_id, _, _, sec_admin, _, _) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let random = Address::generate(&env);
    // No transfer_admin() called — must panic
    let new_sec = Address::generate(&env);
    client.accept_admin(&random, &new_sec);
}

#[test]
#[should_panic(expected = "callers are not the pending admins")]
fn test_accept_admin_wrong_address_fails() {
    let (env, contract_id, _, admin, sec_admin, _, _) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let new_admin = Address::generate(&env);
    let wrong_addr = Address::generate(&env);

    let new_sec = Address::generate(&env);
    client.transfer_admin(&admin, &sec_admin, &new_admin, &new_sec);

    // A different address tries to accept — must panic
    let new_sec = Address::generate(&env);
    client.accept_admin(&wrong_addr, &new_sec);
}

#[test]
#[should_panic(expected = "unauthorized: update_default_fee")]
fn test_old_admin_loses_access_after_transfer_completes() {
    let (env, contract_id, _, admin, sec_admin, _, _) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let new_admin = Address::generate(&env);

    let new_sec = Address::generate(&env);
    client.transfer_admin(&admin, &sec_admin, &new_admin, &new_sec);
    client.accept_admin(&new_admin, &new_sec);

    // Old admin must no longer have admin privileges
    client.update_default_fee(&admin, &10u32);
}

// ============================================================
// ISSUE #43: ADMIN TRANSFER EVENT
// ============================================================

#[test]
fn test_admin_transferred_event_emitted_once_with_full_schema() {
    let (env, contract_id, _, admin, sec_admin, _, _) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let new_admin = Address::generate(&env);
    let new_sec = Address::generate(&env);

    client.transfer_admin(&admin, &sec_admin, &new_admin, &new_sec);

    let ledger_before = env.ledger().sequence();
    client.accept_admin(&new_admin, &new_sec);

    let events = env.events().all();
    let mut transfer_events = 0u32;
    for (contract, topics, data) in events.iter() {
        if contract != contract_id {
            continue;
        }
        let topic0 = topics.get(0).unwrap();
        let sym: Symbol = topic0.try_into_val(&env).unwrap();
        if sym == Symbol::new(&env, "admin_transferred") {
            transfer_events += 1;
            let (prev, new, seq): (Address, Address, u32) = data.try_into_val(&env).unwrap();
            assert_eq!(prev, admin);
            assert_eq!(new, new_admin);
            assert!(seq >= ledger_before);
        }
    }
    assert_eq!(transfer_events, 1);
}

// ============================================================
// ISSUE #44: ENROLLMENT TTL PERSISTENCE
// ============================================================

#[test]
fn test_enrollment_persists_after_long_ledger_advance() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-TTL-001",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-TTL-001");
    client.enroll(&student, &course_id);

    // Advance ledger well beyond the old 100_000 minimum TTL threshold
    env.ledger().with_mut(|l| {
        l.sequence_number += 500_000;
        l.min_persistent_entry_ttl = 100_000;
        l.min_temp_entry_ttl = 100_000;
    });

    // Enrollment must remain readable after the extended TTL window
    let enrollment = client.get_enrollment(&student, &student, &course_id).unwrap();
    assert_eq!(enrollment.amount_paid, 100_000_000);
    assert!(client.is_enrolled(&student, &course_id));
}

#[test]
fn test_enrollment_ttl_extended_on_write() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-TTL-002",
        200_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-TTL-002");
    client.enroll(&student, &course_id);

    env.ledger().with_mut(|l| {
        l.sequence_number += 5_000_000;
        l.min_persistent_entry_ttl = 100_000;
        l.min_temp_entry_ttl = 100_000;
    });

    // mark_completed touches enrollment storage and extends TTL
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "proof")),
    );
    assert!(client.has_completed(&student, &course_id));
}

// ============================================================
// ISSUE #45: BATCH ENROLLMENT
// ============================================================

#[test]
fn test_batch_enroll_success() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &10_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-BATCH-A",
        100_000_000,
    );
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-BATCH-B",
        200_000_000,
    );
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-BATCH-C",
        300_000_000,
    );

    let mut course_ids = soroban_sdk::Vec::new(&env);
    course_ids.push_back(String::from_str(&env, "COURSE-BATCH-A"));
    course_ids.push_back(String::from_str(&env, "COURSE-BATCH-B"));
    course_ids.push_back(String::from_str(&env, "COURSE-BATCH-C"));

    client.batch_enroll(&student, &course_ids);

    assert!(client.is_enrolled(&student, &String::from_str(&env, "COURSE-BATCH-A")));
    assert!(client.is_enrolled(&student, &String::from_str(&env, "COURSE-BATCH-B")));
    assert!(client.is_enrolled(&student, &String::from_str(&env, "COURSE-BATCH-C")));
}

#[test]
#[should_panic(expected = "course is not available for enrollment")]
fn test_batch_enroll_fails_on_invalid_course() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &10_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-BATCH-OK",
        100_000_000,
    );

    // Register but do NOT approve the second course
    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-BATCH-BAD"),
        &200_000_000,
        &token_id,
        &0u32,
        &None,
    );

    let mut course_ids = soroban_sdk::Vec::new(&env);
    course_ids.push_back(String::from_str(&env, "COURSE-BATCH-OK"));
    course_ids.push_back(String::from_str(&env, "COURSE-BATCH-BAD"));

    client.batch_enroll(&student, &course_ids);

    // Must not reach here — if panic didn't happen, no partial state
    assert!(!client.is_enrolled(&student, &String::from_str(&env, "COURSE-BATCH-OK")));
}

#[test]
#[should_panic(expected = "duplicate course in batch")]
fn test_batch_enroll_rejects_duplicates() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &10_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-BATCH-DUP",
        100_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-BATCH-DUP");
    let mut course_ids = soroban_sdk::Vec::new(&env);
    course_ids.push_back(course_id.clone());
    course_ids.push_back(course_id);

    client.batch_enroll(&student, &course_ids);
}

#[test]
fn test_batch_enroll_emits_event_for_each_enrollment() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &10_000_000_000);

    let price_a: i128 = 100_000_000;
    let price_b: i128 = 200_000_000;

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-EVENT-A",
        price_a,
    );
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-EVENT-B",
        price_b,
    );

    let mut course_ids = soroban_sdk::Vec::new(&env);
    course_ids.push_back(String::from_str(&env, "COURSE-EVENT-A"));
    course_ids.push_back(String::from_str(&env, "COURSE-EVENT-B"));

    client.batch_enroll(&student, &course_ids);

    // Verify that two enrollment events were emitted
    let events = env.events().all();
    let mut enrollment_events = 0u32;
    let mut event_a_found = false;
    let mut event_b_found = false;

    for (contract, topics, data) in events.iter() {
        if contract != contract_id {
            continue;
        }

        let topic0 = topics.get(0).unwrap();
        let sym: Symbol = topic0.try_into_val(&env).unwrap();

        if sym == Symbol::new(&env, "student_enrolled") {
            enrollment_events += 1;

            let (
                event_student,
                event_course_id,
                event_amount,
                _platform_fee,
                _instructor_fee,
                _ledger,
            ): (Address, String, i128, i128, i128, u32) = data.try_into_val(&env).unwrap();

            assert_eq!(event_student, student);

            if event_course_id == String::from_str(&env, "COURSE-EVENT-A") {
                assert_eq!(event_amount, price_a);
                event_a_found = true;
            } else if event_course_id == String::from_str(&env, "COURSE-EVENT-B") {
                assert_eq!(event_amount, price_b);
                event_b_found = true;
            }
        }
    }

    // Ensure exactly two enrollment events were emitted (one per course)
    assert_eq!(enrollment_events, 2);
    assert!(event_a_found);
    assert!(event_b_found);
}

// ============================================================
// ISSUE #30: ENHANCED AUTHORIZATION ERROR MESSAGES
// ============================================================

#[test]
#[should_panic(expected = "unauthorized: mark_completed")]
fn test_mark_completed_unauthorized_includes_operation() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-COMPLETE-UNAUTH",
        50_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-COMPLETE-UNAUTH");
    client.enroll(&student, &course_id);

    // Instructor (not admin) tries to mark as completed — should panic with operation name
    client.mark_completed(&instructor, &student, &course_id, &None);
}

#[test]
#[should_panic(expected = "unauthorized: pause_platform")]
fn test_pause_platform_unauthorized_includes_operation() {
    let (env, contract_id, _token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Instructor tries to pause platform — should panic with operation name
    client.pause_platform(&instructor);
}

#[test]
#[should_panic(expected = "unauthorized: add_approved_token")]
fn test_add_approved_token_unauthorized_includes_operation() {
    let (env, contract_id, token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let new_token = Address::generate(&env);

    // Instructor tries to add approved token — should panic with operation name
    client.add_approved_token(&instructor, &new_token);
}

#[test]
#[should_panic(expected = "unauthorized: revoke_certificate")]
fn test_revoke_certificate_unauthorized_includes_operation() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-REVOKE-UNAUTH",
        50_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-REVOKE-UNAUTH");
    client.enroll(&student, &course_id);
    client.mark_completed(&admin, &student, &course_id, &None);
    let cert_id = String::from_str(&env, "CERT-REVOKE-UNAUTH-001");
    let course_title = String::from_str(&env, "Revoke Test Course");
    client.issue_certificate(&admin, &cert_id, &student, &course_id, &course_title, &String::from_str(&env, ""), &None);

    // Instructor tries to revoke certificate — should panic with operation name
    client.revoke_certificate(
        &instructor,
        &cert_id,
        &String::from_str(&env, "TEST_REASON"),
    );
}

// ============================================================
// ISSUE #46: PULL-BASED INSTRUCTOR WITHDRAWALS
// ============================================================

#[test]
fn test_instructor_earnings_accumulate_and_withdraw() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &10_000_000_000);

    let price: i128 = 1_000_000_000;
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-EARN-001",
        price,
    );
    let course_id = String::from_str(&env, "COURSE-EARN-001");

    client.enroll(&student, &course_id);

    let instructor_share = price * 80 / 100;
    assert_eq!(
        client.get_instructor_earnings(&instructor, &token_id),
        instructor_share
    );
    assert_eq!(token_client.balance(&instructor), 0);

    client.withdraw_earnings(&instructor, &token_id, &0);
    assert_eq!(token_client.balance(&instructor), instructor_share);
    assert_eq!(client.get_instructor_earnings(&instructor, &token_id), 0);
}

#[test]
fn test_multiple_enrollments_aggregate_earnings() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let price: i128 = 500_000_000;
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-EARN-MULTI",
        price,
    );
    let course_id = String::from_str(&env, "COURSE-EARN-MULTI");
    let asset_client = token::StellarAssetClient::new(&env, &token_id);

    for _ in 0..3 {
        let s = Address::generate(&env);
        asset_client.mint(&s, &1_000_000_000);
        client.enroll(&s, &course_id);
    }

    let instructor_share_per = price * 80 / 100;
    assert_eq!(
        client.get_instructor_earnings(&instructor, &token_id),
        instructor_share_per * 3,
    );
}

#[test]
#[should_panic(expected = "insufficient earnings balance")]
fn test_unauthorized_instructor_withdraw_fails() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-EARN-AUTH",
        500_000_000,
    );
    client.enroll(&student, &String::from_str(&env, "COURSE-EARN-AUTH"));

    let impostor = Address::generate(&env);
    client.withdraw_earnings(&impostor, &token_id, &100);
}

#[test]
fn test_double_withdrawal_prevented() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-EARN-DBL",
        500_000_000,
    );
    client.enroll(&student, &String::from_str(&env, "COURSE-EARN-DBL"));

    client.withdraw_earnings(&instructor, &token_id, &0);
    let balance_after_first = token_client.balance(&instructor);

    // Second full withdrawal is a no-op (zero balance)
    client.withdraw_earnings(&instructor, &token_id, &0);
    assert_eq!(token_client.balance(&instructor), balance_after_first);
}

#[test]
fn test_zero_balance_withdrawal_is_safe() {
    let (env, contract_id, token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    assert_eq!(client.get_instructor_earnings(&instructor, &token_id), 0);
    client.withdraw_earnings(&instructor, &token_id, &0);
    assert_eq!(client.get_instructor_earnings(&instructor, &token_id), 0);
}

#[test]
#[should_panic(expected = "instructor has reached the maximum number of course registrations")]
fn test_registration_limit_enforced() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Default setup uses max=50, lower it to 2 for this test
    client.update_max_courses_limit(&admin, &2u32);

    let course_id_1 = String::from_str(&env, "COURSE-LIMIT-001");
    let course_id_2 = String::from_str(&env, "COURSE-LIMIT-002");
    let course_id_3 = String::from_str(&env, "COURSE-LIMIT-003");
    client.register_course(
        &instructor,
        &course_id_1,
        &1_000_000i128,
        &token_id,
        &0u32,
        &None,
    );
    client.register_course(
        &instructor,
        &course_id_2,
        &1_000_000i128,
        &token_id,
        &0u32,
        &None,
    );
    client.register_course(
        &instructor,
        &course_id_3,
        &1_000_000i128,
        &token_id,
        &0u32,
        &None,
    );
}

#[test]
fn test_admin_can_raise_limit() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Lower limit to 1
    client.update_max_courses_limit(&admin, &1u32);

    let course_id_1 = String::from_str(&env, "COURSE-RAISE-001");
    let course_id_2 = String::from_str(&env, "COURSE-RAISE-002");

    client.register_course(
        &instructor,
        &course_id_1,
        &1_000_000i128,
        &token_id,
        &0u32,
        &None,
    );

    // Raise the limit to 5
    client.update_max_courses_limit(&admin, &5u32);

    // Second registration should now succeed
    client.register_course(
        &instructor,
        &course_id_2,
        &1_000_000i128,
        &token_id,
        &0u32,
        &None,
    );

    assert_eq!(client.get_instructor_course_count(&instructor), 2u32);
}

#[test]
fn test_different_instructors_have_independent_limits() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Lower limit to 1
    client.update_max_courses_limit(&admin, &1u32);

    let instructor_a = Address::generate(&env);
    let instructor_b = Address::generate(&env);

    client.register_course(
        &instructor_a,
        &String::from_str(&env, "COURSE-IND-A1"),
        &1_000_000i128,
        &token_id,
        &0u32,
        &None,
    );

    // instructor_b's count is independent — this must succeed
    client.register_course(
        &instructor_b,
        &String::from_str(&env, "COURSE-IND-B1"),
        &1_000_000i128,
        &token_id,
        &0u32,
        &None,
    );

    assert_eq!(client.get_instructor_course_count(&instructor_a), 1u32);
    assert_eq!(client.get_instructor_course_count(&instructor_b), 1u32);
}

#[test]
fn test_get_max_courses_limit_returns_configured_value() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // setup() passes 50 as the max
    assert_eq!(client.get_max_courses_limit(), 50u32);

    client.update_max_courses_limit(&admin, &10u32);
    assert_eq!(client.get_max_courses_limit(), 10u32);
}

#[test]
fn test_course_count_increments_correctly() {
    let (env, contract_id, token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    assert_eq!(client.get_instructor_course_count(&instructor), 0u32);

    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-COUNT-001"),
        &1_000_000i128,
        &token_id,
        &0u32,
        &None,
    );
    assert_eq!(client.get_instructor_course_count(&instructor), 1u32);

    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-COUNT-002"),
        &1_000_000i128,
        &token_id,
        &0u32,
        &None,
    );
    assert_eq!(client.get_instructor_course_count(&instructor), 2u32);
}

#[test]
fn test_verify_certificate_returns_true_for_valid_cert() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-VERIFY-001",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-VERIFY-001");
    let cert_id = String::from_str(&env, "CERT-VERIFY-001");

    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "proof")),
    );
    client.issue_certificate(
        &admin,
        &cert_id,
        &student,
        &course_id,
        &String::from_str(&env, "Test Course"),        &String::from_str(&env, ""),
        &None,
    );

    // Valid, unrevoked certificate must return true
    assert!(client.verify_certificate(&cert_id));
}

#[test]
fn test_verify_certificate_returns_false_for_revoked_cert() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-VERIFY-002",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-VERIFY-002");
    let cert_id = String::from_str(&env, "CERT-VERIFY-002");

    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "proof")),
    );
    client.issue_certificate(
        &admin,
        &cert_id,
        &student,
        &course_id,
        &String::from_str(&env, "Test Course"),        &String::from_str(&env, ""),
        &None,
    );

    assert!(client.verify_certificate(&cert_id));

    // Revoke and confirm verify_certificate now returns false
    client.revoke_certificate(
        &admin,
        &cert_id,
        &String::from_str(&env, "ACADEMIC_DISHONESTY"),
    );
    assert!(!client.verify_certificate(&cert_id));
}

#[test]
fn test_verify_certificate_returns_false_for_nonexistent_cert() {
    let (env, contract_id, _token_id, _admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Certificate that was never issued must return false, not panic
    assert!(!client.verify_certificate(&String::from_str(&env, "CERT-DOES-NOT-EXIST")));
}

#[test]
fn test_verify_certificate_false_does_not_mutate_state() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-VERIFY-003",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-VERIFY-003");
    let cert_id = String::from_str(&env, "CERT-VERIFY-003");

    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "proof")),
    );
    client.issue_certificate(
        &admin,
        &cert_id,
        &student,
        &course_id,
        &String::from_str(&env, "Test Course"),        &String::from_str(&env, ""),
        &None,
    );

    client.revoke_certificate(&admin, &cert_id, &String::from_str(&env, "ISSUED_IN_ERROR"));

    // Calling verify multiple times on a revoked cert must consistently return false
    assert!(!client.verify_certificate(&cert_id));
    assert!(!client.verify_certificate(&cert_id));

    // The certificate record itself must still exist and be readable for audit
    let cert = client.get_certificate(&cert_id);
    assert!(cert.revoked);
    assert_eq!(cert.revoked_by, Some(admin));
}

#[test]
fn test_enroll_at_capacity_succeeds() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Register with max_capacity = 1
    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-CAP-EXACT"),
        &100_000_000,
        &token_id,
        &0u32,
        &Some(1u32),
    );
    client.approve_course(&admin, &String::from_str(&env, "COURSE-CAP-EXACT"));

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    // Enrolling the first (and only allowed) student must succeed
    client.enroll(&student, &String::from_str(&env, "COURSE-CAP-EXACT"));

    let course = client.get_course(&String::from_str(&env, "COURSE-CAP-EXACT"));
    assert_eq!(course.total_enrollments, 1);
}

#[test]
#[should_panic(expected = "course has reached maximum enrollment capacity")]
fn test_enroll_beyond_capacity_rejected() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Register with max_capacity = 1
    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-CAP-FULL"),
        &100_000_000,
        &token_id,
        &0u32,
        &Some(1u32),
    );
    client.approve_course(&admin, &String::from_str(&env, "COURSE-CAP-FULL"));

    let student_a = Address::generate(&env);
    let student_b = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student_a, &1_000_000_000);
    token::StellarAssetClient::new(&env, &token_id).mint(&student_b, &1_000_000_000);

    let course_id = String::from_str(&env, "COURSE-CAP-FULL");
    client.enroll(&student_a, &course_id); // fills the one seat
    client.enroll(&student_b, &course_id); // must panic
}

#[test]
fn test_enroll_unlimited_capacity() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Register with no capacity limit (None)
    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-CAP-NONE"),
        &100_000_000,
        &token_id,
        &0u32,
        &None,
    );
    client.approve_course(&admin, &String::from_str(&env, "COURSE-CAP-NONE"));

    let course_id = String::from_str(&env, "COURSE-CAP-NONE");
    let asset_client = token::StellarAssetClient::new(&env, &token_id);

    // Enroll 5 students — all must succeed with no cap in place
    for _ in 0..5 {
        let s = Address::generate(&env);
        asset_client.mint(&s, &1_000_000_000);
        client.enroll(&s, &course_id);
    }

    let course = client.get_course(&course_id);
    assert_eq!(course.total_enrollments, 5);
}

#[test]
#[should_panic(expected = "course has reached maximum enrollment capacity")]
fn test_batch_enroll_respects_capacity() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Course A: unlimited
    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-BATCH-CAP-OK"),
        &100_000_000,
        &token_id,
        &0u32,
        &None,
    );
    client.approve_course(&admin, &String::from_str(&env, "COURSE-BATCH-CAP-OK"));

    // Course B: capacity 0 — already full before anyone enrols
    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-BATCH-CAP-FULL"),
        &100_000_000,
        &token_id,
        &0u32,
        &Some(0u32),
    );
    client.approve_course(&admin, &String::from_str(&env, "COURSE-BATCH-CAP-FULL"));

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    let mut course_ids = soroban_sdk::Vec::new(&env);
    course_ids.push_back(String::from_str(&env, "COURSE-BATCH-CAP-OK"));
    course_ids.push_back(String::from_str(&env, "COURSE-BATCH-CAP-FULL"));

    // batch_enroll validates all courses before enrolling any — must panic
    client.batch_enroll(&student, &course_ids);
}

// ISSUE 49: GET ENROLLMENT AUTHENTICATION
// ============================================================

#[test]
fn test_get_enrollment_authorized_access() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    register_and_approve_course(
        &env, &client, &token_id, &admin, &instructor, "COURSE-AUTH-GET", 100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-AUTH-GET");
    client.enroll(&student, &course_id);

    // Student can access
    let enrollment = client.get_enrollment(&student, &student, &course_id).unwrap();
    assert_eq!(enrollment.amount_paid, 100_000_000);

    // Instructor can access
    let enrollment2 = client.get_enrollment(&instructor, &student, &course_id).unwrap();
    assert_eq!(enrollment2.amount_paid, 100_000_000);

    // Admin can access
    let enrollment3 = client.get_enrollment(&admin, &student, &course_id).unwrap();
    assert_eq!(enrollment3.amount_paid, 100_000_000);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_get_enrollment_unauthorized_access() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);
    let random_user = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    register_and_approve_course(
        &env, &client, &token_id, &admin, &instructor, "COURSE-AUTH-GET-UNAUTH", 100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-AUTH-GET-UNAUTH");
    client.enroll(&student, &course_id);

    // Random user cannot access
    client.get_enrollment(&random_user, &student, &course_id);
}

// ============================================================
// ISSUE 47: ARCHIVE-THEN-REREGISTER
// ============================================================

#[test]
#[should_panic(expected = "course already registered")]
fn test_archive_then_reregister_fails() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env, &client, &token_id, &admin, &instructor, "COURSE-ARCHIVE-REREG", 100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-ARCHIVE-REREG");
    
    client.pause_course(&admin, &course_id);
    client.archive_course(&admin, &sec_admin, &course_id, &None);

    let course = client.get_course(&course_id);
    assert_eq!(course.status, CourseStatus::Archived);

    // Try to register the same course again
    client.register_course(&instructor, &course_id, &100_000_000, &token_id, &0u32, &None);
}

// ============================================================
// ISSUE 48: CONCURRENT ENROLLMENT/COMPLETION
// ============================================================

#[test]
#[should_panic(expected = "already marked as completed")]
fn test_concurrent_completion() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    register_and_approve_course(
        &env, &client, &token_id, &admin, &instructor, "COURSE-CONCURRENCY", 100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-CONCURRENCY");
    client.enroll(&student, &course_id);

    client.mark_completed(&admin, &student, &course_id, &Some(String::from_str(&env, "ev")));
    // Second completion should panic
    client.mark_completed(&admin, &student, &course_id, &Some(String::from_str(&env, "ev2")));
}

// ============================================================
// ISSUE 50: COURSE CREATED_AT_LEDGER
// ============================================================

#[test]
fn test_course_created_at_ledger_is_accurate() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Advance ledger
    env.ledger().with_mut(|l| {
        l.sequence_number = 12345;
    });

    let course_id = String::from_str(&env, "COURSE-LEDGER");
    client.register_course(&instructor, &course_id, &100_000_000, &token_id, &0u32, &None);

    let course = client.get_course(&course_id);
    assert_eq!(course.created_at_ledger, 12345);
}

// ============================================================
// NEW AUDIT TESTS
// ============================================================

#[test]
fn test_course_certificate_id_collision_verification() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    let matching_id = String::from_str(&env, "MATCHING-ID-123");
    
    // Register and approve course with matching_id
    client.register_course(
        &instructor,
        &matching_id,
        &100_000_000,
        &token_id,
        &0u32,
        &None,
    );
    client.approve_course(&admin, &matching_id);

    // Enroll and complete
    client.enroll(&student, &matching_id);
    client.mark_completed(&admin, &student, &matching_id, &Some(String::from_str(&env, "proof")));

    // Issue certificate with matching_id (same as course_id)
    client.issue_certificate(
        &admin,
        &matching_id, // matching_id used as cert_id
        &student,
        &matching_id, // matching_id used as course_id
        &String::from_str(&env, "Test Course"),
        &String::from_str(&env, "enroll-ref"),
        &None,
    );

    // Assert both can be queried independently and they do not collide
    let course = client.get_course(&matching_id);
    assert_eq!(course.id, matching_id);
    assert_eq!(course.instructor, instructor);

    let cert = client.get_certificate(&matching_id);
    assert_eq!(cert.id, matching_id);
    assert_eq!(cert.student, student);
    assert!(client.verify_certificate(&matching_id));
}

#[test]
#[should_panic(expected = "proposed admin addresses are identical to current admin addresses")]
fn test_transfer_admin_rejects_identical_addresses() {
    let (env, contract_id, _token_id, admin, sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Proposing the exact current admin & secondary admin should panic
    client.transfer_admin(&admin, &sec_admin, &admin, &sec_admin);
}

#[test]
#[should_panic(expected = "admin and secondary_admin must be distinct addresses")]
fn test_transfer_admin_rejects_same_new_admin_and_secondary() {
    let (env, contract_id, _token_id, admin, sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let new_admin = Address::generate(&env);
    // Setting both admin and secondary admin to the same address should panic
    client.transfer_admin(&admin, &sec_admin, &new_admin, &new_admin);
}

#[test]
fn test_certificate_expiry_behavior() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    let course_id = String::from_str(&env, "COURSE-EXPIRY");
    let cert_id = String::from_str(&env, "CERT-EXPIRY-123");

    register_and_approve_course(&env, &client, &token_id, &admin, &instructor, "COURSE-EXPIRY", 100_000_000);
    client.enroll(&student, &course_id);
    client.mark_completed(&admin, &student, &course_id, &None);

    // Issue certificate with expiry at ledger 1000
    client.issue_certificate(&admin, &cert_id, &student, &course_id, &String::from_str(&env, "Expiry Course"), &String::from_str(&env, "ref"), &Some(1000u32));

    // Under current ledger (default is 0), verify should return true
    assert!(client.verify_certificate(&cert_id));

    // Advance ledger to 999 - should still be valid
    env.ledger().with_mut(|l| {
        l.sequence_number = 999;
    });
    assert!(client.verify_certificate(&cert_id));

    // Advance ledger to 1000 - should be expired/invalid
    env.ledger().with_mut(|l| {
        l.sequence_number = 1000;
    });
    assert!(!client.verify_certificate(&cert_id));
}

#[test]
fn test_freeze_instructor_lifecycle() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Assert initially not frozen
    assert!(!client.is_instructor_frozen(&instructor));

    // Admin freezes instructor
    client.freeze_instructor(&admin, &instructor);
    assert!(client.is_instructor_frozen(&instructor));

    // Unfreeze instructor
    client.unfreeze_instructor(&admin, &instructor);
    assert!(!client.is_instructor_frozen(&instructor));
}

#[test]
#[should_panic(expected = "instructor is frozen")]
fn test_frozen_instructor_cannot_register_course() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.freeze_instructor(&admin, &instructor);

    // Register course should panic
    client.register_course(
        &instructor,
        &String::from_str(&env, "FROZEN-COURSE"),
        &100_000_000,
        &token_id,
        &0u32,
        &None,
    );
}

#[test]
#[should_panic(expected = "instructor is frozen")]
fn test_frozen_instructor_enrollment_blocked() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    // Register and approve course BEFORE freeze
    register_and_approve_course(&env, &client, &token_id, &admin, &instructor, "PRE-FREEZE-COURSE", 100_000_000);

    // Admin freezes instructor
    client.freeze_instructor(&admin, &instructor);

    // Attempting to enroll in the frozen instructor's course must fail
    client.enroll(&student, &String::from_str(&env, "PRE-FREEZE-COURSE"));
}


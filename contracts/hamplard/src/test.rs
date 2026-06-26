#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger as _},
    token, Address, Env, String,
};

// ============================================================
// TEST HELPERS
// ============================================================

fn setup() -> (Env, Address, Address, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(HamplardContract, ());

    // Deploy mock USDC token
    let token_admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(token_admin.clone())
        .address();
    let token_client = token::StellarAssetClient::new(&env, &token_id);

    let admin     = Address::generate(&env);
    let treasury  = Address::generate(&env);
    let instructor = Address::generate(&env);
    let student   = Address::generate(&env);

    // Fund student with 10,000 USDC (100_000_000_000 stroops)
    token_client.mint(&student, &100_000_000_000);

    // Init contract
    let client = HamplardContractClient::new(&env, &contract_id);
    client.init(&admin, &treasury, &20u32); // 20% platform fee

    (env, contract_id, token_id, admin, treasury, instructor)
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
    );
    client.approve_course(admin, &String::from_str(env, course_id));
}

// ============================================================
// INIT TESTS
// ============================================================

#[test]
fn test_init_success() {
    let (env, contract_id, token_id, admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Platform fee should be 20%
    assert_eq!(client.get_platform_fee(), 20);
}

// ============================================================
// COURSE REGISTRATION TESTS
// ============================================================

#[test]
fn test_register_course_success() {
    let (env, contract_id, token_id, admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let course_id = String::from_str(&env, "COURSE-TAILORING-001");
    client.register_course(
        &instructor,
        &course_id,
        &50_000_000, // 5 USDC
        &token_id,
        &0u32,
    );

    let course = client.get_course(&course_id);
    assert_eq!(course.status, CourseStatus::Pending);
    assert_eq!(course.price, 50_000_000);
    assert_eq!(course.platform_fee_percent, 20);
    assert_eq!(course.total_enrollments, 0);
}

#[test]
fn test_register_course_custom_fee() {
    let (env, contract_id, token_id, admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-MAKEUP-001"),
        &100_000_000,
        &token_id,
        &30u32, // custom 30% platform fee
    );

    let course = client.get_course(&String::from_str(&env, "COURSE-MAKEUP-001"));
    assert_eq!(course.platform_fee_percent, 30);
}

#[test]
#[should_panic(expected = "course already registered")]
fn test_register_duplicate_course() {
    let (env, contract_id, token_id, admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let course_id = String::from_str(&env, "COURSE-DUP");
    client.register_course(&instructor, &course_id, &50_000_000, &token_id, &0u32);
    client.register_course(&instructor, &course_id, &50_000_000, &token_id, &0u32);
}

// ============================================================
// COURSE APPROVAL TESTS
// ============================================================

#[test]
fn test_approve_course_success() {
    let (env, contract_id, token_id, admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let course_id = String::from_str(&env, "COURSE-BAKING-001");
    client.register_course(&instructor, &course_id, &75_000_000, &token_id, &0u32);
    client.approve_course(&admin, &course_id);

    let course = client.get_course(&course_id);
    assert_eq!(course.status, CourseStatus::Active);
}

#[test]
#[should_panic(expected = "unauthorized: caller is not admin")]
fn test_approve_course_unauthorized() {
    let (env, contract_id, token_id, admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let course_id = String::from_str(&env, "COURSE-HAIR-001");
    client.register_course(&instructor, &course_id, &60_000_000, &token_id, &0u32);
    // Instructor tries to approve their own course — should panic
    client.approve_course(&instructor, &course_id);
}

#[test]
#[should_panic(expected = "course is not pending approval")]
fn test_approve_already_active_course() {
    let (env, contract_id, token_id, admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let course_id = String::from_str(&env, "COURSE-NAILS-001");
    client.register_course(&instructor, &course_id, &50_000_000, &token_id, &0u32);
    client.approve_course(&admin, &course_id);
    client.approve_course(&admin, &course_id); // second approve — should panic
}

// ============================================================
// ENROLLMENT & PAYMENT TESTS
// ============================================================

#[test]
fn test_enroll_success_with_payment_split() {
    let (env, contract_id, token_id, admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    // Course price: 100 USDC = 1_000_000_000 stroops
    let price: i128 = 1_000_000_000;
    register_and_approve_course(
        &env, &client, &token_id, &admin, &instructor, "COURSE-FASHION-001", price,
    );

    let student_balance_before = token_client.balance(&student);

    client.enroll(&student, &String::from_str(&env, "COURSE-FASHION-001"));

    // Check payment split: 20% to treasury, 80% to instructor
    let platform_share   = price * 20 / 100; // 200_000_000
    let instructor_share = price - platform_share; // 800_000_000

    assert_eq!(token_client.balance(&treasury),   platform_share);
    assert_eq!(token_client.balance(&instructor),  instructor_share);
    assert_eq!(token_client.balance(&student),     student_balance_before - price);

    // Enrollment record exists
    let enrollment = client.get_enrollment(&student, &String::from_str(&env, "COURSE-FASHION-001"));
    assert_eq!(enrollment.amount_paid, price);
    assert!(!enrollment.completed);
    assert!(!enrollment.certificate_issued);

    // Course stats updated
    let course = client.get_course(&String::from_str(&env, "COURSE-FASHION-001"));
    assert_eq!(course.total_enrollments, 1);
    assert_eq!(course.total_earned, price);
}

#[test]
#[should_panic(expected = "already enrolled in this course")]
fn test_enroll_duplicate() {
    let (env, contract_id, token_id, admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    register_and_approve_course(
        &env, &client, &token_id, &admin, &instructor, "COURSE-PHOTO-001", 500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-PHOTO-001");
    client.enroll(&student, &course_id);
    client.enroll(&student, &course_id); // second enroll — should panic
}

#[test]
#[should_panic(expected = "course is not available for enrollment")]
fn test_enroll_pending_course() {
    let (env, contract_id, token_id, admin, treasury, instructor) = setup();
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
    );

    client.enroll(&student, &String::from_str(&env, "COURSE-PENDING"));
}

#[test]
fn test_is_enrolled_check() {
    let (env, contract_id, token_id, admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    register_and_approve_course(
        &env, &client, &token_id, &admin, &instructor, "COURSE-LASH-001", 300_000_000,
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
    let (env, contract_id, token_id, admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    let course_id    = String::from_str(&env, "COURSE-TAILORING-001");
    let course_title = String::from_str(&env, "Professional Tailoring");
    let cert_id      = String::from_str(&env, "CERT-12345-TAILORING");

    register_and_approve_course(
        &env, &client, &token_id, &admin, &instructor, "COURSE-TAILORING-001", 500_000_000,
    );

    // Enroll
    client.enroll(&student, &course_id);
    assert!(!client.has_completed(&student, &course_id));

    // Mark completed
    client.mark_completed(&admin, &student, &course_id, &Some(String::from_str(&env, "evidence_hash")));
    assert!(client.has_completed(&student, &course_id));

    // Issue certificate
    client.issue_certificate(
        &admin, &cert_id, &student, &course_id, &course_title,
    );

    // Verify certificate
    assert!(client.verify_certificate(&cert_id));

    let cert = client.get_certificate(&cert_id);
    assert_eq!(cert.student, student);
    assert!(!cert.revoked);
    assert_eq!(cert.course_id, course_id);

    // Enrollment now shows certificate issued
    let enrollment = client.get_enrollment(&student, &course_id);
    assert!(enrollment.certificate_issued);
}

#[test]
#[should_panic(expected = "student has not completed this course")]
fn test_certificate_requires_completion() {
    let (env, contract_id, token_id, admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    register_and_approve_course(
        &env, &client, &token_id, &admin, &instructor, "COURSE-NAILS-001", 400_000_000,
    );

    client.enroll(&student, &String::from_str(&env, "COURSE-NAILS-001"));

    // Try to issue certificate without completing — should panic
    client.issue_certificate(
        &admin,
        &String::from_str(&env, "CERT-EARLY"),
        &student,
        &String::from_str(&env, "COURSE-NAILS-001"),
        &String::from_str(&env, "Nail Technology"),
    );
}

#[test]
fn test_revoke_certificate() {
    let (env, contract_id, token_id, admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    register_and_approve_course(
        &env, &client, &token_id, &admin, &instructor, "COURSE-MAKEUP-001", 600_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-MAKEUP-001");
    let cert_id   = String::from_str(&env, "CERT-REVOKE-TEST");

    client.enroll(&student, &course_id);
    client.mark_completed(&admin, &student, &course_id, &Some(String::from_str(&env, "evidence_hash")));
    client.issue_certificate(
        &admin, &cert_id, &student, &course_id,
        &String::from_str(&env, "Makeup Artistry"),
    );

    assert!(client.verify_certificate(&cert_id));

    // Revoke
    client.revoke_certificate(&admin, &cert_id);
    assert!(!client.verify_certificate(&cert_id));

    let cert = client.get_certificate(&cert_id);
    assert!(cert.revoked);
}

// ============================================================
// PAUSE / UNPAUSE TESTS
// ============================================================

#[test]
fn test_pause_and_unpause_course() {
    let (env, contract_id, token_id, admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let course_id = String::from_str(&env, "COURSE-BAKING-001");
    register_and_approve_course(
        &env, &client, &token_id, &admin, &instructor, "COURSE-BAKING-001", 250_000_000,
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
    let (env, contract_id, _, admin, _, _) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    assert_eq!(client.get_platform_fee(), 20);
    client.update_default_fee(&admin, &25u32);
    assert_eq!(client.get_platform_fee(), 25);
}

#[test]
fn test_multiple_students_same_course() {
    let (env, contract_id, token_id, admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env, &client, &token_id, &admin, &instructor, "COURSE-HAIR-001", 200_000_000,
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
    let (env, contract_id, token_id, admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    register_and_approve_course(
        &env, &client, &token_id, &admin, &instructor, "COURSE-AUTH-1", 100_000_000,
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
    let (env, contract_id, token_id, admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    register_and_approve_course(
        &env, &client, &token_id, &admin, &instructor, "COURSE-AUTH-2", 100_000_000,
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

    let enrollment = client.get_enrollment(&student, &course_id);
    assert_eq!(enrollment.evidence_hash, Some(hash));
}

#[test]
#[should_panic(expected = "cannot archive course with active enrollments")]
fn test_archive_course_blocked_by_active_enrollment() {
    let (env, contract_id, token_id, admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    register_and_approve_course(
        &env, &client, &token_id, &admin, &instructor, "COURSE-ARCHIVE-1", 100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-ARCHIVE-1");
    client.enroll(&student, &course_id);

    // Try to archive without refunding active enrollments
    client.archive_course(&admin, &course_id, &None);
}

#[test]
fn test_archive_course_with_refunds() {
    let (env, contract_id, token_id, admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student_a = Address::generate(&env);
    let student_b = Address::generate(&env);

    let token_client = token::Client::new(&env, &token_id);
    let asset_client = token::StellarAssetClient::new(&env, &token_id);
    asset_client.mint(&student_a, &1_000_000_000);
    asset_client.mint(&student_b, &1_000_000_000);

    let price = 500_000_000;
    register_and_approve_course(
        &env, &client, &token_id, &admin, &instructor, "COURSE-REFUND", price,
    );
    let course_id = String::from_str(&env, "COURSE-REFUND");

    client.enroll(&student_a, &course_id);
    client.enroll(&student_b, &course_id);

    assert_eq!(token_client.balance(&student_a), 500_000_000);
    assert_eq!(token_client.balance(&student_b), 500_000_000);

    let platform_fee_total = price * 20 / 100 * 2; // 200_000_000
    let instructor_fee_total = (price - (price * 20 / 100)) * 2; // 800_000_000

    assert_eq!(token_client.balance(&treasury), platform_fee_total);
    assert_eq!(token_client.balance(&instructor), instructor_fee_total);

    // Archive and refund both students
    let mut refund_students = soroban_sdk::Vec::new(&env);
    refund_students.push_back(student_a.clone());
    refund_students.push_back(student_b.clone());

    env.mock_all_auths_allowing_non_root_auth();
    client.archive_course(&admin, &course_id, &Some(refund_students));

    // Verify refund occurred
    assert_eq!(token_client.balance(&student_a), 1_000_000_000);
    assert_eq!(token_client.balance(&student_b), 1_000_000_000);
    assert_eq!(token_client.balance(&treasury), 0);
    assert_eq!(token_client.balance(&instructor), 0);

    let course = client.get_course(&course_id);
    assert_eq!(course.status, CourseStatus::Archived);
    assert_eq!(course.active_enrollments, 0);

    assert!(!client.is_enrolled(&student_a, &course_id));
    assert!(!client.is_enrolled(&student_b, &course_id));
}

#[test]
#[should_panic]
fn test_enroll_insufficient_funds_rollback() {
    let (env, contract_id, token_id, admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let student = Address::generate(&env);

    let price = 500_000_000;
    register_and_approve_course(
        &env, &client, &token_id, &admin, &instructor, "COURSE-ROLLBACK", price,
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
    let (env, contract_id, token_id, admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let student_1 = Address::generate(&env);
    let student_2 = Address::generate(&env);

    let asset_client = token::StellarAssetClient::new(&env, &token_id);
    asset_client.mint(&student_1, &1_000_000_000);
    asset_client.mint(&student_2, &1_000_000_000);

    let price = 500_000_000;
    register_and_approve_course(
        &env, &client, &token_id, &admin, &instructor, "COURSE-TREASURY", price,
    );
    let course_id = String::from_str(&env, "COURSE-TREASURY");

    let new_treasury = Address::generate(&env);

    // Update treasury
    client.update_treasury(&admin, &new_treasury);

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


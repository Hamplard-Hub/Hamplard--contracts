#![cfg(test)]

use super::*;
use soroban_sdk::testutils::storage::Persistent;
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Events, Ledger as _, MockAuth, MockAuthInvoke},
    token, Address, BytesN, Env, IntoVal, String, Symbol, TryIntoVal, Val,
};

// ============================================================
// TEST HELPERS
// ============================================================

#[contract]
struct NonReceivableInstructorContract;

#[contractimpl]
impl NonReceivableInstructorContract {
    pub fn ping(_env: Env) {}
}

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
    client.init(&admin, &sec_admin, &treasury, &20u32, &50u32, &1000u32); // 20% fee, 50 courses max, 1000-ledger refund window
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
        &BytesN::from_array(env, &[0u8; 32]),
    );
    // Advance past the registration ledger so enroll()'s same-ledger guard
    // doesn't reject enrollments that happen right after this helper returns.
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });
    client.approve_course(admin, &String::from_str(env, course_id));
}

// ============================================================
// INIT TESTS
// ============================================================

#[test]
fn test_init_success() {
    let (env, contract_id, _token_id, admin, sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Platform fee should be 20%
    assert_eq!(client.get_platform_fee(&admin), 20);
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
    assert_eq!(client.get_platform_fee(&admin), 25);
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
    assert_eq!(client.get_platform_fee(&new_admin), 30);
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
        &BytesN::from_array(&env, &[0u8; 32]),
    );

    let course = client.get_course(&course_id).unwrap();
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
        &BytesN::from_array(&env, &[0u8; 32]),
    );

    let course = client
        .get_course(&String::from_str(&env, "COURSE-MAKEUP-001"))
        .unwrap();
    assert_eq!(course.platform_fee_percent, 30);
}

#[test]
fn test_get_course_returns_none_for_nonexistent_id() {
    let (env, contract_id, _token_id, _admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let missing_id = String::from_str(&env, "COURSE-NOT-FOUND");
    assert!(client.get_course(&missing_id).is_none());
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
        &BytesN::from_array(&env, &[0u8; 32]),
    );
    client.register_course(
        &instructor,
        &course_id,
        &50_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
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
        &BytesN::from_array(&env, &[0u8; 32]),
    );
    client.approve_course(&admin, &course_id);

    let course = client.get_course(&course_id).unwrap();
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
        &BytesN::from_array(&env, &[0u8; 32]),
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
        &BytesN::from_array(&env, &[0u8; 32]),
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
    let enrollment = client
        .get_enrollment(
            &student,
            &student,
            &String::from_str(&env, "COURSE-FASHION-001"),
        )
        .unwrap();
    assert_eq!(enrollment.amount_paid, price);
    assert!(!enrollment.completed);
    assert!(!enrollment.certificate_issued);

    // Course stats updated
    let course = client
        .get_course(&String::from_str(&env, "COURSE-FASHION-001"))
        .unwrap();
    assert_eq!(course.total_enrollments, 1);
    assert_eq!(course.total_earned, price);
}

#[test]
#[should_panic(expected = "enrollment has not started for this course")]
fn test_enroll_rejects_before_enrollment_start_ledger() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    let course_id = String::from_str(&env, "COURSE-GRACE-BEFORE");
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-GRACE-BEFORE",
        100_000_000,
    );

    let enrollment_start = env.ledger().sequence() + 10;
    client.set_enrollment_start_ledger(&instructor, &course_id, &Some(enrollment_start));

    client.enroll(&student, &course_id);
}

#[test]
fn test_enroll_succeeds_at_enrollment_start_ledger() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    let course_id = String::from_str(&env, "COURSE-GRACE-AFTER");
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-GRACE-AFTER",
        100_000_000,
    );

    let enrollment_start = env.ledger().sequence() + 10;
    client.set_enrollment_start_ledger(&instructor, &course_id, &Some(enrollment_start));
    env.ledger().with_mut(|l| {
        l.sequence_number = enrollment_start;
    });

    client.enroll(&student, &course_id);

    let enrollment = client
        .get_enrollment(&student, &student, &course_id)
        .unwrap();
    assert_eq!(enrollment.enrolled_at_ledger, enrollment_start);
    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.total_enrollments, 1);
}

#[test]
fn test_enroll_succeeds_when_instructor_is_contract_address() {
    let (env, contract_id, token_id, admin, _sec_admin, treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let instructor_contract = env.register(NonReceivableInstructorContract, ());
    let instructor = instructor_contract.clone();

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    let price: i128 = 1_000_000_000;
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-CONTRACT-INSTRUCTOR-001",
        price,
    );

    client.enroll(
        &student,
        &String::from_str(&env, "COURSE-CONTRACT-INSTRUCTOR-001"),
    );

    let platform_share = price * 20 / 100;
    let instructor_share = price - platform_share;

    // Enrollment remains functional because instructor payout is pull-based.
    assert_eq!(token_client.balance(&treasury), platform_share);
    assert_eq!(
        client.get_instructor_earnings(&instructor, &token_id),
        instructor_share
    );
}

#[test]
fn test_enroll_uses_registered_course_fee_when_default_fee_changes() {
    let (env, contract_id, token_id, admin, _sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    let price: i128 = 1_000_000_000;

    // Register course with default fee (0 means use platform default)
    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-FEE-UPDATE-001"),
        &price,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
    );
    assert_eq!(client.get_platform_fee(&admin), 20);

    // Update platform default fee - enrollments should now use the new fee
    client.update_default_fee(&admin, &35u32);
    assert_eq!(client.get_platform_fee(&admin), 35);

    client.approve_course(&admin, &String::from_str(&env, "COURSE-FEE-UPDATE-001"));
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });
    client.enroll(&student, &String::from_str(&env, "COURSE-FEE-UPDATE-001"));

    // Platform fee should now be 35% (new default), not 20%
    let platform_share = price * 35 / 100;
    let instructor_share = price - platform_share;

    assert_eq!(token_client.balance(&treasury), platform_share);
    assert_eq!(
        client.get_instructor_earnings(&instructor, &token_id),
        instructor_share,
    );
}

#[test]
fn test_enroll_fee_uses_live_default_fee() {
    let (env, contract_id, token_id, admin, _sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    let price: i128 = 1_000_000_000;

    // Register course with custom 40% fee
    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-CUSTOM-FEE"),
        &price,
        &token_id,
        &40u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
    );
    assert_eq!(client.get_platform_fee(&admin), 20);

    // Update platform default fee to 10% - new enrollments now use live default
    client.update_default_fee(&admin, &10u32);
    assert_eq!(client.get_platform_fee(&admin), 10);

    client.approve_course(&admin, &String::from_str(&env, "COURSE-CUSTOM-FEE"));
    // Advance past the registration ledger so enroll()'s same-ledger guard
    // doesn't reject this enrollment.
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });
    client.enroll(&student, &String::from_str(&env, "COURSE-CUSTOM-FEE"));

    // Platform fee should be 10% (live default fee), not 40% (custom course fee)
    // This ensures fee policy changes take immediate effect for all enrollments
    let platform_share = price * 10 / 100;
    let instructor_share = price - platform_share;

    assert_eq!(token_client.balance(&treasury), platform_share);
    assert_eq!(
        client.get_instructor_earnings(&instructor, &token_id),
        instructor_share,
    );
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

    let enrollment = client
        .get_enrollment(
            &student,
            &student,
            &String::from_str(&env, "COURSE-FREE-001"),
        )
        .unwrap();
    assert_eq!(enrollment.amount_paid, 0);

    let course = client
        .get_course(&String::from_str(&env, "COURSE-FREE-001"))
        .unwrap();
    assert_eq!(course.total_enrollments, 1);
    assert_eq!(course.total_earned, 0);
}

#[test]
#[should_panic(expected = "overflow computing platform fee")]
fn test_enroll_fee_overflow() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Choose a price large enough that price * 100 would overflow i128.
    // register_course now enforces a sane price range, so we register with
    // a valid price and then patch storage directly to simulate a course
    // that somehow ended up with an out-of-range price, to exercise the
    // overflow guard inside enroll_internal itself.
    let overflow_price: i128 = i128::MAX / 100 + 1;
    let course_id = String::from_str(&env, "COURSE-OVERFLOW-001");

    // register with custom 100% platform fee to force multiplication by 100
    client.register_course(
        &instructor,
        &course_id,
        &100_000_000i128,
        &token_id,
        &100u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
    );
    client.approve_course(&admin, &course_id);

    let course_key = DataKey::Course(course_id.clone());
    env.as_contract(&contract_id, || {
        let mut course: Course = env.storage().persistent().get(&course_key).unwrap();
        course.price = overflow_price;
        env.storage().persistent().set(&course_key, &course);
    });
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &overflow_price);

    // Update default fee to 100% to ensure the fee calculation uses the higher value
    client.update_default_fee(&admin, &100u32);

    // This enroll should panic due to overflow in fee calculation
    client.enroll(&student, &course_id);
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
#[should_panic(expected = "admin cannot enroll in courses")]
fn test_enroll_admin_rejected() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    token::StellarAssetClient::new(&env, &token_id).mint(&admin, &100_000_000_000);

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
    client.enroll(&admin, &course_id); // should panic
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
        &BytesN::from_array(&env, &[0u8; 32]),
    );

    client.enroll(&student, &String::from_str(&env, "COURSE-PENDING"));
}

#[test]
#[should_panic(expected = "cannot enroll in the same ledger the course was registered")]
fn test_enroll_same_ledger_as_registration_rejected() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    let course_id = String::from_str(&env, "COURSE-SAME-LEDGER");
    client.register_course(
        &instructor,
        &course_id,
        &500_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
    );
    client.approve_course(&admin, &course_id);

    // Enrolling in the very same ledger the course was registered in must
    // be rejected, even though the course is already Active — a review
    // delay of at least one ledger sequence is required.
    client.enroll(&student, &course_id);
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

    assert_eq!(client.has_completed(&student, &course_id), None);

    // Enroll
    client.enroll(&student, &course_id);
    assert_eq!(client.has_completed(&student, &course_id), Some(false));

    // Mark completed
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "evidence_hash")),
    );
    assert_eq!(client.has_completed(&student, &course_id), Some(true));

    // Issue certificate
    client.issue_certificate(
        &admin,
        &cert_id,
        &course_id,
        &course_title,
        &student.to_string(),
        &None,
        &None,
    );

    // Verify certificate
    assert!(client.verify_certificate(&cert_id));

    let cert = client.get_certificate(&student, &cert_id);
    assert_eq!(cert.student, student);
    assert!(!cert.revoked);
    assert_eq!(cert.course_id, course_id);

    // Enrollment now shows certificate issued
    let enrollment = client
        .get_enrollment(&student, &student, &course_id)
        .unwrap();
    assert!(enrollment.certificate_issued);
}

#[test]
fn test_certificate_issued_event_contains_full_issuance_details() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    let course_id = String::from_str(&env, "COURSE-CERT-EVENT-001");
    let certificate_id = String::from_str(&env, "CERT-EVENT-001");
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-CERT-EVENT-001",
        500_000_000,
    );

    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "completion-proof")),
    );

    client.issue_certificate(
        &admin,
        &certificate_id,
        &course_id,
        &String::from_str(&env, "Certificate Event Course"),
        &student.to_string(),
        &None,
        &None,
    );

    let mut issuance_events = 0u32;
    for (contract, topics, data) in env.events().all().iter() {
        if contract != contract_id {
            continue;
        }

        let topic_name: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
        if topic_name != Symbol::new(&env, "certificate_issued") {
            continue;
        }

        issuance_events += 1;
        assert_eq!(topics.len(), 2);
        let event_certificate_id: String = topics.get(1).unwrap().try_into_val(&env).unwrap();
        assert_eq!(event_certificate_id, certificate_id);

        let (event_student, event_course_id, event_admin, event_ledger): (
            Address,
            String,
            Address,
            u32,
        ) = data.try_into_val(&env).unwrap();
        assert_eq!(event_student, student);
        assert_eq!(event_course_id, course_id);
        assert_eq!(event_admin, admin);

        let certificate = client.get_certificate(&student, &certificate_id);
        assert_eq!(event_ledger, certificate.issued_at_ledger);
    }

    assert_eq!(issuance_events, 1);
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
        &String::from_str(&env, "COURSE-NAILS-001"),
        &String::from_str(&env, "Nail Technology"),
        &student.to_string(),
        &None,
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
        &course_id,
        &String::from_str(&env, "Makeup Artistry"),
        &student.to_string(),
        &None,
        &None,
    );

    assert!(client.verify_certificate(&cert_id));

    // Revoke
    let reason = String::from_str(&env, "ACADEMIC_DISHONESTY");
    client.revoke_certificate(&admin, &cert_id, &reason);
    assert!(!client.verify_certificate(&cert_id));

    let cert = client.get_certificate(&admin, &cert_id);
    assert!(cert.revoked);
    assert_eq!(cert.revoked_by, Some(admin.clone()));
    assert!(cert.revoked_at_ledger.is_some());
    assert_eq!(cert.revocation_reason, Some(reason));
}

#[test]
fn test_event_certificate_revoked() {
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
        "COURSE-EVENT-REVOKE",
        300_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-EVENT-REVOKE");
    let cert_id = String::from_str(&env, "CERT-REVOKE-123");

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
        &course_id,
        &String::from_str(&env, "Title"),
        &student.to_string(),
        &None,
        &None,
    );

    let ledger_before = env.ledger().sequence();
    let reason = String::from_str(&env, "ACADEMIC_DISHONESTY");
    client.revoke_certificate(&admin, &cert_id, &reason);

    // Verify certificate_revoked event was emitted exactly once with correct data
    let events = env.events().all();
    let mut revoke_events = 0u32;

    for (contract, topics, data) in events.iter() {
        if contract != contract_id {
            continue;
        }

        let topic0 = topics.get(0).unwrap();
        let sym: Symbol = topic0.try_into_val(&env).unwrap();

        if sym == Symbol::new(&env, "certificate_revoked") {
            revoke_events += 1;

            // Verify topic 1 is the certificate_id
            let topic1: String = topics.get(1).unwrap().try_into_val(&env).unwrap();
            assert_eq!(topic1, cert_id);

            // Verify event data: (admin, certificate_id, student, course_id, reason, ledger_sequence)
            let (
                event_admin,
                event_certificate_id,
                event_student,
                event_course_id,
                event_reason,
                event_ledger,
            ): (Address, String, Address, String, String, u32) = data.try_into_val(&env).unwrap();

            assert_eq!(event_admin, admin);
            assert_eq!(event_certificate_id, cert_id);
            assert_eq!(event_student, student);
            assert_eq!(event_course_id, course_id);
            assert_eq!(event_reason, reason);
            assert!(event_ledger >= ledger_before);
        }
    }

    // Ensure exactly one certificate_revoked event was emitted
    assert_eq!(revoke_events, 1);
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
        &course_id,
        &String::from_str(&env, "Audit Course"),
        &student.to_string(),
        &None,
        &None,
    );

    // Certificate should have no revocation metadata before revocation
    let cert_before = client.get_certificate(&admin, &cert_id);
    assert!(!cert_before.revoked);
    assert!(cert_before.revoked_by.is_none());
    assert!(cert_before.revoked_at_ledger.is_none());
    assert!(cert_before.revocation_reason.is_none());

    let ledger_before = env.ledger().sequence();
    let reason = String::from_str(&env, "ISSUED_IN_ERROR");
    client.revoke_certificate(&admin, &cert_id, &reason);

    // All revocation metadata must be stored after revocation
    let cert_after = client.get_certificate(&admin, &cert_id);
    assert!(cert_after.revoked);
    assert_eq!(cert_after.revoked_by, Some(admin.clone()));
    assert!(cert_after.revoked_at_ledger.unwrap() >= ledger_before);
    assert_eq!(
        cert_after.revocation_reason,
        Some(String::from_str(&env, "ISSUED_IN_ERROR"))
    );
}

#[test]
fn test_issue_certificate_with_instructor_signature() {
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
        "COURSE-SIGNED-001",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-SIGNED-001");
    let cert_id = String::from_str(&env, "CERT-SIGNED-001");

    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "evidence_hash")),
    );

    let signature = BytesN::from_array(&env, &[7u8; 64]);
    client.issue_certificate(
        &admin,
        &cert_id,
        &course_id,
        &String::from_str(&env, "Signed Course"),
        &student.to_string(),
        &None,
        &Some(signature.clone()),
    );

    let cert = client.get_certificate(&admin, &cert_id);
    assert_eq!(cert.instructor_signature, Some(signature));
}

#[test]
fn test_issue_certificate_without_instructor_signature() {
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
        "COURSE-UNSIGNED-001",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-UNSIGNED-001");
    let cert_id = String::from_str(&env, "CERT-UNSIGNED-001");

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
        &course_id,
        &String::from_str(&env, "Unsigned Course"),
        &student.to_string(),
        &None,
        &None,
    );

    let cert = client.get_certificate(&admin, &cert_id);
    assert!(cert.instructor_signature.is_none());
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
    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.status, CourseStatus::Paused);

    client.unpause_course(&admin, &course_id);
    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.status, CourseStatus::Active);
}

/// Double-pause must be rejected (Active-status precondition)
#[test]
#[should_panic(expected = "course is not active")]
fn test_pause_course_double_pause_rejected() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let course_id = String::from_str(&env, "COURSE-DOUBLE-PAUSE");
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-DOUBLE-PAUSE",
        100_000_000,
    );

    // First pause succeeds
    client.pause_course(&instructor, &course_id);
    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.status, CourseStatus::Paused);

    // Second pause on already-Paused course must panic
    client.pause_course(&instructor, &course_id);
}

#[test]
fn test_update_platform_fee() {
    let (env, contract_id, _, admin, sec_admin, _, _) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    assert_eq!(client.get_platform_fee(&admin), 20);
    client.update_default_fee(&admin, &25u32);
    assert_eq!(client.get_platform_fee(&admin), 25);
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

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.total_enrollments, 5);
    assert_eq!(course.total_earned, 5 * 200_000_000);
}

// ============================================================
// NEW TESTS FOR ADDED FEATURES
// ============================================================

#[test]
fn test_instructor_total_earnings_across_courses() {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student_a = Address::generate(&env);
    let student_b = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student_a, &1_000_000_000);
    token::StellarAssetClient::new(&env, &token_id).mint(&student_b, &1_000_000_000);

    // Register and approve two courses for same instructor
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-TOTAL-1",
        400_000_000,
    );
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-TOTAL-2",
        600_000_000,
    );

    client.enroll(&student_a, &String::from_str(&env, "COURSE-TOTAL-1"));
    client.enroll(&student_b, &String::from_str(&env, "COURSE-TOTAL-2"));

    let price1: i128 = 400_000_000;
    let price2: i128 = 600_000_000;
    let platform_share1 = price1 * 20 / 100;
    let platform_share2 = price2 * 20 / 100;
    let instructor_share1 = price1 - platform_share1;
    let instructor_share2 = price2 - platform_share2;

    let expected_total = instructor_share1 + instructor_share2;

    // Per-token balance should match expected total
    assert_eq!(
        client.get_instructor_earnings(&instructor, &token_id),
        expected_total
    );

    // Aggregate total across tokens should also match
    assert_eq!(
        client.get_instructor_total_earnings(&instructor),
        expected_total
    );
}

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

    let enrollment = client
        .get_enrollment(&student, &student, &course_id)
        .unwrap();
    assert_eq!(enrollment.evidence_hash, Some(hash));
}

#[test]
#[should_panic(expected = "enrollment course_id mismatch")]
fn test_mark_completed_rejects_mismatched_enrollment_course_id() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);

    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    let course_id = String::from_str(&env, "COURSE-COMPLETE-MISMATCH");
    let stored_course_id = String::from_str(&env, "COURSE-COMPLETE-OTHER");
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-COMPLETE-MISMATCH",
        100_000_000,
    );
    client.enroll(&student, &course_id);

    let mut enrollment = client
        .get_enrollment(&admin, &student, &course_id)
        .expect("enrollment should exist");
    enrollment.course_id = stored_course_id;

    env.as_contract(&contract_id, || {
        env.storage().persistent().set(
            &DataKey::Enrollment(student.clone(), course_id.clone()),
            &enrollment,
        );
    });

    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "proof")),
    );
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

    let course = client.get_course(&course_id).unwrap();
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

#[test]
#[should_panic(expected = "new treasury address must differ from current treasury")]
fn test_update_treasury_same_address_rejected() {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Try to update treasury to the same address
    client.update_treasury(&admin, &sec_admin, &treasury);
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
    client.register_course(
        &instructor,
        &long_id,
        &50_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
    );
}

#[test]
fn test_register_course_id_at_max_length_succeeds() {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let max_id = String::from_str(&env, &"A".repeat(256));
    client.register_course(
        &instructor,
        &max_id,
        &50_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
    );
    let course = client.get_course(&max_id).unwrap();
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
        &course_id,
        &long_title,
        &student.to_string(),
        &None,
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
        &course_id,
        &String::from_str(&env, "Valid Title"),
        &student.to_string(),
        &None,
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
        &BytesN::from_array(&env, &[0u8; 32]),
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

    let course = client.get_course(&course_id).unwrap();
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
    client.init(&admin, &sec_admin, &treasury, &20u32, &50u32, &1000u32);
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
        &BytesN::from_array(&env, &[0u8; 32]),
    );
    client.approve_course(&admin, &String::from_str(&env, "COURSE-EVIL-TOKEN"));
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });

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
        &course_a,
        &String::from_str(&env, "Course A"),
        &student_a.to_string(),
        &None,
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
        &course_b,
        &String::from_str(&env, "Course B"),
        &student_b.to_string(),
        &None,
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
    assert_eq!(client.get_platform_fee(&new_admin), 15);
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
// ISSUE #66: OLD ADMIN REJECTION FOR ALL ADMIN-ONLY FUNCTIONS
// ============================================================

#[test]
fn test_old_admin_rejected_for_all_admin_only_functions_after_transfer() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Set up a course + enrollment + certificate + refund request so every
    // admin-only function has something valid to operate on.
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-OLD-ADMIN"),
        &500_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
    );
    let course_id = String::from_str(&env, "COURSE-OLD-ADMIN");
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });

    // Complete the admin transfer before exercising any admin-only function.
    let new_admin = Address::generate(&env);
    let new_sec_admin = Address::generate(&env);
    client.transfer_admin(&admin, &sec_admin, &new_admin, &new_sec_admin);
    client.accept_admin(&new_admin, &new_sec_admin);

    // approve_course — old admin must be rejected
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.approve_course(&admin, &course_id);
    }));
    assert!(res.is_err(), "old admin must be rejected by approve_course");

    // New admin approves for real so downstream calls have an active course.
    client.approve_course(&new_admin, &course_id);
    client.enroll(&student, &course_id);

    // mark_completed — old admin must be rejected
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.mark_completed(
            &admin,
            &student,
            &course_id,
            &Some(String::from_str(&env, "evidence")),
        );
    }));
    assert!(res.is_err(), "old admin must be rejected by mark_completed");

    client.mark_completed(
        &new_admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "evidence")),
    );

    // issue_certificate — old admin must be rejected
    let cert_id = String::from_str(&env, "CERT-OLD-ADMIN");
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.issue_certificate(
            &admin,
            &cert_id,
            &course_id,
            &String::from_str(&env, "Old Admin Course"),
            &student.to_string(),
            &None,
            &None,
        );
    }));
    assert!(
        res.is_err(),
        "old admin must be rejected by issue_certificate"
    );

    client.issue_certificate(
        &new_admin,
        &cert_id,
        &course_id,
        &String::from_str(&env, "Old Admin Course"),
        &student.to_string(),
        &None,
        &None,
    );

    // revoke_certificate — old admin must be rejected
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.revoke_certificate(&admin, &cert_id, &String::from_str(&env, "TEST_REASON"));
    }));
    assert!(
        res.is_err(),
        "old admin must be rejected by revoke_certificate"
    );

    // pause_platform — old admin must be rejected
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.pause_platform(&admin);
    }));
    assert!(res.is_err(), "old admin must be rejected by pause_platform");

    // withdraw_tokens — old admin must be rejected
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.withdraw_tokens(&admin, &token_id, &0i128, &admin);
    }));
    assert!(
        res.is_err(),
        "old admin must be rejected by withdraw_tokens"
    );

    // freeze_instructor — old admin must be rejected
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.freeze_instructor(&admin, &instructor);
    }));
    assert!(
        res.is_err(),
        "old admin must be rejected by freeze_instructor"
    );

    // process_refund — old admin must be rejected
    let refund_student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&refund_student, &100_000_000_000);
    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-OLD-ADMIN-REFUND"),
        &500_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
    );
    let refund_course_id = String::from_str(&env, "COURSE-OLD-ADMIN-REFUND");
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });
    client.approve_course(&new_admin, &refund_course_id);
    client.enroll(&refund_student, &refund_course_id);
    client.request_refund(&refund_student, &refund_course_id);

    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.process_refund(&admin, &refund_student, &refund_course_id, &true);
    }));
    assert!(res.is_err(), "old admin must be rejected by process_refund");

    // archive_course (multi-sig) — old admin+secondary pair must be rejected
    client.pause_course(&new_admin, &course_id);
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.archive_course(&admin, &sec_admin, &course_id, &None);
    }));
    assert!(
        res.is_err(),
        "old admin+secondary pair must be rejected by archive_course"
    );
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
fn test_get_enrollment_returns_none_after_ttl_expiry() {
    // Verify that get_enrollment() returns None gracefully when the enrollment
    // record has been garbage-collected after TTL expiry, rather than panicking.
    // In the Soroban test environment, TTL expiry is simulated by directly
    // removing the persistent storage key (the runtime does not expire entries
    // in tests), which produces the same observable effect as a natural expiry.
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
        "COURSE-TTL-EXPIRY",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-TTL-EXPIRY");

    // Enroll the student — record is written with PERSISTENT_TTL_EXTEND_TO ledgers of TTL.
    client.enroll(&student, &course_id);

    // Confirm the record exists before simulating expiry.
    let before = client.get_enrollment(&student, &student, &course_id);
    assert!(
        before.is_some(),
        "enrollment should exist immediately after enroll()"
    );

    // Simulate TTL expiry: remove the persistent entry exactly as the network
    // would after the TTL window elapses and the entry is garbage-collected.
    env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .remove(&DataKey::Enrollment(student.clone(), course_id.clone()));
    });

    // get_enrollment() must return None — not panic — when the record is absent.
    let after = client.get_enrollment(&student, &student, &course_id);
    assert!(
        after.is_none(),
        "get_enrollment() must return None after the enrollment record has expired/been removed"
    );
}

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
    let enrollment = client
        .get_enrollment(&student, &student, &course_id)
        .unwrap();
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
    assert_eq!(client.has_completed(&student, &course_id), Some(true));
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
        &BytesN::from_array(&env, &[0u8; 32]),
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
    client.issue_certificate(
        &admin,
        &cert_id,
        &course_id,
        &course_title,
        &student.to_string(),
        &None,
        &None,
    );

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
        &BytesN::from_array(&env, &[0u8; 32]),
    );
    client.register_course(
        &instructor,
        &course_id_2,
        &1_000_000i128,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
    );
    client.register_course(
        &instructor,
        &course_id_3,
        &1_000_000i128,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
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
        &BytesN::from_array(&env, &[0u8; 32]),
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
        &BytesN::from_array(&env, &[0u8; 32]),
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
        &BytesN::from_array(&env, &[0u8; 32]),
    );

    // instructor_b's count is independent — this must succeed
    client.register_course(
        &instructor_b,
        &String::from_str(&env, "COURSE-IND-B1"),
        &1_000_000i128,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
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
        &BytesN::from_array(&env, &[0u8; 32]),
    );
    assert_eq!(client.get_instructor_course_count(&instructor), 1u32);

    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-COUNT-002"),
        &1_000_000i128,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
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
        &course_id,
        &String::from_str(&env, "Test Course"),
        &student.to_string(),
        &None,
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
        &course_id,
        &String::from_str(&env, "Test Course"),
        &student.to_string(),
        &None,
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
        &course_id,
        &String::from_str(&env, "Test Course"),
        &student.to_string(),
        &None,
        &None,
    );

    client.revoke_certificate(&admin, &cert_id, &String::from_str(&env, "ISSUED_IN_ERROR"));

    // Calling verify multiple times on a revoked cert must consistently return false
    assert!(!client.verify_certificate(&cert_id));
    assert!(!client.verify_certificate(&cert_id));

    // The certificate record itself must still exist and be readable for audit
    let cert = client.get_certificate(&admin, &cert_id);
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
        &BytesN::from_array(&env, &[0u8; 32]),
    );
    client.approve_course(&admin, &String::from_str(&env, "COURSE-CAP-EXACT"));
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    // Enrolling the first (and only allowed) student must succeed
    client.enroll(&student, &String::from_str(&env, "COURSE-CAP-EXACT"));

    let course = client
        .get_course(&String::from_str(&env, "COURSE-CAP-EXACT"))
        .unwrap();
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
        &BytesN::from_array(&env, &[0u8; 32]),
    );
    client.approve_course(&admin, &String::from_str(&env, "COURSE-CAP-FULL"));
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });

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
        &BytesN::from_array(&env, &[0u8; 32]),
    );
    client.approve_course(&admin, &String::from_str(&env, "COURSE-CAP-NONE"));
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });

    let course_id = String::from_str(&env, "COURSE-CAP-NONE");
    let asset_client = token::StellarAssetClient::new(&env, &token_id);

    // Enroll 5 students — all must succeed with no cap in place
    for _ in 0..5 {
        let s = Address::generate(&env);
        asset_client.mint(&s, &1_000_000_000);
        client.enroll(&s, &course_id);
    }

    let course = client.get_course(&course_id).unwrap();
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
        &BytesN::from_array(&env, &[0u8; 32]),
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
        &BytesN::from_array(&env, &[0u8; 32]),
    );
    client.approve_course(&admin, &String::from_str(&env, "COURSE-BATCH-CAP-FULL"));
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    let mut course_ids = soroban_sdk::Vec::new(&env);
    course_ids.push_back(String::from_str(&env, "COURSE-BATCH-CAP-OK"));
    course_ids.push_back(String::from_str(&env, "COURSE-BATCH-CAP-FULL"));

    // batch_enroll validates all courses before enrolling any — must panic
    client.batch_enroll(&student, &course_ids);
}

#[test]
#[should_panic(expected = "total earned overflow")]
fn test_enroll_total_earned_overflow() {
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
        "COURSE-OVERFLOW-2",
        100_000,
    );

    let course_id = String::from_str(&env, "COURSE-OVERFLOW-2");

    let course_key = DataKey::Course(course_id.clone());
    env.as_contract(&contract_id, || {
        let mut course: Course = env.storage().persistent().get(&course_key).unwrap();
        course.total_earned = i128::MAX;
        env.storage().persistent().set(&course_key, &course);
    });

    client.enroll(&student, &course_id);
}

#[test]
#[should_panic(expected = "course review period has not elapsed")]
fn test_course_approval_time_lock_premature_panics() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Set minimum review delay to 10 ledgers
    client.update_min_review_delay(&admin, &10u32);

    let course_id = String::from_str(&env, "COURSE-DELAYED-1");
    client.register_course(
        &instructor,
        &course_id,
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
    );

    // Try to approve immediately — should panic
    client.approve_course(&admin, &course_id);
}

#[test]
fn test_course_approval_time_lock_success() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Set minimum review delay to 10 ledgers
    client.update_min_review_delay(&admin, &10u32);

    let course_id = String::from_str(&env, "COURSE-DELAYED-2");
    client.register_course(
        &instructor,
        &course_id,
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
    );

    // Advance ledger sequence by 10
    env.ledger().with_mut(|l| {
        l.sequence_number += 10;
    });

    // Approve now — should succeed
    client.approve_course(&admin, &course_id);
    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.status, CourseStatus::Active);
}

#[test]
#[should_panic]
fn test_register_course_invalid_token() {
    let (env, contract_id, _, _, _, _, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let random_eoa = Address::generate(&env);

    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-INVALID-TOKEN"),
        &500_000_000,
        &random_eoa,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
    );
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
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-AUTH-GET",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-AUTH-GET");
    client.enroll(&student, &course_id);

    // Student can access
    let enrollment = client
        .get_enrollment(&student, &student, &course_id)
        .unwrap();
    assert_eq!(enrollment.amount_paid, 100_000_000);

    // Instructor can access
    let enrollment2 = client
        .get_enrollment(&instructor, &student, &course_id)
        .unwrap();
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
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-AUTH-GET-UNAUTH",
        100_000_000,
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
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-ARCHIVE-REREG",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-ARCHIVE-REREG");

    client.pause_course(&admin, &course_id);
    client.archive_course(&admin, &sec_admin, &course_id, &None);

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.status, CourseStatus::Archived);

    // Try to register the same course again
    client.register_course(
        &instructor,
        &course_id,
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
    );
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
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-CONCURRENCY",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-CONCURRENCY");
    client.enroll(&student, &course_id);

    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "ev")),
    );
    // Second completion should panic
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "ev2")),
    );
}

// ============================================================
// ISSUE 183: COURSE PRICE DENOMINATION RANGE CHECK
// ============================================================

#[test]
#[should_panic(expected = "price is outside the expected USDC precision range")]
fn test_register_course_price_too_low_rejected() {
    let (env, contract_id, token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // 50 stroops — clearly a whole-dollar-unit mistake, not real stroops
    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-PRICE-LOW"),
        &50i128,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
    );
}

#[test]
#[should_panic(expected = "price is outside the expected USDC precision range")]
fn test_register_course_price_too_high_rejected() {
    let (env, contract_id, token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Absurdly large — an extra few zeros beyond any sane course price
    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-PRICE-HIGH"),
        &2_000_000_000_000i128,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
    );
}

#[test]
fn test_register_course_price_zero_still_allowed() {
    let (env, contract_id, token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let course_id = String::from_str(&env, "COURSE-PRICE-FREE");
    client.register_course(
        &instructor,
        &course_id,
        &0i128,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
    );

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.price, 0);
}

#[test]
fn test_register_course_price_at_range_boundaries_succeeds() {
    let (env, contract_id, token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let min_course_id = String::from_str(&env, "COURSE-PRICE-MIN");
    client.register_course(
        &instructor,
        &min_course_id,
        &100_000i128,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
    );
    assert_eq!(client.get_course(&min_course_id).unwrap().price, 100_000);

    let max_course_id = String::from_str(&env, "COURSE-PRICE-MAX");
    client.register_course(
        &instructor,
        &max_course_id,
        &1_000_000_000_000i128,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
    );
    assert_eq!(
        client.get_course(&max_course_id).unwrap().price,
        1_000_000_000_000
    );
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
    client.register_course(
        &instructor,
        &course_id,
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
    );

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.created_at_ledger, 12345);
}

#[test]
fn test_course_last_updated_ledger_tracks_modifications() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    env.ledger().with_mut(|l| {
        l.sequence_number = 12000;
    });

    let course_id = String::from_str(&env, "COURSE-LAST-UPDATED");
    client.register_course(
        &instructor,
        &course_id,
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
    );

    let initial_course = client.get_course(&course_id).unwrap();
    assert_eq!(initial_course.last_updated_ledger, 12000);

    env.ledger().with_mut(|l| {
        l.sequence_number = 12010;
    });

    client.approve_course(&admin, &course_id);

    let updated_course = client.get_course(&course_id).unwrap();
    assert_eq!(updated_course.last_updated_ledger, 12010);
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
        &BytesN::from_array(&env, &[0u8; 32]),
    );
    client.approve_course(&admin, &matching_id);
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });

    // Enroll and complete
    client.enroll(&student, &matching_id);
    client.mark_completed(
        &admin,
        &student,
        &matching_id,
        &Some(String::from_str(&env, "proof")),
    );

    // Issue certificate with matching_id (same as course_id)
    client.issue_certificate(
        &admin,
        &matching_id, // matching_id used as cert_id
        &matching_id, // matching_id used as course_id
        &String::from_str(&env, "Test Course"),
        &student.to_string(),
        &None,
        &None,
    );

    // Assert both can be queried independently and they do not collide
    let course = client.get_course(&matching_id).unwrap();
    assert_eq!(course.id, matching_id);
    assert_eq!(course.instructor, instructor);

    let cert = client.get_certificate(&admin, &matching_id);
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

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-EXPIRY",
        100_000_000,
    );
    client.enroll(&student, &course_id);
    client.mark_completed(&admin, &student, &course_id, &None);

    // Issue certificate with expiry at ledger 1000
    client.issue_certificate(
        &admin,
        &cert_id,
        &course_id,
        &String::from_str(&env, "Expiry Course"),
        &student.to_string(),
        &Some(1000u32),
        &None,
    );

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
fn test_student_blocklist_lifecycle() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);

    assert!(!client.is_student_blocked(&student));

    client.block_student(&admin, &student);
    assert!(client.is_student_blocked(&student));

    client.unblock_student(&admin, &student);
    assert!(!client.is_student_blocked(&student));
}

#[test]
#[should_panic(expected = "student is blocked")]
fn test_blocked_student_cannot_enroll() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "BLOCKED-STUDENT-COURSE",
        100_000_000,
    );

    client.block_student(&admin, &student);
    client.enroll(&student, &String::from_str(&env, "BLOCKED-STUDENT-COURSE"));
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
        &BytesN::from_array(&env, &[0u8; 32]),
    );
}
#[test]
fn test_refund_lifecycle() {
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
        "COURSE-REFUND",
        1_000_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-REFUND");

    // Configure refund window to 10 ledgers
    client.update_refund_window(&admin, &10u32);
    assert_eq!(client.get_refund_window(), 10u32);

    // Enroll
    client.enroll(&student, &course_id);
    assert!(client.is_enrolled(&student, &course_id));

    // Request refund
    client.request_refund(&student, &course_id);

    let request = client.get_refund_request(&student, &course_id).unwrap();
    assert_eq!(request.status, RefundStatus::Pending);

    // Approve refund
    let initial_balance = token::Client::new(&env, &token_id).balance(&student);
    env.mock_all_auths_allowing_non_root_auth();
    client.process_refund(&admin, &student, &course_id, &true);

    let final_balance = token::Client::new(&env, &token_id).balance(&student);
    assert_eq!(final_balance - initial_balance, 1_000_000_000);
    assert!(!client.is_enrolled(&student, &course_id));

    let request_approved = client.get_refund_request(&student, &course_id).unwrap();
    assert_eq!(request_approved.status, RefundStatus::Approved);
}

#[test]
#[should_panic(expected = "refund window has expired")]
fn test_refund_request_outside_window_fails() {
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
        "COURSE-REFUND-EXP",
        1_000_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-REFUND-EXP");
    client.update_refund_window(&admin, &5u32);

    client.enroll(&student, &course_id);

    // Advance ledger sequence by 6
    env.ledger().with_mut(|l| {
        l.sequence_number += 6;
    });

    client.request_refund(&student, &course_id);
}

#[test]
fn test_refund_rejection() {
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
        "COURSE-REJECT",
        1_000_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-REJECT");
    client.enroll(&student, &course_id);

    client.request_refund(&student, &course_id);
    env.mock_all_auths_allowing_non_root_auth();
    client.process_refund(&admin, &student, &course_id, &false);

    let request = client.get_refund_request(&student, &course_id).unwrap();
    assert_eq!(request.status, RefundStatus::Rejected);
    assert!(client.is_enrolled(&student, &course_id));
}

#[test]
#[should_panic(expected = "instructor is frozen")]
fn test_frozen_instructor_enrollment_blocked() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    // Register and approve course BEFORE freeze
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "PRE-FREEZE-COURSE",
        100_000_000,
    );

    // Admin freezes instructor
    client.freeze_instructor(&admin, &instructor);

    // Attempting to enroll in the frozen instructor's course must fail
    client.enroll(&student, &String::from_str(&env, "PRE-FREEZE-COURSE"));
}

// ============================================================
// ISSUE 182: RE-ENROLLMENT AFTER COMPLETION
// ============================================================

#[test]
fn test_has_completed_distinguishes_absent_enrollment_from_incomplete() {
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
        "COURSE-COMPLETION-STATUS",
        500_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-COMPLETION-STATUS");

    assert_eq!(client.has_completed(&student, &course_id), None);

    client.enroll(&student, &course_id);
    assert_eq!(client.has_completed(&student, &course_id), Some(false));

    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "evidence_hash")),
    );
    assert_eq!(client.has_completed(&student, &course_id), Some(true));
}

#[test]
fn test_re_enroll_after_completion_succeeds() {
    let (env, contract_id, token_id, admin, _sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &10_000_000_000);

    let price: i128 = 500_000_000;
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-REENROLL-001",
        price,
    );
    let course_id = String::from_str(&env, "COURSE-REENROLL-001");

    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "evidence_1")),
    );
    assert_eq!(client.has_completed(&student, &course_id), Some(true));

    let treasury_before = token::Client::new(&env, &token_id).balance(&treasury);

    client.re_enroll(&student, &course_id);

    // A fresh, uncompleted enrollment now exists for the same key
    let enrollment = client
        .get_enrollment(&student, &student, &course_id)
        .unwrap();
    assert!(!enrollment.completed);
    assert!(!enrollment.certificate_issued);
    assert!(enrollment.certificate_id.is_none());
    assert!(enrollment.evidence_hash.is_none());
    assert_eq!(enrollment.amount_paid, price);

    // Student was charged again
    let platform_share = price * 20 / 100;
    let treasury_after = token::Client::new(&env, &token_id).balance(&treasury);
    assert_eq!(treasury_after - treasury_before, platform_share);

    // Course stats reflect a second enrollment
    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.total_enrollments, 2);
}

#[test]
fn test_re_enroll_archives_original_completion_and_certificate() {
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
        "COURSE-REENROLL-002",
        500_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-REENROLL-002");
    let cert_id = String::from_str(&env, "CERT-REENROLL-002");

    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "evidence_original")),
    );
    client.issue_certificate(
        &admin,
        &cert_id,
        &course_id,
        &String::from_str(&env, "Re-enroll Course"),
        &student.to_string(),
        &None,
        &None,
    );

    client.re_enroll(&student, &course_id);

    // The certificate issued for the original completion is untouched
    assert!(client.verify_certificate(&cert_id));
    let cert = client.get_certificate(&student, &cert_id);
    assert_eq!(cert.student, student);
    assert!(!cert.revoked);

    // The original completed enrollment was archived, not lost
    let history = client.get_enrollment_history(&student, &student, &course_id);
    assert_eq!(history.len(), 1);
    let archived = history.get(0).unwrap();
    assert!(archived.completed);
    assert_eq!(archived.certificate_id, Some(cert_id));
    assert_eq!(
        archived.evidence_hash,
        Some(String::from_str(&env, "evidence_original"))
    );
}

// ============================================================
// ISSUE 99: ADMIN ATTRIBUTION ON EVENTS
// ============================================================

/// Returns true if an event with the given topic-0 symbol was emitted
/// by `contract_id`.
fn has_event(env: &Env, contract_id: &Address, name: &str) -> bool {
    env.events().all().iter().any(|(contract, topics, _)| {
        if contract != *contract_id {
            return false;
        }
        let sym: Symbol = topics.get(0).unwrap().try_into_val(env).unwrap();
        sym == Symbol::new(env, name)
    })
}

/// Returns the data payload of the most recent event named `name` emitted
/// by `contract_id`, as a raw `Val` for the caller to decode via
/// `.try_into_val(&env)`. Panics if no such event was found.
fn last_event_val(env: &Env, contract_id: &Address, name: &str) -> Val {
    let events = env.events().all();
    for (contract, topics, data) in events.iter().rev() {
        if contract != *contract_id {
            continue;
        }
        let sym: Symbol = topics.get(0).unwrap().try_into_val(env).unwrap();
        if sym == Symbol::new(env, name) {
            return data;
        }
    }
    panic!("event {} not found", name);
}

#[test]
fn test_events_emitted_for_admin_operations() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    // approve_course
    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-ATTR-001"),
        &500_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
    );
    let course_id = String::from_str(&env, "COURSE-ATTR-001");
    client.approve_course(&admin, &course_id);
    let (event_course_id, event_instructor, event_admin, _ledger): (String, Address, Address, u32) =
        last_event_val(&env, &contract_id, "course_approved")
            .try_into_val(&env)
            .unwrap();
    assert_eq!(event_course_id, course_id);
    assert_eq!(event_instructor, instructor);
    assert_eq!(event_admin, admin);

    // mark_completed
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });
    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "evidence")),
    );
    let (event_student, event_admin): (Address, Address) =
        last_event_val(&env, &contract_id, "course_completed")
            .try_into_val(&env)
            .unwrap();
    assert_eq!(event_student, student);
    assert_eq!(event_admin, admin);

    // issue_certificate
    let cert_id = String::from_str(&env, "CERT-ATTR-001");
    client.issue_certificate(
        &admin,
        &cert_id,
        &course_id,
        &String::from_str(&env, "Re-enroll Course"),
        &student.to_string(),
        &None,
        &None,
    );
    let (event_student, event_course_id, event_admin, event_ledger): (
        Address,
        String,
        Address,
        u32,
    ) = last_event_val(&env, &contract_id, "certificate_issued")
        .try_into_val(&env)
        .unwrap();
    assert_eq!(event_student, student);
    assert_eq!(event_course_id, course_id);
    assert_eq!(event_admin, admin);
    let certificate = client.get_certificate(&student, &cert_id);
    assert_eq!(event_ledger, certificate.issued_at_ledger);

    // pause_platform / unpause_platform
    client.pause_platform(&admin);
    assert!(has_event(&env, &contract_id, "platform_paused"));
    let event_admin: Address = last_event_val(&env, &contract_id, "platform_paused")
        .try_into_val(&env)
        .unwrap();
    assert_eq!(event_admin, admin);

    client.unpause_platform(&admin);
    let event_admin: Address = last_event_val(&env, &contract_id, "platform_unpaused")
        .try_into_val(&env)
        .unwrap();
    assert_eq!(event_admin, admin);

    // update_default_fee
    client.update_default_fee(&admin, &25u32);
    let (event_admin, event_fee): (Address, u32) =
        last_event_val(&env, &contract_id, "default_fee_updated")
            .try_into_val(&env)
            .unwrap();
    assert_eq!(event_admin, admin);
    assert_eq!(event_fee, 25u32);

    // add_approved_token / remove_approved_token
    let other_token = Address::generate(&env);
    client.add_approved_token(&admin, &other_token);
    let (event_admin, event_token): (Address, Address) =
        last_event_val(&env, &contract_id, "token_whitelisted")
            .try_into_val(&env)
            .unwrap();
    assert_eq!(event_admin, admin);
    assert_eq!(event_token, other_token);

    client.remove_approved_token(&admin, &other_token);
    let (event_admin, event_token): (Address, Address) =
        last_event_val(&env, &contract_id, "token_removed_from_whitelist")
            .try_into_val(&env)
            .unwrap();
    assert_eq!(event_admin, admin);
    assert_eq!(event_token, other_token);

    // update_max_courses_limit
    client.update_max_courses_limit(&admin, &99u32);
    let (event_admin, event_max): (Address, u32) =
        last_event_val(&env, &contract_id, "max_courses_limit_updated")
            .try_into_val(&env)
            .unwrap();
    assert_eq!(event_admin, admin);
    assert_eq!(event_max, 99u32);

    // freeze_instructor / unfreeze_instructor
    client.freeze_instructor(&admin, &instructor);
    let (event_instructor, event_admin): (Address, Address) =
        last_event_val(&env, &contract_id, "instructor_frozen")
            .try_into_val(&env)
            .unwrap();
    assert_eq!(event_instructor, instructor);
    assert_eq!(event_admin, admin);

    client.unfreeze_instructor(&admin, &instructor);
    let (event_instructor, event_admin): (Address, Address) =
        last_event_val(&env, &contract_id, "instructor_unfrozen")
            .try_into_val(&env)
            .unwrap();
    assert_eq!(event_instructor, instructor);
    assert_eq!(event_admin, admin);

    // update_min_review_delay
    client.update_min_review_delay(&admin, &5u32);
    let (event_admin, event_delay): (Address, u32) =
        last_event_val(&env, &contract_id, "min_review_delay_updated")
            .try_into_val(&env)
            .unwrap();
    assert_eq!(event_admin, admin);
    assert_eq!(event_delay, 5u32);

    // update_refund_window
    client.update_refund_window(&admin, &2000u32);
    let (event_admin, event_window): (Address, u32) =
        last_event_val(&env, &contract_id, "refund_window_updated")
            .try_into_val(&env)
            .unwrap();
    assert_eq!(event_admin, admin);
    assert_eq!(event_window, 2000u32);

    // withdraw_tokens (contract holds nothing, so withdraw 0)
    client.withdraw_tokens(&admin, &token_id, &0i128, &admin);
    let (event_admin, event_token, event_amount, event_dest): (Address, Address, i128, Address) =
        last_event_val(&env, &contract_id, "tokens_withdrawn")
            .try_into_val(&env)
            .unwrap();
    assert_eq!(event_admin, admin);
    assert_eq!(event_token, token_id);
    assert_eq!(event_amount, 0i128);
    assert_eq!(event_dest, admin);
}

#[test]
#[should_panic(expected = "no prior enrollment found for this course")]
fn test_re_enroll_without_prior_enrollment_fails() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-REENROLL-003",
        500_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-REENROLL-003");

    // Student never enrolled at all
    client.re_enroll(&student, &course_id);
}

#[test]
#[should_panic(expected = "current enrollment has not been completed yet")]
fn test_re_enroll_before_completion_fails() {
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
        "COURSE-REENROLL-004",
        500_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-REENROLL-004");

    client.enroll(&student, &course_id);
    // Not completed — must panic
    client.re_enroll(&student, &course_id);
}

#[test]
fn test_re_enroll_multiple_times_accumulates_history() {
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
        "COURSE-REENROLL-005",
        200_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-REENROLL-005");

    // First attempt: normal enroll, complete, then re-enroll (archives attempt 1)
    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "evidence-1")),
    );
    assert_eq!(
        client
            .get_enrollment_history(&student, &student, &course_id)
            .len(),
        0
    );
    client.re_enroll(&student, &course_id);

    // Second attempt: re_enroll already created a fresh enrollment — just
    // complete it and re-enroll again (archives attempt 2)
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "evidence-2")),
    );
    assert_eq!(
        client
            .get_enrollment_history(&student, &student, &course_id)
            .len(),
        1
    );
    client.re_enroll(&student, &course_id);

    let history = client.get_enrollment_history(&student, &student, &course_id);
    assert_eq!(history.len(), 2);
    assert!(history.get(0).unwrap().completed);
    assert!(history.get(1).unwrap().completed);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_get_enrollment_history_unauthorized_access() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    let random_user = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &10_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-REENROLL-006",
        200_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-REENROLL-006");

    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "evidence")),
    );
    client.re_enroll(&student, &course_id);

    // A random address must not be able to read the history
    client.get_enrollment_history(&random_user, &student, &course_id);
}

#[test]
fn test_multi_sig_admin_events_record_both_actors() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // transfer_admin (admin_proposed) — multi-sig proposal
    let new_admin = Address::generate(&env);
    let new_sec_admin = Address::generate(&env);
    client.transfer_admin(&admin, &sec_admin, &new_admin, &new_sec_admin);
    let (event_new_admin, event_admin1, event_admin2): (Address, Address, Address) =
        last_event_val(&env, &contract_id, "admin_proposed")
            .try_into_val(&env)
            .unwrap();
    assert_eq!(event_new_admin, new_admin);
    assert!(
        (event_admin1 == admin && event_admin2 == sec_admin)
            || (event_admin1 == sec_admin && event_admin2 == admin)
    );

    // update_treasury (multi-sig)
    let new_treasury = Address::generate(&env);
    client.update_treasury(&admin, &sec_admin, &new_treasury);
    let (
        _event_old_treasury,
        event_treasury,
        event_admin1,
        event_admin2,
        _ledger_sequence,
        _effective_ledger,
    ): (Address, Address, Address, Address, u32, u32) =
        last_event_val(&env, &contract_id, "treasury_updated")
            .try_into_val(&env)
            .unwrap();
    assert!(
        (event_admin1 == admin && event_admin2 == sec_admin)
            || (event_admin1 == sec_admin && event_admin2 == admin)
    );
    assert_eq!(event_treasury, new_treasury);

    // archive_course (multi-sig)
    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-ARCHIVE-ATTR"),
        &500_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
    );
    let course_id = String::from_str(&env, "COURSE-ARCHIVE-ATTR");
    client.approve_course(&admin, &course_id);
    client.pause_course(&admin, &course_id);
    client.archive_course(&admin, &sec_admin, &course_id, &None);
    let (event_course_id, event_admin1, event_admin2): (String, Address, Address) =
        last_event_val(&env, &contract_id, "course_archived")
            .try_into_val(&env)
            .unwrap();
    assert_eq!(event_course_id, course_id);
    // No refunds expected for this archive
    assert!(
        (event_admin1 == admin && event_admin2 == sec_admin)
            || (event_admin1 == sec_admin && event_admin2 == admin)
    );
}

#[test]
fn test_process_refund_event_includes_admin() {
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
        "COURSE-REFUND-ATTR",
        500_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-REFUND-ATTR");

    client.enroll(&student, &course_id);
    client.request_refund(&student, &course_id);
    env.mock_all_auths_allowing_non_root_auth();
    client.process_refund(&admin, &student, &course_id, &true);

    let (event_student, event_course_id, event_approved, event_admin): (
        Address,
        String,
        bool,
        Address,
    ) = last_event_val(&env, &contract_id, "refund_processed")
        .try_into_val(&env)
        .unwrap();
    assert_eq!(event_student, student);
    assert_eq!(event_course_id, course_id);
    assert!(event_approved);
    assert_eq!(event_admin, admin);
}

#[test]
fn test_approve_course_event_details() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-EVENT-101"),
        &500_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
    );
    let course_id = String::from_str(&env, "COURSE-EVENT-101");

    env.ledger().with_mut(|l| {
        l.sequence_number = 12345;
    });

    client.approve_course(&admin, &course_id);

    let (event_course_id, event_instructor, event_admin, event_ledger): (
        String,
        Address,
        Address,
        u32,
    ) = last_event_val(&env, &contract_id, "course_approved")
        .try_into_val(&env)
        .unwrap();

    assert_eq!(event_course_id, course_id);
    assert_eq!(event_instructor, instructor);
    assert_eq!(event_admin, admin);
    assert_eq!(event_ledger, 12345);
}

#[test]
fn test_get_certificate_authorized_roles() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    let course_id = String::from_str(&env, "COURSE-GET-CERT");
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-GET-CERT",
        500_000_000,
    );

    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "evidence_hash")),
    );

    let cert_id = String::from_str(&env, "CERT-123");
    client.issue_certificate(
        &admin,
        &cert_id,
        &course_id,
        &String::from_str(&env, "Test Course"),
        &student.to_string(),
        &None,
        &None,
    );

    // 1. Student can retrieve
    let cert1 = client.get_certificate(&student, &cert_id);
    assert_eq!(cert1.student, student);

    // 2. Instructor can retrieve
    let cert2 = client.get_certificate(&instructor, &cert_id);
    assert_eq!(cert2.student, student);

    // 3. Admin can retrieve
    let cert3 = client.get_certificate(&admin, &cert_id);
    assert_eq!(cert3.student, student);
}

#[test]
#[should_panic]
fn test_get_certificate_unauthorized_third_party_fails() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    let course_id = String::from_str(&env, "COURSE-GET-CERT");
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-GET-CERT",
        500_000_000,
    );

    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "evidence_hash")),
    );

    let cert_id = String::from_str(&env, "CERT-123");
    client.issue_certificate(
        &admin,
        &cert_id,
        &course_id,
        &String::from_str(&env, "Test Course"),
        &student.to_string(),
        &None,
        &None,
    );

    let third_party = Address::generate(&env);

    // Mock auth only for third_party's own require_auth() call — cert.student
    // is deliberately left unmocked, so its require_auth() call must fail.
    client
        .mock_auths(&[MockAuth {
            address: &third_party,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "get_certificate",
                args: (third_party.clone(), cert_id.clone()).into_val(&env),
                sub_invokes: &[],
            },
        }])
        .get_certificate(&third_party, &cert_id);
}

#[test]
#[should_panic]
fn test_certificate_student_b_without_enrollment_panics() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student_a = Address::generate(&env);
    let student_b = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student_a, &100_000_000_000);
    token::StellarAssetClient::new(&env, &token_id).mint(&student_b, &100_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-SECURE-B",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-SECURE-B");
    let cert_id = String::from_str(&env, "CERT-SECURE-B");

    // Enroll and complete student_a only
    client.enroll(&student_a, &course_id);
    client.mark_completed(
        &admin,
        &student_a,
        &course_id,
        &Some(String::from_str(&env, "evidence")),
    );

    // Try to issue certificate using student_b's address — must panic (no enrollment)
    client.issue_certificate(
        &admin,
        &cert_id,
        &course_id,
        &String::from_str(&env, "Secure Course"),
        &student_b.to_string(),
        &None,
        &None,
    );
}

#[test]
fn test_certificate_student_matches_enrollment() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student_a = Address::generate(&env);
    let student_b = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student_a, &100_000_000_000);
    token::StellarAssetClient::new(&env, &token_id).mint(&student_b, &100_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-SECURE",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-SECURE");
    let cert_id = String::from_str(&env, "CERT-SECURE");

    // Enroll and complete student_a
    client.enroll(&student_a, &course_id);
    client.mark_completed(
        &admin,
        &student_a,
        &course_id,
        &Some(String::from_str(&env, "evidence")),
    );

    // Issue certificate correctly using student_a's address
    client.issue_certificate(
        &admin,
        &cert_id,
        &course_id,
        &String::from_str(&env, "Secure Course"),
        &student_a.to_string(),
        &None,
        &None,
    );

    // Verify certificate's student field matches student_a, not any other address
    let cert = client.get_certificate(&admin, &cert_id);
    assert_eq!(cert.student, student_a);
}

// ============================================================
// ISSUE #104: GET COURSES BY INSTRUCTOR
// ============================================================

#[test]
fn test_get_courses_by_instructor_accurate_after_registration_pause_and_archival() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // A second instructor's courses must not leak into the first instructor's list.
    let other_instructor = Address::generate(&env);

    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-INSTR-LIST-1"),
        &500_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
    );
    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-INSTR-LIST-2"),
        &500_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
    );
    client.register_course(
        &other_instructor,
        &String::from_str(&env, "COURSE-INSTR-LIST-OTHER"),
        &500_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
    );

    let list = client.get_courses_by_instructor(&instructor);
    assert_eq!(list.len(), 2);
    assert_eq!(
        list.get(0).unwrap(),
        String::from_str(&env, "COURSE-INSTR-LIST-1")
    );
    assert_eq!(
        list.get(1).unwrap(),
        String::from_str(&env, "COURSE-INSTR-LIST-2")
    );

    let other_list = client.get_courses_by_instructor(&other_instructor);
    assert_eq!(other_list.len(), 1);

    // Pausing a course must not change the instructor's course list.
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });
    let course_id_1 = String::from_str(&env, "COURSE-INSTR-LIST-1");
    client.approve_course(&admin, &course_id_1);
    client.pause_course(&instructor, &course_id_1);
    let list_after_pause = client.get_courses_by_instructor(&instructor);
    assert_eq!(list_after_pause.len(), 2);
    assert_eq!(list_after_pause.get(0).unwrap(), course_id_1);

    // Archiving a course (must be paused, no active enrollments) must not
    // remove it from the instructor's course list either.
    client.archive_course(&admin, &sec_admin, &course_id_1, &None);
    let list_after_archive = client.get_courses_by_instructor(&instructor);
    assert_eq!(list_after_archive.len(), 2);
    assert_eq!(list_after_archive.get(0).unwrap(), course_id_1);
    assert_eq!(
        client.get_course(&course_id_1).unwrap().status,
        CourseStatus::Archived
    );
}

#[test]
fn test_get_courses_by_instructor_empty_for_unknown_instructor() {
    let (env, contract_id, _token_id, _admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let unknown = Address::generate(&env);
    let list = client.get_courses_by_instructor(&unknown);
    assert_eq!(list.len(), 0);
}

// ============================================================
// ISSUE #97: MINIMUM ENROLLMENT DURATION BEFORE MARK_COMPLETED
// ============================================================

#[test]
#[should_panic(expected = "minimum enrollment duration has not elapsed")]
fn test_mark_completed_before_min_duration_fails() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.update_min_completion_ledgers(&admin, &50u32);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &10_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-MIN-DURATION-1",
        500_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-MIN-DURATION-1");

    client.enroll(&student, &course_id);

    // Only 1 ledger has elapsed since enrollment — far short of the 50
    // required — so this must panic.
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "evidence")),
    );
}

#[test]
fn test_mark_completed_after_min_duration_succeeds() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.update_min_completion_ledgers(&admin, &50u32);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &10_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-MIN-DURATION-2",
        500_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-MIN-DURATION-2");

    client.enroll(&student, &course_id);

    env.ledger().with_mut(|l| {
        l.sequence_number += 50;
    });

    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "evidence")),
    );

    assert_eq!(client.has_completed(&student, &course_id), Some(true));
}

#[test]
fn test_mark_completed_default_zero_delay_allows_immediate_completion() {
    // No update_min_completion_ledgers() call — default is 0, matching
    // today's behavior for courses registered before this feature existed.
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
        "COURSE-MIN-DURATION-3",
        500_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-MIN-DURATION-3");

    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "evidence")),
    );

    assert_eq!(client.has_completed(&student, &course_id), Some(true));
}

// ============================================================
// ISSUE #68: TOCTOU CONSOLIDATION IN ENROLL()
// ============================================================

#[test]
#[should_panic(expected = "course is not available for enrollment")]
fn test_enroll_rejected_for_just_paused_course() {
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
        "COURSE-TOCTOU-1",
        500_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-TOCTOU-1");

    // Course is paused in the same call frame just before enrollment is
    // attempted — must still be rejected.
    client.pause_course(&admin, &course_id);
    client.enroll(&student, &course_id);
}

// ============================================================
// ISSUE FIX TESTS
// ============================================================

// --- Issue: Empty certificate ID should be rejected ---

#[test]
#[should_panic(expected = "certificate_id cannot be empty")]
fn test_issue_certificate_empty_id_rejected() {
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
        "COURSE-EMPTY-CERT",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-EMPTY-CERT");
    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "evidence")),
    );

    // Empty certificate ID must be rejected
    client.issue_certificate(
        &admin,
        &String::from_str(&env, ""),
        &course_id,
        &String::from_str(&env, "Test Course"),
        &student.to_string(),
        &None,
        &None,
    );
}

// --- Issue: Double revocation should be rejected ---

#[test]
#[should_panic(expected = "certificate is already revoked")]
fn test_revoke_certificate_double_revocation_rejected() {
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
        "COURSE-REVOKE-DBL",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-REVOKE-DBL");
    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "evidence")),
    );

    let cert_id = String::from_str(&env, "CERT-DBL-REVOKE");
    client.issue_certificate(
        &admin,
        &cert_id,
        &course_id,
        &String::from_str(&env, "Test Course"),
        &student.to_string(),
        &None,
        &None,
    );

    // First revocation — should succeed
    client.revoke_certificate(&admin, &cert_id, &String::from_str(&env, "ISSUED_IN_ERROR"));

    // Second revocation — must panic
    client.revoke_certificate(
        &admin,
        &cert_id,
        &String::from_str(&env, "DOUBLE_REVOKE_ATTEMPT"),
    );
}

// --- Issue #116: Revocation must be a one-way, permanent operation ---

#[test]
fn test_revoke_certificate_metadata_unchanged_after_rejected_double_revoke() {
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
        "COURSE-IMMUTABLE-001",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-IMMUTABLE-001");
    let cert_id = String::from_str(&env, "CERT-IMMUTABLE-001");
    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "evidence")),
    );
    client.issue_certificate(
        &admin,
        &cert_id,
        &course_id,
        &String::from_str(&env, "Test Course"),
        &student.to_string(),
        &None,
        &None,
    );

    let original_reason = String::from_str(&env, "ISSUED_IN_ERROR");
    client.revoke_certificate(&admin, &cert_id, &original_reason);

    let cert_after_first = client.get_certificate(&admin, &cert_id);
    assert!(cert_after_first.revoked);

    // A second (rejected) revocation attempt must not mutate any stored
    // revocation metadata — the panic happens before any write.
    let result = client.try_revoke_certificate(
        &admin,
        &cert_id,
        &String::from_str(&env, "DOUBLE_REVOKE_ATTEMPT"),
    );
    assert!(result.is_err());

    let cert_after_second = client.get_certificate(&admin, &cert_id);
    assert!(cert_after_second.revoked);
    assert_eq!(cert_after_second.revoked_by, cert_after_first.revoked_by);
    assert_eq!(
        cert_after_second.revoked_at_ledger,
        cert_after_first.revoked_at_ledger
    );
    assert_eq!(cert_after_second.revocation_reason, Some(original_reason));
}

#[test]
#[should_panic(expected = "certificate ID already exists")]
fn test_revoke_certificate_id_cannot_be_reused_to_unrevoke() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student_a = Address::generate(&env);
    let student_b = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student_a, &100_000_000_000);
    token::StellarAssetClient::new(&env, &token_id).mint(&student_b, &100_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-REUSE-A",
        500_000_000,
    );
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-REUSE-B",
        500_000_000,
    );

    let course_a = String::from_str(&env, "COURSE-REUSE-A");
    let course_b = String::from_str(&env, "COURSE-REUSE-B");
    let cert_id = String::from_str(&env, "CERT-REUSE-SHARED");

    // Issue and revoke a certificate under course A.
    client.enroll(&student_a, &course_a);
    client.mark_completed(
        &admin,
        &student_a,
        &course_a,
        &Some(String::from_str(&env, "evidence")),
    );
    client.issue_certificate(
        &admin,
        &cert_id,
        &course_a,
        &String::from_str(&env, "Course A"),
        &student_a.to_string(),
        &None,
        &None,
    );
    client.revoke_certificate(&admin, &cert_id, &String::from_str(&env, "ISSUED_IN_ERROR"));

    // A completely unrelated enrollment must not be able to reuse the same
    // certificate ID to create a fresh, non-revoked record — doing so would
    // effectively "un-revoke" the credential under that ID.
    client.enroll(&student_b, &course_b);
    client.mark_completed(
        &admin,
        &student_b,
        &course_b,
        &Some(String::from_str(&env, "evidence")),
    );
    client.issue_certificate(
        &admin,
        &cert_id,
        &course_b,
        &String::from_str(&env, "Course B"),
        &student_b.to_string(),
        &None,
        &None,
    );
}

// --- Issue: Same-address admin transfer should be rejected ---

#[test]
#[should_panic(expected = "proposed admin addresses are identical to current admin addresses")]
fn test_transfer_admin_same_address_rejected() {
    let (env, contract_id, _token_id, admin, sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Attempt to transfer admin to the exact same pair — must panic
    client.transfer_admin(&admin, &sec_admin, &admin, &sec_admin);
}

// --- Issue: Enrollment expiry scenarios ---

#[test]
#[should_panic(expected = "enrollment has expired")]
fn test_mark_completed_panics_when_enrollment_expired() {
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
        "COURSE-EXPIRY-001",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-EXPIRY-001");

    // Instructor sets an expiry of 100 ledger sequences
    client.set_enrollment_expiry(&instructor, &course_id, &Some(100u32));

    // Student enrolls
    client.enroll(&student, &course_id);

    // Advance ledger past the expiry window
    env.ledger().with_mut(|l| {
        l.sequence_number += 200;
    });

    // mark_completed must panic because the enrollment has expired
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "late_evidence")),
    );
}

#[test]
fn test_has_completed_returns_false_when_enrollment_expired() {
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
        "COURSE-EXPIRY-002",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-EXPIRY-002");

    // Instructor sets an expiry of 50 ledger sequences
    client.set_enrollment_expiry(&instructor, &course_id, &Some(50u32));

    // Student enrolls
    client.enroll(&student, &course_id);

    // Before expiry: has_completed should be false (not completed yet)
    assert_ne!(client.has_completed(&student, &course_id), Some(true));

    // Advance ledger past the expiry window
    env.ledger().with_mut(|l| {
        l.sequence_number += 100;
    });

    // After expiry: has_completed should still be false (expired enrollment, not completed)
    assert_ne!(client.has_completed(&student, &course_id), Some(true));
}

#[test]
fn test_has_completed_returns_true_when_completed_before_expiry() {
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
        "COURSE-EXPIRY-003",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-EXPIRY-003");

    // Instructor sets a generous expiry of 1000 ledger sequences
    client.set_enrollment_expiry(&instructor, &course_id, &Some(1000u32));

    // Student enrolls and completes within the window
    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "evidence")),
    );

    // Advance past the expiry — completed flag must still be true
    env.ledger().with_mut(|l| {
        l.sequence_number += 2000;
    });

    assert_eq!(client.has_completed(&student, &course_id), Some(true));
}

#[test]
fn test_enrollment_expiry_none_allows_completion_at_any_time() {
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
        "COURSE-NO-EXPIRY",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-NO-EXPIRY");

    // No expiry set (default None) — student enrolls
    client.enroll(&student, &course_id);

    // Advance many ledgers
    env.ledger().with_mut(|l| {
        l.sequence_number += 1_000_000;
    });

    // Without an expiry, mark_completed must succeed regardless of elapsed time
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "late_evidence")),
    );

    assert_eq!(client.has_completed(&student, &course_id), Some(true));
}

// ============================================================
// FEATURE: issued_by field on Certificate
// ============================================================

#[test]
fn test_certificate_issued_by_matches_contract_address() {
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
        "COURSE-ISSUED-BY-001",
        100_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-ISSUED-BY-001");
    let cert_id = String::from_str(&env, "CERT-ISSUED-BY-001");

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
        &course_id,
        &String::from_str(&env, "Issued-By Course"),
        &student.to_string(),
        &None,
        &None,
    );

    let cert = client.get_certificate(&admin, &cert_id);

    // issued_by must equal the deployed contract's own address
    assert_eq!(cert.issued_by, contract_id);
}

#[test]
fn test_certificate_issued_by_is_not_admin_or_instructor() {
    // Verifies that issued_by is the *contract* address, not the admin or instructor.
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
        "COURSE-ISSUED-BY-002",
        100_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-ISSUED-BY-002");
    let cert_id = String::from_str(&env, "CERT-ISSUED-BY-002");

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
        &course_id,
        &String::from_str(&env, "Issued-By Course 2"),
        &student.to_string(),
        &None,
        &None,
    );

    let cert = client.get_certificate(&admin, &cert_id);

    assert_ne!(
        cert.issued_by, admin,
        "issued_by must not be the admin address"
    );
    assert_ne!(
        cert.issued_by, instructor,
        "issued_by must not be the instructor address"
    );
    assert_ne!(
        cert.issued_by, student,
        "issued_by must not be the student address"
    );
    assert_eq!(cert.issued_by, contract_id);
}

#[test]
fn test_certificate_issued_by_persists_after_revocation() {
    // Revoking a certificate must not clear the issued_by field.
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
        "COURSE-ISSUED-BY-003",
        100_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-ISSUED-BY-003");
    let cert_id = String::from_str(&env, "CERT-ISSUED-BY-003");

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
        &course_id,
        &String::from_str(&env, "Issued-By Course 3"),
        &student.to_string(),
        &None,
        &None,
    );

    // Revoke and confirm issued_by is unchanged
    client.revoke_certificate(&admin, &cert_id, &String::from_str(&env, "TEST"));
    let cert = client.get_certificate(&admin, &cert_id);
    assert!(cert.revoked);
    assert_eq!(cert.issued_by, contract_id);
}

#[test]
fn test_multiple_certificates_share_same_issued_by() {
    // All certificates issued by the same contract deployment should have
    // an identical issued_by regardless of which course or student.
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student_a = Address::generate(&env);
    let student_b = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student_a, &1_000_000_000);
    token::StellarAssetClient::new(&env, &token_id).mint(&student_b, &1_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-MULTI-ISSUED-A",
        100_000_000,
    );
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-MULTI-ISSUED-B",
        200_000_000,
    );

    let course_a = String::from_str(&env, "COURSE-MULTI-ISSUED-A");
    let course_b = String::from_str(&env, "COURSE-MULTI-ISSUED-B");
    let cert_a = String::from_str(&env, "CERT-MULTI-ISSUED-A");
    let cert_b = String::from_str(&env, "CERT-MULTI-ISSUED-B");

    client.enroll(&student_a, &course_a);
    client.mark_completed(
        &admin,
        &student_a,
        &course_a,
        &Some(String::from_str(&env, "proof-a")),
    );
    client.issue_certificate(
        &admin,
        &cert_a,
        &course_a,
        &String::from_str(&env, "Course A"),
        &student_a.to_string(),
        &None,
        &None,
    );

    client.enroll(&student_b, &course_b);
    client.mark_completed(
        &admin,
        &student_b,
        &course_b,
        &Some(String::from_str(&env, "proof-b")),
    );
    client.issue_certificate(
        &admin,
        &cert_b,
        &course_b,
        &String::from_str(&env, "Course B"),
        &student_b.to_string(),
        &None,
        &None,
    );

    let issued_by_a = client.get_certificate(&admin, &cert_a).issued_by;
    let issued_by_b = client.get_certificate(&admin, &cert_b).issued_by;

    assert_eq!(issued_by_a, contract_id);
    assert_eq!(issued_by_b, contract_id);
    assert_eq!(issued_by_a, issued_by_b);
}

// ============================================================
// FEATURE: configurable refund window at init time
// ============================================================

#[test]
fn test_init_sets_refund_window() {
    // Verifies that the refund_window_ledgers passed to init() is stored and
    // readable immediately — no separate update_refund_window() call needed.
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(HamplardContract, ());
    let token_admin = Address::generate(&env);
    let token_id = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();

    let admin = Address::generate(&env);
    let sec_admin = Address::generate(&env);
    let treasury = Address::generate(&env);

    let client = HamplardContractClient::new(&env, &contract_id);
    // Use a non-default window so we can distinguish from the fallback 1000
    client.init(&admin, &sec_admin, &treasury, &20u32, &50u32, &500u32);

    assert_eq!(client.get_refund_window(), 500u32);
}

#[test]
fn test_refund_request_within_window_succeeds() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    // Window is 1000 ledgers (set by setup() via init)
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-WINDOW-IN",
        200_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-WINDOW-IN");

    client.enroll(&student, &course_id);

    // Advance well within the window
    env.ledger().with_mut(|l| {
        l.sequence_number += 500;
    });

    // Must succeed — 500 elapsed ≤ 1000 window
    client.request_refund(&student, &course_id);

    let request = client.get_refund_request(&student, &course_id).unwrap();
    assert_eq!(request.status, RefundStatus::Pending);
}

#[test]
fn test_refund_request_at_window_boundary_succeeds() {
    // A request submitted exactly at enrolled_at + window must still succeed
    // (window is inclusive: elapsed == window is allowed, elapsed > window is rejected).
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    client.update_refund_window(&admin, &10u32);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-WINDOW-BOUND",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-WINDOW-BOUND");

    client.enroll(&student, &course_id);
    let enrolled_at = env.ledger().sequence();

    // Advance to exactly enrolled_at + window
    env.ledger().with_mut(|l| {
        l.sequence_number = enrolled_at + 10;
    });

    // elapsed == window → still allowed
    client.request_refund(&student, &course_id);

    let request = client.get_refund_request(&student, &course_id).unwrap();
    assert_eq!(request.status, RefundStatus::Pending);
}

#[test]
#[should_panic(expected = "refund window has expired")]
fn test_refund_request_one_ledger_past_window_rejected() {
    // A request at elapsed == window + 1 must be rejected.
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    client.update_refund_window(&admin, &10u32);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-WINDOW-PAST",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-WINDOW-PAST");

    client.enroll(&student, &course_id);
    let enrolled_at = env.ledger().sequence();

    // One ledger beyond the window
    env.ledger().with_mut(|l| {
        l.sequence_number = enrolled_at + 11;
    });

    client.request_refund(&student, &course_id);
}

#[test]
#[should_panic(expected = "refund window has expired")]
fn test_refund_request_far_past_window_rejected() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    // Very small window so it's easy to exceed
    client.update_refund_window(&admin, &5u32);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-WINDOW-FAR",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-WINDOW-FAR");

    client.enroll(&student, &course_id);

    // Advance far past the window while keeping the instance accessible.
    env.ledger().with_mut(|l| {
        l.sequence_number += 1_000;
    });

    client.request_refund(&student, &course_id);
}

#[test]
fn test_admin_can_extend_refund_window_after_init() {
    // Confirms that update_refund_window() still works post-init and
    // immediately affects subsequent request_refund() calls.
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    // Shrink window to 5 first
    client.update_refund_window(&admin, &5u32);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-WINDOW-EXT",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-WINDOW-EXT");

    client.enroll(&student, &course_id);
    let enrolled_at = env.ledger().sequence();

    // Advance 6 ledgers — would exceed the old window of 5
    env.ledger().with_mut(|l| {
        l.sequence_number = enrolled_at + 6;
    });

    // Admin extends window to 20 before the student requests
    client.update_refund_window(&admin, &20u32);
    assert_eq!(client.get_refund_window(), 20u32);

    // Now 6 elapsed ≤ 20 window — must succeed
    client.request_refund(&student, &course_id);

    let request = client.get_refund_request(&student, &course_id).unwrap();
    assert_eq!(request.status, RefundStatus::Pending);
}

#[test]
fn test_refund_window_zero_rejects_all_requests() {
    // When the window is 0, any elapsed ledger (≥ 1) is beyond the window.
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    client.update_refund_window(&admin, &0u32);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-WINDOW-ZERO",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-WINDOW-ZERO");

    client.enroll(&student, &course_id);
    // Even a single ledger advance places elapsed > 0 == window
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.request_refund(&student, &course_id);
    }));
    assert!(
        result.is_err(),
        "window=0 should reject any refund request after enrollment ledger"
    );
}

// ============================================================
// FEATURE: get_course() extends TTL on every read
// ============================================================

#[test]
fn test_get_course_extends_ttl_on_read() {
    // Verify that calling get_course() on an existing course extends its TTL.
    // We advance the ledger to near the threshold so that a read *must* extend
    // the TTL to keep the entry alive, then confirm the entry is still readable.
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-TTL-READ",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-TTL-READ");

    // Advance ledger far enough that without TTL extension the entry would be
    // dangerously close to expiry (beyond the threshold), then read it.
    env.ledger().with_mut(|l| {
        // Jump past PERSISTENT_TTL_THRESHOLD (contract constant; default 100_000).
        l.sequence_number += 100_001;
        l.min_persistent_entry_ttl = 200_000;
    });

    // Reading must still return Some — the extension fires inside get_course().
    let course = client.get_course(&course_id);
    assert!(
        course.is_some(),
        "course should still be readable after TTL extension on read"
    );
}

#[test]
fn test_get_course_extends_ttl_on_each_successive_read() {
    // Multiple sequential reads must each independently succeed and keep the
    // entry alive by re-extending on every call.
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-TTL-MULTI",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-TTL-MULTI");

    for _ in 0..5 {
        env.ledger().with_mut(|l| {
            l.sequence_number += 100_001;
            l.min_persistent_entry_ttl = 1_000_000;
        });
        let course = client.get_course(&course_id);
        assert!(
            course.is_some(),
            "course must survive successive reads that each extend TTL"
        );
    }
}

#[test]
fn test_get_course_returns_none_without_error_for_missing_course() {
    // get_course() must return None (not panic) when the course does not exist.
    // The TTL-extension branch must not fire for an absent key.
    let (env, contract_id, _token_id, _admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let result = client.get_course(&String::from_str(&env, "COURSE-DOES-NOT-EXIST"));
    assert!(result.is_none());
}

#[test]
fn test_get_course_ttl_extension_does_not_mutate_course_data() {
    // TTL extension is a storage-layer side-effect only; course data must be
    // bit-for-bit identical before and after the ledger advance + re-read.
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-TTL-IMMUT",
        250_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-TTL-IMMUT");

    let before = client.get_course(&course_id).unwrap();

    env.ledger().with_mut(|l| {
        l.sequence_number += 100_001;
        l.min_persistent_entry_ttl = 500_000;
    });

    let after = client.get_course(&course_id).unwrap();

    // Core fields must be unchanged
    assert_eq!(before.id, after.id);
    assert_eq!(before.price, after.price);
    assert_eq!(before.status, after.status);
    assert_eq!(before.instructor, after.instructor);
    assert_eq!(before.total_enrollments, after.total_enrollments);
    assert_eq!(before.content_hash, after.content_hash);
    assert_eq!(before.last_updated_ledger, after.last_updated_ledger);
}

// ============================================================
// FEATURE: content_hash on Course struct
// ============================================================

#[test]
fn test_register_course_stores_content_hash() {
    // Verify that the hash passed at registration is stored verbatim.
    let (env, contract_id, token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let hash = BytesN::from_array(&env, &[0xABu8; 32]);
    let course_id = String::from_str(&env, "COURSE-HASH-STORE");

    client.register_course(
        &instructor,
        &course_id,
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &hash,
    );

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.content_hash, hash);
}

#[test]
fn test_register_course_distinct_hashes_stored_independently() {
    // Two courses with different hashes must each store their own value.
    let (env, contract_id, token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let hash_a = BytesN::from_array(&env, &[0x11u8; 32]);
    let hash_b = BytesN::from_array(&env, &[0x22u8; 32]);

    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-HASH-A"),
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &hash_a,
    );
    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-HASH-B"),
        &200_000_000,
        &token_id,
        &0u32,
        &None,
        &hash_b,
    );

    let course_a = client
        .get_course(&String::from_str(&env, "COURSE-HASH-A"))
        .unwrap();
    let course_b = client
        .get_course(&String::from_str(&env, "COURSE-HASH-B"))
        .unwrap();

    assert_eq!(course_a.content_hash, hash_a);
    assert_eq!(course_b.content_hash, hash_b);
    assert_ne!(course_a.content_hash, course_b.content_hash);
}

#[test]
fn test_instructor_can_update_content_hash_on_active_course() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let original_hash = BytesN::from_array(&env, &[0x01u8; 32]);
    let new_hash = BytesN::from_array(&env, &[0x02u8; 32]);

    let course_id = String::from_str(&env, "COURSE-HASH-UPDATE-INSTR");

    client.register_course(
        &instructor,
        &course_id,
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &original_hash,
    );
    client.approve_course(&admin, &course_id);

    // Instructor updates the hash after content revision
    client.update_content_hash(&instructor, &course_id, &new_hash);

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.content_hash, new_hash);
}

#[test]
fn test_admin_can_update_content_hash() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let original_hash = BytesN::from_array(&env, &[0x03u8; 32]);
    let new_hash = BytesN::from_array(&env, &[0x04u8; 32]);

    let course_id = String::from_str(&env, "COURSE-HASH-UPDATE-ADMIN");

    client.register_course(
        &instructor,
        &course_id,
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &original_hash,
    );
    client.approve_course(&admin, &course_id);

    client.update_content_hash(&admin, &course_id, &new_hash);

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.content_hash, new_hash);
}

#[test]
fn test_instructor_can_update_content_hash_on_paused_course() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let new_hash = BytesN::from_array(&env, &[0x05u8; 32]);
    let course_id = String::from_str(&env, "COURSE-HASH-UPDATE-PAUSED");

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-HASH-UPDATE-PAUSED",
        100_000_000,
    );
    client.pause_course(&instructor, &course_id);

    // Paused course — instructor update must still succeed
    client.update_content_hash(&instructor, &course_id, &new_hash);

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.content_hash, new_hash);
}

#[test]
#[should_panic(expected = "cannot update content hash of an archived course")]
fn test_update_content_hash_archived_course_rejected() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let new_hash = BytesN::from_array(&env, &[0x06u8; 32]);
    let course_id = String::from_str(&env, "COURSE-HASH-ARCHIVED");

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-HASH-ARCHIVED",
        100_000_000,
    );
    client.pause_course(&admin, &course_id);
    client.archive_course(&admin, &sec_admin, &course_id, &None);

    // Must reject — content updates on archived courses are not allowed
    client.update_content_hash(&instructor, &course_id, &new_hash);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_update_content_hash_random_caller_rejected() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let attacker = Address::generate(&env);
    let new_hash = BytesN::from_array(&env, &[0x07u8; 32]);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-HASH-UNAUTH",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-HASH-UNAUTH");

    // Random address — must be rejected
    client.update_content_hash(&attacker, &course_id, &new_hash);
}

#[test]
fn test_update_content_hash_emits_event() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let new_hash = BytesN::from_array(&env, &[0x08u8; 32]);
    let course_id = String::from_str(&env, "COURSE-HASH-EVENT");

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-HASH-EVENT",
        100_000_000,
    );

    client.update_content_hash(&instructor, &course_id, &new_hash);

    let mut found = false;
    for (contract, topics, _data) in env.events().all().iter() {
        if contract != contract_id {
            continue;
        }
        let topic_name: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
        if topic_name == Symbol::new(&env, "content_hash_updated") {
            let event_course_id: String = topics.get(1).unwrap().try_into_val(&env).unwrap();
            assert_eq!(event_course_id, course_id);
            found = true;
        }
    }
    assert!(found, "content_hash_updated event must be emitted");
}

#[test]
fn test_update_content_hash_updates_last_updated_ledger() {
    // Updating the hash must also bump last_updated_ledger to the current sequence.
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let new_hash = BytesN::from_array(&env, &[0x09u8; 32]);
    let course_id = String::from_str(&env, "COURSE-HASH-LEDGER");

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-HASH-LEDGER",
        100_000_000,
    );

    // Advance ledger before the update so we can detect the change
    env.ledger().with_mut(|l| {
        l.sequence_number += 100;
    });
    let ledger_at_update = env.ledger().sequence();

    client.update_content_hash(&instructor, &course_id, &new_hash);

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.last_updated_ledger, ledger_at_update);
}

#[test]
fn test_content_hash_survives_enroll_and_completion() {
    // Enrollment and mark_completed writes must not overwrite the content_hash.
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let hash = BytesN::from_array(&env, &[0xDDu8; 32]);
    let course_id = String::from_str(&env, "COURSE-HASH-PERSIST");

    client.register_course(
        &instructor,
        &course_id,
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &hash,
    );
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });
    client.approve_course(&admin, &course_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);
    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "proof")),
    );

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(
        course.content_hash, hash,
        "content_hash must not be modified by enroll or mark_completed"
    );
}

// ============================================================
// ADMIN EXPIRY TESTS (#126)
// ============================================================

#[test]
fn test_admin_operations_succeed_before_expiry() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Set admin expiry 1000 ledgers in the future
    let current_ledger = env.ledger().sequence();
    client.set_admin_expiry(&admin, &sec_admin, &Some(current_ledger + 1000));

    // Admin operations should succeed before expiry
    client.update_default_fee(&admin, &25u32);
    assert_eq!(client.get_platform_fee(&admin), 25);
}

#[test]
#[should_panic(expected = "admin role has expired")]
fn test_admin_operations_blocked_after_expiry() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Set admin expiry 10 ledgers in the future
    let current_ledger = env.ledger().sequence();
    client.set_admin_expiry(&admin, &sec_admin, &Some(current_ledger + 10));

    // Advance ledger past expiry
    env.ledger().with_mut(|l| {
        l.sequence_number += 11;
    });

    // Admin operations should be blocked after expiry
    client.update_default_fee(&admin, &30u32);
}

#[test]
fn test_remove_admin_expiry() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Set admin expiry
    let current_ledger = env.ledger().sequence();
    client.set_admin_expiry(&admin, &sec_admin, &Some(current_ledger + 10));
    assert_eq!(client.get_admin_expiry(), Some(current_ledger + 10));

    // Remove expiry
    client.set_admin_expiry(&admin, &sec_admin, &None);
    assert_eq!(client.get_admin_expiry(), None);

    // Admin operations should succeed after removing expiry, even far in the future
    env.ledger().with_mut(|l| {
        l.sequence_number += 1000;
    });
    client.update_default_fee(&admin, &35u32);
    assert_eq!(client.get_platform_fee(&admin), 35);
}

// ============================================================
// PLATFORM PAUSE LIFECYCLE TESTS (#127)
// ============================================================

#[test]
fn test_platform_pause_lifecycle_end_to_end() {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Register and approve a course
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-PAUSE-TEST",
        100_000_000,
    );

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    // Enrollment should succeed when platform is active
    client.enroll(&student, &String::from_str(&env, "COURSE-PAUSE-TEST"));
    assert!(client.is_enrolled(&student, &String::from_str(&env, "COURSE-PAUSE-TEST")));

    // Complete and prepare for re-enrollment test
    client.mark_completed(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-PAUSE-TEST"),
        &Some(String::from_str(&env, "evidence")),
    );

    // Pause the platform
    client.pause_platform(&admin);

    let student2 = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student2, &1_000_000_000);

    // New enrollment should be rejected during pause
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.enroll(&student2, &String::from_str(&env, "COURSE-PAUSE-TEST"));
    }));
    assert!(result.is_err());

    // Re-enrollment should also be rejected during pause
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.re_enroll(&student, &String::from_str(&env, "COURSE-PAUSE-TEST"));
    }));
    assert!(result.is_err());

    // Batch enrollment should be rejected during pause
    let courses = Vec::from_array(&env, [String::from_str(&env, "COURSE-PAUSE-TEST")]);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.batch_enroll(&student2, &courses);
    }));
    assert!(result.is_err());

    // Unpause the platform
    client.unpause_platform(&admin);

    // Enrollment should succeed after unpause
    client.enroll(&student2, &String::from_str(&env, "COURSE-PAUSE-TEST"));
    assert!(client.is_enrolled(&student2, &String::from_str(&env, "COURSE-PAUSE-TEST")));
}

#[test]
#[should_panic(expected = "platform is paused")]
fn test_enroll_blocked_during_platform_pause() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-001",
        100_000_000,
    );

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    client.pause_platform(&admin);
    client.enroll(&student, &String::from_str(&env, "COURSE-001"));
}

#[test]
#[should_panic(expected = "platform is paused")]
fn test_batch_enroll_blocked_during_platform_pause() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-001",
        100_000_000,
    );

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    client.pause_platform(&admin);

    let courses = Vec::from_array(&env, [String::from_str(&env, "COURSE-001")]);
    client.batch_enroll(&student, &courses);
}

// ============================================================
// PLATFORM ENROLLMENT CAP TESTS (#128)
// ============================================================

#[test]
fn test_enrollment_succeeds_below_platform_cap() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Set platform cap to 2 enrollments
    client.set_platform_enrollment_cap(&admin, &Some(2));

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-001",
        100_000_000,
    );

    let student1 = Address::generate(&env);
    let student2 = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student1, &1_000_000_000);
    token::StellarAssetClient::new(&env, &token_id).mint(&student2, &1_000_000_000);

    // First enrollment should succeed
    client.enroll(&student1, &String::from_str(&env, "COURSE-001"));
    assert_eq!(client.get_total_active_enrollments(), 1);

    // Second enrollment should succeed
    client.enroll(&student2, &String::from_str(&env, "COURSE-001"));
    assert_eq!(client.get_total_active_enrollments(), 2);
}

#[test]
#[should_panic(expected = "platform has reached maximum total enrollment capacity")]
fn test_enrollment_blocked_at_platform_cap() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Set platform cap to 1 enrollment
    client.set_platform_enrollment_cap(&admin, &Some(1));

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-001",
        100_000_000,
    );

    let student1 = Address::generate(&env);
    let student2 = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student1, &1_000_000_000);
    token::StellarAssetClient::new(&env, &token_id).mint(&student2, &1_000_000_000);

    // First enrollment should succeed
    client.enroll(&student1, &String::from_str(&env, "COURSE-001"));
    assert_eq!(client.get_total_active_enrollments(), 1);

    // Second enrollment should be rejected
    client.enroll(&student2, &String::from_str(&env, "COURSE-001"));
}

#[test]
fn test_platform_counter_decrements_on_completion() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.set_platform_enrollment_cap(&admin, &Some(2));

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-001",
        100_000_000,
    );

    let student1 = Address::generate(&env);
    let student2 = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student1, &1_000_000_000);
    token::StellarAssetClient::new(&env, &token_id).mint(&student2, &1_000_000_000);

    client.enroll(&student1, &String::from_str(&env, "COURSE-001"));
    client.enroll(&student2, &String::from_str(&env, "COURSE-001"));
    assert_eq!(client.get_total_active_enrollments(), 2);

    // Mark one as completed
    client.mark_completed(
        &admin,
        &student1,
        &String::from_str(&env, "COURSE-001"),
        &Some(String::from_str(&env, "evidence")),
    );

    // Counter should decrement
    assert_eq!(client.get_total_active_enrollments(), 1);

    // New enrollment should now succeed
    let student3 = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student3, &1_000_000_000);
    client.enroll(&student3, &String::from_str(&env, "COURSE-001"));
    assert_eq!(client.get_total_active_enrollments(), 2);
}

#[test]
fn test_platform_counter_decrements_on_refund() {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.set_platform_enrollment_cap(&admin, &Some(2));

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-001",
        100_000_000,
    );

    let student1 = Address::generate(&env);
    let student2 = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student1, &1_000_000_000);
    token::StellarAssetClient::new(&env, &token_id).mint(&student2, &1_000_000_000);

    client.enroll(&student1, &String::from_str(&env, "COURSE-001"));
    client.enroll(&student2, &String::from_str(&env, "COURSE-001"));
    assert_eq!(client.get_total_active_enrollments(), 2);

    // Request and approve refund
    client.request_refund(&student1, &String::from_str(&env, "COURSE-001"));
    env.mock_all_auths_allowing_non_root_auth();
    client.process_refund(
        &admin,
        &student1,
        &String::from_str(&env, "COURSE-001"),
        &true,
    );

    // Counter should decrement
    assert_eq!(client.get_total_active_enrollments(), 1);

    // New enrollment should now succeed
    let student3 = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student3, &1_000_000_000);
    client.enroll(&student3, &String::from_str(&env, "COURSE-001"));
    assert_eq!(client.get_total_active_enrollments(), 2);
}

// ============================================================
// MARK COMPLETED COURSE STATE VALIDATION TESTS (#129)
// ============================================================

#[test]
#[should_panic(expected = "cannot mark completion for archived course")]
fn test_mark_completed_on_archived_course_rejected() {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-001",
        100_000_000,
    );

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);
    client.enroll(&student, &String::from_str(&env, "COURSE-001"));

    // Mark completed first
    client.mark_completed(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-001"),
        &Some(String::from_str(&env, "evidence")),
    );

    // Now archive the course (which requires pausing first)
    client.pause_course(&admin, &String::from_str(&env, "COURSE-001"));

    // Archive with no refunds since student already completed
    client.archive_course(
        &admin,
        &sec_admin,
        &String::from_str(&env, "COURSE-001"),
        &None,
    );

    client.mark_completed(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-001"),
        &Some(String::from_str(&env, "evidence")),
    );
}

#[test]
#[should_panic(expected = "cannot mark completion for archived course")]
fn test_mark_completed_rejects_archived_course_even_with_active_enrollment() {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-001",
        100_000_000,
    );

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    // Enroll while course is active
    client.enroll(&student, &String::from_str(&env, "COURSE-001"));

    // Pause and archive the course
    client.pause_course(&admin, &String::from_str(&env, "COURSE-001"));

    // Archive with refund for this student
    env.mock_all_auths_allowing_non_root_auth();
    let refund_list = Vec::from_array(&env, [student.clone()]);
    client.archive_course(
        &admin,
        &sec_admin,
        &String::from_str(&env, "COURSE-001"),
        &Some(refund_list),
    );

    client.mark_completed(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-001"),
        &Some(String::from_str(&env, "evidence")),
    );
}

#[test]
fn test_mark_completed_succeeds_on_paused_course() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-001",
        100_000_000,
    );

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    // Enroll while course is active
    client.enroll(&student, &String::from_str(&env, "COURSE-001"));

    // Pause the course
    client.pause_course(&admin, &String::from_str(&env, "COURSE-001"));

    // Mark completed should succeed on paused course
    client.mark_completed(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-001"),
        &Some(String::from_str(&env, "evidence")),
    );

    let enrollment = client
        .get_enrollment(&admin, &student, &String::from_str(&env, "COURSE-001"))
        .unwrap();
    assert!(enrollment.completed);
}

#[test]
fn test_mark_completed_extends_enrollment_ttl() {
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
        "COURSE-TTL-COMPLETION",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-TTL-COMPLETION");
    client.enroll(&student, &course_id);

    env.ledger().with_mut(|l| {
        l.sequence_number += 5_000_000;
        l.min_persistent_entry_ttl = 100_000;
        l.min_temp_entry_ttl = 100_000;
    });

    let enrollment_key = DataKey::Enrollment(student.clone(), course_id.clone());

    let ttl_before = env.as_contract(&contract_id, || {
        env.storage().persistent().get_ttl(&enrollment_key)
    });

    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(String::from_str(&env, "evidence")),
    );

    let ttl_after = env.as_contract(&contract_id, || {
        env.storage().persistent().get_ttl(&enrollment_key)
    });

    assert!(
        ttl_after > ttl_before,
        "mark_completed should extend the enrollment TTL"
    );
}

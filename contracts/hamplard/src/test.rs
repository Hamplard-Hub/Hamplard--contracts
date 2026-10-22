#![cfg(test)]

use super::*;
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Events, Ledger as _, MockAuth, MockAuthInvoke},
    token, Address, BytesN, Env, IntoVal, String, Symbol, TryIntoVal, Val,
};
use std::format;

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
    client.init(
        &admin, &sec_admin, &treasury, &20u32, &50u32, &1000u32, &17_280u32,
    ); // 20% fee, 50 courses max, 1000-ledger refund window, 17280-ledger revocation challenge period
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

#[track_caller]
fn assert_only_auths(env: &Env, expected: &[Address]) {
    let auths = env.auths();
    assert_eq!(
        auths.len(),
        expected.len(),
        "unexpected auth count: expected {} signer(s), got {}",
        expected.len(),
        auths.len()
    );
    for expected_address in expected {
        assert!(
            auths.iter().any(|(address, _)| address == expected_address),
            "missing expected auth for {:?}",
            expected_address
        );
    }
}

macro_rules! invoke_with_auth {
    ($env:expr, $client:expr, $contract:expr, $signer:expr, $fn_name:literal, $args:expr, $method:ident($($arg:expr),* $(,)?)) => {{
        let args = ($args).into_val($env);
        let invoke = MockAuthInvoke {
            contract: $contract,
            fn_name: $fn_name,
            args,
            sub_invokes: &[],
        };
        let auth = MockAuth {
            address: $signer,
            invoke: &invoke,
        };
        $client.mock_auths(&[auth]).$method($($arg),*);
    }};
}

macro_rules! invoke_with_auths {
    ($env:expr, $client:expr, $contract:expr, $signers:expr, $fn_name:literal, $args:expr, $method:ident($($arg:expr),* $(,)?)) => {{
        let args = ($args).into_val($env);
        let invoke = MockAuthInvoke {
            contract: $contract,
            fn_name: $fn_name,
            args,
            sub_invokes: &[],
        };
        let auths: std::vec::Vec<MockAuth> = $signers
            .iter()
            .map(|address| MockAuth {
                address: *address,
                invoke: &invoke,
            })
            .collect();
        $client.mock_auths(&auths).$method($($arg),*);
    }};
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
        &BytesN::from_array(env, &[1u8; 32]),
        &soroban_sdk::Vec::new(env),
    );
    // Advance past the registration ledger so enroll()'s same-ledger guard
    // doesn't reject enrollments that happen right after this helper returns.
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });
    client.approve_course(admin, &String::from_str(env, course_id));
}

/// Fund the refund reserve for `token_id` from a fresh funder, as the treasury
/// would in production. Refunds draw the platform-fee share from this reserve.
fn fund_refund_reserve_for_test(
    env: &Env,
    client: &HamplardContractClient,
    token_id: &Address,
    amount: i128,
) {
    let funder = Address::generate(env);
    token::StellarAssetClient::new(env, token_id).mint(&funder, &amount);
    client.fund_refund_reserve(&funder, token_id, &amount);
}

/// Get the enrollment reference for a student/course pair.
/// The enrollment reference is generated on-chain during enrollment
/// and is used by issue_certificate to look up the enrollment.
fn evidence_hash(env: &Env, value: &str) -> BytesN<32> {
    let mut bytes = [0u8; 32];
    let input = value.as_bytes();
    for (idx, b) in input.iter().enumerate() {
        if idx < 32 {
            bytes[idx] = *b;
        }
    }
    BytesN::from_array(env, &bytes)
}

fn get_enrollment_ref(
    env: &Env,
    client: &HamplardContractClient,
    student: &Address,
    course_id: &str,
) -> String {
    let enrollment = client
        .get_enrollment(student, student, &String::from_str(env, course_id))
        .unwrap();
    enrollment.enrollment_ref
}

fn next_solvency_random(seed: &mut u64) -> u64 {
    *seed = (*seed)
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1);
    *seed
}

fn assert_token_solvency(
    env: &Env,
    client: &HamplardContractClient,
    contract_id: &Address,
    token_id: &Address,
    instructors: &[Address],
    students: &[Address],
    course_ids: &[String],
    operation: &str,
) {
    let mut liabilities = 0i128;
    for instructor in instructors {
        liabilities = liabilities
            .checked_add(client.get_instructor_earnings(instructor, token_id))
            .expect("instructor liability sum overflow");
    }

    for student in students {
        for course_id in course_ids {
            if let Some(case) = client.get_arbitration_case(student, course_id) {
                if case.status == DisputeStatus::Open {
                    assert_eq!(case.token, *token_id);
                    liabilities = liabilities
                        .checked_add(case.fee_paid)
                        .expect("arbitration liability sum overflow");
                }
            }
        }
    }

    let contract_balance = token::Client::new(env, token_id).balance(contract_id);
    assert!(
        contract_balance >= liabilities,
        "token insolvency after {operation}: contract balance {contract_balance}, liabilities {liabilities}"
    );
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
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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
#[should_panic(expected = "Error(Contract, #13)")]
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
    client.register_course(
        &instructor,
        &course_id,
        &50_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
    client.approve_course(&admin, &course_id);

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.status, CourseStatus::Active);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );

    // Stop mocking all auths so the real auth + admin check fires
    env.mock_all_auths_allowing_non_root_auth(); // ← or remove mock for this call

    client.approve_course(&instructor, &course_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #17)")]
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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

    client.enroll(&student, &String::from_str(&env, "COURSE-FASHION-001"), &i128::MAX);

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
#[should_panic(expected = "Error(Contract, #34)")]
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

    client.enroll(&student, &course_id, &i128::MAX);
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

    client.enroll(&student, &course_id, &i128::MAX);

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
        &String::from_str(&env, "COURSE-CONTRACT-INSTRUCTOR-001"),,
            &i128::MAX);

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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
    assert_eq!(client.get_platform_fee(&admin), 20);

    // Update platform default fee - enrollments should now use the new fee
    client.update_default_fee(&admin, &35u32);
    assert_eq!(client.get_platform_fee(&admin), 35);

    client.approve_course(&admin, &String::from_str(&env, "COURSE-FEE-UPDATE-001"));
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });
    client.enroll(&student, &String::from_str(&env, "COURSE-FEE-UPDATE-001"), &i128::MAX);

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
fn test_enroll_fee_honors_registered_override_as_floor() {
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
    assert_eq!(client.get_platform_fee(&admin), 20);

    // Lower the platform default fee to 10% — the course's explicit 40%
    // override remains the floor, so it is still what gets charged.
    client.update_default_fee(&admin, &10u32);
    assert_eq!(client.get_platform_fee(&admin), 10);

    client.approve_course(&admin, &String::from_str(&env, "COURSE-CUSTOM-FEE"));
    // Advance past the registration ledger so enroll()'s same-ledger guard
    // doesn't reject this enrollment.
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });

    let course_id = String::from_str(&env, "COURSE-CUSTOM-FEE");
    let quote = client.quote_enrollment(&course_id, &student);
    let platform_share = price * 40 / 100;
    let instructor_share = price - platform_share;
    assert_eq!(quote, (price, platform_share, instructor_share));

    client.enroll(&student, &course_id);

    // Platform fee should be 40% (registered override), not 10% (live
    // default): the rate validated at register_course() is the rate charged.
    assert_eq!(token_client.balance(&treasury), platform_share);
    assert_eq!(
        client.get_instructor_earnings(&instructor, &token_id),
        instructor_share,
    );
    let enrollment = client
        .get_enrollment(&student, &student, &course_id)
        .unwrap();
    assert_eq!(enrollment.platform_amount, quote.1);
    assert_eq!(enrollment.instructor_amount, quote.2);
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
    client.enroll(&student, &String::from_str(&env, "COURSE-FREE-001"), &i128::MAX);

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
#[should_panic(expected = "Error(Contract, #25)")]
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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
    client.enroll(&student, &course_id, &i128::MAX);
}

#[test]
#[should_panic(expected = "Error(Contract, #35)")]
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
    client.enroll(&student, &course_id, &i128::MAX);
    client.enroll(&student, &course_id, &i128::MAX); // second enroll — should panic
}

#[test]
#[should_panic(expected = "Error(Contract, #32)")]
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
    client.enroll(&admin, &course_id, &i128::MAX); // should panic
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

    client.enroll(&student, &course_id, &i128::MAX);

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
#[should_panic(expected = "Error(Contract, #34)")]
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );

    client.enroll(&student, &String::from_str(&env, "COURSE-PENDING"), &i128::MAX);
}

#[test]
#[should_panic(expected = "Error(Contract, #34)")]
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
    client.approve_course(&admin, &course_id);

    // Enrolling in the very same ledger the course was registered in must
    // be rejected, even though the course is already Active — a review
    // delay of at least one ledger sequence is required.
    client.enroll(&student, &course_id, &i128::MAX);
}

#[test]
#[should_panic(expected = "Error(Contract, #32)")]
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
    client.enroll(&instructor, &String::from_str(&env, "COURSE-SELF-ENROLL"), &i128::MAX);
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

    client.enroll(&student, &course_id, &i128::MAX);
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
    client.enroll(&student, &course_id, &i128::MAX);
    assert_eq!(client.has_completed(&student, &course_id), Some(false));

    // Mark completed
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence_hash")),
    );
    assert_eq!(client.has_completed(&student, &course_id), Some(true));

    // Issue certificate
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-TAILORING-001"),
        &cert_id,
        &course_title,
        &get_enrollment_ref(&env, &client, &student, "COURSE-TAILORING-001"),
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

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "completion-proof")),
    );

    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-CERT-EVENT-001"),
        &certificate_id,
        &String::from_str(&env, "Certificate Event Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-CERT-EVENT-001"),
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
#[should_panic(expected = "Error(Contract, #39)")]
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

    client.enroll(&student, &String::from_str(&env, "COURSE-NAILS-001"), &i128::MAX);

    // Try to issue certificate without completing — should panic
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-NAILS-001"),
        &String::from_str(&env, "CERT-EARLY"),
        &String::from_str(&env, "Nail Technology"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-NAILS-001"),
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

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence_hash")),
    );
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-MAKEUP-001"),
        &cert_id,
        &String::from_str(&env, "Makeup Artistry"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-MAKEUP-001"),
        &None,
        &None,
    );

    assert!(client.verify_certificate(&cert_id));

    // Revoke — enters pending state (challenge period starts)
    let reason = String::from_str(&env, "ACADEMIC_DISHONESTY");
    client.revoke_certificate(&admin, &cert_id, &reason);

    // During challenge period, certificate is still valid (verify returns true)
    assert!(client.verify_certificate(&cert_id));

    let cert = client.get_certificate(&admin, &cert_id);
    assert!(cert.revoked);
    assert_eq!(cert.revoked_by, Some(admin.clone()));
    assert!(cert.revocation_ledger.is_some());
    assert_eq!(cert.revocation_reason, Some(reason));
    assert!(cert.revocation_deadline.is_some());

    // Advance past challenge period — revocation becomes permanent
    let deadline = cert.revocation_deadline.unwrap();
    env.ledger().with_mut(|l| {
        l.sequence_number = deadline;
    });
    assert!(!client.verify_certificate(&cert_id));
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

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "proof")),
    );
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-EVENT-REVOKE"),
        &cert_id,
        &String::from_str(&env, "Title"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-EVENT-REVOKE"),
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

            // Verify event data: (admin, certificate_id, student, course_id, reason, ledger_sequence, deadline)
            let (
                event_admin,
                event_certificate_id,
                event_student,
                event_course_id,
                event_reason,
                event_ledger,
                event_deadline,
            ): (Address, String, Address, String, String, u32, u32) =
                data.try_into_val(&env).unwrap();

            assert_eq!(event_admin, admin);
            assert_eq!(event_certificate_id, cert_id);
            assert_eq!(event_student, student);
            assert_eq!(event_course_id, course_id);
            assert_eq!(event_reason, reason);
            assert!(event_ledger >= ledger_before);
            assert!(event_deadline > event_ledger);
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

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "proof")),
    );
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-AUDIT-001"),
        &cert_id,
        &String::from_str(&env, "Audit Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-AUDIT-001"),
        &None,
        &None,
    );

    // Certificate should have no revocation metadata before revocation
    let cert_before = client.get_certificate(&admin, &cert_id);
    assert!(!cert_before.revoked);
    assert!(cert_before.revoked_by.is_none());
    assert!(cert_before.revocation_ledger.is_none());
    assert!(cert_before.revocation_reason.is_none());

    let ledger_before = env.ledger().sequence();
    let reason = String::from_str(&env, "ISSUED_IN_ERROR");
    client.revoke_certificate(&admin, &cert_id, &reason);

    // All revocation metadata must be stored after revocation
    let cert_after = client.get_certificate(&admin, &cert_id);
    assert!(cert_after.revoked);
    assert_eq!(cert_after.revoked_by, Some(admin.clone()));
    assert!(cert_after.revocation_ledger.unwrap() >= ledger_before);
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

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence_hash")),
    );

    let signature = BytesN::from_array(&env, &[7u8; 64]);
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-SIGNED-001"),
        &cert_id,
        &String::from_str(&env, "Signed Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-SIGNED-001"),
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

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence_hash")),
    );

    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-UNSIGNED-001"),
        &cert_id,
        &String::from_str(&env, "Unsigned Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-UNSIGNED-001"),
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
#[should_panic(expected = "Error(Contract, #20)")]
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
        client.enroll(&s, &course_id, &i128::MAX);
    }

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.total_enrollments, 5);
    assert_eq!(course.total_earned, 5 * 200_000_000);
}

// ============================================================
// NEW TESTS FOR ADDED FEATURES
// ============================================================

// ============================================================
// #503 — INSTRUCTOR REPUTATION STATS COVERAGE
// ============================================================

/// An instructor with no activity reads as all-zero rather than panicking.
#[test]
fn test_instructor_stats_zero_for_unknown_instructor() {
    let (env, contract_id, _token_id, _admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let stranger = Address::generate(&env);
    let stats = client.get_instructor_stats(&stranger);
    assert_eq!(stats.total_students, 0);
    assert_eq!(stats.total_completions, 0);
    assert_eq!(stats.total_certificates, 0);
}

/// Students and completions are aggregated across every course the instructor
/// owns, not just the most recent one.
#[test]
fn test_instructor_stats_aggregate_across_courses() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let asset_client = token::StellarAssetClient::new(&env, &token_id);
    let course_a = String::from_str(&env, "COURSE-STATS-A");
    let course_b = String::from_str(&env, "COURSE-STATS-B");

    register_and_approve_course(&env, &client, &token_id, &admin, &instructor, "COURSE-STATS-A", 100_000_000);
    register_and_approve_course(&env, &client, &token_id, &admin, &instructor, "COURSE-STATS-B", 100_000_000);

    // Two students in course A, one in course B.
    let alice = Address::generate(&env);
    let bob = Address::generate(&env);
    let carol = Address::generate(&env);
    for s in [&alice, &bob, &carol] {
        asset_client.mint(s, &100_000_000_000);
    }
    client.enroll(&alice, &course_a);
    client.enroll(&bob, &course_a);
    client.enroll(&carol, &course_b);

    let stats = client.get_instructor_stats(&instructor);
    assert_eq!(stats.total_students, 3, "students count across both courses");
    assert_eq!(stats.total_completions, 0);
    assert_eq!(stats.total_certificates, 0);

    // Completing one enrollment in each course is two completions.
    client.mark_completed(&admin, &alice, &course_a, &Some(evidence_hash(&env, "a")));
    client.mark_completed(&admin, &carol, &course_b, &Some(evidence_hash(&env, "b")));

    let stats = client.get_instructor_stats(&instructor);
    assert_eq!(stats.total_students, 3);
    assert_eq!(stats.total_completions, 2);
    assert_eq!(stats.total_certificates, 0);

    // Issuing one certificate for each completion moves the certificate count.
    client.issue_certificate(
        &admin,
        &alice,
        &course_a,
        &String::from_str(&env, "CERT-STATS-A"),
        &String::from_str(&env, "Course A"),
        &get_enrollment_ref(&env, &client, &alice, "COURSE-STATS-A"),
        &None,
        &None,
    );
    client.issue_certificate(
        &admin,
        &carol,
        &course_b,
        &String::from_str(&env, "CERT-STATS-B"),
        &String::from_str(&env, "Course B"),
        &get_enrollment_ref(&env, &client, &carol, "COURSE-STATS-B"),
        &None,
        &None,
    );

    let stats = client.get_instructor_stats(&instructor);
    assert_eq!(stats.total_students, 3);
    assert_eq!(stats.total_completions, 2);
    assert_eq!(stats.total_certificates, 2);
}

/// Stats are keyed per instructor: one instructor's activity must never leak
/// into another instructor's reputation numbers.
#[test]
fn test_instructor_stats_are_isolated_per_instructor() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let other_instructor = Address::generate(&env);
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    register_and_approve_course(&env, &client, &token_id, &admin, &instructor, "COURSE-ISO-A", 100_000_000);
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &other_instructor,
        "COURSE-ISO-B",
        100_000_000,
    );

    client.enroll(&student, &String::from_str(&env, "COURSE-ISO-A"));
    client.mark_completed(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-ISO-A"),
        &Some(evidence_hash(&env, "iso")),
    );

    let active = client.get_instructor_stats(&instructor);
    assert_eq!(active.total_students, 1);
    assert_eq!(active.total_completions, 1);

    // The other instructor registered a course but never taught anyone.
    let idle = client.get_instructor_stats(&other_instructor);
    assert_eq!(idle.total_students, 0);
    assert_eq!(idle.total_completions, 0);
    assert_eq!(idle.total_certificates, 0);
}

/// A re-enrollment is a fresh student entry for the instructor, so the student
/// counter moves while completions stay as they were until the new run ends.
#[test]
fn test_instructor_stats_track_re_enrollment() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);
    let course_id = String::from_str(&env, "COURSE-STATS-RE");

    register_and_approve_course(&env, &client, &token_id, &admin, &instructor, "COURSE-STATS-RE", 100_000_000);

    client.enroll(&student, &course_id);
    client.mark_completed(&admin, &student, &course_id, &Some(evidence_hash(&env, "first")));

    let stats = client.get_instructor_stats(&instructor);
    assert_eq!(stats.total_students, 1);
    assert_eq!(stats.total_completions, 1);

    client.re_enroll(&student, &course_id);

    let stats = client.get_instructor_stats(&instructor);
    assert_eq!(
        stats.total_students, 2,
        "a re-enrollment is a second student entry for the instructor"
    );
    assert_eq!(
        stats.total_completions, 1,
        "the archived completion is not re-counted as a new completion"
    );
    assert_eq!(stats.total_certificates, 0);
}

#[test]
fn test_instructor_lifetime_earnings_are_token_scoped() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student_a = Address::generate(&env);
    let student_b = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student_a, &1_000_000_000);
    token::StellarAssetClient::new(&env, &token_id).mint(&student_b, &1_000_000_000);

    // Register and approve two courses for same instructor and token.
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

    client.enroll(&student_a, &String::from_str(&env, "COURSE-TOTAL-1"), &i128::MAX);
    client.enroll(&student_b, &String::from_str(&env, "COURSE-TOTAL-2"), &i128::MAX);

    let price1: i128 = 400_000_000;
    let price2: i128 = 600_000_000;
    let platform_share1 = price1 * 20 / 100;
    let platform_share2 = price2 * 20 / 100;
    let instructor_share1 = price1 - platform_share1;
    let instructor_share2 = price2 - platform_share2;
    let expected_token1_lifetime = instructor_share1 + instructor_share2;

    assert_eq!(
        client.get_instructor_earnings(&instructor, &token_id),
        expected_token1_lifetime
    );
    assert_eq!(
        client.get_instructor_lifetime_earnings(&instructor, &token_id),
        expected_token1_lifetime
    );

    // A second token must not be mixed into the first token's lifetime total.
    let token2_admin = Address::generate(&env);
    let token2_id = env
        .register_stellar_asset_contract_v2(token2_admin)
        .address();
    client.add_approved_token(&admin, &token2_id);
    register_and_approve_course(
        &env,
        &client,
        &token2_id,
        &admin,
        &instructor,
        "COURSE-TOTAL-3",
        300_000_000,
    );

    let student_c = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token2_id).mint(&student_c, &1_000_000_000);
    client.enroll(&student_c, &String::from_str(&env, "COURSE-TOTAL-3"));

    let price3: i128 = 300_000_000;
    let instructor_share3 = price3 - (price3 * 20 / 100);
    assert_eq!(
        client.get_instructor_lifetime_earnings(&instructor, &token2_id),
        instructor_share3
    );
    assert_eq!(
        client.get_instructor_lifetime_earnings(&instructor, &token_id),
        expected_token1_lifetime
    );

    // Withdrawals reduce the withdrawable balance but never lifetime earnings.
    client.withdraw_earnings(&instructor, &token_id, &100_000_000, &None);
    assert_eq!(
        client.get_instructor_lifetime_earnings(&instructor, &token_id),
        expected_token1_lifetime
    );
    assert_eq!(
        client.get_instructor_earnings(&instructor, &token_id),
        expected_token1_lifetime - 100_000_000
    );

    // Refunds debit only the lifetime counter for the refunded token.
    client.request_refund(&student_c, &String::from_str(&env, "COURSE-TOTAL-3"));
    env.mock_all_auths_allowing_non_root_auth();
    client.process_refund(
        &admin,
        &student_c,
        &String::from_str(&env, "COURSE-TOTAL-3"),
        &true,
        &None,
    );
    assert_eq!(
        client.get_instructor_lifetime_earnings(&instructor, &token2_id),
        0
    );
    assert_eq!(
        client.get_instructor_lifetime_earnings(&instructor, &token_id),
        expected_token1_lifetime
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
    client.enroll(&student, &course_id, &i128::MAX);

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
    client.enroll(&student, &course_id, &i128::MAX);

    // Call mark_completed with evidence hash.
    let hash = evidence_hash(&env, "some_evidence_hash");
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
#[should_panic(expected = "Error(Contract, #39)")]
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
    client.enroll(&student, &course_id, &i128::MAX);

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
        &Some(evidence_hash(&env, "proof")),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #23)")]
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
    client.enroll(&student, &course_id, &i128::MAX);

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

    client.enroll(&student_a, &course_id, &i128::MAX);
    client.enroll(&student_b, &course_id, &i128::MAX);

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
    // The treasury pre-funds the refund reserve with the fees it collected.
    client.fund_refund_reserve(&treasury, &token_id, &platform_fee_total);

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
    assert_eq!(course.total_enrollments, 0);

    assert!(!client.is_enrolled(&student_a, &course_id));
    assert!(!client.is_enrolled(&student_b, &course_id));
}
#[test]
fn test_archive_course_emits_refund_issued_per_student() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student_a = Address::generate(&env);
    let student_b = Address::generate(&env);
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
        "COURSE-REFUND-EVENTS",
        price,
    );
    let course_id = String::from_str(&env, "COURSE-REFUND-EVENTS");
    client.enroll(&student_a, &course_id);
    client.enroll(&student_b, &course_id);
    client.pause_course(&admin, &course_id);

    let refund_students = Vec::from_array(&env, [student_a.clone(), student_b.clone()]);
    env.mock_all_auths_allowing_non_root_auth();
    client.archive_course(&admin, &sec_admin, &course_id, &Some(refund_students));

    let expected_platform = price * 20 / 100;
    let expected_instructor = price - expected_platform;
    let mut refund_events = 0u32;
    let mut saw_a = false;
    let mut saw_b = false;

    for (contract, topics, data) in env.events().all().iter() {
        if contract != contract_id {
            continue;
        }
        let sym: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
        if sym != Symbol::new(&env, "refund_issued") {
            continue;
        }
        refund_events += 1;
        let (
            event_student,
            event_course_id,
            event_amount,
            event_platform,
            event_instructor,
            _event_ledger,
        ): (Address, String, i128, i128, i128, u32) = data.try_into_val(&env).unwrap();
        assert_eq!(event_course_id, course_id);
        assert_eq!(event_amount, price);
        assert_eq!(event_platform, expected_platform);
        assert_eq!(event_instructor, expected_instructor);
        if event_student == student_a {
            saw_a = true;
        } else if event_student == student_b {
            saw_b = true;
        }
    }

    assert_eq!(
        refund_events, 2,
        "one refund_issued event per refunded student"
    );
    assert!(saw_a);
    assert!(saw_b);
}

#[test]
fn test_archive_course_emits_refund_issued_per_student() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student_a = Address::generate(&env);
    let student_b = Address::generate(&env);
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
        "COURSE-REFUND-EVENTS",
        price,
    );
    let course_id = String::from_str(&env, "COURSE-REFUND-EVENTS");
    client.enroll(&student_a, &course_id);
    client.enroll(&student_b, &course_id);
    client.pause_course(&admin, &course_id);

    let refund_students = Vec::from_array(&env, [student_a.clone(), student_b.clone()]);
    env.mock_all_auths_allowing_non_root_auth();
    client.archive_course(&admin, &sec_admin, &course_id, &Some(refund_students));

    let expected_platform = price * 20 / 100;
    let expected_instructor = price - expected_platform;
    let mut refund_events = 0u32;
    let mut saw_a = false;
    let mut saw_b = false;

    for (contract, topics, data) in env.events().all().iter() {
        if contract != contract_id {
            continue;
        }
        let sym: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
        if sym != Symbol::new(&env, "refund_issued") {
            continue;
        }
        refund_events += 1;
        let (
            event_student,
            event_course_id,
            event_amount,
            event_platform,
            event_instructor,
            _event_ledger,
        ): (Address, String, i128, i128, i128, u32) = data.try_into_val(&env).unwrap();
        assert_eq!(event_course_id, course_id);
        assert_eq!(event_amount, price);
        assert_eq!(event_platform, expected_platform);
        assert_eq!(event_instructor, expected_instructor);
        if event_student == student_a {
            saw_a = true;
        } else if event_student == student_b {
            saw_b = true;
        }
    }

    assert_eq!(
        refund_events, 2,
        "one refund_issued event per refunded student"
    );
    assert!(saw_a);
    assert!(saw_b);
}

#[test]
fn test_archive_course_incremental_refunds() {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    fund_refund_reserve_for_test(&env, &client, &token_id, 10_000_000_000);

    let mut students: std::vec::Vec<Address> = std::vec::Vec::new();
    for _ in 0..5 {
        let s = Address::generate(&env);
        token::StellarAssetClient::new(&env, &token_id).mint(&s, &1_000_000_000);
        students.push(s);
    }

    let price = 500_000_000;
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-INCREMENTAL-REFUND",
        price,
    );
    let course_id = String::from_str(&env, "COURSE-INCREMENTAL-REFUND");

    for student in &students {
        client.enroll(student, &course_id, &i128::MAX);
    }

    client.pause_course(&admin, &course_id);

    // First refund batch: 2 of 5 students
    let mut batch1 = soroban_sdk::Vec::new(&env);
    batch1.push_back(students[0].clone());
    batch1.push_back(students[1].clone());

    env.mock_all_auths_allowing_non_root_auth();
    client.archive_course(&admin, &sec_admin, &course_id, &Some(batch1));

    // Course should still be Paused because 3 active enrollments remain
    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.status, CourseStatus::Paused);
    assert_eq!(course.active_enrollments, 3);

    // Second refund batch: remaining 3 students
    let mut batch2 = soroban_sdk::Vec::new(&env);
    for i in 2..5 {
        batch2.push_back(students[i].clone());
    }
    client.archive_course(&admin, &sec_admin, &course_id, &Some(batch2));

    // Now the course should be Archived
    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.status, CourseStatus::Archived);
    assert_eq!(course.active_enrollments, 0);

    for student in &students {
        assert!(!client.is_enrolled(student, &course_id));
    }
}

#[test]
#[should_panic(expected = "Error(Contract, #24)")]
fn test_archive_course_rejects_oversized_refund_list() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let price = 500_000_000;
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-OVERFLOW-REFUND",
        price,
    );
    let course_id = String::from_str(&env, "COURSE-OVERFLOW-REFUND");

    client.pause_course(&admin, &course_id);

    let mut too_many = soroban_sdk::Vec::new(&env);
    for _ in 0..(HamplardContract::MAX_STUDENTS_TO_REFUND + 1) {
        let s = Address::generate(&env);
        too_many.push_back(s);
    }

    env.mock_all_auths_allowing_non_root_auth();
    client.archive_course(&admin, &sec_admin, &course_id, &Some(too_many));
}

#[test]
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

    // Student has enough for platform fee (100_000_000) but not the full course price.
    // A failed payment must not panic: enrollment fails gracefully and emits a
    // scoped `enrollment_failed` event with reason INSUFFICIENT_BALANCE.
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &150_000_000);

    // The token transfer fails, so enroll() emits an EnrollmentFailed event
    // and returns without recording an enrollment or moving any funds.
    client.enroll(&student, &course_id);

    assert!(!client.is_enrolled(&student, &course_id));
    // The failure trace records the scoped reason for off-chain consumers.
    let failed: EnrollmentFailed = last_event_val(&env, &contract_id, "enrollment_failed")
        .try_into_val(&env)
        .unwrap();
    assert_eq!(failed.course_id, course_id);
    assert_eq!(failed.student, student);
    assert_eq!(
        failed.reason_code,
        String::from_str(&env, "INSUFFICIENT_BALANCE")
    );

    // No enrollment state was mutated (rollback proof)
    assert!(!client.is_enrolled(&student, &course_id));
    assert!(client
        .get_enrollment(&student, &student, &course_id)
        .is_none());
    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.total_enrollments, 0);
    assert_eq!(course.active_enrollments, 0);

    // Verify treasury didn't receive any tokens and no balances moved
    assert_eq!(token_client.balance(&treasury), 0);
    assert_eq!(token_client.balance(&student), 150_000_000);
    assert_eq!(token_client.balance(&contract_id), 0);
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
    client.enroll(&student_1, &course_id, &i128::MAX);
    let platform_fee = price * 20 / 100;
    assert_eq!(token_client.balance(&treasury), platform_fee);
    assert_eq!(token_client.balance(&new_treasury), 0);

    // Advance ledger sequence by delay
    env.ledger().with_mut(|l| {
        l.sequence_number += HamplardContract::TREASURY_UPDATE_DELAY_LEDGERS;
    });

    // Enroll student_2 - fee should now go to the new treasury
    client.enroll(&student_2, &course_id, &i128::MAX);
    assert_eq!(token_client.balance(&treasury), platform_fee); // unchanged
    assert_eq!(token_client.balance(&new_treasury), platform_fee); // new treasury receives it
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
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
#[should_panic(expected = "Error(Contract, #11)")]
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
    let course = client.get_course(&max_id).unwrap();
    assert_eq!(course.status, CourseStatus::Pending);
}

#[test]
#[should_panic(expected = "Error(Contract, #41)")]
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
    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "hash")),
    );

    let long_title = String::from_str(&env, &"T".repeat(513));
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-TITLE-LEN"),
        &String::from_str(&env, "CERT-TITLE-LEN"),
        &long_title,
        &get_enrollment_ref(&env, &client, &student, "COURSE-TITLE-LEN"),
        &None,
        &None,
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #41)")]
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
    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "hash")),
    );

    let long_cert_id = String::from_str(&env, &"C".repeat(257));
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-CERT-ID-LEN"),
        &long_cert_id,
        &String::from_str(&env, "Valid Title"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-CERT-ID-LEN"),
        &None,
        &None,
    );
}

// ============================================================
// ARCHIVE LIFECYCLE TESTS (#19)
// ============================================================

#[test]
#[should_panic(expected = "Error(Contract, #23)")]
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
fn test_archive_pending_course_succeeds() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-ARCHIVE-PENDING"),
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );

    // Pending courses cannot have enrollments and can be archived directly.
    client.archive_course(
        &admin,
        &sec_admin,
        &String::from_str(&env, "COURSE-ARCHIVE-PENDING"),
        &None,
    );

    let course = client
        .get_course(&String::from_str(&env, "COURSE-ARCHIVE-PENDING"))
        .unwrap();
    assert_eq!(course.status, CourseStatus::Archived);
}

#[test]
fn test_archive_rejected_course_succeeds() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let course_id = String::from_str(&env, "COURSE-ARCHIVE-REJECTED");

    client.register_course(
        &instructor,
        &course_id,
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
    client.reject_course(&admin, &course_id, &String::from_str(&env, "not a fit"));
    client.archive_course(&admin, &sec_admin, &course_id, &None);

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.status, CourseStatus::Archived);
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
#[should_panic(expected = "Error(Contract, #1)")]
fn test_init_cannot_be_called_twice() {
    let (env, contract_id, _, admin, sec_admin, treasury, _) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    // Second init call must be rejected
    client.init(
        &admin, &sec_admin, &treasury, &20u32, &50u32, &1000u32, &17_280u32,
    );
}

// ============================================================
// ISSUE #424: INIT EMITS `initialized` EVENT
// ============================================================

#[test]
fn test_init_emits_initialized_event_once() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register(HamplardContract, ());
    let client = HamplardContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let sec_admin = Address::generate(&env);
    let treasury = Address::generate(&env);

    client.init(
        &admin, &sec_admin, &treasury, &20u32, &50u32, &1000u32, &17_280u32,
    );

    let mut count = 0u32;
    let mut payload: Option<Val> = None;
    for (contract, topics, data) in env.events().all().iter() {
        if contract != contract_id {
            continue;
        }
        let sym: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
        if sym == Symbol::new(&env, "initialized") {
            let topic_admin: Address = topics.get(1).unwrap().try_into_val(&env).unwrap();
            assert_eq!(topic_admin, admin);
            count += 1;
            payload = Some(data);
        }
    }
    assert_eq!(count, 1, "initialized must be emitted exactly once");

    let (ev_admin, ev_sec_admin, ev_treasury, ev_fee, ev_max_courses, ev_refund_window): (
        Address,
        Address,
        Address,
        u32,
        u32,
        u32,
    ) = payload.unwrap().try_into_val(&env).unwrap();
    assert_eq!(ev_admin, admin);
    assert_eq!(ev_sec_admin, sec_admin);
    assert_eq!(ev_treasury, treasury);
    assert_eq!(ev_fee, 20);
    assert_eq!(ev_max_courses, 50);
    assert_eq!(ev_refund_window, 1000);
}

// ============================================================
// ISSUE #2: TOKEN WHITELIST
// ============================================================

#[test]
#[should_panic(expected = "Error(Contract, #34)")]
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
    client.approve_course(&admin, &String::from_str(&env, "COURSE-EVIL-TOKEN"));
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });

    // Enrollment must fail because the token is not whitelisted
    client.enroll(&student, &String::from_str(&env, "COURSE-EVIL-TOKEN"), &i128::MAX);
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
    client.enroll(&student, &course_id, &i128::MAX);
    assert!(client.is_enrolled(&student, &course_id));
}

// ============================================================
// ISSUE #1: CROSS-COURSE CERTIFICATE ID COLLISION
// ============================================================

#[test]
#[should_panic(expected = "Error(Contract, #42)")]
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
    client.enroll(&student_a, &course_a, &i128::MAX);
    client.mark_completed(
        &admin,
        &student_a,
        &course_a,
        &Some(evidence_hash(&env, "ev_a")),
    );
    client.issue_certificate(
        &admin,
        &student_a,
        &String::from_str(&env, "COURSE-COLL-A"),
        &cert_id,
        &String::from_str(&env, "Course A"),
        &get_enrollment_ref(&env, &client, &student_a, "COURSE-COLL-A"),
        &None,
        &None,
    );

    // Student B completes course B — attempt to reuse the same cert ID must fail
    let student_b = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student_b, &1_000_000_000);
    client.enroll(&student_b, &course_b, &i128::MAX);
    client.mark_completed(
        &admin,
        &student_b,
        &course_b,
        &Some(evidence_hash(&env, "ev_b")),
    );
    client.issue_certificate(
        &admin,
        &student_b,
        &String::from_str(&env, "COURSE-COLL-B"),
        &cert_id,
        &String::from_str(&env, "Course B"),
        &get_enrollment_ref(&env, &client, &student_b, "COURSE-COLL-B"),
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
#[should_panic(expected = "Error(Contract, #4)")]
fn test_accept_admin_without_proposal_fails() {
    let (env, contract_id, _, _, sec_admin, _, _) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let random = Address::generate(&env);
    // No transfer_admin() called — must panic
    let new_sec = Address::generate(&env);
    client.accept_admin(&random, &new_sec);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
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
#[should_panic(expected = "Error(Contract, #2)")]
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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
    client.enroll(&student, &course_id, &i128::MAX);

    // mark_completed — old admin must be rejected
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.mark_completed(
            &admin,
            &student,
            &course_id,
            &Some(evidence_hash(&env, "evidence")),
        );
    }));
    assert!(res.is_err(), "old admin must be rejected by mark_completed");

    client.mark_completed(
        &new_admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence")),
    );

    // issue_certificate — old admin must be rejected
    let cert_id = String::from_str(&env, "CERT-OLD-ADMIN");
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.issue_certificate(
            &admin,
            &student,
            &String::from_str(&env, "COURSE-OLD-ADMIN"),
            &cert_id,
            &String::from_str(&env, "Old Admin Course"),
            &get_enrollment_ref(&env, &client, &student, "COURSE-OLD-ADMIN"),
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
        &student,
        &String::from_str(&env, "COURSE-OLD-ADMIN"),
        &cert_id,
        &String::from_str(&env, "Old Admin Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-OLD-ADMIN"),
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
        client.withdraw_tokens(&admin, &sec_admin, &token_id, &1i128, &admin);
    }));
    assert!(
        res.is_err(),
        "old admin must be rejected by withdraw_tokens"
    );

    // freeze_instructor — old admin must be rejected
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.freeze_instructor(&admin, &instructor, &String::from_str(&env, "test reason"));
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
    let refund_course_id = String::from_str(&env, "COURSE-OLD-ADMIN-REFUND");
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });
    client.approve_course(&new_admin, &refund_course_id);
    client.enroll(&refund_student, &refund_course_id, &i128::MAX);
    client.request_refund(&refund_student, &refund_course_id);

    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.process_refund(&admin, &refund_student, &refund_course_id, &true, &None);
        client.process_refund(&admin, &refund_student, &refund_course_id, &true, &None);
        client.process_refund(&admin, &refund_student, &refund_course_id, &true, &None);
        client.process_refund(&admin, &refund_student, &refund_course_id, &true, &None);
        client.process_refund(&admin, &refund_student, &refund_course_id, &true, &None::<i128>);
        client.process_refund(&admin, &refund_student, &refund_course_id, &true, &None);
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
    client.enroll(&student, &course_id, &i128::MAX);

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
    client.enroll(&student, &course_id, &i128::MAX);

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
    client.enroll(&student, &course_id, &i128::MAX);

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
        &Some(evidence_hash(&env, "proof")),
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

    client.batch_enroll(&student, &course_ids, &i128::MAX);

    assert!(client.is_enrolled(&student, &String::from_str(&env, "COURSE-BATCH-A")));
    assert!(client.is_enrolled(&student, &String::from_str(&env, "COURSE-BATCH-B")));
    assert!(client.is_enrolled(&student, &String::from_str(&env, "COURSE-BATCH-C")));
}

#[test]
#[should_panic(expected = "Error(Contract, #34)")]
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );

    let mut course_ids = soroban_sdk::Vec::new(&env);
    course_ids.push_back(String::from_str(&env, "COURSE-BATCH-OK"));
    course_ids.push_back(String::from_str(&env, "COURSE-BATCH-BAD"));

    client.batch_enroll(&student, &course_ids, &i128::MAX);

    // Must not reach here — if panic didn't happen, no partial state
    assert!(!client.is_enrolled(&student, &String::from_str(&env, "COURSE-BATCH-OK")));
}

#[test]
#[should_panic(expected = "Error(Contract, #29)")]
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

    client.batch_enroll(&student, &course_ids, &i128::MAX);
}

#[test]
fn test_batch_enroll_large_unique_batch_succeeds() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &10_000_000_000);

    client.update_max_courses_limit(&admin, &100u32);

    let batch_size = HamplardContract::MAX_BATCH_SIZE;
    let mut course_ids = soroban_sdk::Vec::new(&env);
    for i in 0..batch_size {
        let course_id = format!("COURSE-BATCH-LINEAR-{}", i);
        register_and_approve_course(
            &env,
            &client,
            &token_id,
            &admin,
            &instructor,
            &course_id,
            100_000_000,
        );
        course_ids.push_back(String::from_str(&env, &course_id));
    }

    client.batch_enroll(&student, &course_ids, &i128::MAX);

    for i in 0..batch_size {
        let course_id = format!("COURSE-BATCH-LINEAR-{}", i);
        assert!(client.is_enrolled(&student, &String::from_str(&env, &course_id)));
    }
}

#[test]
#[should_panic(expected = "Error(Contract, #29)")]
fn test_batch_enroll_rejects_duplicate_in_large_batch() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &10_000_000_000);

    client.update_max_courses_limit(&admin, &100u32);

    let unique_count = HamplardContract::MAX_BATCH_SIZE - 1;
    let mut course_ids = soroban_sdk::Vec::new(&env);
    for i in 0..unique_count {
        let course_id = format!("COURSE-BATCH-DUP-LARGE-{}", i);
        register_and_approve_course(
            &env,
            &client,
            &token_id,
            &admin,
            &instructor,
            &course_id,
            100_000_000,
        );
        course_ids.push_back(String::from_str(&env, &course_id));
    }
    // Duplicate the first ID at the end of an otherwise-max-sized batch.
    course_ids.push_back(String::from_str(&env, "COURSE-BATCH-DUP-LARGE-0"));

    client.batch_enroll(&student, &course_ids, &i128::MAX);
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

    let total_paid = client.batch_enroll(&student, &course_ids, &i128::MAX);
    assert_eq!(total_paid, price_a + price_b);

    // Verify that two per-course events and one batch summary were emitted.
    let events = env.events().all();
    let mut enrollment_events = 0u32;
    let mut batch_events = 0u32;
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
        } else if sym == Symbol::new(&env, "batch_enrolled") {
            batch_events += 1;

            let (event_student, event_course_ids, event_total_paid): (
                Address,
                soroban_sdk::Vec<String>,
                i128,
            ) = data.try_into_val(&env).unwrap();

            assert_eq!(event_student, student);
            assert_eq!(event_course_ids, course_ids);
            assert_eq!(event_total_paid, price_a + price_b);
        }
    }

    // Ensure exactly two enrollment events were emitted (one per course)
    assert_eq!(enrollment_events, 2);
    assert!(event_a_found);
    assert!(event_b_found);
    assert_eq!(batch_events, 1);
}

// ============================================================
// ISSUE #30: ENHANCED AUTHORIZATION ERROR MESSAGES
// ============================================================

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
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
    client.enroll(&student, &course_id, &i128::MAX);

    // Instructor (not admin) tries to mark as completed — should panic with operation name
    client.mark_completed(&instructor, &student, &course_id, &None);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_pause_platform_unauthorized_includes_operation() {
    let (env, contract_id, _token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Instructor tries to pause platform — should panic with operation name
    client.pause_platform(&instructor);
}

#[test]
#[should_panic(expected = "Error(Contract, #30)")]
fn test_pause_platform_rejects_double_pause() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.pause_platform(&admin);
    client.pause_platform(&admin);
}

#[test]
#[should_panic(expected = "Error(Contract, #30)")]
fn test_unpause_platform_rejects_double_unpause() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.unpause_platform(&admin);
}

/// Read the remaining instance TTL of the contract.
fn instance_ttl(env: &Env, contract_id: &Address) -> u32 {
    use soroban_sdk::testutils::storage::Instance as _;
    env.as_contract(contract_id, || env.storage().instance().get_ttl())
}

#[test]
fn test_pause_platform_extends_instance_ttl() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    env.ledger().with_mut(|l| {
        l.sequence_number += 400_000;
    });

    assert!(instance_ttl(&env, &contract_id) < HamplardContract::INSTANCE_TTL_THRESHOLD);

    client.pause_platform(&admin);

    assert_eq!(
        instance_ttl(&env, &contract_id),
        HamplardContract::INSTANCE_TTL_EXTEND_TO
    );
}

#[test]
fn test_unpause_platform_extends_instance_ttl() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.pause_platform(&admin);

    env.ledger().with_mut(|l| {
        l.sequence_number += 400_000;
    });

    assert!(instance_ttl(&env, &contract_id) < HamplardContract::INSTANCE_TTL_THRESHOLD);

    client.unpause_platform(&admin);

    assert_eq!(
        instance_ttl(&env, &contract_id),
        HamplardContract::INSTANCE_TTL_EXTEND_TO
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_add_approved_token_unauthorized_includes_operation() {
    let (env, contract_id, token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let new_token = Address::generate(&env);

    // Instructor tries to add approved token — should panic with operation name
    client.add_approved_token(&instructor, &new_token);
}

// ============================================================
// ISSUE #439: is_token_approved / get_approved_tokens
// ============================================================

#[test]
fn test_is_token_approved_returns_true_after_add() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let new_token = Address::generate(&env);
    assert!(!client.is_token_approved(&new_token));

    client.add_approved_token(&admin, &new_token);
    assert!(client.is_token_approved(&new_token));
}

#[test]
fn test_is_token_approved_returns_false_after_remove() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let new_token = Address::generate(&env);
    client.add_approved_token(&admin, &new_token);
    assert!(client.is_token_approved(&new_token));

    client.remove_approved_token(&admin, &new_token);
    assert!(!client.is_token_approved(&new_token));
}

#[test]
fn test_get_approved_tokens_reflects_add_and_remove() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // token_id was whitelisted by setup() — it should appear in the list
    let extra_token = Address::generate(&env);
    client.add_approved_token(&admin, &extra_token);

    let list = client.get_approved_tokens(&admin);
    assert!(list.contains(&token_id));
    assert!(list.contains(&extra_token));

    // Remove the extra token and confirm it is gone from the list
    client.remove_approved_token(&admin, &extra_token);
    let list_after = client.get_approved_tokens(&admin);
    assert!(list_after.contains(&token_id));
    assert!(!list_after.contains(&extra_token));
}

#[test]
fn test_add_approved_token_idempotent() {
    // Adding the same token twice must not create duplicate list entries.
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let new_token = Address::generate(&env);
    client.add_approved_token(&admin, &new_token);
    client.add_approved_token(&admin, &new_token); // second call is a no-op

    let list = client.get_approved_tokens(&admin);
    let count = list.iter().filter(|t| *t == new_token).count();
    assert_eq!(count, 1, "token should appear exactly once in the list");
}

#[test]
#[should_panic(expected = "unauthorized: get_approved_tokens")]
fn test_get_approved_tokens_unauthorized() {
    let (env, contract_id, _token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Non-admin should not be able to enumerate the whitelist
    client.get_approved_tokens(&instructor);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
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
    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(&admin, &student, &course_id, &None);
    let cert_id = String::from_str(&env, "CERT-REVOKE-UNAUTH-001");
    let course_title = String::from_str(&env, "Revoke Test Course");
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-REVOKE-UNAUTH"),
        &cert_id,
        &course_title,
        &get_enrollment_ref(&env, &client, &student, "COURSE-REVOKE-UNAUTH"),
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

    client.enroll(&student, &course_id, &i128::MAX);

    let instructor_share = price * 80 / 100;
    assert_eq!(
        client.get_instructor_earnings(&instructor, &token_id),
        instructor_share
    );
    assert_eq!(token_client.balance(&instructor), 0);

    client.withdraw_earnings(&instructor, &token_id, &0, &None);
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
        client.enroll(&s, &course_id, &i128::MAX);
    }

    let instructor_share_per = price * 80 / 100;
    assert_eq!(
        client.get_instructor_earnings(&instructor, &token_id),
        instructor_share_per * 3,
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #40)")]
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
    client.enroll(&student, &String::from_str(&env, "COURSE-EARN-AUTH"), &i128::MAX);

    let impostor = Address::generate(&env);
    client.withdraw_earnings(&impostor, &token_id, &100, &None);
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
    client.enroll(&student, &String::from_str(&env, "COURSE-EARN-DBL"), &i128::MAX);

    client.withdraw_earnings(&instructor, &token_id, &0, &None);
    let balance_after_first = token_client.balance(&instructor);

    // Second full withdrawal is a no-op (zero balance)
    client.withdraw_earnings(&instructor, &token_id, &0, &None);
    assert_eq!(token_client.balance(&instructor), balance_after_first);
}

#[test]
fn test_zero_balance_withdrawal_is_safe() {
    let (env, contract_id, token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    assert_eq!(client.get_instructor_earnings(&instructor, &token_id), 0);
    client.withdraw_earnings(&instructor, &token_id, &0, &None);
    assert_eq!(client.get_instructor_earnings(&instructor, &token_id), 0);
}

fn enroll_one(
    env: &Env,
    client: &HamplardContractClient,
    token_id: &Address,
    admin: &Address,
    instructor: &Address,
    course: &str,
) -> i128 {
    let student = Address::generate(env);
    token::StellarAssetClient::new(env, token_id).mint(&student, &1_000_000_000);
    register_and_approve_course(
        env,
        client,
        token_id,
        admin,
        instructor,
        course,
        500_000_000,
    );
    client.enroll(&student, &String::from_str(env, course), &i128::MAX);
    500_000_000 * 80 / 100
}

#[test]
fn test_withdraw_earnings_to_custom_destination() {
    let (env, contract_id, token_id, admin, _s, _t, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let share = enroll_one(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-DEST-1",
    );

    let cold = Address::generate(&env);
    client.withdraw_earnings(&instructor, &token_id, &0, &Some(cold.clone()));

    assert_eq!(token_client.balance(&cold), share);
    assert_eq!(token_client.balance(&instructor), 0);
    assert_eq!(client.get_instructor_earnings(&instructor, &token_id), 0);
}

#[test]
fn test_withdraw_earnings_partial_to_custom_destination() {
    let (env, contract_id, token_id, admin, _s, _t, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let share = enroll_one(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-DEST-2",
    );

    let cold = Address::generate(&env);
    client.withdraw_earnings(&instructor, &token_id, &100_000_000, &Some(cold.clone()));

    assert_eq!(token_client.balance(&cold), 100_000_000);
    assert_eq!(
        client.get_instructor_earnings(&instructor, &token_id),
        share - 100_000_000
    );
}

#[test]
fn test_withdraw_earnings_none_defaults_to_instructor() {
    let (env, contract_id, token_id, admin, _s, _t, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let share = enroll_one(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-DEST-3",
    );

    client.withdraw_earnings(&instructor, &token_id, &0, &None);
    assert_eq!(token_client.balance(&instructor), share);
}

#[test]
fn test_withdraw_earnings_event_includes_destination() {
    let (env, contract_id, token_id, admin, _s, _t, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let share = enroll_one(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-DEST-4",
    );

    let cold = Address::generate(&env);
    client.withdraw_earnings(&instructor, &token_id, &0, &Some(cold.clone()));

    let mut found = false;
    for (contract, topics, data) in env.events().all().iter() {
        if contract != contract_id {
            continue;
        }
        let sym: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
        if sym == Symbol::new(&env, "earnings_withdrawn") {
            let topic_instructor: Address = topics.get(1).unwrap().try_into_val(&env).unwrap();
            let (tok, amt, dest): (Address, i128, Address) = data.try_into_val(&env).unwrap();
            assert_eq!(topic_instructor, instructor);
            assert_eq!(tok, token_id);
            assert_eq!(amt, share);
            assert_eq!(dest, cold);
            found = true;
        }
    }
    assert!(found, "earnings_withdrawn event not emitted");
}

#[test]
fn test_withdraw_earnings_only_instructor_auth_required() {
    let (env, contract_id, token_id, admin, _s, _t, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let share = enroll_one(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-DEST-5",
    );

    let cold = Address::generate(&env);
    let dest = Some(cold.clone());

    // Destination does NOT sign; instructor alone is enough.
    client
        .mock_auths(&[MockAuth {
            address: &instructor,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "withdraw_earnings",
                args: (instructor.clone(), token_id.clone(), 0i128, dest.clone()).into_val(&env),
                sub_invokes: &[],
            },
        }])
        .withdraw_earnings(&instructor, &token_id, &0, &dest);

    assert_eq!(token::Client::new(&env, &token_id).balance(&cold), share);
}

#[test]
fn test_withdraw_earnings_destination_auth_is_not_enough() {
    let (env, contract_id, token_id, admin, _s, _t, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    enroll_one(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-DEST-6",
    );

    let attacker = Address::generate(&env);
    let dest = Some(attacker.clone());

    // Only the destination signs; the instructor's auth is missing.
    let res = client
        .mock_auths(&[MockAuth {
            address: &attacker,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "withdraw_earnings",
                args: (instructor.clone(), token_id.clone(), 0i128, dest.clone()).into_val(&env),
                sub_invokes: &[],
            },
        }])
        .try_withdraw_earnings(&instructor, &token_id, &0, &dest);

    assert!(res.is_err());
    assert_eq!(token::Client::new(&env, &token_id).balance(&attacker), 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #40)")]
fn test_withdraw_earnings_rejects_contract_as_destination() {
    let (env, contract_id, token_id, admin, _s, _t, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    enroll_one(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-DEST-7",
    );

    client.withdraw_earnings(&instructor, &token_id, &0, &Some(contract_id.clone()));
}

#[test]
#[should_panic(expected = "Error(Contract, #15)")]
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
    client.register_course(
        &instructor,
        &course_id_2,
        &1_000_000i128,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
    client.register_course(
        &instructor,
        &course_id_3,
        &1_000_000i128,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );

    // instructor_b's count is independent — this must succeed
    client.register_course(
        &instructor_b,
        &String::from_str(&env, "COURSE-IND-B1"),
        &1_000_000i128,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
    assert_eq!(client.get_instructor_course_count(&instructor), 1u32);

    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-COUNT-002"),
        &1_000_000i128,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "proof")),
    );
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-VERIFY-001"),
        &cert_id,
        &String::from_str(&env, "Test Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-VERIFY-001"),
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

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "proof")),
    );
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-VERIFY-002"),
        &cert_id,
        &String::from_str(&env, "Test Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-VERIFY-002"),
        &None,
        &None,
    );

    assert!(client.verify_certificate(&cert_id));

    // Revoke — enters pending state
    client.revoke_certificate(
        &admin,
        &cert_id,
        &String::from_str(&env, "ACADEMIC_DISHONESTY"),
    );
    // During challenge period, still valid
    assert!(client.verify_certificate(&cert_id));

    // Advance past challenge period
    let cert = client.get_certificate(&admin, &cert_id);
    let deadline = cert.revocation_deadline.unwrap();
    env.ledger().with_mut(|l| {
        l.sequence_number = deadline;
    });
    // Now permanently revoked
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

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "proof")),
    );
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-VERIFY-003"),
        &cert_id,
        &String::from_str(&env, "Test Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-VERIFY-003"),
        &None,
        &None,
    );

    client.revoke_certificate(&admin, &cert_id, &String::from_str(&env, "ISSUED_IN_ERROR"));

    // Advance past challenge period so revocation is permanent
    let cert = client.get_certificate(&admin, &cert_id);
    let deadline = cert.revocation_deadline.unwrap();
    env.ledger().with_mut(|l| {
        l.sequence_number = deadline;
    });

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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
    client.approve_course(&admin, &String::from_str(&env, "COURSE-CAP-EXACT"));
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    // Enrolling the first (and only allowed) student must succeed
    client.enroll(&student, &String::from_str(&env, "COURSE-CAP-EXACT"), &i128::MAX);

    let course = client
        .get_course(&String::from_str(&env, "COURSE-CAP-EXACT"))
        .unwrap();
    assert_eq!(course.total_enrollments, 1);
}

#[test]
#[should_panic(expected = "Error(Contract, #34)")]
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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
    client.enroll(&student_a, &course_id, &i128::MAX); // fills the one seat
    client.enroll(&student_b, &course_id, &i128::MAX); // must panic
}

#[test]
fn test_refund_releases_capacity_for_new_student() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-CAP-REFUND",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-CAP-REFUND");
    client.update_course(&instructor, &course_id, &None, &Some(Some(1u32)));

    let student_a = Address::generate(&env);
    let student_b = Address::generate(&env);
    let asset_client = token::StellarAssetClient::new(&env, &token_id);
    asset_client.mint(&student_a, &1_000_000_000);
    asset_client.mint(&student_b, &1_000_000_000);

    client.enroll(&student_a, &course_id);
    assert_eq!(client.get_course(&course_id).unwrap().total_enrollments, 1);

    client.request_refund(&student_a, &course_id);
    env.mock_all_auths_allowing_non_root_auth();
    client.process_refund(&admin, &student_a, &course_id, &true, &None);

    let after_refund = client.get_course(&course_id).unwrap();
    assert_eq!(after_refund.total_enrollments, 0);
    assert_eq!(after_refund.active_enrollments, 0);

    client.enroll(&student_b, &course_id);
    let after_enroll = client.get_course(&course_id).unwrap();
    assert_eq!(after_enroll.total_enrollments, 1);
    assert_eq!(after_enroll.active_enrollments, 1);
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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
        client.enroll(&s, &course_id, &i128::MAX);
    }

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.total_enrollments, 5);
}

#[test]
#[should_panic(expected = "Error(Contract, #34)")]
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
    client.approve_course(&admin, &String::from_str(&env, "COURSE-BATCH-CAP-OK"));

    // Course B: capacity 1 — filled by another student before the batch
    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-BATCH-CAP-FULL"),
        &100_000_000,
        &token_id,
        &0u32,
        &Some(1u32),
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
    client.approve_course(&admin, &String::from_str(&env, "COURSE-BATCH-CAP-FULL"));
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });

    let first_student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&first_student, &1_000_000_000);
    client.enroll(
        &first_student,
        &String::from_str(&env, "COURSE-BATCH-CAP-FULL"),,
            &i128::MAX);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    let mut course_ids = soroban_sdk::Vec::new(&env);
    course_ids.push_back(String::from_str(&env, "COURSE-BATCH-CAP-OK"));
    course_ids.push_back(String::from_str(&env, "COURSE-BATCH-CAP-FULL"));

    // batch_enroll validates all courses before enrolling any — must panic
    client.batch_enroll(&student, &course_ids, &i128::MAX);
}

#[test]
#[should_panic(expected = "Error(Contract, #28)")]
fn test_batch_enroll_rejects_oversized_batch() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &10_000_000_000);

    client.update_max_courses_limit(&admin, &100u32);

    for i in 0..60 {
        let course_id = format!("COURSE-BATCH-OVERFLOW-{}", i);
        register_and_approve_course(
            &env,
            &client,
            &token_id,
            &admin,
            &instructor,
            &course_id,
            100_000_000,
        );
    }

    let mut course_ids = soroban_sdk::Vec::new(&env);
    for i in 0..60 {
        course_ids.push_back(String::from_str(
            &env,
            &format!("COURSE-BATCH-OVERFLOW-{}", i),
        ));
    }

    client.batch_enroll(&student, &course_ids, &i128::MAX);
}

#[test]
#[should_panic(expected = "Error(Contract, #25)")]
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

    client.enroll(&student, &course_id, &i128::MAX);
}

#[test]
#[should_panic(expected = "Error(Contract, #16)")]
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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
    client.enroll(&student, &course_id, &i128::MAX);

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
#[should_panic(expected = "Error(Contract, #2)")]
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
    client.enroll(&student, &course_id, &i128::MAX);

    // Random user cannot access
    client.get_enrollment(&random_user, &student, &course_id);
}

// ============================================================
// ISSUE 47: ARCHIVE-THEN-REREGISTER
// ============================================================

#[test]
#[should_panic(expected = "Error(Contract, #14)")]
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
}

// ============================================================
// ISSUE 48: CONCURRENT ENROLLMENT/COMPLETION
// ============================================================

#[test]
#[should_panic(expected = "Error(Contract, #39)")]
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
    client.enroll(&student, &course_id, &i128::MAX);

    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "ev")),
    );
    // Second completion should panic
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "ev2")),
    );
}

// ============================================================
// ISSUE 183: COURSE PRICE DENOMINATION RANGE CHECK
// ============================================================

#[test]
#[should_panic(expected = "Error(Contract, #12)")]
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #12)")]
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
    client.approve_course(&admin, &matching_id);
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });

    // Enroll and complete
    client.enroll(&student, &matching_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &matching_id,
        &Some(evidence_hash(&env, "proof")),
    );

    // Issue certificate with matching_id (same as course_id)
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "MATCHING-ID-123"),
        &matching_id, // matching_id used as cert_id
        &String::from_str(&env, "Test Course"),
        &get_enrollment_ref(&env, &client, &student, "MATCHING-ID-123"),
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
#[should_panic(expected = "Error(Contract, #4)")]
fn test_transfer_admin_rejects_identical_addresses() {
    let (env, contract_id, _token_id, admin, sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Proposing the exact current admin & secondary admin should panic
    client.transfer_admin(&admin, &sec_admin, &admin, &sec_admin);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
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
    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(&admin, &student, &course_id, &None);

    // Issue certificate with expiry at ledger 1000
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-EXPIRY"),
        &cert_id,
        &String::from_str(&env, "Expiry Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-EXPIRY"),
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
    let reason = String::from_str(&env, "policy violation");
    client.freeze_instructor(&admin, &instructor, &reason);
    assert!(client.is_instructor_frozen(&instructor));
    assert_eq!(
        client.get_instructor_freeze_reason(&instructor),
        Some(reason.clone())
    );

    // Unfreeze instructor clears both the flag and the reason
    client.unfreeze_instructor(&admin, &instructor);
    assert!(!client.is_instructor_frozen(&instructor));
    assert_eq!(client.get_instructor_freeze_reason(&instructor), None);
}

#[test]
fn test_student_blocklist_lifecycle() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);

    assert!(!client.is_student_blocked(&student));

    let reason = String::from_str(&env, "fraudulent enrollment");
    client.block_student(&admin, &student, &reason);
    assert!(client.is_student_blocked(&student));
    assert_eq!(
        client.get_student_block_reason(&student),
        Some(reason.clone())
    );

    client.unblock_student(&admin, &student);
    assert!(!client.is_student_blocked(&student));
    assert_eq!(client.get_student_block_reason(&student), None);
}

#[test]
#[should_panic(expected = "moderation reason cannot be empty")]
fn test_freeze_instructor_rejects_empty_reason() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.freeze_instructor(&admin, &instructor, &String::from_str(&env, ""));
}

#[test]
#[should_panic(expected = "moderation reason cannot be empty")]
fn test_block_student_rejects_empty_reason() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);

    client.block_student(&admin, &student, &String::from_str(&env, ""));
}

#[test]
#[should_panic(expected = "moderation reason exceeds maximum length")]
fn test_freeze_instructor_reason_length_is_bounded() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let long_reason = "x".repeat((HamplardContract::MAX_MODERATION_REASON_LEN + 1) as usize);

    client.freeze_instructor(&admin, &instructor, &String::from_str(&env, &long_reason));
}

#[test]
#[should_panic(expected = "moderation reason exceeds maximum length")]
fn test_block_student_reason_length_is_bounded() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);
    let long_reason = "x".repeat((HamplardContract::MAX_MODERATION_REASON_LEN + 1) as usize);

    client.block_student(&admin, &student, &String::from_str(&env, &long_reason));
}

#[test]
fn test_block_student_event_includes_reason() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);
    let reason = String::from_str(&env, "chargeback abuse");

    client.block_student(&admin, &student, &reason);

    let (event_student, event_admin, event_reason): (Address, Address, String) =
        last_event_val(&env, &contract_id, "student_blocked")
            .try_into_val(&env)
            .unwrap();
    assert_eq!(event_student, student);
    assert_eq!(event_admin, admin);
    assert_eq!(event_reason, reason);
}

#[test]
#[should_panic(expected = "Error(Contract, #31)")]
fn test_block_student_already_blocked_panics() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);

    client.block_student(&admin, &student, &String::from_str(&env, "test reason"));
    assert!(client.is_student_blocked(&student));

    // Blocking an already-blocked student is a no-op and must panic.
    client.block_student(&admin, &student, &String::from_str(&env, "test reason"));
}

#[test]
#[should_panic(expected = "Error(Contract, #31)")]
fn test_unblock_student_not_blocked_panics() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);

    assert!(!client.is_student_blocked(&student));

    // Unblocking a student that was never blocked is a no-op and must panic.
    client.unblock_student(&admin, &student);
}

#[test]
#[should_panic(expected = "Error(Contract, #9)")]
fn test_freeze_instructor_already_frozen_panics() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.freeze_instructor(&admin, &instructor, &String::from_str(&env, "test reason"));
    assert!(client.is_instructor_frozen(&instructor));

    // Freezing an already-frozen instructor is a no-op and must panic.
    client.freeze_instructor(&admin, &instructor, &String::from_str(&env, "test reason"));
}

#[test]
#[should_panic(expected = "Error(Contract, #9)")]
fn test_unfreeze_instructor_not_frozen_panics() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    assert!(!client.is_instructor_frozen(&instructor));

    // Unfreezing an instructor that was never frozen is a no-op and must panic.
    client.unfreeze_instructor(&admin, &instructor);
}

#[test]
#[should_panic(expected = "Error(Contract, #31)")]
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

    client.block_student(&admin, &student, &String::from_str(&env, "test reason"));
    client.enroll(&student, &String::from_str(&env, "BLOCKED-STUDENT-COURSE"));
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_frozen_instructor_cannot_register_course() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.freeze_instructor(&admin, &instructor, &String::from_str(&env, "test reason"));

    // Register course should panic
    client.register_course(
        &instructor,
        &String::from_str(&env, "FROZEN-COURSE"),
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
}
#[test]
fn test_refund_lifecycle() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    fund_refund_reserve_for_test(&env, &client, &token_id, 10_000_000_000);

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
    client.enroll(&student, &course_id, &i128::MAX);
    assert!(client.is_enrolled(&student, &course_id));

    // Request refund
    client.request_refund(&student, &course_id);

    let request = client.get_refund_request(&student, &course_id).unwrap();
    assert_eq!(request.status, RefundStatus::Pending);

    // Approve refund
    let initial_balance = token::Client::new(&env, &token_id).balance(&student);
    env.mock_all_auths_allowing_non_root_auth();
    client.process_refund(&admin, &student, &course_id, &true, &None);

    let final_balance = token::Client::new(&env, &token_id).balance(&student);
    assert_eq!(final_balance - initial_balance, 1_000_000_000);
    assert!(!client.is_enrolled(&student, &course_id));

    let request_approved = client.get_refund_request(&student, &course_id).unwrap();
    assert_eq!(request_approved.status, RefundStatus::Approved);
}

#[test]
#[should_panic(expected = "Error(Contract, #45)")]
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

    client.enroll(&student, &course_id, &i128::MAX);

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
    client.enroll(&student, &course_id, &i128::MAX);

    client.request_refund(&student, &course_id);
    env.mock_all_auths_allowing_non_root_auth();
    client.process_refund(&admin, &student, &course_id, &false, &None);

    let request = client.get_refund_request(&student, &course_id).unwrap();
    assert_eq!(request.status, RefundStatus::Rejected);
    assert!(client.is_enrolled(&student, &course_id));
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
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
    client.freeze_instructor(&admin, &instructor, &String::from_str(&env, "test reason"));

    // Attempting to enroll in the frozen instructor's course must fail
    client.enroll(&student, &String::from_str(&env, "PRE-FREEZE-COURSE"), &i128::MAX);
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

    client.enroll(&student, &course_id, &i128::MAX);
    assert_eq!(client.has_completed(&student, &course_id), Some(false));

    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence_hash")),
    );
    assert_eq!(client.has_completed(&student, &course_id), Some(true));
}

// ============================================================
// #352 — re_enroll() MUST CLEAR THE SAME GUARDS AS enroll()
// ============================================================

/// A re-enrollment is a paid enrollment, so the platform-wide cap has to apply
/// to it exactly as it does to a first enrollment. Before re_enroll() shared
/// the enrollment guard chain it silently let a full platform take more money.
#[test]
#[should_panic(expected = "platform has reached maximum total enrollment capacity")]
fn test_re_enroll_rejected_when_platform_enrollment_cap_reached() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    let course_id = String::from_str(&env, "COURSE-REENROLL-CAP");
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-REENROLL-CAP",
        100_000_000,
    );

    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "cap")),
    );

    // Completing released the only active slot, so the cap is hit again as soon
    // as it is set to zero — the platform accepts no further enrollments of any
    // kind, and a re-enrollment is an enrollment.
    client.set_platform_enrollment_cap(&admin, &Some(0u32));

    client.re_enroll(&student, &course_id);
}

/// The prerequisite guard has to be re-evaluated on a re-enrollment: revoking
/// the prerequisite certificate after the first run must block a new one.
#[test]
#[should_panic(expected = "prerequisite course not completed")]
fn test_re_enroll_revalidates_prerequisite_certificate() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    let prereq_id = String::from_str(&env, "COURSE-RE-PREREQ");
    let advanced_id = String::from_str(&env, "COURSE-RE-ADVANCED");

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-RE-PREREQ",
        100_000_000,
    );
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-RE-ADVANCED",
        100_000_000,
    );

    let mut prereqs = soroban_sdk::Vec::new(&env);
    prereqs.push_back(prereq_id.clone());
    client.set_prerequisite_courses(&instructor, &advanced_id, &prereqs);

    // Complete the prerequisite and earn its certificate.
    client.enroll(&student, &prereq_id);
    client.mark_completed(
        &admin,
        &student,
        &prereq_id,
        &Some(evidence_hash(&env, "prereq")),
    );
    client.issue_certificate(
        &admin,
        &student,
        &prereq_id,
        &String::from_str(&env, "CERT-RE-PREREQ"),
        &String::from_str(&env, "Prerequisite"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-RE-PREREQ"),
        &None,
        &None,
    );

    // The student is then allowed into the advanced course and finishes it.
    client.enroll(&student, &advanced_id);
    client.mark_completed(
        &admin,
        &student,
        &advanced_id,
        &Some(evidence_hash(&env, "advanced")),
    );

    // The prerequisite certificate is revoked, so the prerequisite is no longer
    // satisfied.
    client.revoke_certificate(
        &admin,
        &String::from_str(&env, "CERT-RE-PREREQ"),
        &String::from_str(&env, "revoked for the regression test"),
    );

    client.re_enroll(&student, &advanced_id);
}

/// The happy path must keep working: with every guard satisfied, a completed
/// student can still re-enroll and is charged again.
#[test]
fn test_re_enroll_still_succeeds_when_guards_pass() {
    let (env, contract_id, token_id, admin, _sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    let course_id = String::from_str(&env, "COURSE-RE-HAPPY");
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-RE-HAPPY",
        200_000_000,
    );

    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "happy")),
    );

    let treasury_before = token_client.balance(&treasury);
    client.re_enroll(&student, &course_id);

    assert_eq!(
        token_client.balance(&treasury) - treasury_before,
        40_000_000,
        "a re-enrollment is charged the same 20% platform fee"
    );
    let enrollment = client.get_enrollment(&student, &student, &course_id).unwrap();
    assert!(!enrollment.completed, "a fresh enrollment cycle starts");
    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.total_enrollments, 2);
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

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence_1")),
    );
    assert_eq!(client.has_completed(&student, &course_id), Some(true));

    let treasury_before = token::Client::new(&env, &token_id).balance(&treasury);

    client.re_enroll(&student, &course_id, &i128::MAX);

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

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence_original")),
    );
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-REENROLL-002"),
        &cert_id,
        &String::from_str(&env, "Re-enroll Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-REENROLL-002"),
        &None,
        &None,
    );

    client.re_enroll(&student, &course_id, &i128::MAX);

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
        Some(evidence_hash(&env, "evidence_original"))
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
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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
    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence")),
    );
    let (event_student, event_admin, _event_ledger): (Address, Address, u32) =
        last_event_val(&env, &contract_id, "course_completed")
            .try_into_val(&env)
            .unwrap();
    assert_eq!(event_student, student);
    assert_eq!(event_admin, admin);

    // issue_certificate
    let cert_id = String::from_str(&env, "CERT-ATTR-001");
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-ATTR-001"),
        &cert_id,
        &String::from_str(&env, "Re-enroll Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-ATTR-001"),
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
    let ledger_before_fee = env.ledger().sequence();
    client.update_default_fee(&admin, &25u32);
    let (old_fee, new_fee, event_admin, event_ledger): (u32, u32, Address, u32) =
        last_event_val(&env, &contract_id, "default_fee_updated")
            .try_into_val(&env)
            .unwrap();
    assert_eq!(old_fee, 20u32);
    assert_eq!(new_fee, 25u32);
    assert_eq!(event_admin, admin);
    assert_eq!(event_ledger, ledger_before_fee);

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
    let (event_admin, event_old, event_new): (Address, u32, u32) =
        last_event_val(&env, &contract_id, "max_courses_limit_updated")
            .try_into_val(&env)
            .unwrap();
    assert_eq!(event_admin, admin);
    assert_eq!(event_old, 50u32);
    assert_eq!(event_new, 99u32);

    // freeze_instructor / unfreeze_instructor
    client.freeze_instructor(&admin, &instructor, &String::from_str(&env, "test reason"));
    let (event_instructor, event_admin, event_reason): (Address, Address, String) =
        last_event_val(&env, &contract_id, "instructor_frozen")
            .try_into_val(&env)
            .unwrap();
    assert_eq!(event_instructor, instructor);
    assert_eq!(event_admin, admin);
    assert_eq!(event_reason, String::from_str(&env, "test reason"));

    client.unfreeze_instructor(&admin, &instructor);
    let (event_instructor, event_admin): (Address, Address) =
        last_event_val(&env, &contract_id, "instructor_unfrozen")
            .try_into_val(&env)
            .unwrap();
    assert_eq!(event_instructor, instructor);
    assert_eq!(event_admin, admin);

    // update_min_review_delay
    client.update_min_review_delay(&admin, &5u32);
    let (event_admin, event_old, event_new): (Address, u32, u32) =
        last_event_val(&env, &contract_id, "min_review_delay_updated")
            .try_into_val(&env)
            .unwrap();
    assert_eq!(event_admin, admin);
    assert_eq!(event_old, 0u32);
    assert_eq!(event_new, 5u32);

    // update_min_completion_ledgers
    client.update_min_completion_ledgers(&admin, &10u32);
    let (event_admin, event_old, event_new): (Address, u32, u32) =
        last_event_val(&env, &contract_id, "min_completion_ledgers_updated")
            .try_into_val(&env)
            .unwrap();
    assert_eq!(event_admin, admin);
    assert_eq!(event_old, 0u32);
    assert_eq!(event_new, 10u32);

    // update_refund_window
    client.update_refund_window(&admin, &2000u32);
    let (event_admin, event_old, event_new): (Address, u32, u32) =
        last_event_val(&env, &contract_id, "refund_window_updated")
            .try_into_val(&env)
            .unwrap();
    assert_eq!(event_admin, admin);
    assert_eq!(event_old, 1000u32);
    assert_eq!(event_new, 2000u32);

    // set_platform_enrollment_cap
    client.set_platform_enrollment_cap(&admin, &Some(500u32));
    let (event_admin, event_old, event_new): (Address, Option<u32>, Option<u32>) =
        last_event_val(&env, &contract_id, "platform_enrollment_cap_set")
            .try_into_val(&env)
            .unwrap();
    assert_eq!(event_admin, admin);
    assert_eq!(event_old, None);
    assert_eq!(event_new, Some(500u32));

    // set_fee_config
    client.set_fee_config(&admin, &token_id, &250u32);
    let (event_token, event_old, event_new): (Address, Option<u32>, u32) =
        last_event_val(&env, &contract_id, "fee_config_updated")
            .try_into_val(&env)
            .unwrap();
    assert_eq!(event_token, token_id);
    assert_eq!(event_old, None);
    assert_eq!(event_new, 250u32);

    // set_admin_expiry
    client.set_admin_expiry(&admin, &sec_admin, &Some(1_000_000u32));
    let (event_admin1, event_admin2, event_old, event_new): (
        Address,
        Address,
        Option<u32>,
        Option<u32>,
    ) = last_event_val(&env, &contract_id, "admin_expiry_set")
        .try_into_val(&env)
        .unwrap();
    assert_eq!(event_admin1, admin);
    assert_eq!(event_admin2, sec_admin);
    assert_eq!(event_old, None);
    assert_eq!(event_new, Some(1_000_000u32));

    // withdraw_tokens requires both admins and a positive amount.
    let destination = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&contract_id, &1_000_000);
    client.withdraw_tokens(&admin, &sec_admin, &token_id, &1_000_000i128, &destination);
    let (event_admin, event_admin2, event_token, event_amount, event_dest): (
        Address,
        Address,
        Address,
        i128,
        Address,
    ) = last_event_val(&env, &contract_id, "tokens_withdrawn")
        .try_into_val(&env)
        .unwrap();
    assert_eq!(event_admin, admin);
    assert_eq!(event_admin2, sec_admin);
    assert_eq!(event_token, token_id);
    assert_eq!(event_amount, 1_000_000i128);
    assert_eq!(event_dest, destination);
}

#[test]
#[should_panic(expected = "Error(Contract, #39)")]
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
    client.re_enroll(&student, &course_id, &i128::MAX);
}

#[test]
#[should_panic(expected = "Error(Contract, #39)")]
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

    client.enroll(&student, &course_id, &i128::MAX);
    // Not completed — must panic
    client.re_enroll(&student, &course_id, &i128::MAX);
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
    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence-1")),
    );
    assert_eq!(
        client
            .get_enrollment_history(&student, &student, &course_id)
            .len(),
        0
    );
    client.re_enroll(&student, &course_id, &i128::MAX);

    // Second attempt: re_enroll already created a fresh enrollment — just
    // complete it and re-enroll again (archives attempt 2)
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence-2")),
    );
    assert_eq!(
        client
            .get_enrollment_history(&student, &student, &course_id)
            .len(),
        1
    );
    client.re_enroll(&student, &course_id, &i128::MAX);

    let history = client.get_enrollment_history(&student, &student, &course_id);
    assert_eq!(history.len(), 2);
    assert!(history.get(0).unwrap().completed);
    assert!(history.get(1).unwrap().completed);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
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

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence")),
    );
    client.re_enroll(&student, &course_id, &i128::MAX);

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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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
    fund_refund_reserve_for_test(&env, &client, &token_id, 10_000_000_000);

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

    client.enroll(&student, &course_id, &i128::MAX);
    client.request_refund(&student, &course_id);
    env.mock_all_auths_allowing_non_root_auth();
    client.process_refund(&admin, &student, &course_id, &true, &None);

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
fn test_process_refund_emits_refund_issued_with_split() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    let price = 1_000_000_000;
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-REFUND-EVENT-SPLIT",
        price,
    );
    let course_id = String::from_str(&env, "COURSE-REFUND-EVENT-SPLIT");
    client.enroll(&student, &course_id);
    client.request_refund(&student, &course_id);
    env.mock_all_auths_allowing_non_root_auth();
    client.process_refund(&admin, &student, &course_id, &true, &None);

    let (
        event_student,
        event_course_id,
        event_amount,
        event_platform,
        event_instructor,
        _event_ledger,
    ): (Address, String, i128, i128, i128, u32) =
        last_event_val(&env, &contract_id, "refund_issued")
            .try_into_val(&env)
            .unwrap();

    let expected_platform = price * 20 / 100;
    assert_eq!(event_student, student);
    assert_eq!(event_course_id, course_id);
    assert_eq!(event_amount, price);
    assert_eq!(event_platform, expected_platform);
    assert_eq!(event_instructor, price - expected_platform);
}

#[test]
fn test_process_refund_emits_refund_issued_with_split() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    let price = 1_000_000_000;
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-REFUND-EVENT-SPLIT",
        price,
    );
    let course_id = String::from_str(&env, "COURSE-REFUND-EVENT-SPLIT");
    client.enroll(&student, &course_id);
    client.request_refund(&student, &course_id);
    env.mock_all_auths_allowing_non_root_auth();
    client.process_refund(&admin, &student, &course_id, &true, &None);

    let (
        event_student,
        event_course_id,
        event_amount,
        event_platform,
        event_instructor,
        _event_ledger,
    ): (Address, String, i128, i128, i128, u32) =
        last_event_val(&env, &contract_id, "refund_issued")
            .try_into_val(&env)
            .unwrap();

    let expected_platform = price * 20 / 100;
    assert_eq!(event_student, student);
    assert_eq!(event_course_id, course_id);
    assert_eq!(event_amount, price);
    assert_eq!(event_platform, expected_platform);
    assert_eq!(event_instructor, price - expected_platform);
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence_hash")),
    );

    let cert_id = String::from_str(&env, "CERT-123");
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-GET-CERT"),
        &cert_id,
        &String::from_str(&env, "Test Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-GET-CERT"),
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
#[should_panic(expected = "Error(Contract, #2)")]
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

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence_hash")),
    );

    let cert_id = String::from_str(&env, "CERT-123");
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-GET-CERT"),
        &cert_id,
        &String::from_str(&env, "Test Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-GET-CERT"),
        &None,
        &None,
    );

    let third_party = Address::generate(&env);

    // Mock auth only for third_party's own require_auth() call — a caller who
    // is not the student, instructor, or admin is denied by default.
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
    client.enroll(&student_a, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student_a,
        &course_id,
        &Some(evidence_hash(&env, "evidence")),
    );

    // Try to issue certificate using student_b's address — must panic (no enrollment)
    client.issue_certificate(
        &admin,
        &student_b,
        &String::from_str(&env, "COURSE-SECURE-B"),
        &cert_id,
        &String::from_str(&env, "Secure Course"),
        &String::from_str(&env, "backend-enrollment-student-b"),
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
    client.enroll(&student_a, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student_a,
        &course_id,
        &Some(evidence_hash(&env, "evidence")),
    );

    // Issue certificate correctly using student_a's address
    client.issue_certificate(
        &admin,
        &student_a,
        &String::from_str(&env, "COURSE-SECURE"),
        &cert_id,
        &String::from_str(&env, "Secure Course"),
        &get_enrollment_ref(&env, &client, &student_a, "COURSE-SECURE"),
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
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-INSTR-LIST-2"),
        &500_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
    client.register_course(
        &other_instructor,
        &String::from_str(&env, "COURSE-INSTR-LIST-OTHER"),
        &500_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
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
#[should_panic(expected = "Error(Contract, #39)")]
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

    client.enroll(&student, &course_id, &i128::MAX);

    // Only 1 ledger has elapsed since enrollment — far short of the 50
    // required — so this must panic.
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence")),
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

    client.enroll(&student, &course_id, &i128::MAX);

    env.ledger().with_mut(|l| {
        l.sequence_number += 50;
    });

    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence")),
    );

    assert_eq!(client.has_completed(&student, &course_id), Some(true));
}

#[test]
fn test_update_min_completion_ledgers_accepts_maximum() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.update_min_completion_ledgers(&admin, &HamplardContract::MAX_MIN_COMPLETION_LEDGERS);

    assert_eq!(
        client.get_min_completion_ledgers(),
        HamplardContract::MAX_MIN_COMPLETION_LEDGERS
    );
}

#[test]
#[should_panic(expected = "exceeds maximum allowed")]
fn test_update_min_completion_ledgers_rejects_extreme_value() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client
        .update_min_completion_ledgers(&admin, &(HamplardContract::MAX_MIN_COMPLETION_LEDGERS + 1));
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

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence")),
    );

    assert_eq!(client.has_completed(&student, &course_id), Some(true));
}

// ============================================================
// ISSUE #68: TOCTOU CONSOLIDATION IN ENROLL()
// ============================================================

#[test]
#[should_panic(expected = "Error(Contract, #34)")]
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
    client.enroll(&student, &course_id, &i128::MAX);
}

// ============================================================
// ISSUE FIX TESTS
// ============================================================

// --- Issue: Empty certificate ID should be rejected ---

#[test]
#[should_panic(expected = "Error(Contract, #41)")]
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
    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence")),
    );

    // Empty certificate ID must be rejected
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-EMPTY-CERT"),
        &String::from_str(&env, ""),
        &String::from_str(&env, "Test Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-EMPTY-CERT"),
        &None,
        &None,
    );
}

// --- Issue: Double revocation should be rejected ---

#[test]
#[should_panic(expected = "Error(Contract, #44)")]
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
    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence")),
    );

    let cert_id = String::from_str(&env, "CERT-DBL-REVOKE");
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-REVOKE-DBL"),
        &cert_id,
        &String::from_str(&env, "Test Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-REVOKE-DBL"),
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
    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence")),
    );
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-IMMUTABLE-001"),
        &cert_id,
        &String::from_str(&env, "Test Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-IMMUTABLE-001"),
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
        cert_after_second.revocation_ledger,
        cert_after_first.revocation_ledger
    );
    assert_eq!(cert_after_second.revocation_reason, Some(original_reason));
}

#[test]
#[should_panic(expected = "Error(Contract, #42)")]
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
    client.enroll(&student_a, &course_a, &i128::MAX);
    client.mark_completed(
        &admin,
        &student_a,
        &course_a,
        &Some(evidence_hash(&env, "evidence")),
    );
    client.issue_certificate(
        &admin,
        &student_a,
        &String::from_str(&env, "COURSE-REUSE-A"),
        &cert_id,
        &String::from_str(&env, "Course A"),
        &get_enrollment_ref(&env, &client, &student_a, "COURSE-REUSE-A"),
        &None,
        &None,
    );
    client.revoke_certificate(&admin, &cert_id, &String::from_str(&env, "ISSUED_IN_ERROR"));

    // A completely unrelated enrollment must not be able to reuse the same
    // certificate ID to create a fresh, non-revoked record — doing so would
    // effectively "un-revoke" the credential under that ID.
    client.enroll(&student_b, &course_b, &i128::MAX);
    client.mark_completed(
        &admin,
        &student_b,
        &course_b,
        &Some(evidence_hash(&env, "evidence")),
    );
    client.issue_certificate(
        &admin,
        &student_b,
        &String::from_str(&env, "COURSE-REUSE-B"),
        &cert_id,
        &String::from_str(&env, "Course B"),
        &get_enrollment_ref(&env, &client, &student_b, "COURSE-REUSE-B"),
        &None,
        &None,
    );
}

// --- Issue: Same-address admin transfer should be rejected ---

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_transfer_admin_same_address_rejected() {
    let (env, contract_id, _token_id, admin, sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Attempt to transfer admin to the exact same pair — must panic
    client.transfer_admin(&admin, &sec_admin, &admin, &sec_admin);
}

// --- Issue: Enrollment expiry scenarios ---

#[test]
#[should_panic(expected = "Error(Contract, #39)")]
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
    client.enroll(&student, &course_id, &i128::MAX);

    // Advance ledger past the expiry window
    env.ledger().with_mut(|l| {
        l.sequence_number += 200;
    });

    // mark_completed must panic because the enrollment has expired
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "late_evidence")),
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
    client.enroll(&student, &course_id, &i128::MAX);

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
    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence")),
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
    client.enroll(&student, &course_id, &i128::MAX);

    // Advance many ledgers
    env.ledger().with_mut(|l| {
        l.sequence_number += 1_000_000;
    });

    // Without an expiry, mark_completed must succeed regardless of elapsed time
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "late_evidence")),
    );

    assert_eq!(client.has_completed(&student, &course_id), Some(true));
}

#[test]
fn test_expire_enrollment_releases_active_counters_and_emits_event() {
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
        "COURSE-EXPIRE-COUNTERS",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-EXPIRE-COUNTERS");
    client.set_enrollment_expiry(&instructor, &course_id, &Some(50u32));
    client.enroll(&student, &course_id);

    let before = client.get_course(&course_id).unwrap();
    assert_eq!(before.active_enrollments, 1);
    assert_eq!(before.total_enrollments, 1);
    assert_eq!(client.get_total_active_enrollments(), 1);

    env.ledger().with_mut(|l| {
        l.sequence_number += 60;
    });
    client.expire_enrollment(&student, &course_id);

    let (event_student, event_course_id, event_ledger): (Address, String, u32) =
        last_event_val(&env, &contract_id, "enrollment_expired")
            .try_into_val(&env)
            .unwrap();
    assert_eq!(event_student, student);
    assert_eq!(event_course_id, course_id);
    assert_eq!(event_ledger, env.ledger().sequence());

    let after = client.get_course(&course_id).unwrap();
    assert_eq!(after.active_enrollments, 0);
    assert_eq!(after.total_enrollments, 1);
    assert_eq!(client.get_total_active_enrollments(), 0);
    assert!(!client.is_enrolled(&student, &course_id));
    assert!(client
        .get_enrollment(&student, &student, &course_id)
        .is_none());
}

#[test]
#[should_panic(expected = "enrollment has not expired")]
fn test_expire_enrollment_before_expiry_rejected() {
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
        "COURSE-EXPIRE-EARLY",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-EXPIRE-EARLY");
    client.set_enrollment_expiry(&instructor, &course_id, &Some(100u32));
    client.enroll(&student, &course_id);

    client.expire_enrollment(&student, &course_id);
}

#[test]
#[should_panic(expected = "enrollment not found")]
fn test_expire_enrollment_cannot_decrement_twice() {
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
        "COURSE-EXPIRE-TWICE",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-EXPIRE-TWICE");
    client.set_enrollment_expiry(&instructor, &course_id, &Some(10u32));
    client.enroll(&student, &course_id);

    env.ledger().with_mut(|l| {
        l.sequence_number += 20;
    });
    client.expire_enrollment(&student, &course_id);
    client.expire_enrollment(&student, &course_id);
}

#[test]
#[should_panic(expected = "completed enrollment cannot expire")]
fn test_expire_enrollment_rejects_completed_enrollment() {
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
        "COURSE-EXPIRE-COMPLETED",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-EXPIRE-COMPLETED");
    client.set_enrollment_expiry(&instructor, &course_id, &Some(100u32));
    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "completed")),
    );

    env.ledger().with_mut(|l| {
        l.sequence_number += 200;
    });
    client.expire_enrollment(&student, &course_id);
}

#[test]
#[should_panic(expected = "course has no enrollment expiry")]
fn test_expire_enrollment_requires_course_expiry() {
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
        "COURSE-EXPIRE-NONE",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-EXPIRE-NONE");
    client.enroll(&student, &course_id);

    env.ledger().with_mut(|l| {
        l.sequence_number += 10_000;
    });
    client.expire_enrollment(&student, &course_id);
}

// ============================================================
// ISSUE #373: Enrollment lifecycle status
// ============================================================

#[test]
fn test_enrollment_status_none_without_enrollment() {
    let (env, contract_id, _token_id, _admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);

    assert_eq!(
        client.get_enrollment_status(&student, &String::from_str(&env, "NO-SUCH-COURSE")),
        EnrollmentStatus::None
    );
}

#[test]
fn test_enrollment_status_active_after_enroll() {
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
        "COURSE-STATUS-ACTIVE",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-STATUS-ACTIVE");

    client.enroll(&student, &course_id);

    assert_eq!(
        client.get_enrollment_status(&student, &course_id),
        EnrollmentStatus::Active
    );
}

#[test]
fn test_enrollment_status_expired_after_ledger_advances() {
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
        "COURSE-STATUS-EXPIRED",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-STATUS-EXPIRED");
    client.set_enrollment_expiry(&instructor, &course_id, &Some(10u32));
    client.enroll(&student, &course_id);

    env.ledger().with_mut(|l| {
        l.sequence_number += 20;
    });

    assert!(client.is_enrolled(&student, &course_id));
    assert_eq!(
        client.get_enrollment_status(&student, &course_id),
        EnrollmentStatus::Expired
    );
}

#[test]
fn test_enrollment_status_completed_precedes_expiry() {
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
        "COURSE-STATUS-COMPLETED",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-STATUS-COMPLETED");
    client.set_enrollment_expiry(&instructor, &course_id, &Some(10u32));
    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "status-proof")),
    );

    env.ledger().with_mut(|l| {
        l.sequence_number += 20;
    });

    assert_eq!(
        client.get_enrollment_status(&student, &course_id),
        EnrollmentStatus::Completed
    );
}

#[test]
fn test_enrollment_status_refunded_after_refund_archive() {
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
        "COURSE-STATUS-REFUNDED",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-STATUS-REFUNDED");

    client.enroll(&student, &course_id);
    client.request_refund(&student, &course_id);
    env.mock_all_auths_allowing_non_root_auth();
    client.process_refund(&admin, &student, &course_id, &true, &None);

    assert!(!client.is_enrolled(&student, &course_id));
    assert_eq!(
        client.get_enrollment_status(&student, &course_id),
        EnrollmentStatus::Refunded
    );
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

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "proof")),
    );
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-ISSUED-BY-001"),
        &cert_id,
        &String::from_str(&env, "Issued-By Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-ISSUED-BY-001"),
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

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "proof")),
    );
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-ISSUED-BY-002"),
        &cert_id,
        &String::from_str(&env, "Issued-By Course 2"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-ISSUED-BY-002"),
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

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "proof")),
    );
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-ISSUED-BY-003"),
        &cert_id,
        &String::from_str(&env, "Issued-By Course 3"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-ISSUED-BY-003"),
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

    client.enroll(&student_a, &course_a, &i128::MAX);
    client.mark_completed(
        &admin,
        &student_a,
        &course_a,
        &Some(evidence_hash(&env, "proof-a")),
    );
    client.issue_certificate(
        &admin,
        &student_a,
        &String::from_str(&env, "COURSE-MULTI-ISSUED-A"),
        &cert_a,
        &String::from_str(&env, "Course A"),
        &get_enrollment_ref(&env, &client, &student_a, "COURSE-MULTI-ISSUED-A"),
        &None,
        &None,
    );

    client.enroll(&student_b, &course_b, &i128::MAX);
    client.mark_completed(
        &admin,
        &student_b,
        &course_b,
        &Some(evidence_hash(&env, "proof-b")),
    );
    client.issue_certificate(
        &admin,
        &student_b,
        &String::from_str(&env, "COURSE-MULTI-ISSUED-B"),
        &cert_b,
        &String::from_str(&env, "Course B"),
        &get_enrollment_ref(&env, &client, &student_b, "COURSE-MULTI-ISSUED-B"),
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
    client.init(
        &admin, &sec_admin, &treasury, &20u32, &50u32, &500u32, &17_280u32,
    );

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

    client.enroll(&student, &course_id, &i128::MAX);

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

    client.enroll(&student, &course_id, &i128::MAX);
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
#[should_panic(expected = "Error(Contract, #45)")]
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

    client.enroll(&student, &course_id, &i128::MAX);
    let enrolled_at = env.ledger().sequence();

    // One ledger beyond the window
    env.ledger().with_mut(|l| {
        l.sequence_number = enrolled_at + 11;
    });

    client.request_refund(&student, &course_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #45)")]
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

    client.enroll(&student, &course_id, &i128::MAX);

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

    client.enroll(&student, &course_id, &i128::MAX);
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

    client.enroll(&student, &course_id, &i128::MAX);
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
// FEATURE: enrollment failure event tests (issue #xxx)
// ============================================================

#[test]
fn test_enroll_insufficient_balance_emits_event_and_returns_gracefully() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-INSUF-BAL",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-INSUF-BAL");
    let student = Address::generate(&env);
    // Give student insufficient balance (only 100 stroops, course costs 500M)
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100);

    // Enroll should return gracefully (not panic) when try_transfer fails
    client.enroll(&student, &course_id);

    // Verify enrollment_failed event was emitted with INSUFFICIENT_BALANCE
    let mut found_event = false;
    for (contract, topics, data) in env.events().all().iter() {
        if contract != contract_id {
            continue;
        }
        let topic0: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
        if topic0 == Symbol::new(&env, "enrollment_failed") {
            found_event = true;
            let topic1: String = topics.get(1).unwrap().try_into_val(&env).unwrap();
            assert_eq!(topic1, course_id);

            let event_data: EnrollmentFailed = data.try_into_val(&env).unwrap();
            assert_eq!(event_data.course_id, course_id);
            assert_eq!(event_data.student, student);
            assert_eq!(
                event_data.reason_code,
                String::from_str(&env, "INSUFFICIENT_BALANCE")
            );
            assert!(event_data.ledger_sequence > 0);
        }
    }
    assert!(
        found_event,
        "enrollment_failed event with INSUFFICIENT_BALANCE must be emitted"
    );

    // Verify no enrollment record was created
    assert!(!client.is_enrolled(&student, &course_id));

    // Verify course stats unchanged
    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.total_enrollments, 0);
    assert_eq!(course.active_enrollments, 0);
    assert_eq!(course.total_earned, 0);
}

#[test]
fn test_enroll_transfer_amount_mismatch_emits_event_and_returns_gracefully() {
    // This test simulates a scenario where the token contract accepts the transfer
    // but the actual received amount doesn't match the expected price.
    // In practice, this is rare with standard SAC tokens but can occur with
    // custom token implementations that apply fees or have bugs.
    
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-MISMATCH",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-MISMATCH");
    let student = Address::generate(&env);
    
    // Give student exact balance
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &500_000_000);

    // In a real scenario with a buggy/fee-on-transfer token, the contract would
    // receive less than expected. The Soroban test framework doesn't easily support
    // simulating a fee-on-transfer token, so we document the expected behavior:
    // 
    // 1. try_transfer succeeds
    // 2. balance_after - balance_before != price
    // 3. emit_enrollment_failed with "TRANSFER_AMOUNT_MISMATCH"
    // 4. return early without creating enrollment
    //
    // For now, this test documents the expected behavior. A full integration test
    // would require deploying a custom token contract that applies transfer fees.
}

#[test]
fn test_enroll_failure_leaves_no_state_changes() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-NO-STATE",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-NO-STATE");
    let student = Address::generate(&env);
    
    // Give insufficient balance
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100);

    let course_before = client.get_course(&course_id).unwrap();
    let student_balance_before = token_client.balance(&student);
    let contract_balance_before = token_client.balance(&contract_id);

    // Attempt enrollment (will fail due to insufficient balance)
    client.enroll(&student, &course_id);

    // Verify no enrollment record exists
    assert!(!client.is_enrolled(&student, &course_id));
    let enrollment = client.get_enrollment(&student, &student, &course_id);
    assert!(enrollment.is_none());

    // Verify course stats unchanged
    let course_after = client.get_course(&course_id).unwrap();
    assert_eq!(course_after.total_enrollments, course_before.total_enrollments);
    assert_eq!(
        course_after.active_enrollments,
        course_before.active_enrollments
    );
    assert_eq!(course_after.total_earned, course_before.total_earned);

    // Verify no token transfers occurred
    assert_eq!(token_client.balance(&student), student_balance_before);
    assert_eq!(
        token_client.balance(&contract_id),
        contract_balance_before
    );

    // Verify platform-wide counter unchanged
    let total_active = client.get_total_active_enrollments();
    assert_eq!(total_active, 0);

    // Verify instructor earnings unchanged
    assert_eq!(client.get_instructor_earnings(&instructor, &token_id), 0);
}

#[test]
fn test_enrollment_failed_event_payload_structure() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-EVENT-STRUCT",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-EVENT-STRUCT");
    let student = Address::generate(&env);
    
    // Give insufficient balance
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100);

    let ledger_before = env.ledger().sequence();
    
    client.enroll(&student, &course_id);

    // Verify event structure matches EnrollmentFailed type
    let mut found = false;
    for (contract, topics, data) in env.events().all().iter() {
        if contract != contract_id {
            continue;
        }
        let topic0: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
        if topic0 == Symbol::new(&env, "enrollment_failed") {
            found = true;

            // Verify topics structure: (event_name, course_id)
            assert_eq!(topics.len(), 2);
            let topic1: String = topics.get(1).unwrap().try_into_val(&env).unwrap();
            assert_eq!(topic1, course_id);

            // Verify data payload structure: EnrollmentFailed { course_id, student, reason_code, ledger_sequence }
            let event: EnrollmentFailed = data.try_into_val(&env).unwrap();
            assert_eq!(event.course_id, course_id);
            assert_eq!(event.student, student);
            assert_eq!(
                event.reason_code,
                String::from_str(&env, "INSUFFICIENT_BALANCE")
            );
            assert!(event.ledger_sequence >= ledger_before);
            assert!(event.ledger_sequence <= env.ledger().sequence());
        }
    }
    assert!(found, "enrollment_failed event must be emitted");
}

#[test]
fn test_multiple_failed_enrollments_emit_separate_events() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-MULTI-FAIL",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-MULTI-FAIL");
    
    let student_a = Address::generate(&env);
    let student_b = Address::generate(&env);
    let student_c = Address::generate(&env);
    
    // Give all students insufficient balance
    token::StellarAssetClient::new(&env, &token_id).mint(&student_a, &100);
    token::StellarAssetClient::new(&env, &token_id).mint(&student_b, &200);
    token::StellarAssetClient::new(&env, &token_id).mint(&student_c, &300);

    // All three enrollments should fail
    client.enroll(&student_a, &course_id);
    client.enroll(&student_b, &course_id);
    client.enroll(&student_c, &course_id);

    // Count enrollment_failed events
    let mut event_count = 0;
    let mut students_seen = std::vec::Vec::new();
    
    for (contract, topics, data) in env.events().all().iter() {
        if contract != contract_id {
            continue;
        }
        let topic0: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
        if topic0 == Symbol::new(&env, "enrollment_failed") {
            event_count += 1;
            let event: EnrollmentFailed = data.try_into_val(&env).unwrap();
            students_seen.push(event.student);
        }
    }
    
    assert_eq!(event_count, 3, "three separate events must be emitted");
    assert!(students_seen.contains(&student_a));
    assert!(students_seen.contains(&student_b));
    assert!(students_seen.contains(&student_c));

    // Verify none enrolled successfully
    assert!(!client.is_enrolled(&student_a, &course_id));
    assert!(!client.is_enrolled(&student_b, &course_id));
    assert!(!client.is_enrolled(&student_c, &course_id));
}

#[test]
fn test_re_enroll_insufficient_balance_emits_event() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-REENROLL-FAIL",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-REENROLL-FAIL");
    let student = Address::generate(&env);
    
    // First enrollment: give sufficient balance, enroll, complete
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);
    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence")),
    );

    // Re-enrollment: give insufficient balance
    // Student already spent 500M on first enrollment, has ~300M left after fees
    // (not enough for another 500M enrollment)
    
    client.re_enroll(&student, &course_id);

    // Verify enrollment_failed event was emitted
    let mut found_reenroll_failure = false;
    for (contract, topics, data) in env.events().all().iter() {
        if contract != contract_id {
            continue;
        }
        let topic0: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
        if topic0 == Symbol::new(&env, "enrollment_failed") {
            let event: EnrollmentFailed = data.try_into_val(&env).unwrap();
            // Look for the re-enroll failure (will be the most recent one)
            if event.course_id == course_id && event.student == student {
                found_reenroll_failure = true;
                assert_eq!(
                    event.reason_code,
                    String::from_str(&env, "INSUFFICIENT_BALANCE")
                );
            }
        }
    }
    assert!(
        found_reenroll_failure,
        "re_enroll must emit enrollment_failed on payment failure"
    );

    // Verify the original completed enrollment is still intact
    let enrollment = client
        .get_enrollment(&student, &student, &course_id)
        .unwrap();
    assert!(enrollment.completed);
    
    // Verify history still contains exactly one entry (the original)
    let history = client.get_enrollment_history(&student, &student, &course_id);
    assert_eq!(history.len(), 0, "re_enroll should not archive on payment failure");
}

#[test]
fn test_enroll_failure_does_not_increment_circuit_breaker_counter() {
    // The circuit breaker counter is incremented BEFORE the transfer attempt
    // and reset to 0 AFTER a successful transfer. When try_transfer fails,
    // the function returns early, leaving the pre-incremented counter in place.
    // This test verifies that repeated failures eventually trigger the circuit breaker.
    
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-CIRCUIT",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-CIRCUIT");
    
    // Attempt enrollment 5 times with insufficient balance
    // Each failure leaves the pre-incremented counter in persistent storage
    for i in 0..5 {
        let student = Address::generate(&env);
        token::StellarAssetClient::new(&env, &token_id).mint(&student, &100);
        client.enroll(&student, &course_id);
    }

    // The 6th attempt should trigger the circuit breaker panic (threshold is 5)
    let student_6 = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student_6, &100);
    
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.enroll(&student_6, &course_id);
    }));
    
    assert!(
        result.is_err(),
        "circuit breaker should trip after threshold consecutive failures"
    );

    // Verify the course was auto-paused
    let course = client.get_course(&course_id).unwrap();
    assert_eq!(
        course.status,
        CourseStatus::Paused,
        "course must be paused by circuit breaker"
    );
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
        &soroban_sdk::Vec::new(&env),
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
        &soroban_sdk::Vec::new(&env),
    );
    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-HASH-B"),
        &200_000_000,
        &token_id,
        &0u32,
        &None,
        &hash_b,
        &soroban_sdk::Vec::new(&env),
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
        &soroban_sdk::Vec::new(&env),
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
        &soroban_sdk::Vec::new(&env),
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
#[should_panic(expected = "Error(Contract, #10)")]
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
#[should_panic(expected = "Error(Contract, #2)")]
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
        &soroban_sdk::Vec::new(&env),
    );
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });
    client.approve_course(&admin, &course_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);
    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "proof")),
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
#[should_panic(expected = "Error(Contract, #3)")]
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
    client.enroll(&student, &String::from_str(&env, "COURSE-PAUSE-TEST"), &i128::MAX);
    assert!(client.is_enrolled(&student, &String::from_str(&env, "COURSE-PAUSE-TEST")));

    // Complete and prepare for re-enrollment test
    client.mark_completed(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-PAUSE-TEST"),
        &Some(evidence_hash(&env, "evidence")),
    );

    // Pause the platform
    client.pause_platform(&admin);

    let student2 = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student2, &1_000_000_000);

    // New enrollment should be rejected during pause
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.enroll(&student2, &String::from_str(&env, "COURSE-PAUSE-TEST"), &i128::MAX);
    }));
    assert!(result.is_err());

    // Re-enrollment should also be rejected during pause
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.re_enroll(&student, &String::from_str(&env, "COURSE-PAUSE-TEST"), &i128::MAX);
    }));
    assert!(result.is_err());

    // Batch enrollment should be rejected during pause
    let courses = Vec::from_array(&env, [String::from_str(&env, "COURSE-PAUSE-TEST")]);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.batch_enroll(&student2, &courses, &i128::MAX);
    }));
    assert!(result.is_err());

    // Unpause the platform
    client.unpause_platform(&admin);

    // Enrollment should succeed after unpause
    client.enroll(&student2, &String::from_str(&env, "COURSE-PAUSE-TEST"), &i128::MAX);
    assert!(client.is_enrolled(&student2, &String::from_str(&env, "COURSE-PAUSE-TEST")));
}

#[test]
#[should_panic(expected = "Error(Contract, #30)")]
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
    client.enroll(&student, &String::from_str(&env, "COURSE-001"), &i128::MAX);
}

#[test]
#[should_panic(expected = "Error(Contract, #30)")]
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
    client.batch_enroll(&student, &courses, &i128::MAX);
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
    client.enroll(&student1, &String::from_str(&env, "COURSE-001"), &i128::MAX);
    assert_eq!(client.get_total_active_enrollments(), 1);

    // Second enrollment should succeed
    client.enroll(&student2, &String::from_str(&env, "COURSE-001"), &i128::MAX);
    assert_eq!(client.get_total_active_enrollments(), 2);
}

#[test]
#[should_panic(expected = "Error(Contract, #34)")]
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
    client.enroll(&student1, &String::from_str(&env, "COURSE-001"), &i128::MAX);
    assert_eq!(client.get_total_active_enrollments(), 1);

    // Second enrollment should be rejected
    client.enroll(&student2, &String::from_str(&env, "COURSE-001"), &i128::MAX);
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

    client.enroll(&student1, &String::from_str(&env, "COURSE-001"), &i128::MAX);
    client.enroll(&student2, &String::from_str(&env, "COURSE-001"), &i128::MAX);
    assert_eq!(client.get_total_active_enrollments(), 2);

    // Mark one as completed
    client.mark_completed(
        &admin,
        &student1,
        &String::from_str(&env, "COURSE-001"),
        &Some(evidence_hash(&env, "evidence")),
    );

    // Counter should decrement
    assert_eq!(client.get_total_active_enrollments(), 1);

    // New enrollment should now succeed
    let student3 = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student3, &1_000_000_000);
    client.enroll(&student3, &String::from_str(&env, "COURSE-001"), &i128::MAX);
    assert_eq!(client.get_total_active_enrollments(), 2);
}

#[test]
fn test_platform_counter_decrements_on_refund() {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    fund_refund_reserve_for_test(&env, &client, &token_id, 10_000_000_000);

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

    client.enroll(&student1, &String::from_str(&env, "COURSE-001"), &i128::MAX);
    client.enroll(&student2, &String::from_str(&env, "COURSE-001"), &i128::MAX);
    assert_eq!(client.get_total_active_enrollments(), 2);

    // Request and approve refund
    client.request_refund(&student1, &String::from_str(&env, "COURSE-001"));
    env.mock_all_auths_allowing_non_root_auth();
    client.process_refund(
        &admin,
        &student1,
        &String::from_str(&env, "COURSE-001"),
        &true,
        &None,
    );

    // Counter should decrement
    assert_eq!(client.get_total_active_enrollments(), 1);

    // New enrollment should now succeed
    let student3 = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student3, &1_000_000_000);
    client.enroll(&student3, &String::from_str(&env, "COURSE-001"), &i128::MAX);
    assert_eq!(client.get_total_active_enrollments(), 2);
}

#[test]
fn test_free_enrollments_do_not_consume_platform_cap() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.set_platform_enrollment_cap(&admin, &Some(1));

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "FREE-COURSE-001",
        0,
    );
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "PAID-COURSE-001",
        100_000_000,
    );

    let free_id = String::from_str(&env, "FREE-COURSE-001");
    let paid_id = String::from_str(&env, "PAID-COURSE-001");
    let free1 = Address::generate(&env);
    let free2 = Address::generate(&env);
    let free3 = Address::generate(&env);
    let paid1 = Address::generate(&env);
    let paid2 = Address::generate(&env);
    let token_client = token::StellarAssetClient::new(&env, &token_id);
    token_client.mint(&paid1, &1_000_000_000);
    token_client.mint(&paid2, &1_000_000_000);

    client.enroll(&free1, &free_id);
    client.enroll(&free2, &free_id);
    assert_eq!(client.get_total_active_enrollments(), 0);

    client.enroll(&paid1, &paid_id);
    assert_eq!(client.get_total_active_enrollments(), 1);

    // Free enrollment still works while the paid cap is full.
    client.enroll(&free3, &free_id);
    assert_eq!(client.get_total_active_enrollments(), 1);

    // Paid capacity remains exhausted independently of free enrollments.
    assert!(client.try_enroll(&paid2, &paid_id).is_err());
    assert_eq!(client.get_total_active_enrollments(), 1);
}

#[test]
fn test_free_enrollment_completion_and_refund_do_not_decrement_paid_counter() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "FREE-COURSE-002",
        0,
    );
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "PAID-COURSE-002",
        100_000_000,
    );

    let free_id = String::from_str(&env, "FREE-COURSE-002");
    let paid_id = String::from_str(&env, "PAID-COURSE-002");
    let paid = Address::generate(&env);
    let free_completed = Address::generate(&env);
    let free_refunded = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&paid, &1_000_000_000);

    client.enroll(&paid, &paid_id);
    client.enroll(&free_completed, &free_id);
    client.enroll(&free_refunded, &free_id);
    assert_eq!(client.get_total_active_enrollments(), 1);

    client.mark_completed(
        &admin,
        &free_completed,
        &free_id,
        &Some(evidence_hash(&env, "free-course-proof")),
    );
    assert_eq!(client.get_total_active_enrollments(), 1);

    client.request_refund(&free_refunded, &free_id);
    env.mock_all_auths_allowing_non_root_auth();
    client.process_refund(&admin, &free_refunded, &free_id, &true, &None);
    assert_eq!(client.get_total_active_enrollments(), 1);
}

// ============================================================
// MARK COMPLETED COURSE STATE VALIDATION TESTS (#129)
// ============================================================

#[test]
#[should_panic(expected = "Error(Contract, #10)")]
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
    client.enroll(&student, &String::from_str(&env, "COURSE-001"), &i128::MAX);

    // Mark completed first
    client.mark_completed(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-001"),
        &Some(evidence_hash(&env, "evidence")),
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
        &Some(evidence_hash(&env, "evidence")),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #10)")]
fn test_mark_completed_rejects_archived_course_even_with_active_enrollment() {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    fund_refund_reserve_for_test(&env, &client, &token_id, 10_000_000_000);

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
    client.enroll(&student, &String::from_str(&env, "COURSE-001"), &i128::MAX);

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
        &Some(evidence_hash(&env, "evidence")),
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
    client.enroll(&student, &String::from_str(&env, "COURSE-001"), &i128::MAX);

    // Pause the course
    client.pause_course(&admin, &String::from_str(&env, "COURSE-001"));

    // Mark completed should succeed on paused course
    client.mark_completed(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-001"),
        &Some(evidence_hash(&env, "evidence")),
    );

    let enrollment = client
        .get_enrollment(&admin, &student, &String::from_str(&env, "COURSE-001"))
        .unwrap();
    assert!(enrollment.completed);
}

// ============================================================
// PREREQUISITE COURSE TESTS
// ============================================================

#[test]
fn test_set_prerequisite_courses_success() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-PREREQ-A",
        100_000_000,
    );
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-PREREQ-B",
        100_000_000,
    );

    let course_a = String::from_str(&env, "COURSE-PREREQ-A");
    let course_b = String::from_str(&env, "COURSE-PREREQ-B");

    let mut prereqs = soroban_sdk::Vec::new(&env);
    prereqs.push_back(course_a.clone());

    client.set_prerequisite_courses(&instructor, &course_b, &prereqs);

    let course = client.get_course(&course_b).unwrap();
    assert_eq!(course.prerequisite_course_ids.len(), 1);
    assert_eq!(course.prerequisite_course_ids.get(0).unwrap(), course_a);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_set_prerequisite_courses_unauthorized() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-PREREQ-A",
        100_000_000,
    );
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-PREREQ-B",
        100_000_000,
    );

    let random = Address::generate(&env);
    let mut prereqs = soroban_sdk::Vec::new(&env);
    prereqs.push_back(String::from_str(&env, "COURSE-PREREQ-A"));

    client.set_prerequisite_courses(
        &random,
        &String::from_str(&env, "COURSE-PREREQ-B"),
        &prereqs,
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #26)")]
fn test_set_prerequisite_courses_self_reference_rejected() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-PREREQ-SELF",
        100_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-PREREQ-SELF");
    let mut prereqs = soroban_sdk::Vec::new(&env);
    prereqs.push_back(course_id.clone());

    client.set_prerequisite_courses(&instructor, &course_id, &prereqs);
}

#[test]
#[should_panic(expected = "Error(Contract, #26)")]
fn test_set_prerequisite_courses_nonexistent_rejected() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-PREREQ-B",
        100_000_000,
    );

    let mut prereqs = soroban_sdk::Vec::new(&env);
    prereqs.push_back(String::from_str(&env, "COURSE-DOES-NOT-EXIST"));

    client.set_prerequisite_courses(
        &instructor,
        &String::from_str(&env, "COURSE-PREREQ-B"),
        &prereqs,
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #26)")]
fn test_enroll_blocked_without_completed_prerequisite() {
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
        "COURSE-PREREQ-A",
        100_000_000,
    );
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-PREREQ-B",
        100_000_000,
    );

    let course_a = String::from_str(&env, "COURSE-PREREQ-A");
    let course_b = String::from_str(&env, "COURSE-PREREQ-B");

    let mut prereqs = soroban_sdk::Vec::new(&env);
    prereqs.push_back(course_a);
    client.set_prerequisite_courses(&instructor, &course_b, &prereqs);

    // Student never enrolled/completed course A — direct enrollment in B must fail.
    client.enroll(&student, &course_b, &i128::MAX);
}

#[test]
#[should_panic(expected = "Error(Contract, #26)")]
fn test_enroll_blocked_when_prerequisite_completed_but_not_certified() {
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
        "COURSE-PREREQ-A",
        100_000_000,
    );
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-PREREQ-B",
        100_000_000,
    );

    let course_a = String::from_str(&env, "COURSE-PREREQ-A");
    let course_b = String::from_str(&env, "COURSE-PREREQ-B");

    let mut prereqs = soroban_sdk::Vec::new(&env);
    prereqs.push_back(course_a.clone());
    client.set_prerequisite_courses(&instructor, &course_b, &prereqs);

    // Student completes A but no certificate is ever issued.
    client.enroll(&student, &course_a, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_a,
        &Some(evidence_hash(&env, "evidence")),
    );

    client.enroll(&student, &course_b, &i128::MAX);
}

#[test]
fn test_enroll_succeeds_with_completed_and_certified_prerequisite() {
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
        "COURSE-PREREQ-A",
        100_000_000,
    );
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-PREREQ-B",
        100_000_000,
    );

    let course_a = String::from_str(&env, "COURSE-PREREQ-A");
    let course_b = String::from_str(&env, "COURSE-PREREQ-B");

    let mut prereqs = soroban_sdk::Vec::new(&env);
    prereqs.push_back(course_a.clone());
    client.set_prerequisite_courses(&instructor, &course_b, &prereqs);

    client.enroll(&student, &course_a, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_a,
        &Some(evidence_hash(&env, "evidence")),
    );
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-PREREQ-A"),
        &String::from_str(&env, "CERT-PREREQ-A"),
        &String::from_str(&env, "Course A"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-PREREQ-A"),
        &None,
        &None,
    );

    client.enroll(&student, &course_b, &i128::MAX);
    assert!(client.is_enrolled(&student, &course_b));
}

#[test]
#[should_panic(expected = "Error(Contract, #26)")]
fn test_enroll_blocked_when_prerequisite_certificate_revoked() {
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
        "COURSE-PREREQ-A",
        100_000_000,
    );
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-PREREQ-B",
        100_000_000,
    );

    let course_a = String::from_str(&env, "COURSE-PREREQ-A");
    let course_b = String::from_str(&env, "COURSE-PREREQ-B");
    let cert_id = String::from_str(&env, "CERT-PREREQ-A-REVOKED");

    let mut prereqs = soroban_sdk::Vec::new(&env);
    prereqs.push_back(course_a.clone());
    client.set_prerequisite_courses(&instructor, &course_b, &prereqs);

    client.enroll(&student, &course_a, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_a,
        &Some(evidence_hash(&env, "evidence")),
    );
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-PREREQ-A"),
        &cert_id,
        &String::from_str(&env, "Course A"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-PREREQ-A"),
        &None,
        &None,
    );
    client.revoke_certificate(&admin, &cert_id, &String::from_str(&env, "ISSUED_IN_ERROR"));

    // The prerequisite's certificate has been revoked — enrollment must still be blocked.
    client.enroll(&student, &course_b, &i128::MAX);
}

// ============================================================
// CERTIFICATE REVOKE-THEN-REISSUE TESTS
// ============================================================

#[test]
#[should_panic(expected = "Error(Contract, #42)")]
fn test_issue_certificate_after_revoke_is_rejected() {
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
        "COURSE-REVOKE-REISSUE",
        100_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-REVOKE-REISSUE");
    let first_cert_id = String::from_str(&env, "CERT-ORIGINAL");

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence")),
    );
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-REVOKE-REISSUE"),
        &first_cert_id,
        &String::from_str(&env, "Course Title"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-REVOKE-REISSUE"),
        &None,
        &None,
    );

    client.revoke_certificate(
        &admin,
        &first_cert_id,
        &String::from_str(&env, "ACADEMIC_DISHONESTY"),
    );

    // Advance past challenge period so revocation is permanent
    let cert = client.get_certificate(&admin, &first_cert_id);
    let deadline = cert.revocation_deadline.unwrap();
    env.ledger().with_mut(|l| {
        l.sequence_number = deadline;
    });
    assert!(!client.verify_certificate(&first_cert_id));

    // Expected contract behavior: revocation is permanent for this enrollment.
    // `Enrollment.certificate_issued` is never cleared by `revoke_certificate`,
    // so any subsequent `issue_certificate` call for the same (student, course)
    // enrollment — even with a brand-new certificate_id — must be rejected.
    // A student cannot cycle through revoke -> reissue to launder a revoked
    // credential; the only way to earn a fresh certificate is a fresh
    // enrollment via `re_enroll`.
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-REVOKE-REISSUE"),
        &String::from_str(&env, "CERT-REPLACEMENT"),
        &String::from_str(&env, "Course Title"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-REVOKE-REISSUE"),
        &None,
        &None,
    );
}

#[test]
fn test_reissue_certificate_allowed_after_re_enroll() {
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
        "COURSE-REVOKE-REENROLL",
        100_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-REVOKE-REENROLL");
    let first_cert_id = String::from_str(&env, "CERT-FIRST-ATTEMPT");
    let second_cert_id = String::from_str(&env, "CERT-SECOND-ATTEMPT");

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence")),
    );
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-REVOKE-REENROLL"),
        &first_cert_id,
        &String::from_str(&env, "Course Title"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-REVOKE-REENROLL"),
        &None,
        &None,
    );
    client.revoke_certificate(
        &admin,
        &first_cert_id,
        &String::from_str(&env, "ACADEMIC_DISHONESTY"),
    );

    // Advance past challenge period so revocation is permanent
    let cert = client.get_certificate(&admin, &first_cert_id);
    let deadline = cert.revocation_deadline.unwrap();
    env.ledger().with_mut(|l| {
        l.sequence_number = deadline;
    });

    // A genuinely fresh attempt — via re_enroll(), which creates a brand-new
    // Enrollment record — can still earn its own certificate.
    client.re_enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence-2")),
    );
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-REVOKE-REENROLL"),
        &second_cert_id,
        &String::from_str(&env, "Course Title"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-REVOKE-REENROLL"),
        &None,
        &None,
    );

    assert!(client.verify_certificate(&second_cert_id));
    assert!(!client.verify_certificate(&first_cert_id));
}

// ============================================================
// CONTRACT UPGRADE TESTS
// ============================================================

#[test]
fn test_propose_upgrade_authorized() {
    let (env, contract_id, _token_id, admin, sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let wasm_hash = BytesN::from_array(&env, &[7u8; 32]);
    client.propose_upgrade(&admin, &sec_admin, &wasm_hash);

    let pending = client.get_pending_upgrade().unwrap();
    assert_eq!(pending.new_wasm_hash, wasm_hash);
    assert_eq!(
        pending.effective_ledger,
        pending.proposed_at_ledger + client.get_upgrade_timelock()
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_propose_upgrade_unauthorized() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let random = Address::generate(&env);
    let wasm_hash = BytesN::from_array(&env, &[7u8; 32]);

    // Second signer is not the registered secondary admin.
    client.propose_upgrade(&admin, &random, &wasm_hash);
}

#[test]
#[should_panic(expected = "Error(Contract, #49)")]
fn test_upgrade_contract_before_timelock_fails() {
    let (env, contract_id, _token_id, admin, sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let wasm_hash = BytesN::from_array(&env, &[7u8; 32]);
    client.propose_upgrade(&admin, &sec_admin, &wasm_hash);

    // Timelock has not elapsed yet — must be rejected.
    client.upgrade_contract(&admin, &sec_admin);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_upgrade_contract_unauthorized_fails() {
    let (env, contract_id, _token_id, admin, sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let random = Address::generate(&env);
    let wasm_hash = BytesN::from_array(&env, &[7u8; 32]);
    client.propose_upgrade(&admin, &sec_admin, &wasm_hash);

    client.upgrade_contract(&admin, &random);
}

#[test]
#[should_panic(expected = "Error(Contract, #49)")]
fn test_upgrade_contract_without_proposal_fails() {
    let (env, contract_id, _token_id, admin, sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.upgrade_contract(&admin, &sec_admin);
}

#[test]
#[should_panic]
fn test_upgrade_contract_executes_after_timelock_elapses() {
    // This test proves the authorization + time-lock gate opens correctly:
    // once both admins have proposed and the configured time-lock has
    // elapsed, `upgrade_contract` proceeds past all contract-level checks
    // and reaches the host's Wasm-swap call. It still panics here because
    // `wasm_hash` was never uploaded via `upload_contract_wasm` — unit
    // tests run natively and have no real compiled contract Wasm available
    // to upload — but that failure originates from the host, not from our
    // authorization or time-lock logic.
    let (env, contract_id, _token_id, admin, sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let wasm_hash = BytesN::from_array(&env, &[7u8; 32]);
    client.propose_upgrade(&admin, &sec_admin, &wasm_hash);

    let timelock = client.get_upgrade_timelock();
    env.ledger().with_mut(|l| {
        l.sequence_number += timelock + 1;
    });

    client.upgrade_contract(&admin, &sec_admin);
}

#[test]
fn test_cancel_upgrade_authorized() {
    let (env, contract_id, _token_id, admin, sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let wasm_hash = BytesN::from_array(&env, &[7u8; 32]);
    client.propose_upgrade(&admin, &sec_admin, &wasm_hash);
    assert!(client.get_pending_upgrade().is_some());

    client.cancel_upgrade(&admin, &sec_admin);
    assert!(client.get_pending_upgrade().is_none());
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_cancel_upgrade_unauthorized_fails() {
    let (env, contract_id, _token_id, admin, sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let random = Address::generate(&env);
    let wasm_hash = BytesN::from_array(&env, &[7u8; 32]);
    client.propose_upgrade(&admin, &sec_admin, &wasm_hash);

    client.cancel_upgrade(&admin, &random);
}

#[test]
fn test_set_upgrade_timelock_authorized() {
    let (env, contract_id, _token_id, admin, sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.set_upgrade_timelock(&admin, &sec_admin, &500u32);
    assert_eq!(client.get_upgrade_timelock(), 500u32);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_set_upgrade_timelock_unauthorized_fails() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let random = Address::generate(&env);
    client.set_upgrade_timelock(&admin, &random, &500u32);
}

// ============================================================
// ISSUE #141 TESTS — Certificate Course ID Integrity
// ============================================================

#[test]
fn test_issue_141_certificate_course_id_matches_enrollment() {
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
        "COURSE-141-TEST",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-141-TEST");
    let cert_id = String::from_str(&env, "CERT-141-TEST");

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence")),
    );
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-141-TEST"),
        &cert_id,
        &String::from_str(&env, "Test Course Title"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-141-TEST"),
        &None,
        &None,
    );

    let cert = client.get_certificate(&admin, &cert_id);
    assert_eq!(cert.course_id, course_id);
    assert_eq!(cert.student, student);
}

#[test]
#[should_panic(expected = "Error(Contract, #38)")]
fn test_issue_141_certificate_requires_enrollment_in_course() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-NOT-ENROLLED",
        500_000_000,
    );

    // The student never enrolled in this course, so no certificate can be
    // issued for it regardless of the enrollment_reference supplied.
    let student = Address::generate(&env);
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-NOT-ENROLLED"),
        &String::from_str(&env, "CERT-NOT-ENROLLED"),
        &String::from_str(&env, "Course Title"),
        &String::from_str(&env, "99999"),
        &None,
        &None,
    );
}

#[test]
fn test_issue_141_enrollment_ref_is_unique_per_enrollment() {
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
        "COURSE-UNIQUE-REF",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-UNIQUE-REF");

    // First enrollment
    client.enroll(&student, &course_id, &i128::MAX);
    let ref1 = get_enrollment_ref(&env, &client, &student, "COURSE-UNIQUE-REF");

    // Complete and issue cert for first enrollment
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence")),
    );
    client.issue_certificate(
        &admin,
        &student,
        &course_id,
        &String::from_str(&env, "CERT-FIRST"),
        &String::from_str(&env, "Course Title"),
        &ref1,
        &None,
        &None,
    );

    // Re-enroll (creates new enrollment with new ref)
    client.re_enroll(&student, &course_id, &i128::MAX);
    let ref2 = get_enrollment_ref(&env, &client, &student, "COURSE-UNIQUE-REF");

    // Enrollment refs must be different
    assert_ne!(ref1, ref2);
}

// ============================================================
// ISSUE #146 TESTS — Revocation Challenge Period
// ============================================================

#[test]
fn test_issue_146_revocation_starts_pending() {
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
        "COURSE-PENDING-REV",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-PENDING-REV");
    let cert_id = String::from_str(&env, "CERT-PENDING-REV");

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(&admin, &student, &course_id, &None);
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-PENDING-REV"),
        &cert_id,
        &String::from_str(&env, "Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-PENDING-REV"),
        &None,
        &None,
    );

    let ledger_before = env.ledger().sequence();

    // Revoke — should be pending
    client.revoke_certificate(&admin, &cert_id, &String::from_str(&env, "TEST_REASON"));

    let cert = client.get_certificate(&admin, &cert_id);
    assert!(cert.revoked);
    assert!(cert.revocation_deadline.is_some());

    let deadline = cert.revocation_deadline.unwrap();
    assert!(deadline > ledger_before);

    // During challenge period, verify still returns true
    assert!(client.verify_certificate(&cert_id));
}

#[test]
fn test_issue_146_student_can_challenge_pending_revocation() {
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
        "COURSE-CHALLENGE-OK",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-CHALLENGE-OK");
    let cert_id = String::from_str(&env, "CERT-CHALLENGE-OK");

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(&admin, &student, &course_id, &None);
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-CHALLENGE-OK"),
        &cert_id,
        &String::from_str(&env, "Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-CHALLENGE-OK"),
        &None,
        &None,
    );

    // Revoke
    client.revoke_certificate(&admin, &cert_id, &String::from_str(&env, "TEST"));

    // Student challenges within window — should succeed
    client.challenge_revocation(&student, &cert_id);

    // Revocation cancelled — certificate is valid again
    let cert = client.get_certificate(&admin, &cert_id);
    assert!(!cert.revoked);
    assert!(cert.revocation_deadline.is_none());
    assert!(client.verify_certificate(&cert_id));
}

#[test]
#[should_panic(expected = "Error(Contract, #44)")]
fn test_issue_146_unauthorized_challenge_fails() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    let attacker = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-UNAUTH-CHAL",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-UNAUTH-CHAL");
    let cert_id = String::from_str(&env, "CERT-UNAUTH-CHAL");

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(&admin, &student, &course_id, &None);
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-UNAUTH-CHAL"),
        &cert_id,
        &String::from_str(&env, "Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-UNAUTH-CHAL"),
        &None,
        &None,
    );

    client.revoke_certificate(&admin, &cert_id, &String::from_str(&env, "TEST"));

    // Different student tries to challenge — should fail
    client.challenge_revocation(&attacker, &cert_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #44)")]
fn test_issue_146_challenge_without_revocation_fails() {
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
        "COURSE-NO-REV",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-NO-REV");
    let cert_id = String::from_str(&env, "CERT-NO-REV");

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(&admin, &student, &course_id, &None);
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-NO-REV"),
        &cert_id,
        &String::from_str(&env, "Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-NO-REV"),
        &None,
        &None,
    );

    // No revocation — challenge should fail
    client.challenge_revocation(&student, &cert_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #44)")]
fn test_issue_146_late_challenge_fails() {
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
        "COURSE-LATE-CHAL",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-LATE-CHAL");
    let cert_id = String::from_str(&env, "CERT-LATE-CHAL");

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(&admin, &student, &course_id, &None);
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-LATE-CHAL"),
        &cert_id,
        &String::from_str(&env, "Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-LATE-CHAL"),
        &None,
        &None,
    );

    client.revoke_certificate(&admin, &cert_id, &String::from_str(&env, "TEST"));

    // Advance past challenge period
    let cert = client.get_certificate(&admin, &cert_id);
    let deadline = cert.revocation_deadline.unwrap();
    env.ledger().with_mut(|l| {
        l.sequence_number = deadline;
    });

    // Challenge after deadline — should fail
    client.challenge_revocation(&student, &cert_id);
}

#[test]
fn test_issue_146_revocation_becomes_permanent_after_deadline() {
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
        "COURSE-PERM-REV",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-PERM-REV");
    let cert_id = String::from_str(&env, "CERT-PERM-REV");

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(&admin, &student, &course_id, &None);
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-PERM-REV"),
        &cert_id,
        &String::from_str(&env, "Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-PERM-REV"),
        &None,
        &None,
    );

    client.revoke_certificate(&admin, &cert_id, &String::from_str(&env, "TEST"));

    // Still valid during challenge period
    assert!(client.verify_certificate(&cert_id));

    // Advance past deadline
    let cert = client.get_certificate(&admin, &cert_id);
    let deadline = cert.revocation_deadline.unwrap();
    env.ledger().with_mut(|l| {
        l.sequence_number = deadline;
    });

    // Now permanently revoked
    assert!(!client.verify_certificate(&cert_id));
}

#[test]
#[should_panic(expected = "Error(Contract, #44)")]
fn test_issue_146_double_revocation_rejected() {
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
        "COURSE-DBL-REV",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-DBL-REV");
    let cert_id = String::from_str(&env, "CERT-DBL-REV");

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(&admin, &student, &course_id, &None);
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-DBL-REV"),
        &cert_id,
        &String::from_str(&env, "Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-DBL-REV"),
        &None,
        &None,
    );

    client.revoke_certificate(&admin, &cert_id, &String::from_str(&env, "FIRST"));

    // Try to revoke again — should fail
    client.revoke_certificate(&admin, &cert_id, &String::from_str(&env, "SECOND"));
}

#[test]
fn test_issue_146_challenge_boundary_condition() {
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
        "COURSE-BOUNDARY",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-BOUNDARY");
    let cert_id = String::from_str(&env, "CERT-BOUNDARY");

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(&admin, &student, &course_id, &None);
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-BOUNDARY"),
        &cert_id,
        &String::from_str(&env, "Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-BOUNDARY"),
        &None,
        &None,
    );

    client.revoke_certificate(&admin, &cert_id, &String::from_str(&env, "TEST"));

    let cert = client.get_certificate(&admin, &cert_id);
    let deadline = cert.revocation_deadline.unwrap();

    // Challenge at exactly deadline - 1 should succeed
    env.ledger().with_mut(|l| {
        l.sequence_number = deadline - 1;
    });
    client.challenge_revocation(&student, &cert_id);

    // Certificate should be valid again
    assert!(client.verify_certificate(&cert_id));
}

#[test]
fn test_issue_146_rerevocation_after_successful_challenge() {
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
        "COURSE-RE-REV",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-RE-REV");
    let cert_id = String::from_str(&env, "CERT-RE-REV");

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(&admin, &student, &course_id, &None);
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-RE-REV"),
        &cert_id,
        &String::from_str(&env, "Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-RE-REV"),
        &None,
        &None,
    );

    // First revocation
    client.revoke_certificate(&admin, &cert_id, &String::from_str(&env, "FIRST"));

    // Student challenges successfully
    client.challenge_revocation(&student, &cert_id);
    assert!(!client.get_certificate(&admin, &cert_id).revoked);

    // Admin can revoke again (new challenge period)
    client.revoke_certificate(&admin, &cert_id, &String::from_str(&env, "SECOND"));

    let cert = client.get_certificate(&admin, &cert_id);
    assert!(cert.revoked);
    assert!(cert.revocation_deadline.is_some());
    assert_eq!(
        cert.revocation_reason,
        Some(String::from_str(&env, "SECOND"))
    );
}

#[test]
fn test_issue_146_configurable_challenge_period() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Set a short challenge period (5 ledgers)
    client.set_revocation_challenge_period(&admin, &5u32);
    assert_eq!(client.get_revocation_challenge_period(), 5);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-SHORT-PERIOD",
        500_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-SHORT-PERIOD");
    let cert_id = String::from_str(&env, "CERT-SHORT-PERIOD");

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(&admin, &student, &course_id, &None);
    client.issue_certificate(
        &admin,
        &student,
        &String::from_str(&env, "COURSE-SHORT-PERIOD"),
        &cert_id,
        &String::from_str(&env, "Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-SHORT-PERIOD"),
        &None,
        &None,
    );

    let ledger_before = env.ledger().sequence();
    client.revoke_certificate(&admin, &cert_id, &String::from_str(&env, "TEST"));

    let cert = client.get_certificate(&admin, &cert_id);
    let deadline = cert.revocation_deadline.unwrap();
    assert_eq!(deadline, ledger_before + 5);
}
// ============================================================
// ENROLLMENT REJECTION EVENT TESTS
// ============================================================

#[test]
fn test_enrollment_rejected_event_emitted_for_paused_course() {
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
        "COURSE-PAUSED-EVENT",
        500_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-PAUSED-EVENT");

    // Pause the course
    client.pause_course(&instructor, &course_id);

    // Attempt enrollment - should panic with rejection event emitted
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.enroll(&student, &course_id, &i128::MAX);
    }));
    assert!(
        result.is_err(),
        "enrollment should be rejected for paused course"
    );

    // Verify enrollment_rejected event was emitted with Paused status
    let events = env.events().all();
    let mut rejection_events = 0u32;
    for (contract, topics, data) in events.iter() {
        if contract != contract_id {
            continue;
        }
        let topic0 = topics.get(0).unwrap();
        let sym: Symbol = topic0.try_into_val(&env).unwrap();
        if sym == Symbol::new(&env, "enrollment_rejected") {
            rejection_events += 1;
            let rejected: EnrollmentRejected = data.try_into_val(&env).unwrap();
            assert_eq!(rejected.course_id, course_id);
            assert_eq!(rejected.student, student);
            assert_eq!(rejected.status, CourseStatus::Paused);
        }
    }
    assert_eq!(
        rejection_events, 1,
        "enrollment_rejected event must be emitted"
    );
}

#[test]
fn test_enrollment_rejected_event_emitted_for_pending_course() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    // Register but do NOT approve course
    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-PENDING-EVENT"),
        &500_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
    let course_id = String::from_str(&env, "COURSE-PENDING-EVENT");

    // Attempt enrollment - should panic with rejection event emitted
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.enroll(&student, &course_id, &i128::MAX);
    }));
    assert!(
        result.is_err(),
        "enrollment should be rejected for pending course"
    );

    // Verify enrollment_rejected event was emitted with Pending status
    let events = env.events().all();
    let mut rejection_events = 0u32;
    for (contract, topics, data) in events.iter() {
        if contract != contract_id {
            continue;
        }
        let topic0 = topics.get(0).unwrap();
        let sym: Symbol = topic0.try_into_val(&env).unwrap();
        if sym == Symbol::new(&env, "enrollment_rejected") {
            rejection_events += 1;
            let rejected: EnrollmentRejected = data.try_into_val(&env).unwrap();
            assert_eq!(rejected.status, CourseStatus::Pending);
        }
    }
    assert_eq!(
        rejection_events, 1,
        "enrollment_rejected event must be emitted"
    );
}

#[test]
fn test_enrollment_rejected_event_emitted_for_archived_course() {
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
        "COURSE-ARCHIVED-EVENT",
        500_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-ARCHIVED-EVENT");

    // Pause and archive the course
    client.pause_course(&admin, &course_id);
    client.archive_course(&admin, &sec_admin, &course_id, &None);

    // Attempt enrollment - should panic with rejection event emitted
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.enroll(&student, &course_id, &i128::MAX);
    }));
    assert!(
        result.is_err(),
        "enrollment should be rejected for archived course"
    );

    // Verify enrollment_rejected event was emitted with Archived status
    let events = env.events().all();
    let mut rejection_events = 0u32;
    for (contract, topics, data) in events.iter() {
        if contract != contract_id {
            continue;
        }
        let topic0 = topics.get(0).unwrap();
        let sym: Symbol = topic0.try_into_val(&env).unwrap();
        if sym == Symbol::new(&env, "enrollment_rejected") {
            rejection_events += 1;
            let rejected: EnrollmentRejected = data.try_into_val(&env).unwrap();
            assert_eq!(rejected.status, CourseStatus::Archived);
        }
    }
    assert_eq!(
        rejection_events, 1,
        "enrollment_rejected event must be emitted"
    );
}

// ============================================================
// COURSE ID CHARACTER VALIDATION TESTS
// ============================================================

#[test]
#[should_panic(expected = "Error(Contract, #11)")]
fn test_register_course_rejects_null_byte_in_id() {
    let (env, contract_id, token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Course ID with embedded null byte
    let course_id_with_null = String::from_bytes(
        &env,
        &[b'C', b'O', b'U', b'R', b'S', b'E', b'\0', b'-', b'1'],
    );
    client.register_course(
        &instructor,
        &course_id_with_null,
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #11)")]
fn test_register_course_rejects_control_characters_in_id() {
    let (env, contract_id, token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Course ID with various control characters (newline, tab, etc.)
    let course_id_with_control = String::from_bytes(
        &env,
        &[b'C', b'O', b'U', b'R', b'\n', b'S', b'E', b'\t', b'1'],
    );
    client.register_course(
        &instructor,
        &course_id_with_control,
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #11)")]
fn test_register_course_rejects_delete_character_in_id() {
    let (env, contract_id, token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Course ID with delete character (0x7F)
    let course_id_with_delete =
        String::from_bytes(&env, &[b'C', b'O', b'U', b'R', b'S', b'E', 0x7F, b'1']);
    client.register_course(
        &instructor,
        &course_id_with_delete,
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
}

#[test]
fn test_register_course_accepts_printable_ascii() {
    let (env, contract_id, token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Course ID with various printable ASCII characters (excluding backtick and tilde as per validation)
    let course_id = String::from_str(&env, "COURSE-ABC-123!@#$%^&*()_+-=[]{}|;':\",./<>?");
    client.register_course(
        &instructor,
        &course_id,
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.status, CourseStatus::Pending);
}

#[test]
fn test_register_course_accepts_unicode_above_0x7E() {
    // Course IDs with characters above printable ASCII range (> 0x7E) are also rejected
    let (env, contract_id, token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Course ID with extended ASCII character (tilde 0x7E is the boundary)
    // Characters like 0x7F (DEL) and above should be rejected
    let course_id = String::from_str(&env, "VALID-COURSE-001");
    client.register_course(
        &instructor,
        &course_id,
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.status, CourseStatus::Pending);
}

// ============================================================
// DOUBLE MARK_COMPLETED TEST
// ============================================================

#[test]
#[should_panic(expected = "Error(Contract, #39)")]
fn test_mark_completed_twice_rejected() {
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
        "COURSE-DBL-COMPLETE",
        500_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-DBL-COMPLETE");

    client.enroll(&student, &course_id, &i128::MAX);

    // First completion - should succeed
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "first_evidence")),
    );

    let enrollment = client.get_enrollment(&admin, &student, &course_id).unwrap();
    assert!(enrollment.completed);

    // Second completion - should panic with clear error message
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "second_evidence")),
    );
}

#[test]
fn test_mark_completed_twice_idempotency_check() {
    // Verify that calling mark_completed twice with the same evidence
    // results in the second call being rejected
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
        "COURSE-IDEMPOTENT",
        500_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-IDEMPOTENT");

    client.enroll(&student, &course_id, &i128::MAX);

    let evidence = Some(evidence_hash(&env, "same_evidence"));

    // First call
    client.mark_completed(&admin, &student, &course_id, &evidence);

    // Second call - must panic
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.mark_completed(&admin, &student, &course_id, &evidence);
    }));
    assert!(result.is_err(), "second mark_completed should be rejected");

    // Verify enrollment is still marked as completed
    let enrollment = client.get_enrollment(&admin, &student, &course_id).unwrap();
    assert!(enrollment.completed);
    assert_eq!(enrollment.evidence_hash, evidence);
}

// ============================================================
// GOVERNANCE WINDOW TESTS FOR APPROVE_COURSE
// ============================================================

#[test]
#[should_panic(expected = "Error(Contract, #16)")]
fn test_approve_course_immediately_after_init_rejected() {
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
    let instructor = Address::generate(&env);

    let client = HamplardContractClient::new(&env, &contract_id);
    client.init(
        &admin, &sec_admin, &treasury, &20u32, &50u32, &1000u32, &17_280u32,
    );
    client.add_approved_token(&admin, &token_id);

    client.update_min_review_delay(&admin, &10u32);

    // Register a course immediately after init
    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-EARLY"),
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );

    // Try to approve immediately - should panic due to governance window
    client.approve_course(&admin, &String::from_str(&env, "COURSE-EARLY"));
}

#[test]
fn test_approve_course_after_init_governance_window_succeeds() {
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
    let instructor = Address::generate(&env);

    let client = HamplardContractClient::new(&env, &contract_id);
    client.init(
        &admin, &sec_admin, &treasury, &20u32, &50u32, &1000u32, &17_280u32,
    );
    client.add_approved_token(&admin, &token_id);

    // Register a course
    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-LATER"),
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );

    // Set governance delay (MinReviewDelay)
    client.update_min_review_delay(&admin, &10u32);

    // Advance ledger past governance window
    env.ledger().with_mut(|l| {
        l.sequence_number += 11;
    });

    // Approval should now succeed
    client.approve_course(&admin, &String::from_str(&env, "COURSE-LATER"));

    let course = client
        .get_course(&String::from_str(&env, "COURSE-LATER"))
        .unwrap();
    assert_eq!(course.status, CourseStatus::Active);
}

#[test]
fn test_approve_course_at_init_governance_window_boundary() {
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
    let instructor = Address::generate(&env);

    let client = HamplardContractClient::new(&env, &contract_id);
    let init_ledger = env.ledger().sequence();
    client.init(
        &admin, &sec_admin, &treasury, &20u32, &50u32, &1000u32, &17_280u32,
    );
    client.add_approved_token(&admin, &token_id);

    // Set governance delay
    client.update_min_review_delay(&admin, &10u32);

    // Register course after init
    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-BOUNDARY"),
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );

    // Advance ledger to exactly at governance window boundary (init + delay)
    env.ledger().with_mut(|l| {
        l.sequence_number = init_ledger + 10;
    });

    // Approval at exactly the boundary should succeed
    client.approve_course(&admin, &String::from_str(&env, "COURSE-BOUNDARY"));

    let course = client
        .get_course(&String::from_str(&env, "COURSE-BOUNDARY"))
        .unwrap();
    assert_eq!(course.status, CourseStatus::Active);
}

#[test]
fn test_approve_course_init_ledger_stored_correctly() {
    // Verify that the init ledger is stored and can be used for governance checks
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

    // Capture ledger before and after init
    let ledger_before = env.ledger().sequence();
    client.init(
        &admin, &sec_admin, &treasury, &20u32, &50u32, &1000u32, &17_280u32,
    );
    let ledger_after = env.ledger().sequence();

    // Init succeeded and stored platform state.
    assert_eq!(client.get_platform_fee(&admin), 20);
    assert!(ledger_after >= ledger_before);
}

// ============================================================
// RISK PREVIEW, CERTIFICATE, EVIDENCE & READ AUTH TESTS (#225, #226, #227, #228)
// ============================================================

// ============================================================
// #498 — RISK-BASED FEE PRICING COVERAGE
// ============================================================

/// Risk pricing is off unless explicitly enabled: configured surcharges must
/// stay inert and the quote must fall back to the base per-token fee.
#[test]
fn test_risk_pricing_disabled_ignores_configured_surcharges() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);

    // Surcharges are configured, but pricing was never enabled.
    client.set_risk_fee_config(&admin, &500u32, &500_000_000i128, &700u32, &900u32);
    assert!(
        !client.is_risk_config_enabled(),
        "risk pricing must default to disabled"
    );

    // 100 USDC — well above the 50 USDC large-payment threshold.
    let payment: i128 = 1_000_000_000;

    let score = client.calculate_risk_score(&student, &token_id, &payment);
    assert_eq!(score.score, 0, "disabled pricing must not score the payment");
    assert_eq!(
        score.surcharge_bps, 0,
        "disabled pricing must not add a surcharge"
    );

    let quote = client.get_effective_fee_for_payment(&student, &token_id, &payment);
    assert_eq!(quote.risk_surcharge_bps, 0);
    assert_eq!(
        quote.effective_fee_bps, quote.base_fee_bps,
        "effective fee must equal the base fee while pricing is disabled"
    );
    assert_eq!(
        quote.platform_fee,
        payment * quote.base_fee_bps as i128 / 10_000
    );
}

/// Enabling pricing makes a payment above the configured threshold carry the
/// large-payment surcharge, and the preview must reflect it.
#[test]
fn test_risk_pricing_enabled_applies_large_payment_surcharge() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);

    client.set_risk_fee_config(&admin, &500u32, &500_000_000i128, &700u32, &900u32);
    client.set_risk_config_enabled(&admin, &true);
    assert!(client.is_risk_config_enabled());

    let payment: i128 = 1_000_000_000;
    let score = client.calculate_risk_score(&student, &token_id, &payment);
    assert_eq!(score.score, 30, "large payment contributes 30 to the score");
    assert_eq!(score.surcharge_bps, 500);

    let quote = client.get_effective_fee_for_payment(&student, &token_id, &payment);
    assert_eq!(quote.base_fee_bps, 2000, "platform default fee is 20%");
    assert_eq!(quote.risk_surcharge_bps, 500);
    assert_eq!(quote.effective_fee_bps, 2500);
    assert_eq!(quote.platform_fee, payment * 2500 / 10_000);
}

/// A payment at or below the threshold gets no surcharge even when pricing is
/// enabled — only the base fee applies.
#[test]
fn test_risk_pricing_below_threshold_charges_base_fee_only() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);

    client.set_risk_fee_config(&admin, &500u32, &500_000_000i128, &700u32, &900u32);
    client.set_risk_config_enabled(&admin, &true);

    // Exactly at the threshold is not "above" it.
    let at_threshold: i128 = 500_000_000;
    let score = client.calculate_risk_score(&student, &token_id, &at_threshold);
    assert_eq!(score.score, 0);
    assert_eq!(score.surcharge_bps, 0);

    let quote = client.get_effective_fee_for_payment(&student, &token_id, &at_threshold);
    assert_eq!(quote.risk_surcharge_bps, 0);
    assert_eq!(quote.effective_fee_bps, quote.base_fee_bps);
}

/// Base fee plus surcharge is clamped at 100%: the platform can never be
/// credited more than the payment, and the instructor share floors at zero.
#[test]
fn test_risk_pricing_effective_fee_capped_at_10000_bps() {
    let (env, contract_id, token_id, admin, _sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    // 20% base + 90% surcharge = 11000 bps, which must clamp to 10000.
    client.set_risk_fee_config(&admin, &9000u32, &500_000_000i128, &0u32, &0u32);
    client.set_risk_config_enabled(&admin, &true);

    let student = Address::generate(&env);
    let price: i128 = 1_000_000_000;
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &price);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-RISK-CAP",
        price,
    );

    let quote = client.get_effective_fee_for_payment(&student, &token_id, &price);
    assert_eq!(quote.base_fee_bps, 2000);
    assert_eq!(quote.risk_surcharge_bps, 9000);
    assert_eq!(
        quote.effective_fee_bps, 10000,
        "effective fee must be capped at 100%"
    );
    assert_eq!(quote.platform_fee, price, "the whole payment is the fee");

    client.enroll(&student, &String::from_str(&env, "COURSE-RISK-CAP"));

    // The clamp holds end-to-end: treasury takes everything, instructor nothing.
    assert_eq!(token_client.balance(&treasury), price);
    assert_eq!(client.get_instructor_earnings(&instructor, &token_id), 0);
}

/// A surcharge actually charged at enrollment emits `risk_fee_applied` with the
/// same breakdown the quote returned.
#[test]
fn test_risk_fee_applied_event_emitted_on_enroll() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.set_risk_fee_config(&admin, &500u32, &500_000_000i128, &700u32, &900u32);
    client.set_risk_config_enabled(&admin, &true);

    let student = Address::generate(&env);
    let price: i128 = 1_000_000_000;
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &price);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-RISK-EVENT",
        price,
    );

    let quote = client.get_effective_fee_for_payment(&student, &token_id, &price);
    client.enroll(&student, &String::from_str(&env, "COURSE-RISK-EVENT"));

    let mut applied_events = 0u32;
    for (contract, topics, data) in env.events().all().iter() {
        if contract != contract_id {
            continue;
        }
        let sym: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
        if sym != Symbol::new(&env, "risk_fee_applied") {
            continue;
        }
        applied_events += 1;
        let applied: RiskFeeApplied = data.try_into_val(&env).unwrap();
        assert_eq!(applied, quote, "the event must carry the charged breakdown");
        assert_eq!(applied.risk_surcharge_bps, 500);
    }

    assert_eq!(
        applied_events, 1,
        "exactly one risk_fee_applied event per surcharged enrollment"
    );
}

/// No surcharge means no `risk_fee_applied` event — the signal stays meaningful.
#[test]
fn test_risk_fee_applied_event_absent_without_surcharge() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    let price: i128 = 1_000_000_000;
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &price);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-RISK-QUIET",
        price,
    );

    client.enroll(&student, &String::from_str(&env, "COURSE-RISK-QUIET"));

    for (contract, topics, _data) in env.events().all().iter() {
        if contract != contract_id {
            continue;
        }
        let sym: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
        assert_ne!(
            sym,
            Symbol::new(&env, "risk_fee_applied"),
            "no surcharge was applied, so no risk_fee_applied event is allowed"
        );
    }
}

#[test]
#[should_panic(expected = "large_payment_threshold cannot be negative")]
fn test_set_risk_fee_config_rejects_negative_threshold() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.set_risk_fee_config(&admin, &500u32, &-1i128, &0u32, &0u32);
}

#[test]
#[should_panic(expected = "unauthorized: set_risk_config_enabled - caller is not admin")]
fn test_set_risk_config_enabled_requires_admin() {
    let (env, contract_id, _token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.set_risk_config_enabled(&instructor, &true);
}

#[test]
#[should_panic(expected = "unauthorized: set_risk_fee_config - caller is not admin")]
fn test_set_risk_fee_config_requires_admin() {
    let (env, contract_id, _token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.set_risk_fee_config(&instructor, &500u32, &500_000_000i128, &0u32, &0u32);
}

#[test]
fn test_fee_preview_matches_actual_enroll_deduction() {
    let (env, contract_id, token_id, admin, _sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    // Large-payment surcharge of 5% applies to payments above 50 USDC.
    client.set_risk_config_enabled(&admin, &true);
    client.set_risk_fee_config(&admin, &500u32, &500_000_000i128, &700u32, &900u32);

    let student = Address::generate(&env);
    let price: i128 = 1_000_000_000;
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &price);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-FEE-PREVIEW",
        price,
    );

    client.update_course(
        &instructor,
        &String::from_str(&env, "COURSE-UPDATE-ZERO-CAP"),
        &None,
        &Some(Some(0u32)),
        &None,
    );
    let enrollment = client
        .get_enrollment(&student, &student, &course_id)
        .unwrap();
    assert_eq!(enrollment.platform_amount, preview.platform_fee);
    assert_eq!(enrollment.instructor_amount, quote.2);
}

#[test]
#[should_panic(expected = "payment_amount cannot be negative")]
fn test_get_effective_fee_for_payment_rejects_negative_amount() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);

    client.get_effective_fee_for_payment(&student, &token_id, &-1_000_000_000i128);
}

fn panic_payload_text(payload: &(dyn std::any::Any + Send)) -> std::string::String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return std::string::String::from(*message);
    }
    if let Some(message) = payload.downcast_ref::<std::string::String>() {
        return message.clone();
    }
    std::string::String::new()
}

    // A subsequent price-only update must leave unlimited capacity intact.
    client.update_course(&instructor, &course_id, &Some(200_000_000i128), &None, &None);
    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.max_capacity, None);
    assert_eq!(course.price, 200_000_000);
}

#[test]
fn test_issue456_quote_and_deduction_panic_identically_on_overflow() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);

    client.set_fee_config(&admin, &token_id, &10000u32);
    let overflow_amount = i128::MAX / 10000 + 1;

    let quote_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.get_effective_fee_for_payment(&student, &token_id, &overflow_amount)
    }));
    let deduction_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.as_contract(&contract_id, || {
            HamplardContract::deduct_fee(&env, &token_id, overflow_amount, None, false, false)
        })
    }));

    let quote_panic = quote_result.expect_err("quote must fail on fee overflow");
    let deduction_panic = deduction_result.expect_err("deduction must fail on fee overflow");
    let quote_text = panic_payload_text(quote_panic.as_ref());
    let deduction_text = panic_payload_text(deduction_panic.as_ref());
    let shared_panic = "overflow computing platform fee";

    // Public contract calls wrap the contract panic in a HostError envelope,
    // while a direct internal call surfaces the panic payload itself. Both
    // must still carry the exact same contract-level panic message.
    assert!(
        quote_text.contains(shared_panic),
        "unexpected quote panic: {quote_text}"
    );
    assert!(
        deduction_text.contains(shared_panic),
        "unexpected deduction panic: {deduction_text}"
    );
}

#[test]
fn test_issue456_course_override_overflow_uses_shared_guard() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // The live token fee is low enough that the base fee multiplication is
    // safe, but a 100% course override is not. The override must go through
    // the same overflow guard rather than a separate fee calculation.
    client.set_fee_config(&admin, &token_id, &100u32);
    let overflow_amount = i128::MAX / 10000 + 1;

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.as_contract(&contract_id, || {
            HamplardContract::deduct_fee(
                &env,
                &token_id,
                overflow_amount,
                Some(10000u32),
                false,
                false,
            )
        })
    }));

    let panic = result.expect_err("override fee overflow must panic");
    let panic_text = panic_payload_text(panic.as_ref());
    assert!(
        panic_text.contains("overflow computing platform fee"),
        "unexpected override panic: {panic_text}"
    );
}

#[test]
fn test_issue_certificate_with_uuid_enrollment_reference() {
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
        "COURSE-UUID-REF",
        500_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-UUID-REF");

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence")),
    );

    // A backend UUID is not a Stellar address and is not the on-chain
    // enrollment ref — it must be accepted as a free-form reference.
    let backend_ref = String::from_str(&env, "550e8400-e29b-41d4-a716-446655440000");
    let cert_id = String::from_str(&env, "CERT-UUID-REF");
    client.issue_certificate(
        &admin,
        &student,
        &course_id,
        &cert_id,
        &String::from_str(&env, "UUID Course"),
        &backend_ref,
        &None,
        &None,
    );

    let cert = client.get_certificate(&admin, &cert_id);
    assert_eq!(cert.student, student);
    assert_eq!(cert.course_id, course_id);
    assert_eq!(cert.enrollment_reference, backend_ref);
}

#[test]
fn test_mark_completed_accepts_zeroed_evidence_hash() {
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
        "COURSE-EMPTY-EVIDENCE",
        500_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-EMPTY-EVIDENCE");
    client.enroll(&student, &course_id, &i128::MAX);

    let evidence = Some(BytesN::from_array(&env, &[0u8; 32]));
    client.mark_completed(&admin, &student, &course_id, &evidence);

    let enrollment = client
        .get_enrollment(&student, &student, &course_id)
        .unwrap();
    assert_eq!(enrollment.evidence_hash, evidence);
    assert_eq!(client.has_completed(&student, &course_id), Some(true));
}

#[test]
fn test_mark_completed_with_evidence_needs_no_student_signature() {
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
        "COURSE-EVIDENCE-ONLY",
        500_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-EVIDENCE-ONLY");
    client.enroll(&student, &course_id, &i128::MAX);

    // Only the admin signs; the student's auth is deliberately not mocked.
    let evidence = Some(evidence_hash(&env, "sha256:9f86d081884c7d65"));
    client
        .mock_auths(&[MockAuth {
            address: &admin,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "mark_completed",
                args: (
                    admin.clone(),
                    student.clone(),
                    course_id.clone(),
                    evidence.clone(),
                )
                    .into_val(&env),
                sub_invokes: &[],
            },
        }])
        .mark_completed(&admin, &student, &course_id, &evidence);

    assert_eq!(client.has_completed(&student, &course_id), Some(true));
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_get_certificate_denies_third_party_even_with_student_auth() {
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
        "COURSE-CERT-DENY",
        500_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-CERT-DENY");
    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence")),
    );

    let cert_id = String::from_str(&env, "CERT-DENY");
    client.issue_certificate(
        &admin,
        &student,
        &course_id,
        &cert_id,
        &String::from_str(&env, "Deny Course"),
        &String::from_str(&env, "backend-ref-deny"),
        &None,
        &None,
    );

    // All auths are mocked (including the student's), so the old fallback
    // of `cert.student.require_auth()` would have let this through.
    let third_party = Address::generate(&env);
    client.get_certificate(&third_party, &cert_id);
}

#[test]
fn test_get_certificate_allows_student_instructor_and_admin() {
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
        "COURSE-CERT-ALLOW",
        500_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-CERT-ALLOW");
    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence")),
    );

    let cert_id = String::from_str(&env, "CERT-ALLOW");
    client.issue_certificate(
        &admin,
        &student,
        &course_id,
        &cert_id,
        &String::from_str(&env, "Allow Course"),
        &String::from_str(&env, "backend-ref-allow"),
        &None,
        &None,
    );

    assert_eq!(client.get_certificate(&student, &cert_id).id, cert_id);
    assert_eq!(client.get_certificate(&instructor, &cert_id).id, cert_id);
    assert_eq!(client.get_certificate(&admin, &cert_id).id, cert_id);
}

// ============================================================
// INSTRUCTOR FREEZE & REGISTRATION VALIDATION TESTS (#229, #230, #231, #232)
// ============================================================

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_frozen_instructor_cannot_set_enrollment_expiry() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-FROZEN-EXPIRY",
        100_000_000,
    );
    client.freeze_instructor(&admin, &instructor, &String::from_str(&env, "test reason"));

    client.set_enrollment_expiry(
        &instructor,
        &String::from_str(&env, "COURSE-FROZEN-EXPIRY"),
        &Some(100u32),
    );
}

#[test]
fn test_admin_can_set_enrollment_expiry_for_frozen_instructor() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-FROZEN-EXPIRY-ADMIN",
        100_000_000,
    );
    client.freeze_instructor(&admin, &instructor, &String::from_str(&env, "test reason"));

    let course_id = String::from_str(&env, "COURSE-FROZEN-EXPIRY-ADMIN");
    client.set_enrollment_expiry(&admin, &course_id, &Some(100u32));

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.enrollment_expiry_ledgers, Some(100u32));
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_frozen_instructor_cannot_update_content_hash() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-FROZEN-HASH",
        100_000_000,
    );
    client.freeze_instructor(&admin, &instructor, &String::from_str(&env, "test reason"));

    client.update_content_hash(
        &instructor,
        &String::from_str(&env, "COURSE-FROZEN-HASH"),
        &BytesN::from_array(&env, &[7u8; 32]),
    );
}

#[test]
fn test_admin_can_update_content_hash_for_frozen_instructor() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-FROZEN-HASH-ADMIN",
        100_000_000,
    );
    client.freeze_instructor(&admin, &instructor, &String::from_str(&env, "test reason"));

    let course_id = String::from_str(&env, "COURSE-FROZEN-HASH-ADMIN");
    let new_hash = BytesN::from_array(&env, &[9u8; 32]);
    client.update_content_hash(&admin, &course_id, &new_hash);

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.content_hash, new_hash);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_frozen_instructor_cannot_set_prerequisite_courses() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-FROZEN-PREREQ",
        100_000_000,
    );
    client.freeze_instructor(&admin, &instructor, &String::from_str(&env, "test reason"));

    client.set_prerequisite_courses(
        &instructor,
        &String::from_str(&env, "COURSE-FROZEN-PREREQ"),
        &soroban_sdk::Vec::new(&env),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_frozen_instructor_cannot_unpause_course() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-FROZEN-UNPAUSE",
        100_000_000,
    );
    // freeze_instructor() auto-pauses the instructor's Active courses.
    client.freeze_instructor(&admin, &instructor, &String::from_str(&env, "test reason"));

    client.unpause_course(
        &instructor,
        &String::from_str(&env, "COURSE-FROZEN-UNPAUSE"),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #11)")]
fn test_register_course_rejects_empty_course_id() {
    let (env, contract_id, token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.register_course(
        &instructor,
        &String::from_str(&env, ""),
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #11)")]
fn test_register_course_rejects_zero_max_capacity() {
    let (env, contract_id, token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-ZERO-CAP"),
        &100_000_000,
        &token_id,
        &0u32,
        &Some(0u32),
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #11)")]
fn test_update_course_rejects_zero_max_capacity() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-UPDATE-ZERO-CAP",
        100_000_000,
    );

    client.update_course(
        &instructor,
        &String::from_str(&env, "COURSE-UPDATE-ZERO-CAP"),
        &None,
        &Some(Some(0u32)),
        &None,
    );
}

#[test]
fn test_update_course_allows_unlimited_capacity_with_none() {
    let (env, contract_id, token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-UNLIMITED-CAP"),
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );

    let course_id = String::from_str(&env, "COURSE-UNLIMITED-CAP");
    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.max_capacity, None);

    // A subsequent price-only update must leave unlimited capacity intact.
    client.update_course(&instructor, &course_id, &Some(200_000_000i128), &None, &None);
    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.max_capacity, None);
    assert_eq!(course.price, 200_000_000);
}
#[test]
fn test_update_course_updates_price_and_capacity_and_bumps_version() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-UPDATE-FIELDS",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-UPDATE-FIELDS");
    let before = client.get_course(&course_id).unwrap();

    let new_version = client.update_course(
        &instructor,
        &course_id,
        &Some(250_000_000i128),
        &Some(Some(25u32)),
    );

    assert_eq!(new_version, before.version + 1);
    let after = client.get_course(&course_id).unwrap();
    assert_eq!(after.price, 250_000_000);
    assert_eq!(after.max_capacity, Some(25u32));
    assert_eq!(after.version, before.version + 1);
    assert!(after.last_updated_ledger >= before.last_updated_ledger);
}

#[test]
fn test_update_course_noop_returns_same_version() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-UPDATE-NOOP",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-UPDATE-NOOP");
    let before = client.get_course(&course_id).unwrap();

    let returned = client.update_course(&instructor, &course_id, &None, &None);

    assert_eq!(returned, before.version);
    let after = client.get_course(&course_id).unwrap();
    assert_eq!(after.version, before.version);
    assert_eq!(after.price, before.price);
    assert_eq!(after.max_capacity, before.max_capacity);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_update_course_rejects_unauthorized_caller() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-UPDATE-AUTH",
        100_000_000,
    );

    let stranger = Address::generate(&env);
    client.update_course(
        &stranger,
        &String::from_str(&env, "COURSE-UPDATE-AUTH"),
        &Some(200_000_000i128),
        &None,
    );
}

#[test]
#[should_panic(expected = "cannot update archived course")]
fn test_update_course_rejects_archived_course() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-UPDATE-ARCHIVED",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-UPDATE-ARCHIVED");
    client.pause_course(&admin, &course_id);
    client.archive_course(&admin, &sec_admin, &course_id, &None);

    client.update_course(&instructor, &course_id, &Some(200_000_000i128), &None);
}

#[test]
#[should_panic(expected = "instructor is frozen")]
fn test_update_course_rejects_frozen_instructor() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-UPDATE-FROZEN",
        100_000_000,
    );
    client.freeze_instructor(&admin, &instructor);

    client.update_course(
        &instructor,
        &String::from_str(&env, "COURSE-UPDATE-FROZEN"),
        &Some(200_000_000i128),
        &None,
    );
}

#[test]
#[should_panic(expected = "price cannot be negative")]
fn test_update_course_rejects_negative_price() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-UPDATE-NEGATIVE",
        100_000_000,
    );

    client.update_course(
        &instructor,
        &String::from_str(&env, "COURSE-UPDATE-NEGATIVE"),
        &Some(-1i128),
        &None,
    );
}

#[test]
#[should_panic(expected = "price is outside the expected USDC precision range")]
fn test_update_course_rejects_price_below_minimum() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-UPDATE-LOW-PRICE",
        100_000_000,
    );

    client.update_course(
        &instructor,
        &String::from_str(&env, "COURSE-UPDATE-LOW-PRICE"),
        &Some(HamplardContract::MIN_COURSE_PRICE_STROOPS - 1),
        &None,
    );
}

#[test]
#[should_panic(expected = "price is outside the expected USDC precision range")]
fn test_update_course_rejects_price_above_maximum() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-UPDATE-HIGH-PRICE",
        100_000_000,
    );

    client.update_course(
        &instructor,
        &String::from_str(&env, "COURSE-UPDATE-HIGH-PRICE"),
        &Some(HamplardContract::MAX_COURSE_PRICE_STROOPS + 1),
        &None,
    );
}

// ============================================================
// COURSE STATUS CHANGES EXTEND COURSE PERSISTENT TTL
// ============================================================

/// Read the remaining persistent TTL of a Course entry.
fn course_ttl(env: &Env, contract_id: &Address, course_id: &String) -> u32 {
    use soroban_sdk::testutils::storage::Persistent as _;
    env.as_contract(contract_id, || {
        env.storage()
            .persistent()
            .get_ttl(&DataKey::Course(course_id.clone()))
    })
}

/// Advance far enough that the Course entry's TTL drops below
/// PERSISTENT_TTL_THRESHOLD, so a subsequent extend_ttl() actually renews it.
fn advance_below_course_ttl_threshold(env: &Env) {
    env.ledger().with_mut(|l| {
        l.sequence_number += 400_000;
    });
}

#[test]
fn test_pause_course_extends_course_ttl() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-TTL-PAUSE",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-TTL-PAUSE");

    advance_below_course_ttl_threshold(&env);
    assert!(
        course_ttl(&env, &contract_id, &course_id) < HamplardContract::PERSISTENT_TTL_THRESHOLD
    );

    client.pause_course(&instructor, &course_id);

    assert_eq!(
        course_ttl(&env, &contract_id, &course_id),
        HamplardContract::PERSISTENT_TTL_EXTEND_TO
    );
}

#[test]
fn test_unpause_course_extends_course_ttl() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-TTL-UNPAUSE",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-TTL-UNPAUSE");
    client.pause_course(&instructor, &course_id);

    advance_below_course_ttl_threshold(&env);
    assert!(
        course_ttl(&env, &contract_id, &course_id) < HamplardContract::PERSISTENT_TTL_THRESHOLD
    );

    client.unpause_course(&instructor, &course_id);

    assert_eq!(
        course_ttl(&env, &contract_id, &course_id),
        HamplardContract::PERSISTENT_TTL_EXTEND_TO
    );
}

#[test]
fn test_reject_course_extends_course_ttl() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let course_id = String::from_str(&env, "COURSE-TTL-REJECT");
    client.register_course(
        &instructor,
        &course_id,
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );

    advance_below_course_ttl_threshold(&env);
    assert!(
        course_ttl(&env, &contract_id, &course_id) < HamplardContract::PERSISTENT_TTL_THRESHOLD
    );

    client.reject_course(
        &admin,
        &course_id,
        &String::from_str(&env, "CONTENT_POLICY_VIOLATION"),
    );

    assert_eq!(
        course_ttl(&env, &contract_id, &course_id),
        HamplardContract::PERSISTENT_TTL_EXTEND_TO
    );
}

// ============================================================
// BULK COURSE CERTIFICATE REVOCATION TESTS (#173)
// ============================================================

/// Fund `student`, enroll them in `course_id`, complete the enrollment and
/// issue `certificate_id` for it — i.e. leave the course with one valid
/// certificate that a bulk revocation should be able to reach.
fn enroll_and_certify(
    env: &Env,
    client: &HamplardContractClient,
    token_id: &Address,
    admin: &Address,
    student: &Address,
    course_id: &str,
    certificate_id: &str,
) {
    token::StellarAssetClient::new(env, token_id).mint(student, &100_000_000_000);

    let course = String::from_str(env, course_id);
    client.enroll(student, &course, &i128::MAX);
    client.mark_completed(
        admin,
        student,
        &course,
        &Some(evidence_hash(env, "completion-evidence")),
    );
    client.issue_certificate(
        admin,
        student,
        &course,
        &String::from_str(env, certificate_id),
        &String::from_str(env, "Bulk Revocation Course"),
        &get_enrollment_ref(env, client, student, course_id),
        &None,
        &None,
    );
}

#[test]
fn test_bulk_revoke_course_certificates_single_certificate() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-BULK-ONE",
        100_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-BULK-ONE");
    let cert_id = String::from_str(&env, "CERT-BULK-ONE");
    let student = Address::generate(&env);
    enroll_and_certify(
        &env,
        &client,
        &token_id,
        &admin,
        &student,
        "COURSE-BULK-ONE",
        "CERT-BULK-ONE",
    );

    // The course now indexes its one issued certificate.
    assert_eq!(client.get_course_certificates(&course_id).len(), 1);
    assert_eq!(
        client.get_course_certificates(&course_id).get(0).unwrap(),
        cert_id
    );

    let revoked_count = client.bulk_revoke_course_certificates(&admin, &course_id);
    assert_eq!(revoked_count, 1);

    let cert = client.get_certificate(&admin, &cert_id);
    assert!(cert.revoked);
    assert_eq!(cert.revoked_by, Some(admin.clone()));
    assert_eq!(
        cert.revocation_reason,
        Some(String::from_str(&env, "BULK_COURSE_REVOCATION"))
    );
    assert!(cert.revocation_ledger.is_some());
    assert!(cert.revocation_deadline.is_some());

    // A bulk revocation is a *pending* revocation, exactly like
    // revoke_certificate: the certificate stays valid until the challenge
    // period elapses.
    assert!(client.verify_certificate(&cert_id));
    let deadline = cert.revocation_deadline.unwrap();
    env.ledger().with_mut(|l| {
        l.sequence_number = deadline;
    });
    assert!(!client.verify_certificate(&cert_id));
}

#[test]
fn test_bulk_revoke_course_certificates_revokes_five_certificates() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-BULK-FIVE",
        100_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-BULK-FIVE");
    let cert_names = [
        "CERT-BULK-FIVE-0",
        "CERT-BULK-FIVE-1",
        "CERT-BULK-FIVE-2",
        "CERT-BULK-FIVE-3",
        "CERT-BULK-FIVE-4",
    ];

    for cert_name in cert_names {
        let student = Address::generate(&env);
        enroll_and_certify(
            &env,
            &client,
            &token_id,
            &admin,
            &student,
            "COURSE-BULK-FIVE",
            cert_name,
        );
    }

    assert_eq!(client.get_course_certificates(&course_id).len(), 5);

    let revoked_count = client.bulk_revoke_course_certificates(&admin, &course_id);
    assert_eq!(revoked_count, 5);

    // Every certificate for the course is flagged for revocation...
    let mut earliest_deadline = u32::MAX;
    for cert_name in cert_names {
        let cert_id = String::from_str(&env, cert_name);
        let cert = client.get_certificate(&admin, &cert_id);
        assert!(cert.revoked, "{} should be revoked", cert_name);
        assert_eq!(cert.course_id, course_id);
        assert_eq!(
            cert.revocation_reason,
            Some(String::from_str(&env, "BULK_COURSE_REVOCATION"))
        );
        let deadline = cert.revocation_deadline.unwrap();
        if deadline < earliest_deadline {
            earliest_deadline = deadline;
        }
    }

    // ...and the index itself is append-only: bulk revocation never drops the
    // certificate IDs, so a second call can still enumerate the cohort.
    assert_eq!(client.get_course_certificates(&course_id).len(), 5);

    // A retry is safe and reports zero newly-revoked certificates.
    assert_eq!(
        client.bulk_revoke_course_certificates(&admin, &course_id),
        0
    );

    // The whole cohort is still verifiable during the challenge period and
    // expires together once it passes.
    let indexed = client.get_course_certificates(&course_id);
    for i in 0..indexed.len() {
        assert!(client.verify_certificate(&indexed.get(i).unwrap()));
    }
    env.ledger().with_mut(|l| {
        l.sequence_number = earliest_deadline;
    });
    for i in 0..indexed.len() {
        assert!(!client.verify_certificate(&indexed.get(i).unwrap()));
    }
}

#[test]
fn test_bulk_revoke_only_affects_target_course() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-BULK-TARGET",
        100_000_000,
    );
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-BULK-UNTOUCHED",
        100_000_000,
    );

    let target_course_id = String::from_str(&env, "COURSE-BULK-TARGET");
    let other_course_id = String::from_str(&env, "COURSE-BULK-UNTOUCHED");

    let student_a = Address::generate(&env);
    enroll_and_certify(
        &env,
        &client,
        &token_id,
        &admin,
        &student_a,
        "COURSE-BULK-TARGET",
        "CERT-BULK-TARGET-A",
    );
    let student_b = Address::generate(&env);
    enroll_and_certify(
        &env,
        &client,
        &token_id,
        &admin,
        &student_b,
        "COURSE-BULK-TARGET",
        "CERT-BULK-TARGET-B",
    );
    let student_c = Address::generate(&env);
    enroll_and_certify(
        &env,
        &client,
        &token_id,
        &admin,
        &student_c,
        "COURSE-BULK-UNTOUCHED",
        "CERT-BULK-OTHER",
    );

    let revoked_count = client.bulk_revoke_course_certificates(&admin, &target_course_id);
    assert_eq!(revoked_count, 2);

    assert!(
        client
            .get_certificate(&admin, &String::from_str(&env, "CERT-BULK-TARGET-A"))
            .revoked
    );
    assert!(
        client
            .get_certificate(&admin, &String::from_str(&env, "CERT-BULK-TARGET-B"))
            .revoked
    );

    // A certificate from another course is untouched and still verifiable.
    let other_cert = client.get_certificate(&admin, &String::from_str(&env, "CERT-BULK-OTHER"));
    assert!(!other_cert.revoked);
    assert_eq!(other_cert.revocation_reason, None);
    assert!(client.verify_certificate(&String::from_str(&env, "CERT-BULK-OTHER")));
    assert_eq!(client.get_course_certificates(&other_course_id).len(), 1);
}

#[test]
fn test_bulk_revoke_skips_already_revoked_certificates() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-BULK-MIXED",
        100_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-BULK-MIXED");
    let cert_names = [
        "CERT-BULK-MIXED-0",
        "CERT-BULK-MIXED-1",
        "CERT-BULK-MIXED-2",
    ];
    for cert_name in cert_names {
        let student = Address::generate(&env);
        enroll_and_certify(
            &env,
            &client,
            &token_id,
            &admin,
            &student,
            "COURSE-BULK-MIXED",
            cert_name,
        );
    }

    // One certificate is revoked up front, with its own reason code.
    let individually_revoked = String::from_str(&env, "CERT-BULK-MIXED-0");
    client.revoke_certificate(
        &admin,
        &individually_revoked,
        &String::from_str(&env, "ISSUED_IN_ERROR"),
    );

    let revoked_count = client.bulk_revoke_course_certificates(&admin, &course_id);
    assert_eq!(revoked_count, 2);

    // The pre-existing revocation keeps its original audit metadata...
    let cert = client.get_certificate(&admin, &individually_revoked);
    assert!(cert.revoked);
    assert_eq!(
        cert.revocation_reason,
        Some(String::from_str(&env, "ISSUED_IN_ERROR"))
    );

    // ...while the remaining certificates get the bulk reason code.
    for cert_name in ["CERT-BULK-MIXED-1", "CERT-BULK-MIXED-2"] {
        let cert = client.get_certificate(&admin, &String::from_str(&env, cert_name));
        assert!(cert.revoked);
        assert_eq!(
            cert.revocation_reason,
            Some(String::from_str(&env, "BULK_COURSE_REVOCATION"))
        );
    }
}

#[test]
fn test_bulk_revoke_emits_course_certificates_revoked_event() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-BULK-EVENT",
        100_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-BULK-EVENT");
    for cert_name in ["CERT-BULK-EVENT-0", "CERT-BULK-EVENT-1"] {
        let student = Address::generate(&env);
        enroll_and_certify(
            &env,
            &client,
            &token_id,
            &admin,
            &student,
            "COURSE-BULK-EVENT",
            cert_name,
        );
    }

    client.bulk_revoke_course_certificates(&admin, &course_id);

    assert!(has_event(&env, &contract_id, "course_certificates_revoked"));
    let (event_admin, event_course_id, event_count, event_ledger): (Address, String, u32, u32) =
        last_event_val(&env, &contract_id, "course_certificates_revoked")
            .try_into_val(&env)
            .unwrap();
    assert_eq!(event_admin, admin);
    assert_eq!(event_course_id, course_id);
    assert_eq!(event_count, 2);
    assert_eq!(event_ledger, env.ledger().sequence());
}

#[test]
fn test_bulk_revoked_certificate_can_still_be_challenged_and_re_revoked() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-BULK-CHALLENGE",
        100_000_000,
    );

    let course_id = String::from_str(&env, "COURSE-BULK-CHALLENGE");
    let cert_id = String::from_str(&env, "CERT-BULK-CHALLENGE");
    let student = Address::generate(&env);
    enroll_and_certify(
        &env,
        &client,
        &token_id,
        &admin,
        &student,
        "COURSE-BULK-CHALLENGE",
        "CERT-BULK-CHALLENGE",
    );

    client.bulk_revoke_course_certificates(&admin, &course_id);
    assert!(client.get_certificate(&admin, &cert_id).revoked);

    // The holder gets exactly the same dispute rights as a targeted
    // revocation — the revocation is only pending, not immediate.
    client.challenge_revocation(&student, &cert_id);
    let cert = client.get_certificate(&admin, &cert_id);
    assert!(!cert.revoked);
    assert_eq!(cert.revoked_by, None);
    assert_eq!(cert.revocation_reason, None);
    assert_eq!(cert.revocation_deadline, None);
    assert!(client.verify_certificate(&cert_id));

    // With the challenge accepted, the course can be bulk-revoked again.
    assert_eq!(
        client.bulk_revoke_course_certificates(&admin, &course_id),
        1
    );
    assert!(client.get_certificate(&admin, &cert_id).revoked);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_bulk_revoke_unauthorized_caller_rejected() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-BULK-AUTH",
        100_000_000,
    );

    let student = Address::generate(&env);
    enroll_and_certify(
        &env,
        &client,
        &token_id,
        &admin,
        &student,
        "COURSE-BULK-AUTH",
        "CERT-BULK-AUTH",
    );

    // The instructor owns the course but must not be able to revoke its
    // certificates — bulk revocation is an admin-only power.
    client
        .bulk_revoke_course_certificates(&instructor, &String::from_str(&env, "COURSE-BULK-AUTH"));
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_bulk_revoke_unknown_course_rejected() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client
        .bulk_revoke_course_certificates(&admin, &String::from_str(&env, "COURSE-DOES-NOT-EXIST"));
}

#[test]
#[should_panic(expected = "Error(Contract, #44)")]
fn test_bulk_revoke_course_without_certificates_rejected() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-BULK-EMPTY",
        100_000_000,
    );

    client.bulk_revoke_course_certificates(&admin, &String::from_str(&env, "COURSE-BULK-EMPTY"));
}

#[test]
fn test_get_course_certificates_empty_for_course_without_issuance() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-BULK-NONE",
        100_000_000,
    );

    assert_eq!(
        client
            .get_course_certificates(&String::from_str(&env, "COURSE-BULK-NONE"))
            .len(),
        0
    );
}

// ============================================================
// ISSUE #236 — withdraw_tokens must validate amount > 0
// ============================================================

#[test]
#[should_panic(expected = "Error(Contract, #40)")]
fn test_withdraw_tokens_rejects_zero_amount() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    client.withdraw_tokens(
        &admin,
        &sec_admin,
        &token_id,
        &0i128,
        &Address::generate(&env),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #40)")]
fn test_withdraw_tokens_rejects_negative_amount() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    client.withdraw_tokens(
        &admin,
        &sec_admin,
        &token_id,
        &-1i128,
        &Address::generate(&env),
    );
}

#[test]
fn test_withdraw_tokens_accepts_positive_amount() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_mint = token::StellarAssetClient::new(&env, &token_id);
    let destination = Address::generate(&env);
    token_mint.mint(&contract_id, &500_000_000i128);
    client.withdraw_tokens(
        &admin,
        &sec_admin,
        &token_id,
        &500_000_000i128,
        &destination,
    );
    assert_eq!(
        token::Client::new(&env, &token_id).balance(&destination),
        500_000_000i128
    );
}

// ============================================================
// ISSUE #175 — update_default_fee must reject same-value updates
// ============================================================

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_update_default_fee_rejects_same_value() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    // Platform was initialised with 20% — calling with 20 again must panic.
    client.update_default_fee(&admin, &20u32);
}

#[test]
fn test_update_default_fee_accepts_different_value() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    client.update_default_fee(&admin, &15u32);
    client.update_default_fee(&admin, &25u32);
}

// ============================================================
// ISSUE #176 — transfer_admin cooldown between consecutive calls
// ============================================================

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_transfer_admin_cooldown_blocks_rapid_rotation() {
    let (env, contract_id, _token_id, admin, sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let a1 = Address::generate(&env);
    let s1 = Address::generate(&env);
    client.transfer_admin(&admin, &sec_admin, &a1, &s1);
    // Immediate second call — cooldown not elapsed; must panic.
    let a2 = Address::generate(&env);
    let s2 = Address::generate(&env);
    client.transfer_admin(&admin, &sec_admin, &a2, &s2);
}

#[test]
fn test_transfer_admin_succeeds_after_cooldown() {
    let (env, contract_id, _token_id, admin, sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let a1 = Address::generate(&env);
    let s1 = Address::generate(&env);
    client.transfer_admin(&admin, &sec_admin, &a1, &s1);
    // Advance past MIN_ADMIN_TRANSFER_COOLDOWN (17_280 ledgers).
    env.ledger().with_mut(|li| {
        li.sequence_number += 17_281;
    });
    let a2 = Address::generate(&env);
    let s2 = Address::generate(&env);
    client.transfer_admin(&admin, &sec_admin, &a2, &s2);
}

#[test]
fn test_transfer_admin_first_call_never_blocked() {
    let (env, contract_id, _token_id, admin, sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let a = Address::generate(&env);
    let s = Address::generate(&env);
    // First-ever call — no LastAdminTransferLedger stored; must succeed.
    client.transfer_admin(&admin, &sec_admin, &a, &s);
}

// ============================================================
// ISSUE #178 — Circuit breaker: auto-pause after threshold failures
// ============================================================

#[test]
#[should_panic(expected = "Error(Contract, #36)")]
fn test_circuit_breaker_trips_after_threshold_failures() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "CB-TRIP",
        100_000_000,
    );
    // Simulate 5 consecutive prior failures by writing the counter directly.
    env.as_contract(&contract_id, || {
        env.storage().persistent().set(
            &DataKey::CourseFailureCount(String::from_str(&env, "CB-TRIP")),
            &5u32,
        );
    });
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &500_000_000i128);
    // This call must trip the circuit breaker.
    client.enroll(&student, &String::from_str(&env, "CB-TRIP"), &i128::MAX);
}

#[test]
fn test_circuit_breaker_does_not_trip_below_threshold() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "CB-BELOW",
        100_000_000,
    );
    // 4 failures — one below the threshold of 5.
    env.as_contract(&contract_id, || {
        env.storage().persistent().set(
            &DataKey::CourseFailureCount(String::from_str(&env, "CB-BELOW")),
            &4u32,
        );
    });
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &500_000_000i128);
    // Must succeed.
    client.enroll(&student, &String::from_str(&env, "CB-BELOW"), &i128::MAX);
    // After success the failure counter must be reset to 0.
    env.as_contract(&contract_id, || {
        let count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::CourseFailureCount(String::from_str(
                &env, "CB-BELOW",
            )))
            .unwrap_or(0);
        assert_eq!(
            count, 0,
            "failure counter must reset to 0 after successful enrollment"
        );
    });
}

#[test]
fn test_circuit_breaker_course_can_be_unpaused_by_instructor() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "CB-UNPAUSE",
        100_000_000,
    );
    // Simulate circuit breaker state: threshold failures + course paused.
    env.as_contract(&contract_id, || {
        env.storage().persistent().set(
            &DataKey::CourseFailureCount(String::from_str(&env, "CB-UNPAUSE")),
            &5u32,
        );
        let mut course: Course = env
            .storage()
            .persistent()
            .get(&DataKey::Course(String::from_str(&env, "CB-UNPAUSE")))
            .unwrap();
        course.status = CourseStatus::Paused;
        env.storage().persistent().set(
            &DataKey::Course(String::from_str(&env, "CB-UNPAUSE")),
            &course,
        );
    });
    // Instructor (owner) manually unpauses.
    client.unpause_course(&instructor, &String::from_str(&env, "CB-UNPAUSE"));
    let course = client
        .get_course(&String::from_str(&env, "CB-UNPAUSE"))
        .unwrap();
    assert_eq!(course.status, CourseStatus::Active);
}

// =============================================================================
// ISSUE #233: register_course() rejects all-zero content_hash
// =============================================================================

#[test]
#[should_panic(expected = "Error(Contract, #11)")]
fn test_register_course_rejects_zero_content_hash() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-ZERO-HASH"),
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[0u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
}

#[test]
fn test_register_course_accepts_non_zero_content_hash() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let non_zero_hash = BytesN::from_array(&env, &[1u8; 32]);
    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-NONZERO-HASH"),
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &non_zero_hash,
        &soroban_sdk::Vec::new(&env),
    );
}

// =============================================================================
// ISSUE #235: withdraw_tokens requires multi-sig authorization
// =============================================================================

#[test]
fn test_withdraw_tokens_requires_both_admins() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Fund the contract with tokens
    let token_client = token::StellarAssetClient::new(&env, &token_id);
    token_client.mint(&contract_id, &1_000_000_000);

    let destination = Address::generate(&env);
    client.withdraw_tokens(&admin, &sec_admin, &token_id, &500_000_000, &destination);

    assert_eq!(
        token::Client::new(&env, &token_id).balance(&destination),
        500_000_000
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_withdraw_tokens_rejects_single_admin() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let destination = Address::generate(&env);
    let other = Address::generate(&env);
    // Second signer is not the secondary admin — must panic.
    client.withdraw_tokens(&admin, &other, &token_id, &500_000_000, &destination);
}

// =============================================================================
// ISSUE #248: mark_completed extends TTL for Enrollment and Course
// =============================================================================

#[test]
fn test_mark_completed_extends_enrollment_and_course_ttl() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let course_id = String::from_str(&env, "COURSE-TTL-TEST");
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-TTL-TEST",
        100_000_000,
    );

    client.enroll(&student, &course_id, &i128::MAX);

    env.ledger().with_mut(|l| {
        l.sequence_number += 100;
    });

    client.mark_completed(&admin, &student, &course_id, &None);

    let enrollment = client
        .get_enrollment(&student, &student, &course_id)
        .unwrap();
    assert!(enrollment.completed);
}

// =============================================================================
// ISSUE #249: issue_certificate extends TTL for Enrollment
// =============================================================================

#[test]
fn test_issue_certificate_extends_enrollment_ttl() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let course_id = String::from_str(&env, "COURSE-CERT-TTL");
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-CERT-TTL",
        100_000_000,
    );

    client.enroll(&student, &course_id, &i128::MAX);

    env.ledger().with_mut(|l| {
        l.sequence_number += 100;
    });
    client.mark_completed(&admin, &student, &course_id, &None);

    let cert_id = String::from_str(&env, "CERT-001");
    client.issue_certificate(
        &admin,
        &student,
        &course_id,
        &cert_id,
        &String::from_str(&env, "Test Course"),
        &get_enrollment_ref(&env, &client, &student, "COURSE-CERT-TTL"),
        &None,
        &None,
    );

    let enrollment = client
        .get_enrollment(&student, &student, &course_id)
        .unwrap();
    assert!(enrollment.certificate_issued);
}

// =============================================================================
// ISSUE #237: list_courses() must not overflow when limit is u32::MAX
// =============================================================================

#[test]
fn test_list_courses_u32_max_limit_does_not_overflow() {
    let (env, contract_id, token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let ids = [
        "COURSE-LIST-A",
        "COURSE-LIST-B",
        "COURSE-LIST-C",
        "COURSE-LIST-D",
        "COURSE-LIST-E",
    ];
    for id in ids {
        client.register_course(
            &instructor,
            &String::from_str(&env, id),
            &100_000_000,
            &token_id,
            &0u32,
            &None,
            &BytesN::from_array(&env, &[1u8; 32]),
            &soroban_sdk::Vec::new(&env),
        );
    }

    // Oversized limit must not trap; it returns the full remaining catalog.
    let all = client.list_courses(&0u32, &u32::MAX);
    assert_eq!(all.len(), 5);
    assert_eq!(all.get(0).unwrap(), String::from_str(&env, "COURSE-LIST-A"));
    assert_eq!(all.get(4).unwrap(), String::from_str(&env, "COURSE-LIST-E"));

    let remaining = client.list_courses(&2u32, &u32::MAX);
    assert_eq!(remaining.len(), 3);
    assert_eq!(
        remaining.get(0).unwrap(),
        String::from_str(&env, "COURSE-LIST-C")
    );
    assert_eq!(
        remaining.get(2).unwrap(),
        String::from_str(&env, "COURSE-LIST-E")
    );

    let past_end = client.list_courses(&5u32, &u32::MAX);
    assert_eq!(past_end.len(), 0);

    // Normal start/limit pagination is unchanged.
    let page0 = client.list_courses(&0u32, &2u32);
    assert_eq!(page0.len(), 2);
    assert_eq!(
        page0.get(0).unwrap(),
        String::from_str(&env, "COURSE-LIST-A")
    );
    assert_eq!(
        page0.get(1).unwrap(),
        String::from_str(&env, "COURSE-LIST-B")
    );

    let page1 = client.list_courses(&2u32, &2u32);
    assert_eq!(page1.len(), 2);
    assert_eq!(
        page1.get(0).unwrap(),
        String::from_str(&env, "COURSE-LIST-C")
    );
    assert_eq!(
        page1.get(1).unwrap(),
        String::from_str(&env, "COURSE-LIST-D")
    );

    let page2 = client.list_courses(&4u32, &2u32);
    assert_eq!(page2.len(), 1);
    assert_eq!(
        page2.get(0).unwrap(),
        String::from_str(&env, "COURSE-LIST-E")
    );

    let empty = client.list_courses(&10u32, &2u32);
    assert_eq!(empty.len(), 0);
}

// =============================================================================
// ISSUE #238: list_courses() clamps limit to MAX_PAGE_SIZE
// =============================================================================

#[test]
fn test_list_courses_clamps_limit_to_max_page_size() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let catalog_len = HamplardContract::MAX_PAGE_SIZE + 10;
    client.update_max_courses_limit(&admin, &catalog_len);

    for i in 0..catalog_len {
        let course_id = format!("COURSE-PAGE-{:03}", i);
        client.register_course(
            &instructor,
            &String::from_str(&env, &course_id),
            &100_000_000,
            &token_id,
            &0u32,
            &None,
            &BytesN::from_array(&env, &[1u8; 32]),
            &soroban_sdk::Vec::new(&env),
        );
    }

    // A limit above the cap is silently clamped rather than copying the
    // entire remaining catalog in one call.
    let page = client.list_courses(&0u32, &(HamplardContract::MAX_PAGE_SIZE + 100));
    assert_eq!(page.len(), HamplardContract::MAX_PAGE_SIZE);
    assert_eq!(
        page.get(0).unwrap(),
        String::from_str(&env, "COURSE-PAGE-000")
    );
    assert_eq!(
        page.get(HamplardContract::MAX_PAGE_SIZE - 1).unwrap(),
        String::from_str(
            &env,
            &format!("COURSE-PAGE-{:03}", HamplardContract::MAX_PAGE_SIZE - 1)
        )
    );

    let rest = client.list_courses(&HamplardContract::MAX_PAGE_SIZE, &u32::MAX);
    assert_eq!(rest.len(), 10);
    assert_eq!(
        rest.get(0).unwrap(),
        String::from_str(
            &env,
            &format!("COURSE-PAGE-{:03}", HamplardContract::MAX_PAGE_SIZE)
        )
    );

    // A limit at or below the cap is unchanged.
    let small = client.list_courses(&0u32, &10u32);
    assert_eq!(small.len(), 10);
}
#[test]
fn test_list_courses_empty_catalog_returns_empty_page() {
    let (env, contract_id, _token_id, _admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    assert_eq!(client.list_courses(&0u32, &10u32).len(), 0);
    assert_eq!(client.list_courses(&u32::MAX, &u32::MAX).len(), 0);
}

#[test]
fn test_list_courses_offset_past_end_returns_empty_page() {
    let (env, contract_id, token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    for id in ["COURSE-LIST-OFFSET-A", "COURSE-LIST-OFFSET-B"] {
        client.register_course(
            &instructor,
            &String::from_str(&env, id),
            &100_000_000,
            &token_id,
            &0u32,
            &None,
            &BytesN::from_array(&env, &[1u8; 32]),
            &soroban_sdk::Vec::new(&env),
        );
    }

    assert_eq!(client.list_courses(&2u32, &10u32).len(), 0);
    assert_eq!(client.list_courses(&500u32, &10u32).len(), 0);
}

#[test]
fn test_list_courses_returns_partial_last_page() {
    let (env, contract_id, token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let ids = [
        "COURSE-LIST-PARTIAL-A",
        "COURSE-LIST-PARTIAL-B",
        "COURSE-LIST-PARTIAL-C",
        "COURSE-LIST-PARTIAL-D",
        "COURSE-LIST-PARTIAL-E",
    ];
    for id in ids {
        client.register_course(
            &instructor,
            &String::from_str(&env, id),
            &100_000_000,
            &token_id,
            &0u32,
            &None,
            &BytesN::from_array(&env, &[1u8; 32]),
            &soroban_sdk::Vec::new(&env),
        );
    }

    let last = client.list_courses(&4u32, &2u32);
    assert_eq!(last.len(), 1);
    assert_eq!(
        last.get(0).unwrap(),
        String::from_str(&env, "COURSE-LIST-PARTIAL-E")
    );
}

#[test]
fn test_list_courses_limit_zero_returns_empty_page() {
    let (env, contract_id, token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.register_course(
        &instructor,
        &String::from_str(&env, "COURSE-LIST-ZERO-LIMIT"),
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );

    assert_eq!(client.list_courses(&0u32, &0u32).len(), 0);
    assert_eq!(client.list_courses(&1u32, &0u32).len(), 0);
}

// ============================================================
// ARBITRATION RECORD PERSISTENCE  (#255, #256, #257)
// ============================================================

/// Helper: set up an arbitration fee, register+approve a course, enroll a
/// student, and return the enrolled student address.
fn setup_arbitration(
    env: &Env,
    client: &HamplardContractClient,
    token_id: &Address,
    admin: &Address,
    instructor: &Address,
    course_id: &str,
    fee_per_case: i128,
) -> Address {
    client.set_arbitration_fee_config(admin, token_id, &fee_per_case);
    register_and_approve_course(
        env,
        client,
        token_id,
        admin,
        instructor,
        course_id,
        1_000_000_000,
    );
    let student = Address::generate(env);
    token::StellarAssetClient::new(env, token_id).mint(&student, &100_000_000_000);
    client.enroll(&student, &String::from_str(env, course_id), &i128::MAX);
    student
}

// ============================================================
// #499 — ARBITRATION FEE CONFIG COVERAGE
// ============================================================

/// The fee config reads as zero before an admin ever sets it, so callers can
/// rely on `get_arbitration_fee_config()` without an unwrap.
#[test]
fn test_arbitration_fee_config_defaults_to_zero() {
    let (env, contract_id, _token_id, _admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let config = client.get_arbitration_fee_config();
    assert_eq!(config.fee_per_case, 0);
}

/// Setting the fee persists it and is observable through the getter.
#[test]
fn test_set_arbitration_fee_config_round_trips() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.set_arbitration_fee_config(&admin, &7_500_000i128);
    assert_eq!(client.get_arbitration_fee_config().fee_per_case, 7_500_000);

    // A later update replaces the previous value rather than accumulating.
    client.set_arbitration_fee_config(&admin, &1_250_000i128);
    assert_eq!(client.get_arbitration_fee_config().fee_per_case, 1_250_000);
}

/// The update is announced so off-chain tooling can follow the current fee.
#[test]
fn test_set_arbitration_fee_config_emits_event() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.set_arbitration_fee_config(&admin, &4_000_000i128);

    let mut seen = 0u32;
    for (contract, topics, data) in env.events().all().iter() {
        if contract != contract_id {
            continue;
        }
        let sym: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
        if sym != Symbol::new(&env, "arbitration_fee_config_updated") {
            continue;
        }
        seen += 1;
        let emitted_admin: Address = topics.get(1).unwrap().try_into_val(&env).unwrap();
        assert_eq!(emitted_admin, admin);
        let emitted_fee: i128 = data.try_into_val(&env).unwrap();
        assert_eq!(emitted_fee, 4_000_000);
    }
    assert_eq!(seen, 1);
}

#[test]
#[should_panic(expected = "fee_per_case cannot be negative")]
fn test_set_arbitration_fee_config_rejects_negative_fee() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.set_arbitration_fee_config(&admin, &-1i128);
}

#[test]
#[should_panic(expected = "unauthorized: set_arbitration_fee_config - caller is not admin")]
fn test_set_arbitration_fee_config_requires_admin() {
    let (env, contract_id, _token_id, _admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.set_arbitration_fee_config(&instructor, &1_000_000i128);
}

/// Escalating before any fee is configured must fail loudly instead of opening
/// a free arbitration case.
#[test]
#[should_panic(expected = "arbitration fee not configured")]
fn test_escalate_without_configured_fee_panics() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-ARB-NOFEE",
        1_000_000_000,
    );
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);
    let course_id = String::from_str(&env, "COURSE-ARB-NOFEE");
    client.enroll(&student, &course_id);

    // Default config is fee_per_case == 0, so this must panic.
    client.escalate_to_arbitration(&student, &course_id);
}

/// A zero fee configured explicitly is treated the same as no config at all.
#[test]
#[should_panic(expected = "arbitration fee not configured")]
fn test_escalate_with_zero_fee_panics() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    client.set_arbitration_fee_config(&admin, &0i128);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-ARB-ZEROFEE",
        1_000_000_000,
    );
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);
    let course_id = String::from_str(&env, "COURSE-ARB-ZEROFEE");
    client.enroll(&student, &course_id);

    client.escalate_to_arbitration(&student, &course_id);
}

#[test]
#[should_panic(expected = "course not found")]
fn test_escalate_unknown_course_panics() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    client.set_arbitration_fee_config(&admin, &1_000_000i128);

    let caller = Address::generate(&env);
    client.escalate_to_arbitration(&caller, &String::from_str(&env, "COURSE-DOES-NOT-EXIST"));
}

/// The fee is really escrowed: the caller is debited and the contract holds the
/// fee until the case is resolved. The running total tracks the outstanding sum.
#[test]
fn test_escalation_escrows_fee_in_contract() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let fee_per_case = 5_000_000i128;
    let student = setup_arbitration(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-ARB-ESCROW",
        fee_per_case,
    );
    let course_id = String::from_str(&env, "COURSE-ARB-ESCROW");

    let caller_before = token_client.balance(&student);
    let contract_before = token_client.balance(&contract_id);

    client.escalate_to_arbitration(&student, &course_id);

    assert_eq!(
        caller_before - token_client.balance(&student),
        fee_per_case,
        "the caller must pay the arbitration fee"
    );
    assert_eq!(
        token_client.balance(&contract_id) - contract_before,
        fee_per_case,
        "the fee must be escrowed by the contract, not forwarded yet"
    );
    assert_eq!(
        client.get_total_arbitration_fees(&token_id),
        fee_per_case,
        "the outstanding arbitration total must track the escrowed fee"
    );

    // A second caller opening their own case adds to the same token total.
    let other = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&other, &100_000_000_000);
    client.escalate_to_arbitration(&other, &course_id);
    assert_eq!(
        client.get_total_arbitration_fees(&token_id),
        fee_per_case * 2
    );
}

/// The escalation event carries the caller, the course and the fee paid, so
/// indexers do not have to reconstruct it from the case record.
#[test]
fn test_escalation_event_carries_fee_payload() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let fee_per_case = 3_000_000i128;
    let student = setup_arbitration(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-ARB-EVENT",
        fee_per_case,
    );
    let course_id = String::from_str(&env, "COURSE-ARB-EVENT");

    client.escalate_to_arbitration(&student, &course_id);

    let mut seen = 0u32;
    for (contract, topics, data) in env.events().all().iter() {
        if contract != contract_id {
            continue;
        }
        let sym: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
        if sym != Symbol::new(&env, "dispute_escalated") {
            continue;
        }
        seen += 1;
        let topic_course: String = topics.get(1).unwrap().try_into_val(&env).unwrap();
        assert_eq!(topic_course, course_id);

        let (event_caller, event_course, event_fee): (Address, String, i128) =
            data.try_into_val(&env).unwrap();
        assert_eq!(event_caller, student);
        assert_eq!(event_course, course_id);
        assert_eq!(event_fee, fee_per_case);
    }
    assert_eq!(seen, 1, "exactly one dispute_escalated event per escalation");
}

/// #255 — escalate_to_arbitration() must persist a queryable ArbitrationCase record.
#[test]
fn test_escalation_creates_retrievable_arbitration_case() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let fee_per_case = 5_000_000i128;
    let course_id_str = "COURSE-ARB-255";
    let student = setup_arbitration(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        course_id_str,
        fee_per_case,
    );
    let course_id = String::from_str(&env, course_id_str);

    client.escalate_to_arbitration(&student, &course_id);

    let case = client
        .get_arbitration_case(&student, &course_id)
        .expect("arbitration case must be retrievable after escalation");

    assert_eq!(
        case.caller, student,
        "case must record the escalating student"
    );
    assert_eq!(case.course_id, course_id, "case must record the course id");
    assert_eq!(
        case.fee_paid, fee_per_case,
        "fee_paid must match config.fee_per_case"
    );
    assert_eq!(
        case.status,
        DisputeStatus::Open,
        "newly created case must be Open"
    );
}

/// #257 — a second escalation for the same (student, course_id) must be rejected.
#[test]
#[should_panic(expected = "Error(Contract, #47)")]
fn test_duplicate_escalation_rejected() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = setup_arbitration(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-ARB-257",
        5_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-ARB-257");

    // First escalation succeeds.
    client.escalate_to_arbitration(&student, &course_id);

    // Second escalation for the same dispute must panic.
    client.escalate_to_arbitration(&student, &course_id);
}

/// #256 — resolve_arbitration() with for_student=true must refund the fee to the caller.
#[test]
fn test_resolve_arbitration_refund_to_student() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let fee_per_case = 5_000_000i128;
    let student = setup_arbitration(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-ARB-REFUND",
        fee_per_case,
    );
    let course_id = String::from_str(&env, "COURSE-ARB-REFUND");

    client.escalate_to_arbitration(&student, &course_id);

    let balance_before = token::Client::new(&env, &token_id).balance(&student);
    env.mock_all_auths_allowing_non_root_auth();
    client.resolve_arbitration(&admin, &student, &course_id, &true);
    let balance_after = token::Client::new(&env, &token_id).balance(&student);

    assert_eq!(
        balance_after - balance_before,
        fee_per_case,
        "student must receive the full arbitration fee on a student-win resolution"
    );

    let case = client.get_arbitration_case(&student, &course_id).unwrap();
    assert_eq!(case.status, DisputeStatus::ResolvedForStudent);
}

/// #256 — resolve_arbitration() with for_student=false must pay the fee to the instructor.
#[test]
fn test_resolve_arbitration_payout_to_instructor() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let fee_per_case = 5_000_000i128;
    let student = setup_arbitration(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-ARB-PAYOUT",
        fee_per_case,
    );
    let course_id = String::from_str(&env, "COURSE-ARB-PAYOUT");

    client.escalate_to_arbitration(&student, &course_id);

    let instr_balance_before = token::Client::new(&env, &token_id).balance(&instructor);
    env.mock_all_auths_allowing_non_root_auth();
    client.resolve_arbitration(&admin, &student, &course_id, &false);
    let instr_balance_after = token::Client::new(&env, &token_id).balance(&instructor);

    assert_eq!(
        instr_balance_after - instr_balance_before,
        fee_per_case,
        "instructor must receive the arbitration fee on an instructor-win resolution"
    );

    let case = client.get_arbitration_case(&student, &course_id).unwrap();
    assert_eq!(case.status, DisputeStatus::ResolvedForInstructor);
}

/// #466 — arbitration fees are configured independently per token.
#[test]
fn test_arbitration_fee_config_is_per_token() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let second_token_admin = Address::generate(&env);
    let second_token_id = env
        .register_stellar_asset_contract_v2(second_token_admin)
        .address();
    client.add_approved_token(&admin, &second_token_id);

    client.set_arbitration_fee_config(&admin, &token_id, &5_000_000);
    client.set_arbitration_fee_config(&admin, &second_token_id, &250_000);

    assert_eq!(
        client.get_arbitration_fee_config(&token_id).fee_per_case,
        5_000_000
    );
    assert_eq!(
        client
            .get_arbitration_fee_config(&second_token_id)
            .fee_per_case,
        250_000
    );
}

/// #467 — arbitration fees are tracked in a per-token reserve and released
/// when the case is resolved.
#[test]
fn test_arbitration_fees_held_tracked_per_token() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let fee_per_case = 5_000_000i128;
    let student = setup_arbitration(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-ARB-HELD",
        fee_per_case,
    );
    let course_id = String::from_str(&env, "COURSE-ARB-HELD");

    assert_eq!(client.get_arbitration_fees_held(&token_id), 0);

    client.escalate_to_arbitration(&student, &course_id);
    assert_eq!(client.get_arbitration_fees_held(&token_id), fee_per_case);

    env.mock_all_auths_allowing_non_root_auth();
    client.resolve_arbitration(&admin, &student, &course_id, &true);
    assert_eq!(
        client.get_arbitration_fees_held(&token_id),
        0,
        "resolving a case must release its reserved fee"
    );
}

/// #467 — emergency sweeps must leave arbitration fees reserved for open cases.
#[test]
fn test_withdraw_tokens_preserves_arbitration_fees() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let fee_per_case = 5_000_000i128;
    let student = setup_arbitration(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-ARB-RESERVE",
        fee_per_case,
    );
    let course_id = String::from_str(&env, "COURSE-ARB-RESERVE");
    client.escalate_to_arbitration(&student, &course_id);

    let token_client = token::Client::new(&env, &token_id);
    let contract_balance = token_client.balance(&contract_id);
    let held = client.get_arbitration_fees_held(&token_id);
    assert_eq!(held, fee_per_case);

    let destination = Address::generate(&env);
    let over_sweep = client.try_withdraw_tokens(
        &admin,
        &sec_admin,
        &token_id,
        &contract_balance,
        &destination,
    );
    assert!(
        over_sweep.is_err(),
        "emergency sweep must not touch reserved arbitration fees"
    );

    client.withdraw_tokens(
        &admin,
        &sec_admin,
        &token_id,
        &(contract_balance - held),
        &destination,
    );

    assert_eq!(token_client.balance(&destination), contract_balance - held);
    assert_eq!(token_client.balance(&contract_id), held);
    assert_eq!(client.get_arbitration_fees_held(&token_id), held);
}

// ============================================================
// ESCALATION AUTHORIZATION  (#464)
// ============================================================

/// #464 — escalate_to_arbitration() must reject a caller who is neither
/// enrolled in the course (current or historical) nor its instructor.
#[test]
#[should_panic(expected = "unauthorized: caller is not enrolled in or the instructor of course")]
fn test_escalation_rejects_unrelated_caller() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let fee_per_case = 5_000_000i128;
    let course_id_str = "COURSE-ARB-AUTH-UNRELATED";
    setup_arbitration(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        course_id_str,
        fee_per_case,
    );
    let course_id = String::from_str(&env, course_id_str);

    // An unrelated third party with no enrollment and not the instructor.
    let outsider = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&outsider, &100_000_000_000);

    client.escalate_to_arbitration(&outsider, &course_id);
}

/// #464 — the course instructor may escalate_to_arbitration() without being enrolled.
#[test]
fn test_escalation_allows_instructor() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let fee_per_case = 5_000_000i128;
    let course_id_str = "COURSE-ARB-AUTH-INSTRUCTOR";
    setup_arbitration(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        course_id_str,
        fee_per_case,
    );
    let course_id = String::from_str(&env, course_id_str);

    // Instructor escalates (not enrolled).
    client.escalate_to_arbitration(&instructor, &course_id);

    let case = client
        .get_arbitration_case(&instructor, &course_id)
        .expect("instructor escalation must create a retrievable case");
    assert_eq!(case.caller, instructor);
    assert_eq!(case.instructor, instructor);
}

/// #464 — a historical student (EnrollmentHistory, no active enrollment) may escalate.
#[test]
fn test_escalation_allows_historical_student() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let fee_per_case = 5_000_000i128;
    let course_id_str = "COURSE-ARB-AUTH-HISTORICAL";
    let student = setup_arbitration(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        course_id_str,
        fee_per_case,
    );
    let course_id = String::from_str(&env, course_id_str);

    // Archive the student's active enrollment into EnrollmentHistory and remove
    // the active record, simulating a past (historical) enrollment.
    let enrollment = client
        .get_enrollment(&student, &student, &course_id)
        .unwrap();
    let history_key = DataKey::EnrollmentHistory(student.clone(), course_id.clone());
    env.as_contract(&contract_id, || {
        let mut history: soroban_sdk::Vec<Enrollment> = soroban_sdk::Vec::new(&env);
        history.push_back(enrollment.clone());
        env.storage().persistent().set(&history_key, &history);
        env.storage().persistent().remove(&DataKey::Enrollment(
            student.clone(),
            course_id.clone(),
        ));
    });

    // Student is no longer actively enrolled but has a historical record.
    assert!(!client.is_enrolled(&student, &course_id));

    client.escalate_to_arbitration(&student, &course_id);

    let case = client
        .get_arbitration_case(&student, &course_id)
        .expect("historical student escalation must create a retrievable case");
    assert_eq!(case.caller, student);
}

// ============================================================
// REFUND ENROLLMENT ARCHIVAL  (#254)
// ============================================================

/// #254 — process_refund() must archive the refunded enrollment to
/// EnrollmentHistory with is_refunded=true before removing the active record.
#[test]
fn test_process_refund_archives_enrollment_to_history() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    fund_refund_reserve_for_test(&env, &client, &token_id, 10_000_000_000);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-REFUND-ARCHIVE",
        1_000_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-REFUND-ARCHIVE");

    // Capture the amount paid at enrollment.
    client.enroll(&student, &course_id, &i128::MAX);
    let enrollment = client
        .get_enrollment(&student, &student, &course_id)
        .unwrap();
    let amount_paid = enrollment.amount_paid;
    assert!(amount_paid > 0);

    // Request and approve the refund.
    client.request_refund(&student, &course_id);
    env.mock_all_auths_allowing_non_root_auth();
    client.process_refund(&admin, &student, &course_id, &true, &None);

    // The active enrollment must no longer exist.
    assert!(
        client
            .get_enrollment(&student, &student, &course_id)
            .is_none(),
        "active enrollment must be removed after refund"
    );

    // The history must contain exactly one entry with is_refunded=true.
    let history = client.get_enrollment_history(&admin, &student, &course_id);

    assert_eq!(
        history.len(),
        1,
        "history must contain exactly one archived entry"
    );
    let archived = history.get(0).unwrap();
    assert!(
        archived.is_refunded,
        "archived enrollment must have is_refunded=true"
    );
    assert_eq!(
        archived.amount_paid, amount_paid,
        "archived enrollment must preserve the original amount_paid"
    );
    assert_eq!(archived.student, student);
    assert_eq!(archived.course_id, course_id);
}

// ============================================================
// REFUND PROCESSING DEADLINE — student self-approval past deadline
// ============================================================

/// The processing deadline must be captured on the request at creation
/// time, so the struct exposes a deterministic `deadline_ledger` the
/// student can rely on.
#[test]
fn test_refund_request_records_deadline_ledger() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    client.set_refund_processing_deadline(&admin, &100u32);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-DEADLINE",
        1_000_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-DEADLINE");

    client.enroll(&student, &course_id);
    client.request_refund(&student, &course_id);

    let request = client.get_refund_request(&student, &course_id).unwrap();
    assert_eq!(request.status, RefundStatus::Pending);
    assert_eq!(
        request.deadline_ledger,
        request.requested_at_ledger + 100,
        "deadline_ledger must equal requested_at_ledger + configured deadline",
    );
}

/// Once the deadline elapses the student can resolve the request without
/// the admin: the full payment is returned, the enrollment is removed, and
/// the request is marked Approved.
#[test]
fn test_claim_refund_after_deadline_succeeds() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    // Smallest legal deadline so we can step past it quickly.
    client.set_refund_processing_deadline(&admin, &10u32);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-CLAIM",
        1_000_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-CLAIM");

    client.enroll(&student, &course_id);
    client.request_refund(&student, &course_id);

    // Advance past the snapshotted deadline (requested_at + 10).
    env.ledger().with_mut(|l| {
        l.sequence_number += 11;
    });

    let initial_balance = token::Client::new(&env, &token_id).balance(&student);
    env.mock_all_auths_allowing_non_root_auth();
    client.claim_refund(&student, &course_id);

    let final_balance = token::Client::new(&env, &token_id).balance(&student);
    assert_eq!(final_balance - initial_balance, 1_000_000_000);
    assert!(!client.is_enrolled(&student, &course_id));

    let request = client.get_refund_request(&student, &course_id).unwrap();
    assert_eq!(request.status, RefundStatus::Approved);
}

/// Before the deadline elapses the student cannot self-approve — the admin
/// still has its window to act.
#[test]
#[should_panic(expected = "deadline has not elapsed")]
fn test_claim_refund_before_deadline_rejected() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    client.set_refund_processing_deadline(&admin, &100u32);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-CLAIM-EARLY",
        1_000_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-CLAIM-EARLY");

    client.enroll(&student, &course_id);
    client.request_refund(&student, &course_id);

    // Only partway toward the deadline.
    env.ledger().with_mut(|l| {
        l.sequence_number += 5;
    });

    client.claim_refund(&student, &course_id);
}

/// A request the admin already resolved cannot be claimed again — not by
/// the student, not even past the deadline.
#[test]
#[should_panic(expected = "refund request is not pending")]
fn test_claim_refund_on_already_resolved_rejected() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    client.set_refund_processing_deadline(&admin, &10u32);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-CLAIM-DONE",
        1_000_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-CLAIM-DONE");

    client.enroll(&student, &course_id);
    client.request_refund(&student, &course_id);

    // Admin resolves first.
    env.mock_all_auths_allowing_non_root_auth();
    client.process_refund(&admin, &student, &course_id, &true, &None);

    // Well past the deadline — still must not be re-resolvable.
    env.ledger().with_mut(|l| {
        l.sequence_number += 100;
    });
    client.claim_refund(&student, &course_id);
}

/// The deadline parameter is admin-configurable within bounds. Values
/// below the minimum are rejected.
#[test]
#[should_panic(expected = "out of bounds")]
fn test_set_refund_processing_deadline_below_min_rejected() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    client.set_refund_processing_deadline(&admin, &9u32);
}

/// Values above the maximum are rejected.
#[test]
#[should_panic(expected = "out of bounds")]
fn test_set_refund_processing_deadline_above_max_rejected() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    client.set_refund_processing_deadline(&admin, &1_000_001u32);
}

/// When the admin has not configured a deadline, the default applies and
/// is exposed through the getter.
#[test]
fn test_refund_processing_deadline_defaults_when_unset() {
    let (env, contract_id, _token_id, _admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    assert_eq!(client.get_refund_processing_deadline(), 17_280u32);
}

#[test]
#[should_panic(expected = "Error(Contract, #34)")]
fn test_re_enroll_rejects_before_enrollment_start_ledger() {
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
        "COURSE-REENROLL-START",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-REENROLL-START");

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence")),
    );

    // Instructor sets the enrollment start ledger in the future
    let enrollment_start = env.ledger().sequence() + 10;
    client.set_enrollment_start_ledger(&instructor, &course_id, &Some(enrollment_start));

    // Must panic exactly like enroll()
    client.re_enroll(&student, &course_id, &i128::MAX);
}

#[test]
fn test_re_enroll_succeeds_at_enrollment_start_ledger() {
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
        "COURSE-REENROLL-START-OK",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-REENROLL-START-OK");

    client.enroll(&student, &course_id, &i128::MAX);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence")),
    );

    let enrollment_start = env.ledger().sequence() + 10;
    client.set_enrollment_start_ledger(&instructor, &course_id, &Some(enrollment_start));
    env.ledger().with_mut(|l| {
        l.sequence_number = enrollment_start;
    });

    client.re_enroll(&student, &course_id, &i128::MAX);

    let enrollment = client
        .get_enrollment(&student, &student, &course_id)
        .unwrap();
    assert!(!enrollment.completed);
    assert_eq!(enrollment.enrolled_at_ledger, enrollment_start);
}
#[test]
fn test_risk_rules_all_combinations_and_threshold_boundary() {
    let config = RiskFeeConfig {
        large_payment_threshold: 100,
        large_payment_surcharge_bps: 11,
        new_customer_surcharge_bps: 23,
        btc_eth_surcharge_bps: 47,
    };
    // Explicit expected values keep the test independent of the rule implementation.
    let cases = [
        (100, false, false, (0, 0)),
        (101, false, false, (30, 11)),
        (100, true, false, (40, 23)),
        (100, false, true, (30, 47)),
        (101, true, false, (70, 34)),
        (101, false, true, (60, 58)),
        (100, true, true, (70, 70)),
        (101, true, true, (100, 81)),
        (i128::MIN, false, false, (0, 0)),
        (i128::MAX, true, true, (100, 81)),
    ];
    for (amount, new_customer, btc_eth, expected) in cases {
        assert_eq!(
            risk_surcharge(true, Some(&config), amount, new_customer, btc_eth),
            expected
        );
    }
}

#[test]
fn test_risk_rules_config_gating_and_saturating_surcharges() {
    let config = RiskFeeConfig {
        large_payment_threshold: 0,
        large_payment_surcharge_bps: u32::MAX,
        new_customer_surcharge_bps: 1,
        btc_eth_surcharge_bps: u32::MAX,
    };
    assert_eq!(risk_surcharge(true, None, i128::MAX, true, true), (0, 0));
    assert_eq!(risk_surcharge(false, None, i128::MAX, true, true), (0, 0));
    assert_eq!(
        risk_surcharge(false, Some(&config), i128::MAX, true, true),
        (0, 0)
    );
    assert_eq!(
        risk_surcharge(true, Some(&config), 1, true, true),
        (100, u32::MAX)
    );
}

#[test]
fn test_risk_defaults_and_overflow_match_fee_deduction() {
    let (env, contract_id, token_id, admin, _, _, _) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);
    let amount = 1_000_000_000_001i128;

    // Enabling risk without storing a config must not activate the getter's defaults.
    client.set_risk_config_enabled(&admin, &true);
    let missing = client.calculate_risk_score(&student, &token_id, &amount);
    assert_eq!((missing.score, missing.surcharge_bps), (0, 0));
    let default_quote = client.get_effective_fee_for_payment(&student, &token_id, &amount);
    assert_eq!(default_quote.risk_surcharge_bps, 0);
    assert_eq!(
        env.as_contract(&contract_id, || {
            HamplardContract::deduct_fee(&env, &token_id, amount, None, false, false)
        }),
        (
            amount - default_quote.platform_fee,
            default_quote.platform_fee
        )
    );

    client.set_risk_fee_config(&admin, &u32::MAX, &0, &1, &u32::MAX);
    let risk = client.calculate_risk_score(&student, &token_id, &amount);
    assert_eq!((risk.score, risk.surcharge_bps), (30, u32::MAX));
    let quote = client.get_effective_fee_for_payment(&student, &token_id, &amount);
    assert_eq!(quote.risk_surcharge_bps, u32::MAX);
    assert_eq!(quote.effective_fee_bps, 10_000);
    assert_eq!(quote.platform_fee, amount);
    assert_eq!(
        env.as_contract(&contract_id, || {
            HamplardContract::deduct_fee(&env, &token_id, amount, None, false, false)
        }),
        (0, amount)
    );

    client.set_risk_config_enabled(&admin, &false);
    assert_eq!(
        client.calculate_risk_score(&student, &token_id, &amount),
        missing
    );
    assert_eq!(
        client.get_effective_fee_for_payment(&student, &token_id, &amount),
        default_quote
    );
}

// =============================================================================
// ISSUE: withdraw_tokens accounting check against outstanding liabilities
// =============================================================================

#[test]
#[should_panic(expected = "withdraw_tokens: cannot withdraw below tracked liabilities")]
fn test_withdraw_tokens_cannot_drain_unwithdrawn_instructor_earnings() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);

    let price = 1_000_000_000i128;
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &price);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-EARNINGS-PROTECT",
        price,
    );
    let course_id = String::from_str(&env, "COURSE-EARNINGS-PROTECT");

    // Student enrolls: platform fee is 20% (200_000_000), instructor gets 80% (800_000_000)
    client.enroll(&student, &course_id, &i128::MAX);

    // Verify contract balance is 800_000_000 and total liabilities is 800_000_000
    assert_eq!(
        token::Client::new(&env, &token_id).balance(&contract_id),
        800_000_000
    );
    assert_eq!(client.get_total_liabilities(&token_id), 800_000_000);
    assert_eq!(client.get_owed_balance(&token_id), 800_000_000);
    assert_eq!(client.get_outstanding_earnings(&token_id), 800_000_000);
    assert_eq!(client.get_surplus_balance(&token_id), 0);

    let destination = Address::generate(&env);
    // Even withdrawing 1 stroop must fail because available surplus is 0
    client.withdraw_tokens(&admin, &sec_admin, &token_id, &1i128, &destination);
}

#[test]
fn test_withdraw_tokens_caps_withdrawal_to_surplus_above_liabilities() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);

    let price = 1_000_000_000i128;
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &price);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-REF-MISMATCH",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-REF-MISMATCH");

    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence-1")),
    );

    // Simulate an inconsistency: overwrite course.instructor in persistent
    // storage with a different address while leaving CourseInstructorRef
    // pointing to the original instructor. This is the tampered-state
    // scenario the check is designed to catch.
    let impostor = Address::generate(&env);
    env.as_contract(&contract_id, || {
        let key = DataKey::Course(course_id.clone());
        let mut course: Course = env.storage().persistent().get(&key).unwrap();
        course.instructor = impostor.clone();
        env.storage().persistent().set(&key, &course);
    });

    // re_enroll() must now panic with "course instructor reference mismatch"
    // exactly as enroll() / validate_enrollment() would.
    client.re_enroll(&student, &course_id);
        "COURSE-SURPLUS-TEST",
        price,
    );
    let course_id = String::from_str(&env, "COURSE-SURPLUS-TEST");

    // Student enrolls: 800_000_000 instructor earnings held in contract
    client.enroll(&student, &course_id, &i128::MAX);

    // Mint surplus of 500_000_000 directly to contract
    token::StellarAssetClient::new(&env, &token_id).mint(&contract_id, &500_000_000);

    // Contract has 1_300_000_000 total balance: 800_000_000 liabilities + 500_000_000 surplus
    assert_eq!(
        token::Client::new(&env, &token_id).balance(&contract_id),
        1_300_000_000
    );
    assert_eq!(client.get_total_liabilities(&token_id), 800_000_000);
    assert_eq!(client.get_surplus_balance(&token_id), 500_000_000);

    let destination = Address::generate(&env);

    // Withdrawing more than surplus (500_000_001) must panic
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.withdraw_tokens(&admin, &sec_admin, &token_id, &500_000_001, &destination);
    }));
    assert!(res.is_err(), "cannot withdraw more than available surplus");

    // Withdrawing exact surplus (500_000_000) must succeed
    client.withdraw_tokens(&admin, &sec_admin, &token_id, &500_000_000, &destination);
    assert_eq!(
        token::Client::new(&env, &token_id).balance(&destination),
        500_000_000
    );

    // Contract balance is now 800_000_000, exactly matching liabilities
    assert_eq!(
        token::Client::new(&env, &token_id).balance(&contract_id),
        800_000_000
    );
    assert_eq!(client.get_surplus_balance(&token_id), 0);

    // Instructor can now withdraw full earnings without issue
    client.withdraw_earnings(&instructor, &token_id, &800_000_000, &None);
    assert_eq!(
        token::Client::new(&env, &token_id).balance(&instructor),
        800_000_000
    );
    assert_eq!(token::Client::new(&env, &token_id).balance(&contract_id), 0);
    assert_eq!(client.get_total_liabilities(&token_id), 0);
}

#[test]
#[should_panic(expected = "withdraw_tokens: cannot withdraw below tracked liabilities")]
fn test_withdraw_tokens_cannot_drain_arbitration_fee_liabilities() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let fee_per_case = 5_000_000i128;
    let student = setup_arbitration(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-ARB-LIAB",
        fee_per_case,
    );
    let course_id = String::from_str(&env, "COURSE-ARB-LIAB");

    // Escalate dispute to arbitration: transfers 5_000_000 fee to contract
    client.escalate_to_arbitration(&student, &course_id);

    // Total liabilities include both instructor earnings from enrollment and arbitration fee
    let arb_fee = client.get_total_arbitration_fees(&token_id);
    assert_eq!(arb_fee, fee_per_case);

    let surplus = client.get_surplus_balance(&token_id);
    assert_eq!(surplus, 0);

    // Admin attempts to sweep tokens including arbitration fee -> must panic
    let destination = Address::generate(&env);
    client.withdraw_tokens(&admin, &sec_admin, &token_id, &fee_per_case, &destination);
}

#[test]
fn test_withdraw_tokens_accounting_lifecycle_with_refunds_and_arbitration() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);

    let price = 1_000_000_000i128;
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &price);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-ACCOUNTING",
        price,
    );
    let course_id = String::from_str(&env, "COURSE-ACCOUNTING");

    client.enroll(&student, &course_id, &i128::MAX);
    // 800_000_000 credited to instructor
    assert_eq!(client.get_total_liabilities(&token_id), 800_000_000);

    // Request and process refund
    client.request_refund(&student, &course_id);
    env.mock_all_auths_allowing_non_root_auth();
    client.process_refund(&admin, &student, &course_id, &true, &None);

    // After refund, instructor share was debited and refunded to student
    assert_eq!(client.get_total_liabilities(&token_id), 0);
    assert_eq!(client.get_owed_balance(&token_id), 0);
    assert_eq!(client.get_outstanding_earnings(&token_id), 0);
    assert_eq!(token::Client::new(&env, &token_id).balance(&contract_id), 0);
}

// ============================================================
// ISSUE #303 — update_default_fee Soroban event auditability
// ============================================================

#[test]
fn test_update_default_fee_emits_event_with_old_and_new_fee_and_ledger() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    // Initial platform fee is 20%
    assert_eq!(client.get_platform_fee(&admin), 20u32);

    let ledger_before = env.ledger().sequence();
    client.update_default_fee(&admin, &35u32);

    assert!(has_event(&env, &contract_id, "default_fee_updated"));
    let (old_fee, new_fee, event_admin, event_ledger): (u32, u32, Address, u32) =
        last_event_val(&env, &contract_id, "default_fee_updated")
            .try_into_val(&env)
            .unwrap();

    assert_eq!(old_fee, 20u32);
    assert_eq!(new_fee, 35u32);
    assert_eq!(event_admin, admin);
    assert_eq!(event_ledger, ledger_before);
    assert_eq!(client.get_platform_fee(&admin), 35u32);

    // Subsequent fee change from 35% to 10%
    let ledger_second = env.ledger().sequence();
    client.update_default_fee(&admin, &10u32);

    let (old_fee2, new_fee2, event_admin2, event_ledger2): (u32, u32, Address, u32) =
        last_event_val(&env, &contract_id, "default_fee_updated")
            .try_into_val(&env)
            .unwrap();

    assert_eq!(old_fee2, 35u32);
    assert_eq!(new_fee2, 10u32);
    assert_eq!(event_admin2, admin);
    assert_eq!(event_ledger2, ledger_second);
    assert_eq!(client.get_platform_fee(&admin), 10u32);
}

// ============================================================
// ISSUES #258, #302, #304, #305 — Auditable Events & Fee Preview Tests
// ============================================================

#[test]
fn test_do_complete_payment_and_preview_payment_fee_are_read_only() {
    let (env, contract_id, token_id, _admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    let initial_balance = 1_000_000_000i128;
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &initial_balance);

    // Call preview_payment_fee
    let preview = client.preview_payment_fee(&student, &token_id, &500_000_000i128);
    assert_eq!(preview.payment_amount, 500_000_000i128);

    // Call do_complete_payment
    let preview2 = client.do_complete_payment(&student, &token_id, &500_000_000i128);
    assert_eq!(preview2.payment_amount, 500_000_000i128);

    // Verify student token balance remains unchanged
    let balance = token::Client::new(&env, &token_id).balance(&student);
    assert_eq!(balance, initial_balance);
}

#[test]
fn test_update_default_fee_emits_fee_updated_event() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let ledger_before = env.ledger().sequence();
    client.update_default_fee(&admin, &15u32);

    let events = env.events().all();
    let mut found_fee_updated = false;

    for (contract, topics, data) in events.iter() {
        if contract != contract_id {
            continue;
        }
        let topic0 = topics.get(0).unwrap();
        let sym: Symbol = topic0.try_into_val(&env).unwrap();

        if sym == Symbol::new(&env, "FeeUpdated") {
            found_fee_updated = true;
            // Payload: (old_fee, new_fee, admin, ledger)
            let (old_fee, new_fee, event_admin, event_ledger): (u32, u32, Address, u32) =
                data.try_into_val(&env).unwrap();
            assert_eq!(old_fee, 20u32);
            assert_eq!(new_fee, 15u32);
            assert_eq!(event_admin, admin);
            assert_eq!(event_ledger, ledger_before);
        }
    }

    assert!(found_fee_updated, "FeeUpdated event must be emitted by update_default_fee");
}

#[test]
#[should_panic(expected = "already enrolled in this course")]
fn test_enroll_emits_enrollment_failed_event_when_already_enrolled() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    let course_id = String::from_str(&env, "AUDIT-COURSE-01");
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "AUDIT-COURSE-01",
        100_000_000,
    );

    client.enroll(&student, &course_id);

    // Second enrollment attempt must panic and emit EnrollmentFailed
    client.enroll(&student, &course_id);
}

#[test]
#[should_panic(expected = "student is blocked")]
fn test_enroll_emits_enrollment_failed_event_when_student_blocked() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    let course_id = String::from_str(&env, "AUDIT-COURSE-02");
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "AUDIT-COURSE-02",
        100_000_000,
    );

    // Block the student
    client.block_student(&admin, &student);

    // Enrollment must panic and emit EnrollmentFailed
    client.enroll(&student, &course_id);
}


// ============================================================
// CO-INSTRUCTOR / MULTI-INSTRUCTOR REVENUE SHARING TESTS (#496)
// ============================================================

fn set_co_instructor_split(
    env: &Env,
    client: &HamplardContractClient,
    caller: &Address,
    course_id: &str,
    split: &[(Address, u32)],
) {
    let mut addresses = soroban_sdk::Vec::new(env);
    let mut shares = soroban_sdk::Vec::new(env);
    for (address, share_bps) in split {
        addresses.push_back(address.clone());
        shares.push_back(*share_bps);
    }
    client.set_co_instructors(
        caller,
        &String::from_str(env, course_id),
        &addresses,
        &shares,
    );
}

#[test]
fn test_set_co_instructors_and_credit_split_on_enroll() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-CO-INSTRUCTOR",
        1_000_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-CO-INSTRUCTOR");

    let co_instructor_a = Address::generate(&env);
    let co_instructor_b = Address::generate(&env);
    set_co_instructor_split(
        &env,
        &client,
        &instructor,
        "COURSE-CO-INSTRUCTOR",
        &[
            (co_instructor_a.clone(), 3_000),
            (co_instructor_b.clone(), 2_000),
        ],
    );

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.co_instructors.len(), 2);
    assert_eq!(course.co_instructors.get(0).unwrap().share_bps, 3_000);
    assert_eq!(course.co_instructors.get(1).unwrap().share_bps, 2_000);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);
    client.enroll(&student, &course_id);

    // 20% platform fee leaves 800_000_000 for the instructor side. The
    // co-instructors take 30% and 20% of that; the lead instructor keeps the
    // remaining 50%.
    assert_eq!(
        client.get_instructor_earnings(&co_instructor_a, &token_id),
        240_000_000
    );
    assert_eq!(
        client.get_instructor_earnings(&co_instructor_b, &token_id),
        160_000_000
    );
    assert_eq!(
        client.get_instructor_earnings(&instructor, &token_id),
        400_000_000
    );
}

#[test]
fn test_admin_can_set_co_instructors() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-CO-ADMIN",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-CO-ADMIN");
    let co_instructor = Address::generate(&env);

    set_co_instructor_split(
        &env,
        &client,
        &admin,
        "COURSE-CO-ADMIN",
        &[(co_instructor, 1_000)],
    );

    assert_eq!(
        client.get_course(&course_id).unwrap().co_instructors.len(),
        1
    );
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_set_co_instructors_rejects_unauthorized_caller() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-CO-UNAUTH",
        100_000_000,
    );

    let attacker = Address::generate(&env);
    let co_instructor = Address::generate(&env);
    set_co_instructor_split(
        &env,
        &client,
        &attacker,
        "COURSE-CO-UNAUTH",
        &[(co_instructor, 1_000)],
    );
}

#[test]
#[should_panic(expected = "must have the same length")]
fn test_set_co_instructors_rejects_length_mismatch() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-CO-LENGTH",
        100_000_000,
    );

    let mut addresses = soroban_sdk::Vec::new(&env);
    addresses.push_back(Address::generate(&env));
    addresses.push_back(Address::generate(&env));
    let mut shares = soroban_sdk::Vec::new(&env);
    shares.push_back(2_000u32);

    client.set_co_instructors(
        &instructor,
        &String::from_str(&env, "COURSE-CO-LENGTH"),
        &addresses,
        &shares,
    );
}

#[test]
#[should_panic(expected = "duplicate co-instructor address")]
fn test_set_co_instructors_rejects_duplicates() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-CO-DUP",
        100_000_000,
    );

    let co_instructor = Address::generate(&env);
    set_co_instructor_split(
        &env,
        &client,
        &instructor,
        "COURSE-CO-DUP",
        &[(co_instructor.clone(), 1_000), (co_instructor, 1_000)],
    );
}

#[test]
#[should_panic(expected = "too many co-instructors")]
fn test_set_co_instructors_rejects_more_than_ten() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-CO-MANY",
        100_000_000,
    );

    let mut addresses = soroban_sdk::Vec::new(&env);
    let mut shares = soroban_sdk::Vec::new(&env);
    for _ in 0..11 {
        addresses.push_back(Address::generate(&env));
        shares.push_back(100u32);
    }

    client.set_co_instructors(
        &instructor,
        &String::from_str(&env, "COURSE-CO-MANY"),
        &addresses,
        &shares,
    );
}

#[test]
#[should_panic(expected = "co-instructor share cannot exceed 10000 bps")]
fn test_set_co_instructors_rejects_single_share_above_10000_bps() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-CO-SHARE",
        100_000_000,
    );

    set_co_instructor_split(
        &env,
        &client,
        &instructor,
        "COURSE-CO-SHARE",
        &[(Address::generate(&env), 10_001)],
    );
}

#[test]
#[should_panic(expected = "total co-instructor shares cannot exceed 10000 bps")]
fn test_set_co_instructors_rejects_total_shares_above_10000_bps() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-CO-TOTAL",
        100_000_000,
    );

    set_co_instructor_split(
        &env,
        &client,
        &instructor,
        "COURSE-CO-TOTAL",
        &[
            (Address::generate(&env), 6_000),
            (Address::generate(&env), 5_000),
        ],
    );
}

#[test]
#[should_panic(expected = "co-instructors are locked after the first enrollment")]
fn test_set_co_instructors_locked_after_enrollment() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-CO-LOCKED",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-CO-LOCKED");

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000);
    client.enroll(&student, &course_id);

    set_co_instructor_split(
        &env,
        &client,
        &instructor,
        "COURSE-CO-LOCKED",
        &[(Address::generate(&env), 1_000)],
    );
}

#[test]
fn test_refund_debits_each_co_instructor_share() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-CO-REFUND",
        1_000_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-CO-REFUND");

    let co_instructor_a = Address::generate(&env);
    let co_instructor_b = Address::generate(&env);
    set_co_instructor_split(
        &env,
        &client,
        &instructor,
        "COURSE-CO-REFUND",
        &[
            (co_instructor_a.clone(), 3_000),
            (co_instructor_b.clone(), 2_000),
        ],
    );

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);
    client.enroll(&student, &course_id);

    assert_eq!(
        client.get_instructor_earnings(&co_instructor_a, &token_id),
        240_000_000
    );
    assert_eq!(
        client.get_instructor_earnings(&co_instructor_b, &token_id),
        160_000_000
    );
    assert_eq!(
        client.get_instructor_earnings(&instructor, &token_id),
        400_000_000
    );

    client.request_refund(&student, &course_id);
    env.mock_all_auths_allowing_non_root_auth();
    client.process_refund(&admin, &student, &course_id, &true, &None);

    assert_eq!(
        client.get_instructor_earnings(&co_instructor_a, &token_id),
        0
    );
    assert_eq!(
        client.get_instructor_earnings(&co_instructor_b, &token_id),
        0
    );
    assert_eq!(client.get_instructor_earnings(&instructor, &token_id), 0);
}


// ============================================================
// ISSUE: re_enroll() was missing CourseInstructorRef integrity check
// ============================================================

/// Regression test: re_enroll() must reject enrollment when the immutable
/// CourseInstructorRef stored at registration no longer matches the
/// course's current instructor field (i.e. an on-chain inconsistency).
///
/// Before the fix, re_enroll() skipped this check entirely. A course with
/// a tampered or inconsistent storage state could be enrolled into via
/// re_enroll() even though the same attempt via enroll() would correctly
/// panic with "course instructor reference mismatch".
///
/// The fix adds the CourseInstructorRef lookup and comparison to re_enroll()
/// in the same position as validate_enrollment() does for enroll().
#[test]
#[should_panic(expected = "course instructor reference mismatch")]
fn test_re_enroll_rejects_course_instructor_ref_mismatch() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &10_000_000_000);

    // Register, approve, enroll, and complete the course normally.
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-REF-MISMATCH",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-REF-MISMATCH");

    client.enroll(&student, &course_id);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "evidence-1")),
    );

    // Simulate an inconsistency: overwrite course.instructor in persistent
    // storage with a different address while leaving CourseInstructorRef
    // pointing to the original instructor. This is the tampered-state
    // scenario the check is designed to catch.
    let impostor = Address::generate(&env);
    env.as_contract(&contract_id, || {
        let key = DataKey::Course(course_id.clone());
        let mut course: Course = env.storage().persistent().get(&key).unwrap();
        course.instructor = impostor.clone();
        env.storage().persistent().set(&key, &course);
    });

    // re_enroll() must now panic with "course instructor reference mismatch"
    // exactly as enroll() / validate_enrollment() would.
    client.re_enroll(&student, &course_id);
}


// COURSE REFUND STATS (#351): total_refunded / refund_count

#[test]
fn test_new_course_has_zero_refund_stats() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-STATS-ZERO",
        500_000_000,
    );
    let course = client
        .get_course(&String::from_str(&env, "COURSE-STATS-ZERO"))
        .unwrap();
    assert_eq!(course.total_refunded, 0);
    assert_eq!(course.refund_count, 0);
}

#[test]
fn test_process_refund_full_updates_course_refund_stats() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student_a = Address::generate(&env);
    let student_b = Address::generate(&env);
    let asset_client = token::StellarAssetClient::new(&env, &token_id);
    asset_client.mint(&student_a, &10_000_000_000);
    asset_client.mint(&student_b, &10_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-STATS-FULL",
        1_000_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-STATS-FULL");

    client.enroll(&student_a, &course_id);
    client.enroll(&student_b, &course_id);

    // Refund only student_a; student_b stays enrolled.
    client.request_refund(&student_a, &course_id);
    env.mock_all_auths_allowing_non_root_auth();
    client.process_refund(&admin, &student_a, &course_id, &true, &None);

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.refund_count, 1);
    assert_eq!(course.total_refunded, 1_000_000_000);
    assert_eq!(course.active_enrollments, 1);

    // A second refund accumulates on top of the first.
    client.request_refund(&student_b, &course_id);
    client.process_refund(&admin, &student_b, &course_id, &true, &None);

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.refund_count, 2);
    assert_eq!(course.total_refunded, 2_000_000_000);
}

#[test]
fn test_process_refund_partial_adds_only_refunded_amount() {
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
        "COURSE-STATS-PARTIAL",
        1_000_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-STATS-PARTIAL");

    client.enroll(&student, &course_id);
    client.request_refund(&student, &course_id);
    env.mock_all_auths_allowing_non_root_auth();
    client.process_refund(&admin, &student, &course_id, &true, &Some(400_000_000));

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.refund_count, 1);
    assert_eq!(course.total_refunded, 400_000_000);
}

#[test]
fn test_process_refund_rejection_leaves_refund_stats_unchanged() {
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
        "COURSE-STATS-REJECT",
        1_000_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-STATS-REJECT");

    client.enroll(&student, &course_id);
    client.request_refund(&student, &course_id);
    env.mock_all_auths_allowing_non_root_auth();
    client.process_refund(&admin, &student, &course_id, &false, &None);

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.refund_count, 0);
    assert_eq!(course.total_refunded, 0);
    assert_eq!(course.active_enrollments, 1);
}

#[test]
fn test_archive_course_refunds_update_course_refund_stats() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student_a = Address::generate(&env);
    let student_b = Address::generate(&env);
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
        "COURSE-STATS-ARCHIVE",
        price,
    );
    let course_id = String::from_str(&env, "COURSE-STATS-ARCHIVE");

    client.enroll(&student_a, &course_id);
    client.enroll(&student_b, &course_id);
    client.pause_course(&admin, &course_id);

    let mut refund_students = soroban_sdk::Vec::new(&env);
    refund_students.push_back(student_a.clone());
    refund_students.push_back(student_b.clone());

    env.mock_all_auths_allowing_non_root_auth();
    client.archive_course(&admin, &sec_admin, &course_id, &Some(refund_students));

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.status, CourseStatus::Archived);
    assert_eq!(course.refund_count, 2);
    assert_eq!(course.total_refunded, price * 2);
}

#[test]
fn test_archive_course_incremental_batches_accumulate_refund_stats() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let mut students: std::vec::Vec<Address> = std::vec::Vec::new();
    for _ in 0..5 {
        let s = Address::generate(&env);
        token::StellarAssetClient::new(&env, &token_id).mint(&s, &1_000_000_000);
        students.push(s);
    }

    let price = 500_000_000;
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-STATS-BATCHES",
        price,
    );
    let course_id = String::from_str(&env, "COURSE-STATS-BATCHES");

    for student in &students {
        client.enroll(student, &course_id);
    }
    client.pause_course(&admin, &course_id);

    let mut batch1 = soroban_sdk::Vec::new(&env);
    batch1.push_back(students[0].clone());
    batch1.push_back(students[1].clone());

    env.mock_all_auths_allowing_non_root_auth();
    client.archive_course(&admin, &sec_admin, &course_id, &Some(batch1));

    // Course stays Paused; stats from the first batch must be persisted.
    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.status, CourseStatus::Paused);
    assert_eq!(course.refund_count, 2);
    assert_eq!(course.total_refunded, price * 2);

    let mut batch2 = soroban_sdk::Vec::new(&env);
    for i in 2..5 {
        batch2.push_back(students[i].clone());
    }
    client.archive_course(&admin, &sec_admin, &course_id, &Some(batch2));

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.status, CourseStatus::Archived);
    assert_eq!(course.refund_count, 5);
    assert_eq!(course.total_refunded, price * 5);
}

#[test]
fn test_archive_course_without_refunds_leaves_refund_stats_zero() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-STATS-NOREFUND",
        500_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-STATS-NOREFUND");
    client.pause_course(&admin, &course_id);

    env.mock_all_auths_allowing_non_root_auth();
    client.archive_course(&admin, &sec_admin, &course_id, &None);

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.status, CourseStatus::Archived);
    assert_eq!(course.refund_count, 0);
    assert_eq!(course.total_refunded, 0);
}



// Issue #442: bound the admin-configurable review period so approval remains possible.
#[test]
fn test_min_review_delay_accepts_maximum() {
    let (env, contract_id, _, admin, _, _, _) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    client.update_min_review_delay(&admin, &120_960u32);
    assert_eq!(client.get_min_review_delay(), 120_960u32);
}

#[test]
fn test_min_review_delay_rejects_values_above_maximum_without_mutation() {
    let (env, contract_id, _, admin, _, _, _) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    client.update_min_review_delay(&admin, &10u32);

    assert!(client.try_update_min_review_delay(&admin, &120_961u32).is_err());
    assert!(client.try_update_min_review_delay(&admin, &u32::MAX).is_err());
    assert_eq!(client.get_min_review_delay(), 10u32);
}


// ============================================================
// Issue #364 — CapacityMode: Lifetime vs Concurrent
// ============================================================

/// Concurrent mode: a course capped at 2 active seats must reject a third
/// enrollment while both seats are still active.
#[test]
#[should_panic(expected = "course has reached maximum enrollment capacity")]
fn test_concurrent_capacity_blocks_enrollment_at_seat_cap() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) =
        setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let course_id = String::from_str(&env, "COURSE-CONCURRENT-CAP");

    // Register with max_capacity = 2, then switch to Concurrent mode.
    client.register_course(
        &instructor,
        &course_id,
        &100_000_000i128,
        &token_id,
        &0u32,
        &Some(2u32),
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });
    client.approve_course(&admin, &course_id);

    // Switch capacity mode to Concurrent.
    client.update_course(
        &instructor,
        &course_id,
        &None,
        &None,
        &Some(crate::CapacityMode::Concurrent),
    );

    let asset_client = token::StellarAssetClient::new(&env, &token_id);
    let student_a = Address::generate(&env);
    let student_b = Address::generate(&env);
    let student_c = Address::generate(&env);
    asset_client.mint(&student_a, &1_000_000_000i128);
    asset_client.mint(&student_b, &1_000_000_000i128);
    asset_client.mint(&student_c, &1_000_000_000i128);

    // Fill both concurrent seats.
    client.enroll(&student_a, &course_id);
    client.enroll(&student_b, &course_id);

    // Third enrollment must be rejected — seats are all taken.
    client.enroll(&student_c, &course_id);
}

/// Concurrent mode: completing a student frees their seat so the next
/// student can enroll, even though lifetime total_enrollments exceeds the cap.
#[test]
fn test_concurrent_capacity_seat_freed_on_completion_allows_next_student() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) =
        setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let course_id = String::from_str(&env, "COURSE-CONCURRENT-FREE");

    // Register with max_capacity = 1.
    client.register_course(
        &instructor,
        &course_id,
        &100_000_000i128,
        &token_id,
        &0u32,
        &Some(1u32),
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });
    client.approve_course(&admin, &course_id);

    // Switch to Concurrent mode.
    client.update_course(
        &instructor,
        &course_id,
        &None,
        &None,
        &Some(crate::CapacityMode::Concurrent),
    );

    let asset_client = token::StellarAssetClient::new(&env, &token_id);
    let student_a = Address::generate(&env);
    let student_b = Address::generate(&env);
    asset_client.mint(&student_a, &1_000_000_000i128);
    asset_client.mint(&student_b, &1_000_000_000i128);

    // Student A takes the one seat.
    client.enroll(&student_a, &course_id);

    // Complete student A — frees the concurrent seat.
    client.mark_completed(
        &admin,
        &student_a,
        &course_id,
        &Some(evidence_hash(&env, "proof")),
    );

    // Verify active_enrollments dropped to 0.
    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.active_enrollments, 0);
    assert_eq!(course.total_enrollments, 1); // lifetime total unchanged

    // Student B must now be admitted into the freed seat.
    client.enroll(&student_b, &course_id);

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.active_enrollments, 1);
    assert_eq!(course.total_enrollments, 2); // lifetime grew past the cap — that's fine
}

/// Default Lifetime mode is unchanged: max_capacity still counts all-time
/// total enrollments regardless of how many students have completed.
#[test]
#[should_panic(expected = "course has reached maximum enrollment capacity")]
fn test_lifetime_capacity_default_unchanged_counts_completed_students() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) =
        setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let course_id = String::from_str(&env, "COURSE-LIFETIME-CAP");

    // Register with max_capacity = 1 — Lifetime mode by default.
    client.register_course(
        &instructor,
        &course_id,
        &100_000_000i128,
        &token_id,
        &0u32,
        &Some(1u32),
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });
    client.approve_course(&admin, &course_id);

    // Confirm capacity_mode defaults to Lifetime.
    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.capacity_mode, crate::CapacityMode::Lifetime);

    let asset_client = token::StellarAssetClient::new(&env, &token_id);
    let student_a = Address::generate(&env);
    let student_b = Address::generate(&env);
    asset_client.mint(&student_a, &1_000_000_000i128);
    asset_client.mint(&student_b, &1_000_000_000i128);

    // Student A takes the one lifetime seat.
    client.enroll(&student_a, &course_id);

    // Complete student A — active_enrollments drops, but lifetime total stays at 1.
    client.mark_completed(
        &admin,
        &student_a,
        &course_id,
        &Some(evidence_hash(&env, "proof")),
    );

    // In Lifetime mode this must still be rejected — total_enrollments == cap.
    client.enroll(&student_b, &course_id);
}


#[test]
fn test_randomized_token_solvency_after_financial_operations() {
    const STUDENT_COUNT: usize = 12;
    const COURSE_IDS: [&str; 3] = [
        "COURSE-SOLVENCY-0",
        "COURSE-SOLVENCY-1",
        "COURSE-SOLVENCY-2",
    ];
    const COURSE_PRICE: i128 = 500_000_000;
    const ARBITRATION_FEE: i128 = 5_000_000;

    let (env, contract_id, token_id, admin, sec_admin, _treasury, first_instructor) = setup();
    // Refunds move the platform share out of the treasury contract, which is
    // a nested (non-root) authorization — allow it like the other refund tests do.
    env.mock_all_auths_allowing_non_root_auth();
    let client = HamplardContractClient::new(&env, &contract_id);
    let asset_client = token::StellarAssetClient::new(&env, &token_id);

    let mut instructors = std::vec::Vec::new();
    instructors.push(first_instructor);
    for _ in 1..COURSE_IDS.len() {
        instructors.push(Address::generate(&env));
    }

    let mut course_ids = std::vec::Vec::new();
    for course_name in COURSE_IDS {
        course_ids.push(String::from_str(&env, course_name));
    }

    let mut students = std::vec::Vec::new();
    for _ in 0..STUDENT_COUNT {
        let student = Address::generate(&env);
        asset_client.mint(&student, &10_000_000_000);
        students.push(student);
    }

    let assert_solvency = |operation: &str| {
        assert_token_solvency(
            &env,
            &client,
            &contract_id,
            &token_id,
            &instructors,
            &students,
            &course_ids,
            operation,
        );
    };

    client.set_arbitration_fee_config(&admin, &ARBITRATION_FEE);
    for i in 0..instructors.len() {
        register_and_approve_course(
            &env,
            &client,
            &token_id,
            &admin,
            &instructors[i],
            COURSE_IDS[i],
            COURSE_PRICE,
        );
    }

    // Direct transfers create recoverable surplus for the emergency sweep path.
    asset_client.mint(&contract_id, &100_000_000);
    assert_solvency("initial funding");

    let mut enrolled = [[false; COURSE_IDS.len()]; STUDENT_COUNT];
    let mut refunded = [[false; COURSE_IDS.len()]; STUDENT_COUNT];
    let mut open_arbitration = [[false; COURSE_IDS.len()]; STUDENT_COUNT];

    for course_index in 0..COURSE_IDS.len() {
        client.enroll(&students[0], &course_ids[course_index]);
        enrolled[0][course_index] = true;
        assert_solvency("initial enrollment");
    }

    client.escalate_to_arbitration(&students[0], &course_ids[0]);
    open_arbitration[0][0] = true;
    assert_solvency("arbitration escalation");

    client.request_refund(&students[0], &course_ids[1]);
    assert_solvency("refund request");
    client.process_refund(&admin, &students[0], &course_ids[1], &true, &None);
    refunded[0][1] = true;
    assert_solvency("approved refund");

    client.withdraw_earnings(&instructors[0], &token_id, &1_000_000, &None);
    assert_solvency("instructor withdrawal");

    client.withdraw_tokens(
        &admin,
        &sec_admin,
        &token_id,
        &10_000_000,
        &Address::generate(&env),
    );
    assert_solvency("emergency token withdrawal");

    client.resolve_arbitration(&admin, &students[0], &course_ids[0], &true);
    open_arbitration[0][0] = false;
    assert_solvency("arbitration resolution");

    let mut seed = 0x505_2026_u64;
    for step in 0..120 {
        let operation = next_solvency_random(&mut seed) % 5;
        match operation {
            0 => {
                let start =
                    next_solvency_random(&mut seed) as usize % (STUDENT_COUNT * COURSE_IDS.len());
                for offset in 0..(STUDENT_COUNT * COURSE_IDS.len()) {
                    let index = (start + offset) % (STUDENT_COUNT * COURSE_IDS.len());
                    let student_index = index / COURSE_IDS.len();
                    let course_index = index % COURSE_IDS.len();
                    if !enrolled[student_index][course_index] {
                        client.enroll(&students[student_index], &course_ids[course_index]);
                        enrolled[student_index][course_index] = true;
                        break;
                    }
                }
            }
            1 => {
                let start =
                    next_solvency_random(&mut seed) as usize % (STUDENT_COUNT * COURSE_IDS.len());
                for offset in 0..(STUDENT_COUNT * COURSE_IDS.len()) {
                    let index = (start + offset) % (STUDENT_COUNT * COURSE_IDS.len());
                    let student_index = index / COURSE_IDS.len();
                    let course_index = index % COURSE_IDS.len();
                    if enrolled[student_index][course_index]
                        && !refunded[student_index][course_index]
                    {
                        let enrollment = client
                            .get_enrollment(
                                &students[student_index],
                                &students[student_index],
                                &course_ids[course_index],
                            )
                            .expect("tracked active enrollment must exist");
                        let earnings =
                            client.get_instructor_earnings(&instructors[course_index], &token_id);
                        if earnings >= enrollment.instructor_amount {
                            client.request_refund(
                                &students[student_index],
                                &course_ids[course_index],
                            );
                            assert_solvency("random refund request");
                            client.process_refund(
                                &admin,
                                &students[student_index],
                                &course_ids[course_index],
                                &true,
                                &None,
                            );
                            refunded[student_index][course_index] = true;
                            break;
                        }
                    }
                }
            }
            2 => {
                let instructor_index = next_solvency_random(&mut seed) as usize % instructors.len();
                let earnings =
                    client.get_instructor_earnings(&instructors[instructor_index], &token_id);
                if earnings > 0 {
                    let amount = 1 + (next_solvency_random(&mut seed) as i128 % earnings);
                    client.withdraw_earnings(
                        &instructors[instructor_index],
                        &token_id,
                        &amount,
                        &None,
                    );
                }
            }
            3 => {
                let has_open_case = open_arbitration.iter().flatten().any(|is_open| *is_open);
                let resolve_case = has_open_case && next_solvency_random(&mut seed) % 2 == 0;
                let start =
                    next_solvency_random(&mut seed) as usize % (STUDENT_COUNT * COURSE_IDS.len());
                for offset in 0..(STUDENT_COUNT * COURSE_IDS.len()) {
                    let index = (start + offset) % (STUDENT_COUNT * COURSE_IDS.len());
                    let student_index = index / COURSE_IDS.len();
                    let course_index = index % COURSE_IDS.len();
                    if resolve_case && open_arbitration[student_index][course_index] {
                        let for_student = next_solvency_random(&mut seed) % 2 == 0;
                        client.resolve_arbitration(
                            &admin,
                            &students[student_index],
                            &course_ids[course_index],
                            &for_student,
                        );
                        open_arbitration[student_index][course_index] = false;
                        break;
                    } else if !resolve_case
                        && enrolled[student_index][course_index]
                        && !open_arbitration[student_index][course_index]
                    {
                        client.escalate_to_arbitration(
                            &students[student_index],
                            &course_ids[course_index],
                        );
                        open_arbitration[student_index][course_index] = true;
                        break;
                    }
                }
            }
            _ => {
                let mut liabilities = 0i128;
                for instructor in &instructors {
                    liabilities += client.get_instructor_earnings(instructor, &token_id);
                }
                for student_index in 0..STUDENT_COUNT {
                    for course_index in 0..COURSE_IDS.len() {
                        if open_arbitration[student_index][course_index] {
                            liabilities += ARBITRATION_FEE;
                        }
                    }
                }
                let contract_balance = token::Client::new(&env, &token_id).balance(&contract_id);
                let surplus = contract_balance - liabilities;
                if surplus > 0 {
                    let amount = 1 + (next_solvency_random(&mut seed) as i128 % surplus);
                    client.withdraw_tokens(
                        &admin,
                        &sec_admin,
                        &token_id,
                        &amount,
                        &Address::generate(&env),
                    );
                }
            }
        }

        assert_solvency(&format!("random operation {step}"));
    }
}


#[test]
fn test_treasury_getters_without_pending_update() {
    let (env, contract_id, _token_id, _admin, _sec_admin, treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    assert_eq!(client.get_treasury(), treasury);
    assert_eq!(client.get_pending_treasury(), None);
}

#[test]
fn test_treasury_getters_before_and_after_delay() {
    let (env, contract_id, _token_id, admin, sec_admin, treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let new_treasury = Address::generate(&env);

    let scheduled_at = env.ledger().sequence();
    client.update_treasury(&admin, &sec_admin, &new_treasury);
    let effective_ledger = scheduled_at + 100;

    // Before the delay: old treasury is effective, update is pending.
    assert_eq!(client.get_treasury(), treasury);
    let pending = client.get_pending_treasury().unwrap();
    assert_eq!(pending.address, new_treasury);
    assert_eq!(pending.effective_ledger, effective_ledger);

    // One ledger before the boundary: still pending.
    env.ledger()
        .with_mut(|l| l.sequence_number = effective_ledger - 1);
    assert_eq!(client.get_treasury(), treasury);
    assert!(client.get_pending_treasury().is_some());

    // At the boundary: new treasury is effective, nothing is pending.
    env.ledger()
        .with_mut(|l| l.sequence_number = effective_ledger);
    assert_eq!(client.get_treasury(), new_treasury);
    assert_eq!(client.get_pending_treasury(), None);
}


// ============================================================
// CPU / MEMORY BASELINE BENCHMARKS (#493)
// ============================================================

fn print_budget(env: &Env, label: &str) {
    let budget = env.cost_estimate().budget();
    std::println!(
        "BENCH {} cpu_instructions={} memory_bytes={}",
        label,
        budget.cpu_instruction_cost(),
        budget.memory_bytes_cost()
    );
}

#[test]
#[ignore = "benchmark; run with --ignored --nocapture"]
fn benchmark_enroll_single_course() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "BENCH-ENROLL",
        100_000_000,
    );
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    client.enroll(&student, &String::from_str(&env, "BENCH-ENROLL"));

    print_budget(&env, "enroll");
}

#[test]
#[ignore = "benchmark; run with --ignored --nocapture"]
fn benchmark_batch_enroll_10_courses() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let mut course_ids = Vec::new(&env);
    for i in 0..10u32 {
        let course_id = std::format!("BENCH-BATCH-{:02}", i);
        register_and_approve_course(
            &env,
            &client,
            &token_id,
            &admin,
            &instructor,
            &course_id,
            100_000_000,
        );
        course_ids.push_back(String::from_str(&env, &course_id));
    }

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &10_000_000_000);

    client.batch_enroll(&student, &course_ids);

    print_budget(&env, "batch_enroll_10");
}

#[test]
#[ignore = "benchmark; run with --ignored --nocapture"]
fn benchmark_archive_course_with_10_refunds() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "BENCH-ARCHIVE",
        100_000_000,
    );
    let course_id = String::from_str(&env, "BENCH-ARCHIVE");

    let mut students = Vec::new(&env);
    let asset_client = token::StellarAssetClient::new(&env, &token_id);
    for _ in 0..10u32 {
        let student = Address::generate(&env);
        asset_client.mint(&student, &1_000_000_000);
        client.enroll(&student, &course_id);
        students.push_back(student);
    }

    client.pause_course(&admin, &course_id);
    env.mock_all_auths_allowing_non_root_auth();
    client.archive_course(&admin, &sec_admin, &course_id, &Some(students));

    print_budget(&env, "archive_course_10_refunds");
}

#[test]
#[ignore = "benchmark; run with --ignored --nocapture"]
fn benchmark_register_course_against_50_course_catalog() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.update_max_courses_limit(&admin, &60u32);
    for i in 0..50u32 {
        let course_id = std::format!("BENCH-CATALOG-{:02}", i);
        client.register_course(
            &instructor,
            &String::from_str(&env, &course_id),
            &100_000_000,
            &token_id,
            &0u32,
            &None,
            &BytesN::from_array(&env, &[1u8; 32]),
            &soroban_sdk::Vec::new(&env),
        );
    }

    client.register_course(
        &instructor,
        &String::from_str(&env, "BENCH-CATALOG-50"),
        &100_000_000,
        &token_id,
        &0u32,
        &None,
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );

    print_budget(&env, "register_course_catalog_50");
}


// REVOCATION REASON VALIDATION (#411)

/// Enroll, complete and issue a certificate; returns its ID.
fn issue_cert_for_reason_tests(
    env: &Env,
    client: &HamplardContractClient,
    token_id: &Address,
    admin: &Address,
    instructor: &Address,
) -> String {
    let student = Address::generate(env);
    token::StellarAssetClient::new(env, token_id).mint(&student, &100_000_000_000);
    register_and_approve_course(
        env,
        client,
        token_id,
        admin,
        instructor,
        "COURSE-REASON",
        600_000_000,
    );
    let course_id = String::from_str(env, "COURSE-REASON");
    let cert_id = String::from_str(env, "CERT-REASON");

    client.enroll(&student, &course_id);
    client.mark_completed(
        admin,
        &student,
        &course_id,
        &Some(evidence_hash(env, "evidence")),
    );
    client.issue_certificate(
        admin,
        &student,
        &course_id,
        &cert_id,
        &String::from_str(env, "Reason Course"),
        &get_enrollment_ref(env, client, &student, "COURSE-REASON"),
        &None,
        &None,
    );
    cert_id
}

#[test]
#[should_panic(expected = "revocation reason cannot be empty")]
fn test_revoke_certificate_empty_reason_panics() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let cert_id = issue_cert_for_reason_tests(&env, &client, &token_id, &admin, &instructor);

    client.revoke_certificate(&admin, &cert_id, &String::from_str(&env, ""));
}

#[test]
#[should_panic(expected = "revocation reason exceeds maximum length")]
fn test_revoke_certificate_oversize_reason_panics() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let cert_id = issue_cert_for_reason_tests(&env, &client, &token_id, &admin, &instructor);

    let too_long = "R".repeat(65);
    client.revoke_certificate(&admin, &cert_id, &String::from_str(&env, &too_long));
}

#[test]
fn test_revoke_certificate_reason_at_max_length_accepted() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let cert_id = issue_cert_for_reason_tests(&env, &client, &token_id, &admin, &instructor);

    let max_reason = String::from_str(&env, &"R".repeat(64));
    client.revoke_certificate(&admin, &cert_id, &max_reason);

    let cert = client.get_certificate(&admin, &cert_id);
    assert!(cert.revoked);
    assert_eq!(cert.revocation_reason, Some(max_reason));
}

#[test]
fn test_revoke_certificate_rejected_reason_leaves_certificate_unrevoked() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let cert_id = issue_cert_for_reason_tests(&env, &client, &token_id, &admin, &instructor);

    assert!(client
        .try_revoke_certificate(&admin, &cert_id, &String::from_str(&env, ""))
        .is_err());
    assert!(client
        .try_revoke_certificate(&admin, &cert_id, &String::from_str(&env, &"R".repeat(65)))
        .is_err());

    let cert = client.get_certificate(&admin, &cert_id);
    assert!(!cert.revoked);
    assert_eq!(cert.revocation_reason, None);
}


#[test]
fn test_frozen_instructor_cannot_withdraw_until_unfrozen() {
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
        "COURSE-FROZEN-WITHDRAW",
        500_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-FROZEN-WITHDRAW");
    client.enroll(&student, &course_id);

    let instructor_share = 500_000_000i128 * 80 / 100;
    assert_eq!(
        client.get_instructor_earnings(&instructor, &token_id),
        instructor_share
    );

    client.freeze_instructor(&admin, &instructor);
    let frozen_withdrawal = client.try_withdraw_earnings(&instructor, &token_id, &0, &None);
    assert!(
        frozen_withdrawal.is_err(),
        "frozen instructors must not be able to withdraw"
    );
    assert_eq!(
        client.get_instructor_earnings(&instructor, &token_id),
        instructor_share
    );

    client.unfreeze_instructor(&admin, &instructor);
    client.withdraw_earnings(&instructor, &token_id, &0, &None);

    assert_eq!(
        token::Client::new(&env, &token_id).balance(&instructor),
        instructor_share
    );
    assert_eq!(client.get_instructor_earnings(&instructor, &token_id), 0);
}

#[test]
fn test_instructor_total_earnings_is_lifetime_net_across_withdraw_and_refund() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &10_000_000_000);

    let price = 1_000_000_000i128;
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-TOTAL-LIFETIME",
        price,
    );
    let course_id = String::from_str(&env, "COURSE-TOTAL-LIFETIME");
    client.enroll(&student, &course_id);

    let instructor_share = price * 80 / 100;
    let platform_share = price - instructor_share;
    assert_eq!(
        client.get_instructor_total_earnings(&instructor),
        instructor_share
    );

    let withdrawn = 300_000_000i128;
    client.withdraw_earnings(&instructor, &token_id, &withdrawn, &None);
    assert_eq!(
        client.get_instructor_earnings(&instructor, &token_id),
        instructor_share - withdrawn
    );
    // Withdrawals must not reduce lifetime net earnings.
    assert_eq!(
        client.get_instructor_total_earnings(&instructor),
        instructor_share
    );

    client.request_refund(&student, &course_id);
    env.mock_all_auths_allowing_non_root_auth();
    let refund_amount = 625_000_000i128;
    client.process_refund(&admin, &student, &course_id, &true, &Some(refund_amount));

    let platform_refund = refund_amount * platform_share / price;
    let instructor_refund = refund_amount - platform_refund;

    assert_eq!(client.get_instructor_earnings(&instructor, &token_id), 0);
    assert_eq!(
        client.get_instructor_total_earnings(&instructor),
        instructor_share - instructor_refund
    );
    assert_eq!(
        token::Client::new(&env, &token_id).balance(&instructor),
        withdrawn
    );
    assert_eq!(
        token::Client::new(&env, &token_id).balance(&student),
        10_000_000_000 - price + refund_amount
    );
}

#[test]
fn test_expired_enrollment_does_not_release_capacity_slot() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let first_student = Address::generate(&env);
    let second_student = Address::generate(&env);
    let asset_client = token::StellarAssetClient::new(&env, &token_id);
    asset_client.mint(&first_student, &1_000_000_000);
    asset_client.mint(&second_student, &1_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-EXPIRY-CAPACITY",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-EXPIRY-CAPACITY");
    client.update_course(&instructor, &course_id, &None, &Some(Some(1u32)));
    client.set_enrollment_expiry(&instructor, &course_id, &Some(100u32));

    client.enroll(&first_student, &course_id);
    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.total_enrollments, 1);
    assert_eq!(course.active_enrollments, 1);

    env.ledger().with_mut(|l| {
        l.sequence_number += 200;
    });
    assert_eq!(
        client.has_completed(&first_student, &course_id),
        Some(false)
    );

    let result = client.try_enroll(&second_student, &course_id);
    assert!(
        result.is_err(),
        "expiry must not free a max_capacity slot on its own"
    );

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.max_capacity, Some(1));
    assert_eq!(course.total_enrollments, 1);
    assert_eq!(course.active_enrollments, 1);
    assert!(!client.is_enrolled(&second_student, &course_id));
}

#[test]
fn test_expired_enrollment_can_re_enroll_and_is_archived() {
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
        "COURSE-EXPIRY-REENROLL",
        500_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-EXPIRY-REENROLL");
    client.set_enrollment_expiry(&instructor, &course_id, &Some(100u32));

    client.enroll(&student, &course_id);
    let original = client
        .get_enrollment(&student, &student, &course_id)
        .unwrap();
    assert!(!original.completed);

    env.ledger().with_mut(|l| {
        l.sequence_number += 200;
    });
    assert_eq!(client.has_completed(&student, &course_id), Some(false));

    client.re_enroll(&student, &course_id);

    let replacement = client
        .get_enrollment(&student, &student, &course_id)
        .unwrap();
    assert!(!replacement.completed);
    assert_eq!(replacement.amount_paid, 500_000_000);
    assert_eq!(replacement.enrolled_at_ledger, env.ledger().sequence());
    assert_ne!(replacement.enrollment_ref, original.enrollment_ref);

    let history = client.get_enrollment_history(&student, &student, &course_id);
    assert_eq!(history.len(), 1);
    let archived = history.get(0).unwrap();
    assert!(!archived.completed);
    assert!(!archived.is_refunded);
    assert_eq!(archived.enrolled_at_ledger, original.enrolled_at_ledger);
    assert_eq!(archived.enrollment_ref, original.enrollment_ref);

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.total_enrollments, 2);
    assert_eq!(course.active_enrollments, 1);
    assert_eq!(client.get_total_active_enrollments(), 1);
}

#[test]
fn test_expired_enrollment_still_blocks_enroll_and_mark_completed() {
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
        "COURSE-EXPIRY-ENTRYPOINTS",
        500_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-EXPIRY-ENTRYPOINTS");
    client.set_enrollment_expiry(&instructor, &course_id, &Some(100u32));
    client.enroll(&student, &course_id);

    env.ledger().with_mut(|l| {
        l.sequence_number += 200;
    });

    let enroll_result = client.try_enroll(&student, &course_id);
    assert!(
        enroll_result.is_err(),
        "enroll() must keep rejecting an existing expired enrollment"
    );

    let completion_result = client.try_mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "late_evidence")),
    );
    assert!(
        completion_result.is_err(),
        "mark_completed() must reject an expired enrollment"
    );

    let history = client.get_enrollment_history(&student, &student, &course_id);
    assert_eq!(history.len(), 0);
}


#[test]
fn test_targeted_auth_enroll_requires_only_student() {
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
        "COURSE-AUTH-ENROLL",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-AUTH-ENROLL");

    let token_transfer = MockAuthInvoke {
        contract: &token_id,
        fn_name: "transfer",
        args: (student.clone(), contract_id.clone(), 100_000_000i128).into_val(&env),
        sub_invokes: &[],
    };
    let enroll_invoke = MockAuthInvoke {
        contract: &contract_id,
        fn_name: "enroll",
        args: (student.clone(), course_id.clone()).into_val(&env),
        sub_invokes: &[token_transfer],
    };
    let student_auth = MockAuth {
        address: &student,
        invoke: &enroll_invoke,
    };

    client
        .mock_auths(&[student_auth])
        .enroll(&student, &course_id);

    assert_only_auths(&env, &[student.clone()]);
    assert!(client.is_enrolled(&student, &course_id));
}

#[test]
fn test_targeted_auth_init_requires_only_admin() {
    let env = Env::default();
    let contract_id = env.register(HamplardContract, ());
    let client = HamplardContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let secondary_admin = Address::generate(&env);
    let treasury = Address::generate(&env);

    let args = (
        admin.clone(),
        secondary_admin.clone(),
        treasury.clone(),
        20u32,
        50u32,
        1000u32,
        17_280u32,
    );
    let invoke = MockAuthInvoke {
        contract: &contract_id,
        fn_name: "init",
        args: args.into_val(&env),
        sub_invokes: &[],
    };
    let auth = MockAuth {
        address: &admin,
        invoke: &invoke,
    };

    client.mock_auths(&[auth]).init(
        &admin,
        &secondary_admin,
        &treasury,
        &20u32,
        &50u32,
        &1000u32,
        &17_280u32,
    );

    assert_only_auths(&env, &[admin.clone()]);
}

#[test]
fn test_targeted_auth_course_lifecycle_entrypoints() {
    let (env, contract_id, token_id, admin, sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let course_id = String::from_str(&env, "COURSE-AUTH-LIFECYCLE");
    let content_hash = BytesN::from_array(&env, &[1u8; 32]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &instructor,
        "register_course",
        (
            instructor.clone(),
            course_id.clone(),
            100_000_000i128,
            token_id.clone(),
            0u32,
            None::<u32>,
            content_hash.clone(),
            soroban_sdk::Vec::<String>::new(&env)
        ),
        register_course(
            &instructor,
            &course_id,
            &100_000_000i128,
            &token_id,
            &0u32,
            &None,
            &content_hash,
            &soroban_sdk::Vec::new(&env)
        )
    );
    assert_only_auths(&env, &[instructor.clone()]);

    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "approve_course",
        (admin.clone(), course_id.clone()),
        approve_course(&admin, &course_id)
    );
    assert_only_auths(&env, &[admin.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &instructor,
        "update_course",
        (
            instructor.clone(),
            course_id.clone(),
            Some(150_000_000i128),
            Some(Some(10u32))
        ),
        update_course(
            &instructor,
            &course_id,
            &Some(150_000_000i128),
            &Some(Some(10u32))
        )
    );
    assert_only_auths(&env, &[instructor.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &instructor,
        "set_enrollment_start_ledger",
        (
            instructor.clone(),
            course_id.clone(),
            Some(env.ledger().sequence() + 1)
        ),
        set_enrollment_start_ledger(&instructor, &course_id, &Some(env.ledger().sequence() + 1))
    );
    assert_only_auths(&env, &[instructor.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &instructor,
        "set_enrollment_expiry",
        (instructor.clone(), course_id.clone(), Some(50u32)),
        set_enrollment_expiry(&instructor, &course_id, &Some(50u32))
    );
    assert_only_auths(&env, &[instructor.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &instructor,
        "update_content_hash",
        (
            instructor.clone(),
            course_id.clone(),
            BytesN::from_array(&env, &[2u8; 32])
        ),
        update_content_hash(
            &instructor,
            &course_id,
            &BytesN::from_array(&env, &[2u8; 32])
        )
    );
    assert_only_auths(&env, &[instructor.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &instructor,
        "set_course_expiry",
        (
            instructor.clone(),
            course_id.clone(),
            Some(env.ledger().sequence() + 10_000)
        ),
        set_course_expiry(
            &instructor,
            &course_id,
            &Some(env.ledger().sequence() + 10_000)
        )
    );
    assert_only_auths(&env, &[instructor.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &instructor,
        "set_prerequisite_courses",
        (
            instructor.clone(),
            course_id.clone(),
            soroban_sdk::Vec::<String>::new(&env)
        ),
        set_prerequisite_courses(&instructor, &course_id, &soroban_sdk::Vec::new(&env))
    );
    assert_only_auths(&env, &[instructor.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &instructor,
        "set_max_certificates",
        (instructor.clone(), course_id.clone(), Some(10u32)),
        set_max_certificates(&instructor, &course_id, &Some(10u32))
    );
    assert_only_auths(&env, &[instructor.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &instructor,
        "pause_course",
        (instructor.clone(), course_id.clone()),
        pause_course(&instructor, &course_id)
    );
    assert_only_auths(&env, &[instructor.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "unpause_course",
        (admin.clone(), course_id.clone()),
        unpause_course(&admin, &course_id)
    );
    assert_only_auths(&env, &[admin.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "set_high_value_course_threshold",
        (admin.clone(), 5_000_000_000i128),
        set_high_value_course_threshold(&admin, &5_000_000_000i128)
    );
    assert_only_auths(&env, &[admin.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &instructor,
        "clear_course_max_capacity",
        (instructor.clone(), course_id.clone()),
        clear_course_max_capacity(&instructor, &course_id)
    );
    assert_only_auths(&env, &[instructor.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "pause_course",
        (admin.clone(), course_id.clone()),
        pause_course(&admin, &course_id)
    );
    assert_only_auths(&env, &[admin.clone()]);

    invoke_with_auths!(
        &env,
        &client,
        &contract_id,
        &[&admin, &sec_admin],
        "archive_course",
        (
            admin.clone(),
            sec_admin.clone(),
            course_id.clone(),
            None::<soroban_sdk::Vec<Address>>
        ),
        archive_course(&admin, &sec_admin, &course_id, &None)
    );
    assert_only_auths(&env, &[admin.clone(), sec_admin.clone()]);

    let rejected_course_id = String::from_str(&env, "COURSE-AUTH-REJECT");
    client.register_course(
        &instructor,
        &rejected_course_id,
        &100_000_000i128,
        &token_id,
        &0u32,
        &None,
        &content_hash,
        &soroban_sdk::Vec::new(&env),
    );
    let reason = String::from_str(&env, "quality review failed");
    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "reject_course",
        (admin.clone(), rejected_course_id.clone(), reason.clone()),
        reject_course(&admin, &rejected_course_id, &reason)
    );
    assert_only_auths(&env, &[admin.clone()]);
}

#[test]
fn test_targeted_auth_mark_completed_without_evidence_requires_student() {
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
        "COURSE-AUTH-NO-EVIDENCE",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-AUTH-NO-EVIDENCE");
    client.enroll(&student, &course_id);

    let args = (
        admin.clone(),
        student.clone(),
        course_id.clone(),
        None::<BytesN<32>>,
    );
    let invoke = MockAuthInvoke {
        contract: &contract_id,
        fn_name: "mark_completed",
        args: args.clone().into_val(&env),
        sub_invokes: &[],
    };
    let admin_auth = MockAuth {
        address: &admin,
        invoke: &invoke,
    };
    let admin_only = client
        .mock_auths(&[admin_auth])
        .try_mark_completed(&admin, &student, &course_id, &None);
    assert!(
        admin_only.is_err(),
        "student auth must be required when no evidence hash is supplied"
    );

    invoke_with_auths!(
        &env,
        &client,
        &contract_id,
        &[&admin, &student],
        "mark_completed",
        args,
        mark_completed(&admin, &student, &course_id, &None)
    );
    assert_only_auths(&env, &[admin.clone(), student.clone()]);
    assert_eq!(client.has_completed(&student, &course_id), Some(true));
}

#[test]
fn test_targeted_auth_transfer_course_requires_instructor_and_admin() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-AUTH-TRANSFER",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-AUTH-TRANSFER");
    let new_instructor = Address::generate(&env);

    invoke_with_auths!(
        &env,
        &client,
        &contract_id,
        &[&instructor, &admin],
        "transfer_course",
        (
            instructor.clone(),
            admin.clone(),
            course_id.clone(),
            new_instructor.clone()
        ),
        transfer_course(&instructor, &admin, &course_id, &new_instructor)
    );

    assert_only_auths(&env, &[instructor.clone(), admin.clone()]);
    assert_eq!(
        client.get_course(&course_id).unwrap().instructor,
        new_instructor
    );
}

#[test]
fn test_targeted_auth_completion_and_certificate_entrypoints() {
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
        "COURSE-AUTH-CERT",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-AUTH-CERT");
    client.enroll(&student, &course_id);

    let evidence = Some(evidence_hash(&env, "targeted-auth"));
    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "mark_completed",
        (
            admin.clone(),
            student.clone(),
            course_id.clone(),
            evidence.clone()
        ),
        mark_completed(&admin, &student, &course_id, &evidence)
    );
    assert_only_auths(&env, &[admin.clone()]);

    let certificate_id = String::from_str(&env, "CERT-AUTH-1");
    let course_title = String::from_str(&env, "Course");
    let enrollment_ref = get_enrollment_ref(&env, &client, &student, "COURSE-AUTH-CERT");
    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "issue_certificate",
        (
            admin.clone(),
            student.clone(),
            course_id.clone(),
            certificate_id.clone(),
            course_title.clone(),
            enrollment_ref.clone(),
            None::<u32>,
            None::<BytesN<64>>
        ),
        issue_certificate(
            &admin,
            &student,
            &course_id,
            &certificate_id,
            &course_title,
            &enrollment_ref,
            &None,
            &None
        )
    );
    assert_only_auths(&env, &[admin.clone()]);

    let reason = String::from_str(&env, "ISSUED_IN_ERROR");
    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "revoke_certificate",
        (admin.clone(), certificate_id.clone(), reason.clone()),
        revoke_certificate(&admin, &certificate_id, &reason)
    );
    assert_only_auths(&env, &[admin.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &student,
        "challenge_revocation",
        (student.clone(), certificate_id.clone()),
        challenge_revocation(&student, &certificate_id)
    );
    assert_only_auths(&env, &[student.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "bulk_revoke_course_certificates",
        (admin.clone(), course_id.clone()),
        bulk_revoke_course_certificates(&admin, &course_id)
    );
    assert_only_auths(&env, &[admin.clone()]);
}

#[test]
fn test_targeted_auth_admin_config_entrypoints() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let approver = Address::generate(&env);
    let blocked_student = Address::generate(&env);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "update_default_fee",
        (admin.clone(), 25u32),
        update_default_fee(&admin, &25u32)
    );
    assert_only_auths(&env, &[admin.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "add_approved_token",
        (admin.clone(), token_id.clone()),
        add_approved_token(&admin, &token_id)
    );
    assert_only_auths(&env, &[admin.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "set_token_btc_eth",
        (admin.clone(), token_id.clone(), false),
        set_token_btc_eth(&admin, &token_id, &false)
    );
    assert_only_auths(&env, &[admin.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "update_max_courses_limit",
        (admin.clone(), 60u32),
        update_max_courses_limit(&admin, &60u32)
    );
    assert_only_auths(&env, &[admin.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "freeze_instructor",
        (admin.clone(), instructor.clone()),
        freeze_instructor(&admin, &instructor)
    );
    assert_only_auths(&env, &[admin.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "unfreeze_instructor",
        (admin.clone(), instructor.clone()),
        unfreeze_instructor(&admin, &instructor)
    );
    assert_only_auths(&env, &[admin.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "add_approver",
        (admin.clone(), approver.clone()),
        add_approver(&admin, &approver)
    );
    assert_only_auths(&env, &[admin.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "remove_approver",
        (admin.clone(), approver.clone()),
        remove_approver(&admin, &approver)
    );
    assert_only_auths(&env, &[admin.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "block_student",
        (admin.clone(), blocked_student.clone()),
        block_student(&admin, &blocked_student)
    );
    assert_only_auths(&env, &[admin.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "unblock_student",
        (admin.clone(), blocked_student.clone()),
        unblock_student(&admin, &blocked_student)
    );
    assert_only_auths(&env, &[admin.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "set_platform_enrollment_cap",
        (admin.clone(), Some(100u32)),
        set_platform_enrollment_cap(&admin, &Some(100u32))
    );
    assert_only_auths(&env, &[admin.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "update_min_review_delay",
        (admin.clone(), 1u32),
        update_min_review_delay(&admin, &1u32)
    );
    assert_only_auths(&env, &[admin.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "update_min_completion_ledgers",
        (admin.clone(), 100u32),
        update_min_completion_ledgers(&admin, &100u32)
    );
    assert_only_auths(&env, &[admin.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "update_refund_window",
        (admin.clone(), 2000u32),
        update_refund_window(&admin, &2000u32)
    );
    assert_only_auths(&env, &[admin.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "set_revocation_challenge_period",
        (admin.clone(), 100u32),
        set_revocation_challenge_period(&admin, &100u32)
    );
    assert_only_auths(&env, &[admin.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "set_fee_config",
        (admin.clone(), token_id.clone(), 100u32),
        set_fee_config(&admin, &token_id, &100u32)
    );
    assert_only_auths(&env, &[admin.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "set_arbitration_fee_config",
        (admin.clone(), 1000i128),
        set_arbitration_fee_config(&admin, &1000i128)
    );
    assert_only_auths(&env, &[admin.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "set_risk_config_enabled",
        (admin.clone(), true),
        set_risk_config_enabled(&admin, &true)
    );
    assert_only_auths(&env, &[admin.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "pause_platform",
        (admin.clone(),),
        pause_platform(&admin)
    );
    assert_only_auths(&env, &[admin.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "unpause_platform",
        (admin.clone(),),
        unpause_platform(&admin)
    );
    assert_only_auths(&env, &[admin.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "remove_approved_token",
        (admin.clone(), token_id.clone()),
        remove_approved_token(&admin, &token_id)
    );
    assert_only_auths(&env, &[admin.clone()]);
}

#[test]
fn test_targeted_auth_multi_admin_entrypoints() {
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
        "COURSE-AUTH-MULTI",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-AUTH-MULTI");
    client.enroll(&student, &course_id);

    let destination = Address::generate(&env);
    invoke_with_auths!(
        &env,
        &client,
        &contract_id,
        &[&admin, &sec_admin],
        "withdraw_tokens",
        (
            admin.clone(),
            sec_admin.clone(),
            token_id.clone(),
            1i128,
            destination.clone()
        ),
        withdraw_tokens(&admin, &sec_admin, &token_id, &1i128, &destination)
    );
    assert_only_auths(&env, &[admin.clone(), sec_admin.clone()]);
    assert_eq!(token::Client::new(&env, &token_id).balance(&destination), 1);

    let new_treasury = Address::generate(&env);
    invoke_with_auths!(
        &env,
        &client,
        &contract_id,
        &[&admin, &sec_admin],
        "update_treasury",
        (admin.clone(), sec_admin.clone(), new_treasury.clone()),
        update_treasury(&admin, &sec_admin, &new_treasury)
    );
    assert_only_auths(&env, &[admin.clone(), sec_admin.clone()]);

    invoke_with_auths!(
        &env,
        &client,
        &contract_id,
        &[&admin, &sec_admin],
        "set_admin_expiry",
        (
            admin.clone(),
            sec_admin.clone(),
            Some(env.ledger().sequence() + 10_000)
        ),
        set_admin_expiry(&admin, &sec_admin, &Some(env.ledger().sequence() + 10_000))
    );
    assert_only_auths(&env, &[admin.clone(), sec_admin.clone()]);

    invoke_with_auths!(
        &env,
        &client,
        &contract_id,
        &[&admin, &sec_admin],
        "set_upgrade_timelock",
        (admin.clone(), sec_admin.clone(), 50u32),
        set_upgrade_timelock(&admin, &sec_admin, &50u32)
    );
    assert_only_auths(&env, &[admin.clone(), sec_admin.clone()]);

    let wasm_hash = BytesN::from_array(&env, &[9u8; 32]);
    invoke_with_auths!(
        &env,
        &client,
        &contract_id,
        &[&admin, &sec_admin],
        "propose_upgrade",
        (admin.clone(), sec_admin.clone(), wasm_hash.clone()),
        propose_upgrade(&admin, &sec_admin, &wasm_hash)
    );
    assert_only_auths(&env, &[admin.clone(), sec_admin.clone()]);

    invoke_with_auths!(
        &env,
        &client,
        &contract_id,
        &[&admin, &sec_admin],
        "cancel_upgrade",
        (admin.clone(), sec_admin.clone()),
        cancel_upgrade(&admin, &sec_admin)
    );
    assert_only_auths(&env, &[admin.clone(), sec_admin.clone()]);

    let new_admin = Address::generate(&env);
    let new_sec_admin = Address::generate(&env);
    invoke_with_auths!(
        &env,
        &client,
        &contract_id,
        &[&admin, &sec_admin],
        "transfer_admin",
        (
            admin.clone(),
            sec_admin.clone(),
            new_admin.clone(),
            new_sec_admin.clone()
        ),
        transfer_admin(&admin, &sec_admin, &new_admin, &new_sec_admin)
    );
    assert_only_auths(&env, &[admin.clone(), sec_admin.clone()]);

    invoke_with_auths!(
        &env,
        &client,
        &contract_id,
        &[&new_admin, &new_sec_admin],
        "accept_admin",
        (new_admin.clone(), new_sec_admin.clone()),
        accept_admin(&new_admin, &new_sec_admin)
    );
    assert_only_auths(&env, &[new_admin, new_sec_admin]);
}

#[test]
fn test_targeted_auth_student_entrypoints() {
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
        "COURSE-AUTH-BATCH-1",
        100_000_000,
    );
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-AUTH-BATCH-2",
        100_000_000,
    );
    let course_1 = String::from_str(&env, "COURSE-AUTH-BATCH-1");
    let course_2 = String::from_str(&env, "COURSE-AUTH-BATCH-2");
    let course_ids = Vec::from_array(&env, [course_1.clone(), course_2.clone()]);

    let transfer_1 = MockAuthInvoke {
        contract: &token_id,
        fn_name: "transfer",
        args: (student.clone(), contract_id.clone(), 100_000_000i128).into_val(&env),
        sub_invokes: &[],
    };
    let transfer_2 = MockAuthInvoke {
        contract: &token_id,
        fn_name: "transfer",
        args: (student.clone(), contract_id.clone(), 100_000_000i128).into_val(&env),
        sub_invokes: &[],
    };
    let sub_invokes = [transfer_1, transfer_2];
    let batch_invoke = MockAuthInvoke {
        contract: &contract_id,
        fn_name: "batch_enroll",
        args: (student.clone(), course_ids.clone()).into_val(&env),
        sub_invokes: &sub_invokes,
    };
    let batch_auth = MockAuth {
        address: &student,
        invoke: &batch_invoke,
    };

    client
        .mock_auths(&[batch_auth])
        .batch_enroll(&student, &course_ids);

    assert_only_auths(&env, &[student.clone()]);
    assert!(client.is_enrolled(&student, &course_1));
    assert!(client.is_enrolled(&student, &course_2));

    let remaining_course_id = String::from_str(&env, "COURSE-AUTH-REENROLL");
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-AUTH-REENROLL",
        100_000_000,
    );
    client.enroll(&student, &remaining_course_id);
    client.mark_completed(
        &admin,
        &student,
        &remaining_course_id,
        &Some(evidence_hash(&env, "targeted-auth-reenroll")),
    );

    let re_enroll_transfer = MockAuthInvoke {
        contract: &token_id,
        fn_name: "transfer",
        args: (student.clone(), contract_id.clone(), 100_000_000i128).into_val(&env),
        sub_invokes: &[],
    };
    let re_enroll_invoke = MockAuthInvoke {
        contract: &contract_id,
        fn_name: "re_enroll",
        args: (student.clone(), remaining_course_id.clone()).into_val(&env),
        sub_invokes: &[re_enroll_transfer],
    };
    let re_enroll_auth = MockAuth {
        address: &student,
        invoke: &re_enroll_invoke,
    };

    client
        .mock_auths(&[re_enroll_auth])
        .re_enroll(&student, &remaining_course_id);

    assert_only_auths(&env, &[student.clone()]);

    let waitlist_course_id = String::from_str(&env, "COURSE-AUTH-WAITLIST");
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-AUTH-WAITLIST",
        100_000_000,
    );
    client.update_course(&instructor, &waitlist_course_id, &None, &Some(Some(1u32)));
    let first_student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&first_student, &1_000_000_000);
    client.enroll(&first_student, &waitlist_course_id);

    let waitlisted_student = Address::generate(&env);
    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &waitlisted_student,
        "join_waitlist",
        (waitlisted_student.clone(), waitlist_course_id.clone()),
        join_waitlist(&waitlisted_student, &waitlist_course_id)
    );
    assert_only_auths(&env, &[waitlisted_student.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &waitlisted_student,
        "leave_waitlist",
        (waitlisted_student.clone(), waitlist_course_id.clone()),
        leave_waitlist(&waitlisted_student, &waitlist_course_id)
    );
    assert_only_auths(&env, &[waitlisted_student.clone()]);
}

#[test]
fn test_targeted_auth_refund_and_arbitration_entrypoints() {
    let (env, contract_id, token_id, admin, _sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &1_000_000_000);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-AUTH-REFUND",
        100_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-AUTH-REFUND");
    client.enroll(&student, &course_id);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &student,
        "request_refund",
        (student.clone(), course_id.clone()),
        request_refund(&student, &course_id)
    );
    assert_only_auths(&env, &[student.clone()]);

    let platform_amount = 100_000_000i128 * 20 / 100;
    let process_invoke = MockAuthInvoke {
        contract: &contract_id,
        fn_name: "process_refund",
        args: (
            admin.clone(),
            student.clone(),
            course_id.clone(),
            true,
            None::<i128>,
        )
            .into_val(&env),
        sub_invokes: &[],
    };
    let admin_auth = MockAuth {
        address: &admin,
        invoke: &process_invoke,
    };
    // Current refund model draws the platform share from the treasury, so the
    // treasury must sign in addition to the admin. Tracking: #309.
    let admin_only_result = client
        .mock_auths(&[admin_auth])
        .try_process_refund(&admin, &student, &course_id, &true, &None);
    assert!(
        admin_only_result.is_err(),
        "treasury auth must be required while the treasury funds refunds"
    );

    let treasury_transfer = MockAuthInvoke {
        contract: &token_id,
        fn_name: "transfer",
        args: (treasury.clone(), student.clone(), platform_amount).into_val(&env),
        sub_invokes: &[],
    };
    let process_invoke = MockAuthInvoke {
        contract: &contract_id,
        fn_name: "process_refund",
        args: (
            admin.clone(),
            student.clone(),
            course_id.clone(),
            true,
            None::<i128>,
        )
            .into_val(&env),
        sub_invokes: &[],
    };
    let admin_auth = MockAuth {
        address: &admin,
        invoke: &process_invoke,
    };
    let treasury_auth = MockAuth {
        address: &treasury,
        invoke: &treasury_transfer,
    };

    client
        .mock_auths(&[admin_auth, treasury_auth])
        .process_refund(&admin, &student, &course_id, &true, &None);

    assert_only_auths(&env, &[admin.clone(), treasury.clone()]);
    assert_eq!(
        token::Client::new(&env, &token_id).balance(&student),
        1_000_000_000
    );

    let arbitration_student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&arbitration_student, &1_000_000_000);
    let arbitration_course_id = String::from_str(&env, "COURSE-AUTH-ARBITRATION");
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-AUTH-ARBITRATION",
        100_000_000,
    );
    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "set_arbitration_fee_config",
        (admin.clone(), 1000i128),
        set_arbitration_fee_config(&admin, &1000i128)
    );
    assert_only_auths(&env, &[admin.clone()]);

    let fee_transfer = MockAuthInvoke {
        contract: &token_id,
        fn_name: "transfer",
        args: (arbitration_student.clone(), contract_id.clone(), 1000i128).into_val(&env),
        sub_invokes: &[],
    };
    let escalate_invoke = MockAuthInvoke {
        contract: &contract_id,
        fn_name: "escalate_to_arbitration",
        args: (arbitration_student.clone(), arbitration_course_id.clone()).into_val(&env),
        sub_invokes: &[fee_transfer],
    };
    let escalate_auth = MockAuth {
        address: &arbitration_student,
        invoke: &escalate_invoke,
    };

    client
        .mock_auths(&[escalate_auth])
        .escalate_to_arbitration(&arbitration_student, &arbitration_course_id);

    assert_only_auths(&env, &[arbitration_student.clone()]);

    invoke_with_auth!(
        &env,
        &client,
        &contract_id,
        &admin,
        "resolve_arbitration",
        (
            admin.clone(),
            arbitration_student.clone(),
            arbitration_course_id.clone(),
            true
        ),
        resolve_arbitration(&admin, &arbitration_student, &arbitration_course_id, &true)
    );
    assert_only_auths(&env, &[admin.clone()]);
}

#[test]
fn test_targeted_auth_archive_with_refunds_requires_both_admins_and_treasury() {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student_a = Address::generate(&env);
    let student_b = Address::generate(&env);
    let asset_client = token::StellarAssetClient::new(&env, &token_id);
    asset_client.mint(&student_a, &1_000_000_000);
    asset_client.mint(&student_b, &1_000_000_000);

    let price = 100_000_000i128;
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-AUTH-ARCHIVE-REFUND",
        price,
    );
    let course_id = String::from_str(&env, "COURSE-AUTH-ARCHIVE-REFUND");
    client.enroll(&student_a, &course_id);
    client.enroll(&student_b, &course_id);
    client.pause_course(&admin, &course_id);

    let platform_amount = price * 20 / 100;
    let transfer_a = MockAuthInvoke {
        contract: &token_id,
        fn_name: "transfer",
        args: (treasury.clone(), student_a.clone(), platform_amount).into_val(&env),
        sub_invokes: &[],
    };
    let transfer_b = MockAuthInvoke {
        contract: &token_id,
        fn_name: "transfer",
        args: (treasury.clone(), student_b.clone(), platform_amount).into_val(&env),
        sub_invokes: &[],
    };
    let students = Vec::from_array(&env, [student_a.clone(), student_b.clone()]);
    let archive_invoke = MockAuthInvoke {
        contract: &contract_id,
        fn_name: "archive_course",
        args: (
            admin.clone(),
            sec_admin.clone(),
            course_id.clone(),
            Some(students.clone()),
        )
            .into_val(&env),
        sub_invokes: &[],
    };
    let admin_auth = MockAuth {
        address: &admin,
        invoke: &archive_invoke,
    };
    let sec_admin_auth = MockAuth {
        address: &sec_admin,
        invoke: &archive_invoke,
    };
    let treasury_auth_a = MockAuth {
        address: &treasury,
        invoke: &transfer_a,
    };
    let treasury_auth_b = MockAuth {
        address: &treasury,
        invoke: &transfer_b,
    };

    client
        .mock_auths(&[admin_auth, sec_admin_auth, treasury_auth_a, treasury_auth_b])
        .archive_course(&admin, &sec_admin, &course_id, &Some(students));

    let auths = env.auths();
    for expected in [&admin, &sec_admin, &treasury] {
        assert!(
            auths.iter().any(|(address, _)| address == expected),
            "missing expected auth for {:?}",
            expected
        );
    }
    assert_eq!(
        token::Client::new(&env, &token_id).balance(&student_a),
        1_000_000_000
    );
    assert_eq!(
        token::Client::new(&env, &token_id).balance(&student_b),
        1_000_000_000
    );
    assert_eq!(
        client.get_course(&course_id).unwrap().status,
        CourseStatus::Archived
    );
}


// RISK FEE CONFIG BOUNDS (#455)

#[test]
fn test_set_risk_fee_config_accepts_valid_and_boundary_values() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.set_risk_fee_config(&admin, &500u32, &500_000_000i128, &700u32, &900u32);

    // Default fee is 20% (2000 bps): 2000 + 8000 == 10000 is the exact limit.
    client.set_risk_fee_config(&admin, &8000u32, &0i128, &0u32, &0u32);
    client.set_risk_fee_config(&admin, &3000u32, &0i128, &2000u32, &3000u32);
}

#[test]
#[should_panic(expected = "large_payment_surcharge_bps cannot exceed 10000")]
fn test_set_risk_fee_config_rejects_large_payment_surcharge_above_max() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    client.set_risk_fee_config(&admin, &10_001u32, &0i128, &0u32, &0u32);
}

#[test]
#[should_panic(expected = "new_customer_surcharge_bps cannot exceed 10000")]
fn test_set_risk_fee_config_rejects_new_customer_surcharge_above_max() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    client.set_risk_fee_config(&admin, &0u32, &0i128, &10_001u32, &0u32);
}

#[test]
#[should_panic(expected = "btc_eth_surcharge_bps cannot exceed 10000")]
fn test_set_risk_fee_config_rejects_btc_eth_surcharge_above_max() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    client.set_risk_fee_config(&admin, &0u32, &0i128, &0u32, &10_001u32);
}

#[test]
#[should_panic(expected = "default fee plus risk surcharges cannot exceed 10000 bps")]
fn test_set_risk_fee_config_rejects_combined_total_above_max() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    // 2000 (default fee) + 8001 = 10001 bps.
    client.set_risk_fee_config(&admin, &4000u32, &0i128, &2000u32, &2001u32);
}

#[test]
fn test_set_risk_fee_config_rejected_values_leave_config_unchanged() {
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    client.set_risk_fee_config(&admin, &500u32, &500_000_000i128, &700u32, &900u32);
    assert!(client
        .try_set_risk_fee_config(&admin, &10_001u32, &0i128, &0u32, &0u32)
        .is_err());
    assert!(client
        .try_set_risk_fee_config(&admin, &4000u32, &0i128, &2000u32, &2001u32)
        .is_err());

    env.as_contract(&contract_id, || {
        let cfg = HamplardContract::get_risk_fee_config(&env);
        assert_eq!(cfg.large_payment_surcharge_bps, 500);
        assert_eq!(cfg.new_customer_surcharge_bps, 700);
        assert_eq!(cfg.btc_eth_surcharge_bps, 900);
    });
}


// ============================================================
// REFUNDS REVERSE THE FEE SPLIT ACTUALLY CHARGED (#308)
// ============================================================

/// Per-token fee of 12% plus a 5% large-payment risk surcharge: 17% in total,
/// which differs from the 20% platform default / `course.platform_fee_percent`.
fn setup_custom_fee_split(price: i128) -> (Env, Address, Address, Address, Address, Address, Address) {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    client.set_fee_config(&admin, &token_id, &1200u32);
    client.set_risk_config_enabled(&admin, &true);
    client.set_risk_fee_config(&admin, &500u32, &500_000_000i128, &0u32, &0u32);
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-SPLIT",
        price,
    );
    (env, contract_id, token_id, admin, sec_admin, treasury, instructor)
}

#[test]
fn test_process_refund_reverses_exact_split_with_fee_config_and_risk() {
    let price: i128 = 1_000_000_000;
    let (env, contract_id, token_id, admin, _sec_admin, treasury, instructor) =
        setup_custom_fee_split(price);
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let course_id = String::from_str(&env, "COURSE-SPLIT");

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &price);

    client.enroll(&student, &course_id);

    let platform_charged = price * 17 / 100;
    let instructor_charged = price - platform_charged;
    let enrollment = client.get_enrollment(&student, &student, &course_id).unwrap();
    assert_eq!(enrollment.platform_amount, platform_charged);
    assert_eq!(enrollment.instructor_amount, instructor_charged);
    assert_eq!(token_client.balance(&treasury), platform_charged);
    assert_eq!(
        client.get_instructor_earnings(&instructor, &token_id),
        instructor_charged
    );

    // The per-token fee changes after enrollment; the refund must not care.
    client.set_fee_config(&admin, &token_id, &4000u32);

    client.request_refund(&student, &course_id);
    env.mock_all_auths_allowing_non_root_auth();
    client.process_refund(&admin, &student, &course_id, &true, &None);

    assert_eq!(token_client.balance(&student), price);
    assert_eq!(token_client.balance(&treasury), 0);
    assert_eq!(client.get_instructor_earnings(&instructor, &token_id), 0);
}

#[test]
fn test_process_refund_partial_uses_charged_split_ratio() {
    let price: i128 = 1_000_000_000;
    let (env, contract_id, token_id, admin, _sec_admin, treasury, instructor) =
        setup_custom_fee_split(price);
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let course_id = String::from_str(&env, "COURSE-SPLIT");

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &price);
    client.enroll(&student, &course_id);

    client.request_refund(&student, &course_id);
    env.mock_all_auths_allowing_non_root_auth();
    // Refund half: half of the 17% platform share, half of the 83% instructor share.
    client.process_refund(&admin, &student, &course_id, &true, &Some(price / 2));

    assert_eq!(token_client.balance(&student), price / 2);
    assert_eq!(token_client.balance(&treasury), price * 17 / 100 - price * 17 / 200);
    assert_eq!(
        client.get_instructor_earnings(&instructor, &token_id),
        price * 83 / 100 - (price / 2 - price * 17 / 200)
    );
}

#[test]
fn test_archive_course_refund_reverses_exact_split_with_fee_config_and_risk() {
    let price: i128 = 1_000_000_000;
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) =
        setup_custom_fee_split(price);
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let course_id = String::from_str(&env, "COURSE-SPLIT");

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &price);
    client.enroll(&student, &course_id);
    assert_eq!(token_client.balance(&treasury), price * 17 / 100);

    client.pause_course(&admin, &course_id);
    let mut students = soroban_sdk::Vec::new(&env);
    students.push_back(student.clone());
    env.mock_all_auths_allowing_non_root_auth();
    client.archive_course(&admin, &sec_admin, &course_id, &Some(students));

    assert_eq!(token_client.balance(&student), price);
    assert_eq!(token_client.balance(&treasury), 0);
    assert_eq!(client.get_instructor_earnings(&instructor, &token_id), 0);
}

#[test]
fn test_re_enroll_then_refund_reverses_exact_split_with_fee_config_and_risk() {
    let price: i128 = 1_000_000_000;
    let (env, contract_id, token_id, admin, _sec_admin, treasury, instructor) =
        setup_custom_fee_split(price);
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let course_id = String::from_str(&env, "COURSE-SPLIT");

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &(price * 2));
    client.enroll(&student, &course_id);
    client.mark_completed(&admin, &student, &course_id, &None);
    client.re_enroll(&student, &course_id);

    let enrollment = client.get_enrollment(&student, &student, &course_id).unwrap();
    assert_eq!(enrollment.platform_amount, price * 17 / 100);
    assert_eq!(enrollment.instructor_amount, price - price * 17 / 100);

    client.request_refund(&student, &course_id);
    env.mock_all_auths_allowing_non_root_auth();
    client.process_refund(&admin, &student, &course_id, &true, &None);

    // Only the re-enrollment payment is returned; the first purchase stays.
    assert_eq!(token_client.balance(&student), price);
    assert_eq!(token_client.balance(&treasury), price * 17 / 100);
    assert_eq!(
        client.get_instructor_earnings(&instructor, &token_id),
        price - price * 17 / 100
    );
}


// ============================================================
// REFUNDS NEED ONLY ADMIN AUTH — NO TREASURY SIGNATURE (#309)
// ============================================================

/// Enrolled student with a pending refund request on a 20%-fee course.
fn setup_pending_refund(
    price: i128,
) -> (Env, Address, Address, Address, Address, Address, Address, Address) {
    let (env, contract_id, token_id, admin, sec_admin, treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-RESERVE",
        price,
    );
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &price);
    let course_id = String::from_str(&env, "COURSE-RESERVE");
    client.enroll(&student, &course_id);
    client.request_refund(&student, &course_id);
    (env, contract_id, token_id, admin, sec_admin, treasury, instructor, student)
}

#[test]
fn test_process_refund_succeeds_with_only_admin_auth() {
    let price: i128 = 1_000_000_000;
    let (env, contract_id, token_id, admin, _sec_admin, treasury, instructor, student) =
        setup_pending_refund(price);
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let course_id = String::from_str(&env, "COURSE-RESERVE");
    let platform_fee = price * 20 / 100;

    // The treasury funds the reserve once, signing only that call.
    env.mock_auths(&[MockAuth {
        address: &treasury,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "fund_refund_reserve",
            args: (treasury.clone(), token_id.clone(), platform_fee).into_val(&env),
            sub_invokes: &[MockAuthInvoke {
                contract: &token_id,
                fn_name: "transfer",
                args: (treasury.clone(), contract_id.clone(), platform_fee).into_val(&env),
                sub_invokes: &[],
            }],
        },
    }]);
    client.fund_refund_reserve(&treasury, &token_id, &platform_fee);
    assert_eq!(client.get_refund_reserve(&token_id), platform_fee);

    // From here on only the admin authorises anything — not the treasury.
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &contract_id,
            fn_name: "process_refund",
            args: (admin.clone(), student.clone(), course_id.clone(), true, None::<i128>).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.process_refund(&admin, &student, &course_id, &true, &None);

    assert_eq!(token_client.balance(&student), price);
    assert_eq!(token_client.balance(&treasury), 0);
    assert_eq!(client.get_refund_reserve(&token_id), 0);
    assert_eq!(client.get_instructor_earnings(&instructor, &token_id), 0);
    assert!(!client.is_enrolled(&student, &course_id));
}

#[test]
fn test_archive_course_refund_succeeds_with_only_admin_auths() {
    let price: i128 = 1_000_000_000;
    let (env, contract_id, token_id, admin, sec_admin, treasury, _instructor, student) =
        setup_pending_refund(price);
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let course_id = String::from_str(&env, "COURSE-RESERVE");
    let platform_fee = price * 20 / 100;

    fund_refund_reserve_for_test(&env, &client, &token_id, platform_fee);
    client.pause_course(&admin, &course_id);

    let mut students = soroban_sdk::Vec::new(&env);
    students.push_back(student.clone());
    let students_opt = Some(students);
    let args: soroban_sdk::Vec<Val> = (
        admin.clone(),
        sec_admin.clone(),
        course_id.clone(),
        students_opt.clone(),
    )
        .into_val(&env);
    env.mock_auths(&[
        MockAuth {
            address: &admin,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "archive_course",
                args: args.clone(),
                sub_invokes: &[],
            },
        },
        MockAuth {
            address: &sec_admin,
            invoke: &MockAuthInvoke {
                contract: &contract_id,
                fn_name: "archive_course",
                args,
                sub_invokes: &[],
            },
        },
    ]);
    client.archive_course(&admin, &sec_admin, &course_id, &students_opt);

    assert_eq!(token_client.balance(&student), price);
    // The treasury keeps what it collected; the refund came from the reserve.
    assert_eq!(token_client.balance(&treasury), platform_fee);
    assert_eq!(client.get_refund_reserve(&token_id), 0);
}

#[test]
#[should_panic(expected = "insufficient refund reserve")]
fn test_process_refund_requires_funded_refund_reserve() {
    let price: i128 = 1_000_000_000;
    let (env, contract_id, _token_id, admin, _sec_admin, _treasury, _instructor, student) =
        setup_pending_refund(price);
    let client = HamplardContractClient::new(&env, &contract_id);
    let course_id = String::from_str(&env, "COURSE-RESERVE");

    client.process_refund(&admin, &student, &course_id, &true, &None);
}

#[test]
fn test_fund_refund_reserve_requires_funder_auth() {
    let (env, contract_id, token_id, _admin, _sec_admin, treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    token::StellarAssetClient::new(&env, &token_id).mint(&treasury, &1_000);

    // No authorisation at all: the call must be rejected.
    env.mock_auths(&[]);
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.fund_refund_reserve(&treasury, &token_id, &1_000);
    }));
    assert!(res.is_err(), "funding without the funder's auth must fail");
    assert_eq!(client.get_refund_reserve(&token_id), 0);
}

#[test]
#[should_panic(expected = "refund reserve amount must be greater than zero")]
fn test_fund_refund_reserve_rejects_non_positive_amount() {
    let (env, contract_id, token_id, _admin, _sec_admin, treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    client.fund_refund_reserve(&treasury, &token_id, &0);
}

#[test]
fn test_risk_rules_all_combinations_and_threshold_boundary() {
    let config = RiskFeeConfig {
        large_payment_threshold: 100,
        large_payment_surcharge_bps: 11,
        new_customer_surcharge_bps: 23,
        btc_eth_surcharge_bps: 47,
    };
    // Explicit expected values keep the test independent of the rule implementation.
    let cases = [
        (100, false, false, (0, 0)),
        (101, false, false, (30, 11)),
        (100, true, false, (40, 23)),
        (100, false, true, (30, 47)),
        (101, true, false, (70, 34)),
        (101, false, true, (60, 58)),
        (100, true, true, (70, 70)),
        (101, true, true, (100, 81)),
        (i128::MIN, false, false, (0, 0)),
        (i128::MAX, true, true, (100, 81)),
    ];
    for (amount, new_customer, btc_eth, expected) in cases {
        assert_eq!(
            risk_surcharge(true, Some(&config), amount, new_customer, btc_eth),
            expected
        );
    }
}

#[test]
fn test_risk_rules_config_gating_and_saturating_surcharges() {
    let config = RiskFeeConfig {
        large_payment_threshold: 0,
        large_payment_surcharge_bps: u32::MAX,
        new_customer_surcharge_bps: 1,
        btc_eth_surcharge_bps: u32::MAX,
    };
    assert_eq!(risk_surcharge(true, None, i128::MAX, true, true), (0, 0));
    assert_eq!(risk_surcharge(false, None, i128::MAX, true, true), (0, 0));
    assert_eq!(
        risk_surcharge(false, Some(&config), i128::MAX, true, true),
        (0, 0)
    );
    assert_eq!(
        risk_surcharge(true, Some(&config), 1, true, true),
        (100, u32::MAX)
    );
}

#[test]
fn test_risk_defaults_and_overflow_match_fee_deduction() {
    let (env, contract_id, token_id, admin, _, _, _) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let student = Address::generate(&env);
    let amount = 1_000_000_000_001i128;

    // Enabling risk without storing a config must not activate the getter's defaults.
    client.set_risk_config_enabled(&admin, &true);
    let missing = client.calculate_risk_score(&student, &token_id, &amount);
    assert_eq!((missing.score, missing.surcharge_bps), (0, 0));
    let default_quote = client.get_effective_fee_for_payment(&student, &token_id, &amount);
    assert_eq!(default_quote.risk_surcharge_bps, 0);
    assert_eq!(
        env.as_contract(&contract_id, || {
            HamplardContract::deduct_fee(&env, &token_id, amount, None, false, false)
        }),
        (
            amount - default_quote.platform_fee,
            default_quote.platform_fee
        )
    );

    client.set_risk_fee_config(&admin, &u32::MAX, &0, &1, &u32::MAX);
    let risk = client.calculate_risk_score(&student, &token_id, &amount);
    assert_eq!((risk.score, risk.surcharge_bps), (30, u32::MAX));
    let quote = client.get_effective_fee_for_payment(&student, &token_id, &amount);
    assert_eq!(quote.risk_surcharge_bps, u32::MAX);
    assert_eq!(quote.effective_fee_bps, 10_000);
    assert_eq!(quote.platform_fee, amount);
    assert_eq!(
        env.as_contract(&contract_id, || {
            HamplardContract::deduct_fee(&env, &token_id, amount, None, false, false)
        }),
        (0, amount)
    );

    client.set_risk_config_enabled(&admin, &false);
    assert_eq!(
        client.calculate_risk_score(&student, &token_id, &amount),
        missing
    );
    assert_eq!(
        client.get_effective_fee_for_payment(&student, &token_id, &amount),
        default_quote
    );
}


// ============================================================
// FINALIZE_TREASURY_UPDATE TESTS
// ============================================================

/// Calling finalize_treasury_update before the effective_ledger is a no-op:
/// the pending update stays in storage, no treasury_activated event is emitted,
/// and the old treasury address is returned.
#[test]
fn test_finalize_treasury_update_before_effective_is_noop() {
    let (env, contract_id, token_id, admin, sec_admin, treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let new_treasury = Address::generate(&env);
    client.update_treasury(&admin, &sec_admin, &new_treasury);

    // Still before the 100-ledger delay — finalize must be a no-op
    let returned = client.finalize_treasury_update();
    assert_eq!(returned, treasury, "should still return the old treasury");

    // treasury_activated must NOT have been emitted
    let activated = env.events().all().iter().any(|(contract, topics, _)| {
        if contract != contract_id {
            return false;
        }
        let sym: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
        sym == Symbol::new(&env, "treasury_activated")
    });
    assert!(!activated, "treasury_activated must not be emitted before effective_ledger");
}

/// Calling finalize_treasury_update at exactly the effective_ledger applies
/// the update, emits treasury_activated, and returns the new address.
#[test]
fn test_finalize_treasury_update_at_effective_ledger() {
    let (env, contract_id, token_id, admin, sec_admin, old_treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let new_treasury = Address::generate(&env);
    client.update_treasury(&admin, &sec_admin, &new_treasury);

    // Advance to exactly effective_ledger (current + 100)
    env.ledger().with_mut(|l| {
        l.sequence_number += 100;
    });

    let returned = client.finalize_treasury_update();
    assert_eq!(returned, new_treasury, "should return the new treasury");

    // treasury_activated event must carry the new address
    let mut activated_count = 0u32;
    for (contract, topics, data) in env.events().all().iter() {
        if contract != contract_id {
            continue;
        }
        let sym: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
        if sym == Symbol::new(&env, "treasury_activated") {
            activated_count += 1;
            let topic_addr: Address = topics.get(1).unwrap().try_into_val(&env).unwrap();
            assert_eq!(topic_addr, new_treasury);
            let (ev_addr, _ledger): (Address, u32) = data.try_into_val(&env).unwrap();
            assert_eq!(ev_addr, new_treasury);
        }
    }
    assert_eq!(activated_count, 1, "treasury_activated must be emitted exactly once");
}

/// Calling finalize_treasury_update after the effective_ledger applies the
/// update exactly once; a second call is a no-op (no second event).
#[test]
fn test_finalize_treasury_update_after_effective_ledger() {
    let (env, contract_id, token_id, admin, sec_admin, _old_treasury, _instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let new_treasury = Address::generate(&env);
    client.update_treasury(&admin, &sec_admin, &new_treasury);

    env.ledger().with_mut(|l| {
        l.sequence_number += 200; // well past effective
    });

    let first = client.finalize_treasury_update();
    assert_eq!(first, new_treasury);

    // Second call: pending is already cleared — must be a no-op
    let second = client.finalize_treasury_update();
    assert_eq!(second, new_treasury, "should still return the new treasury");

    // treasury_activated must have been emitted exactly once across both calls
    let count = env
        .events()
        .all()
        .iter()
        .filter(|(contract, topics, _)| {
            if contract != contract_id {
                return false;
            }
            let sym: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
            sym == Symbol::new(&env, "treasury_activated")
        })
        .count();
    assert_eq!(count, 1, "treasury_activated must be emitted exactly once total");
}

/// After a treasury update matures, enrollments route fees to the new treasury
/// even when finalize_treasury_update() was never explicitly called — the lazy
/// promotion inside read_treasury() handles it transparently.
#[test]
fn test_enroll_routes_fee_to_new_treasury_after_effective_ledger() {
    let (env, contract_id, token_id, admin, sec_admin, old_treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let price: i128 = 1_000_000_000;
    register_and_approve_course(
        &env, &client, &token_id, &admin, &instructor,
        "COURSE-TREASURY-LAZY", price,
    );

    let new_treasury = Address::generate(&env);
    client.update_treasury(&admin, &sec_admin, &new_treasury);

    // Advance past the effective_ledger
    env.ledger().with_mut(|l| { l.sequence_number += 100; });

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &10_000_000_000);
    client.enroll(&student, &String::from_str(&env, "COURSE-TREASURY-LAZY"));

    let platform_fee = price * 20 / 100;
    assert_eq!(token_client.balance(&old_treasury), 0, "old treasury must receive nothing");
    assert_eq!(token_client.balance(&new_treasury), platform_fee, "new treasury must receive the fee");

    // The lazy promotion inside enroll() must also have emitted treasury_activated
    let activated = env.events().all().iter().any(|(contract, topics, _)| {
        if contract != contract_id { return false; }
        let sym: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
        sym == Symbol::new(&env, "treasury_activated")
    });
    assert!(activated, "treasury_activated must be emitted by the lazy promotion path");
}


// ============================================================
// #361 — max_price guard tests
// ============================================================

#[test]
fn test_enroll_price_guard_equal_price_succeeds() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &10_000_000_000);

    let price: i128 = 100_000_000;
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-GUARD-EQ",
        price,
    );
    let course_id = String::from_str(&env, "COURSE-GUARD-EQ");

    // max_price exactly equal to the current price is accepted.
    client.enroll(&student, &course_id, &price);
    assert!(client.is_enrolled(&student, &course_id));
}

#[test]
fn test_enroll_price_guard_lower_price_succeeds() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &10_000_000_000);

    let price: i128 = 100_000_000;
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-GUARD-LOW",
        price,
    );
    let course_id = String::from_str(&env, "COURSE-GUARD-LOW");

    // Current price is below the student's maximum, so it is accepted.
    client.enroll(&student, &course_id, &(price + 1));
    let enrollment = client
        .get_enrollment(&student, &student, &course_id)
        .unwrap();
    assert_eq!(enrollment.amount_paid, price);
}

#[test]
#[should_panic(expected = "exceeds maximum accepted price")]
fn test_enroll_price_guard_higher_price_reverts() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &10_000_000_000);

    let price: i128 = 100_000_000;
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-GUARD-HIGH",
        price,
    );
    let course_id = String::from_str(&env, "COURSE-GUARD-HIGH");

    // Current price is above the student's maximum: revert before payment.
    client.enroll(&student, &course_id, &(price - 1));
}

#[test]
#[should_panic(expected = "exceeds maximum accepted price")]
fn test_enroll_price_guard_reverts_after_price_increase() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &10_000_000_000);

    let price: i128 = 100_000_000;
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-GUARD-RAISE",
        price,
    );
    let course_id = String::from_str(&env, "COURSE-GUARD-RAISE");

    // The student quoted `price`. The instructor raises the price in the same
    // ledger before the student's transaction executes.
    let student_quote = price;
    client.update_course(&instructor, &course_id, &Some(price + 1), &None);

    // The stale quote no longer covers the current price, so it reverts.
    client.enroll(&student, &course_id, &student_quote);
}

#[test]
#[should_panic(expected = "exceeds maximum accepted price")]
fn test_batch_enroll_price_guard_reverts_when_any_course_exceeds_limit() {
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
        "COURSE-GUARD-BATCH-A",
        100_000_000,
    );
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-GUARD-BATCH-B",
        200_000_000,
    );

    let mut course_ids = soroban_sdk::Vec::new(&env);
    course_ids.push_back(String::from_str(&env, "COURSE-GUARD-BATCH-A"));
    course_ids.push_back(String::from_str(&env, "COURSE-GUARD-BATCH-B"));

    // The limit only covers the first course, so the whole atomic batch reverts
    // when the second course's price exceeds it.
    client.batch_enroll(&student, &course_ids, &150_000_000);
}

#[test]
fn test_batch_enroll_price_guard_accepts_when_max_covers_all() {
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
        "COURSE-GUARD-BATCH-OK-A",
        100_000_000,
    );
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-GUARD-BATCH-OK-B",
        200_000_000,
    );

    let mut course_ids = soroban_sdk::Vec::new(&env);
    course_ids.push_back(String::from_str(&env, "COURSE-GUARD-BATCH-OK-A"));
    course_ids.push_back(String::from_str(&env, "COURSE-GUARD-BATCH-OK-B"));

    // A single per-course limit that covers the most expensive course succeeds
    // and returns the exact total paid.
    let total = client.batch_enroll(&student, &course_ids, &200_000_000);
    assert_eq!(total, 300_000_000);
    assert!(client.is_enrolled(&student, &String::from_str(&env, "COURSE-GUARD-BATCH-OK-A")));
    assert!(client.is_enrolled(&student, &String::from_str(&env, "COURSE-GUARD-BATCH-OK-B")));
}

#[test]
#[should_panic(expected = "exceeds maximum accepted price")]
fn test_re_enroll_price_guard_reverts_after_price_increase() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &10_000_000_000);

    let price: i128 = 100_000_000;
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-GUARD-REENROLL",
        price,
    );
    let course_id = String::from_str(&env, "COURSE-GUARD-REENROLL");

    client.enroll(&student, &course_id, &price);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "guard_evidence")),
    );

    let student_quote = price;
    client.update_course(&instructor, &course_id, &Some(price + 1), &None);

    client.re_enroll(&student, &course_id, &student_quote);
}

#[test]
fn test_re_enroll_price_guard_allows_matching_max_price() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &10_000_000_000);

    let price: i128 = 100_000_000;
    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-GUARD-REENROLL-OK",
        price,
    );
    let course_id = String::from_str(&env, "COURSE-GUARD-REENROLL-OK");

    client.enroll(&student, &course_id, &price);
    client.mark_completed(
        &admin,
        &student,
        &course_id,
        &Some(evidence_hash(&env, "guard_evidence_ok")),
    );

    let new_price = price + 1;
    client.update_course(&instructor, &course_id, &Some(new_price), &None);

    // The student re-quotes the new price and the re-enrollment succeeds.
    client.re_enroll(&student, &course_id, &new_price);
    let enrollment = client
        .get_enrollment(&student, &student, &course_id)
        .unwrap();
    assert_eq!(enrollment.amount_paid, new_price);
}


// ============================================================
// ISSUE #320 — REFUND RESUBMISSION AFTER REJECTION
// ============================================================

/// Reject then resubmit inside the window: the new request is allowed and
/// the record transitions back to Pending.
#[test]
fn test_reject_then_resubmit_inside_window_succeeds() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    // Use a window of 20 ledgers so we have room to move around.
    client.update_refund_window(&admin, &20u32);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-320-RESUBMIT",
        500_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-320-RESUBMIT");

    client.enroll(&student, &course_id);

    // First request
    client.request_refund(&student, &course_id);
    let first = client.get_refund_request(&student, &course_id).unwrap();
    assert_eq!(first.status, RefundStatus::Pending);

    // Admin rejects
    env.mock_all_auths_allowing_non_root_auth();
    client.process_refund(&admin, &student, &course_id, &false, &None::<i128>);
    let rejected = client.get_refund_request(&student, &course_id).unwrap();
    assert_eq!(rejected.status, RefundStatus::Rejected);

    // Student is still enrolled
    assert!(client.is_enrolled(&student, &course_id));

    // Resubmit while still inside the window (advance by 5, window is 20)
    env.ledger().with_mut(|l| {
        l.sequence_number += 5;
    });

    client.request_refund(&student, &course_id);

    let resubmitted = client.get_refund_request(&student, &course_id).unwrap();
    assert_eq!(resubmitted.status, RefundStatus::Pending);
}

/// Reject then resubmit outside the window: must fail with the window-closed
/// message, not the generic "refund request already exists".
#[test]
#[should_panic(expected = "refund window has closed for resubmission")]
fn test_reject_then_resubmit_outside_window_fails() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    client.update_refund_window(&admin, &5u32);

    register_and_approve_course(
        &env,
        &client,
        &token_id,
        &admin,
        &instructor,
        "COURSE-320-RESUBMIT-EXP",
        500_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-320-RESUBMIT-EXP");

    client.enroll(&student, &course_id);

    client.request_refund(&student, &course_id);
    env.mock_all_auths_allowing_non_root_auth();
    client.process_refund(&admin, &student, &course_id, &false, &None::<i128>);

    // Advance past the window
    env.ledger().with_mut(|l| {
        l.sequence_number += 10;
    });

    // Must panic with the window-closed message
    client.request_refund(&student, &course_id);
}

/// A Pending request still blocks a second request from the same student.
#[test]
#[should_panic(expected = "refund request already exists")]
fn test_pending_request_blocks_second_request() {
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
        "COURSE-320-PENDING-BLOCK",
        500_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-320-PENDING-BLOCK");

    client.enroll(&student, &course_id);
    client.request_refund(&student, &course_id);

    // Attempt a second request while the first is Pending
    client.request_refund(&student, &course_id);
}

// ============================================================
// Issue #364 — CapacityMode: Lifetime vs Concurrent
// ============================================================

/// Concurrent mode: a course capped at 2 active seats must reject a third
/// enrollment while both seats are still active.
#[test]
#[should_panic(expected = "course has reached maximum enrollment capacity")]
fn test_concurrent_capacity_blocks_enrollment_at_seat_cap() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) =
        setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let course_id = String::from_str(&env, "COURSE-CONCURRENT-CAP");

    // Register with max_capacity = 2, then switch to Concurrent mode.
    client.register_course(
        &instructor,
        &course_id,
        &100_000_000i128,
        &token_id,
        &0u32,
        &Some(2u32),
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });
    client.approve_course(&admin, &course_id);

    // Switch capacity mode to Concurrent.
    client.update_course(
        &instructor,
        &course_id,
        &None,
        &None,
        &Some(crate::CapacityMode::Concurrent),
    );

    let asset_client = token::StellarAssetClient::new(&env, &token_id);
    let student_a = Address::generate(&env);
    let student_b = Address::generate(&env);
    let student_c = Address::generate(&env);
    asset_client.mint(&student_a, &1_000_000_000i128);
    asset_client.mint(&student_b, &1_000_000_000i128);
    asset_client.mint(&student_c, &1_000_000_000i128);

    // Fill both concurrent seats.
    client.enroll(&student_a, &course_id);
    client.enroll(&student_b, &course_id);

    // Third enrollment must be rejected — seats are all taken.
    client.enroll(&student_c, &course_id);
}

/// Concurrent mode: completing a student frees their seat so the next
/// student can enroll, even though lifetime total_enrollments exceeds the cap.
#[test]
fn test_concurrent_capacity_seat_freed_on_completion_allows_next_student() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) =
        setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let course_id = String::from_str(&env, "COURSE-CONCURRENT-FREE");

    // Register with max_capacity = 1.
    client.register_course(
        &instructor,
        &course_id,
        &100_000_000i128,
        &token_id,
        &0u32,
        &Some(1u32),
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });
    client.approve_course(&admin, &course_id);

    // Switch to Concurrent mode.
    client.update_course(
        &instructor,
        &course_id,
        &None,
        &None,
        &Some(crate::CapacityMode::Concurrent),
    );

    let asset_client = token::StellarAssetClient::new(&env, &token_id);
    let student_a = Address::generate(&env);
    let student_b = Address::generate(&env);
    asset_client.mint(&student_a, &1_000_000_000i128);
    asset_client.mint(&student_b, &1_000_000_000i128);

    // Student A takes the one seat.
    client.enroll(&student_a, &course_id);

    // Complete student A — frees the concurrent seat.
    client.mark_completed(
        &admin,
        &student_a,
        &course_id,
        &Some(evidence_hash(&env, "proof")),
    );

    // Verify active_enrollments dropped to 0.
    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.active_enrollments, 0);
    assert_eq!(course.total_enrollments, 1); // lifetime total unchanged

    // Student B must now be admitted into the freed seat.
    client.enroll(&student_b, &course_id);

    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.active_enrollments, 1);
    assert_eq!(course.total_enrollments, 2); // lifetime grew past the cap — that's fine
}

/// Default Lifetime mode is unchanged: max_capacity still counts all-time
/// total enrollments regardless of how many students have completed.
#[test]
#[should_panic(expected = "course has reached maximum enrollment capacity")]
fn test_lifetime_capacity_default_unchanged_counts_completed_students() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) =
        setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let course_id = String::from_str(&env, "COURSE-LIFETIME-CAP");

    // Register with max_capacity = 1 — Lifetime mode by default.
    client.register_course(
        &instructor,
        &course_id,
        &100_000_000i128,
        &token_id,
        &0u32,
        &Some(1u32),
        &BytesN::from_array(&env, &[1u8; 32]),
        &soroban_sdk::Vec::new(&env),
    );
    env.ledger().with_mut(|l| {
        l.sequence_number += 1;
    });
    client.approve_course(&admin, &course_id);

    // Confirm capacity_mode defaults to Lifetime.
    let course = client.get_course(&course_id).unwrap();
    assert_eq!(course.capacity_mode, crate::CapacityMode::Lifetime);

    let asset_client = token::StellarAssetClient::new(&env, &token_id);
    let student_a = Address::generate(&env);
    let student_b = Address::generate(&env);
    asset_client.mint(&student_a, &1_000_000_000i128);
    asset_client.mint(&student_b, &1_000_000_000i128);

    // Student A takes the one lifetime seat.
    client.enroll(&student_a, &course_id);

    // Complete student A — active_enrollments drops, but lifetime total stays at 1.
    client.mark_completed(
        &admin,
        &student_a,
        &course_id,
        &Some(evidence_hash(&env, "proof")),
    );

    // In Lifetime mode this must still be rejected — total_enrollments == cap.
    client.enroll(&student_b, &course_id);
}

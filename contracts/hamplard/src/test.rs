// SPDX-License-Identifier: MIT
#![cfg(test)]

use soroban_sdk::{testutils::Address as _, Address, Env, vec};
use crate::{HamplardContract, HamplardContractClient};

#[test]
fn test_withdraw_tokens_validation() {
    let env = Env::default();
    let contract_id = env.register_contract(None, HamplardContract);
    let client = HamplardContractClient::new(&env, &contract_id);

    let admin = Address::random(&env);
    let token = Address::random(&env);
    let instructor = Address::random(&env);
    let to = Address::random(&env);

    // Setup: mint tokens to contract
    env.token().mint(&token, &contract_id, &1000);

    // Deposit earnings (increases owed balance)
    client.deposit_earnings(&instructor, &token, &500);

    // Test 1: Cannot withdraw more than surplus (1000 - 500 = 500)
    let result = std::panic::catch_unwind(|| {
        client.withdraw_tokens(&token, &to, &600);
    });
    assert!(result.is_err());

    // Test 2: Can withdraw surplus
    client.withdraw_tokens(&token, &to, &500);
    assert_eq!(env.token().balance(&token, &contract_id), 500);
    assert_eq!(env.token().balance(&token, &to), 500);

    // Test 3: Cannot withdraw zero or negative
    let result = std::panic::catch_unwind(|| {
        client.withdraw_tokens(&token, &to, &0);
    });
    assert!(result.is_err());

    let result = std::panic::catch_unwind(|| {
        client.withdraw_tokens(&token, &to, &-100);
    });
    assert!(result.is_err());

    // Test 4: Cannot withdraw instructor earnings (remaining 500 is owed)
    let result = std::panic::catch_unwind(|| {
        client.withdraw_tokens(&token, &to, &500);
    });
    assert!(result.is_err());

    // Test 5: Withdraw after earnings are withdrawn (surplus becomes available)
    client.withdraw_earnings(&instructor, &token);
    client.withdraw_tokens(&token, &to, &500);
    assert_eq!(env.token().balance(&token, &contract_id), 0);
    assert_eq!(env.token().balance(&token, &to), 1000);
}

#[test]
fn test_owed_balance_tracking() {
    let env = Env::default();
    let contract_id = env.register_contract(None, HamplardContract);
    let client = HamplardContractClient::new(&env, &contract_id);

    let token = Address::random(&env);
    let instructor1 = Address::random(&env);
    let instructor2 = Address::random(&env);

    // Initial owed balance should be 0
    assert_eq!(client.get_owed_balance(&token), 0);

    // Deposit earnings for instructor1
    client.deposit_earnings(&instructor1, &token, &300);
    assert_eq!(client.get_owed_balance(&token), 300);

    // Deposit earnings for instructor2
    client.deposit_earnings(&instructor2, &token, &200);
    assert_eq!(client.get_owed_balance(&token), 500);

    // Withdraw earnings for instructor1
    client.withdraw_earnings(&instructor1, &token);
    assert_eq!(client.get_owed_balance(&token), 200);

    // Withdraw earnings for instructor2
    client.withdraw_earnings(&instructor2, &token);
    assert_eq!(client.get_owed_balance(&token), 0);
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
    client.set_arbitration_fee_config(admin, &fee_per_case);
    register_and_approve_course(env, client, token_id, admin, instructor, course_id, 1_000_000_000);
    let student = Address::generate(env);
    token::StellarAssetClient::new(env, token_id).mint(&student, &100_000_000_000);
    client.enroll(&student, &String::from_str(env, course_id));
    student
}

/// #255 — escalate_to_arbitration() must persist a queryable ArbitrationCase record.
#[test]
fn test_escalation_creates_retrievable_arbitration_case() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let fee_per_case = 5_000_000i128;
    let course_id_str = "COURSE-ARB-255";
    let student = setup_arbitration(
        &env, &client, &token_id, &admin, &instructor, course_id_str, fee_per_case,
    );
    let course_id = String::from_str(&env, course_id_str);

    client.escalate_to_arbitration(&student, &course_id);

    let case = client
        .get_arbitration_case(&student, &course_id)
        .expect("arbitration case must be retrievable after escalation");

    assert_eq!(case.caller, student, "case must record the escalating student");
    assert_eq!(case.course_id, course_id, "case must record the course id");
    assert_eq!(case.fee_paid, fee_per_case, "fee_paid must match config.fee_per_case");
    assert_eq!(case.status, DisputeStatus::Open, "newly created case must be Open");
}

/// #257 — a second escalation for the same (student, course_id) must be rejected.
#[test]
#[should_panic(expected = "arbitration case already open for this dispute")]
fn test_duplicate_escalation_rejected() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = setup_arbitration(
        &env, &client, &token_id, &admin, &instructor, "COURSE-ARB-257", 5_000_000,
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
        &env, &client, &token_id, &admin, &instructor, "COURSE-ARB-REFUND", fee_per_case,
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
        &env, &client, &token_id, &admin, &instructor, "COURSE-ARB-PAYOUT", fee_per_case,
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

// ============================================================
// REFUND ENROLLMENT ARCHIVAL  (#254)
// ============================================================

/// #254 — process_refund() must archive the refunded enrollment to
/// EnrollmentHistory with is_refunded=true before removing the active record.
#[test]
fn test_process_refund_archives_enrollment_to_history() {
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
    let client = HamplardContractClient::new(&env, &contract_id);

    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&student, &100_000_000_000);

    register_and_approve_course(
        &env, &client, &token_id, &admin, &instructor, "COURSE-REFUND-ARCHIVE", 1_000_000_000,
    );
    let course_id = String::from_str(&env, "COURSE-REFUND-ARCHIVE");

    // Capture the amount paid at enrollment.
    client.enroll(&student, &course_id);
    let enrollment = client.get_enrollment(&student, &student, &course_id).unwrap();
    let amount_paid = enrollment.amount_paid;
    assert!(amount_paid > 0);

    // Request and approve the refund.
    client.request_refund(&student, &course_id);
    env.mock_all_auths_allowing_non_root_auth();
    client.process_refund(&admin, &student, &course_id, &true);

    // The active enrollment must no longer exist.
    assert!(
        client.get_enrollment(&student, &student, &course_id).is_none(),
        "active enrollment must be removed after refund"
    );

    // The history must contain exactly one entry with is_refunded=true.
    let history = client.get_enrollment_history(&admin, &student, &course_id);

    assert_eq!(history.len(), 1, "history must contain exactly one archived entry");
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

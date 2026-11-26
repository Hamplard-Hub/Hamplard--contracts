use hamplard::{DataKey, HamplardContract, HamplardContractClient};
use soroban_sdk::{
    testutils::{storage::Instance, Address as _, Ledger},
    token, Address, BytesN, Env, String, Vec,
};

fn setup() -> (Env, Address, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let id = env.register(
        HamplardContract,
        (
            &admin,
            &Address::generate(&env),
            &Address::generate(&env),
            &20u32,
            &50u32,
            &1_000u32,
            &17_280u32,
        ),
    );
    let client = HamplardContractClient::new(&env, &id);
    let token = env
        .register_stellar_asset_contract_v2(Address::generate(&env))
        .address();
    client.add_approved_token(&admin, &token);
    let instructor = Address::generate(&env);
    for name in ["BUDGET-ONE", "BUDGET-TWO"] {
        let course = String::from_str(&env, name);
        client.register_course(
            &instructor,
            &course,
            &100_000_000,
            &token,
            &0,
            &None,
            &BytesN::from_array(&env, &[1; 32]),
            &Vec::new(&env),
        );
        env.ledger().with_mut(|ledger| ledger.sequence_number += 1);
        client.approve_course(&admin, &course);
    }
    let student = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token).mint(&student, &1_000_000_000);
    (env, id, admin, student, token)
}

#[test]
fn enrollment_counter_budget() {
    let (env, id, admin, student, _) = setup();
    let client = HamplardContractClient::new(&env, &id);
    client.set_platform_enrollment_cap(&admin, &Some(2));
    for name in ["BUDGET-ONE", "BUDGET-TWO"] {
        let before = env.as_contract(&id, || env.storage().instance().all());
        env.cost_estimate().budget().reset_default();
        let course = String::from_str(&env, name);
        client.enroll(&student, &course, &i128::MAX);
        let cpu = env.cost_estimate().budget().cpu_instruction_cost();
        let memory = env.cost_estimate().budget().memory_bytes_cost();
        let after = env.as_contract(&id, || env.storage().instance().all());
        println!(
            "{name}: cpu={cpu}, memory={memory}, instance_unchanged={}",
            before == after
        );
        assert_eq!(before, after, "enrollment must not rewrite instance data");
        let enrollment = client.get_enrollment(&student, &student, &course).unwrap();
        env.as_contract(&id, || {
            assert!(!env
                .storage()
                .persistent()
                .has(&DataKey::EnrollmentByRef(enrollment.enrollment_ref)));
        });
    }
    assert_eq!(client.get_total_active_enrollments(), 2);
    env.as_contract(&id, || {
        assert_eq!(
            env.storage()
                .persistent()
                .get::<_, u32>(&DataKey::TotalActiveEnrollments),
            Some(2)
        );
        assert!(!env
            .storage()
            .instance()
            .has(&DataKey::TotalActiveEnrollments));
    });
}

#[test]
fn legacy_count_survives_migration_and_completion_preserves_cap() {
    let (env, id, admin, student, token_id) = setup();
    let client = HamplardContractClient::new(&env, &id);
    env.as_contract(&id, || {
        env.storage()
            .instance()
            .set(&DataKey::TotalActiveEnrollments, &7u32);
        env.storage()
            .instance()
            .set(&DataKey::EnrollmentRefCounter, &42u32);
    });
    client.set_platform_enrollment_cap(&admin, &Some(8));
    assert_eq!(client.get_total_active_enrollments(), 7);

    let free_instructor = Address::generate(&env);
    let free_course = String::from_str(&env, "BUDGET-FREE");
    client.register_course(
        &free_instructor,
        &free_course,
        &0,
        &token_id,
        &0,
        &None,
        &BytesN::from_array(&env, &[2; 32]),
        &Vec::new(&env),
    );
    env.ledger().with_mut(|ledger| ledger.sequence_number += 1);
    client.approve_course(&admin, &free_course);
    client.enroll(&student, &free_course, &i128::MAX);
    assert_eq!(client.get_total_active_enrollments(), 7);
    env.as_contract(&id, || {
        assert_eq!(
            env.storage()
                .instance()
                .get::<_, u32>(&DataKey::TotalActiveEnrollments),
            Some(7),
            "a free enrollment must not migrate or mutate the paid counter"
        );
    });

    let course = String::from_str(&env, "BUDGET-ONE");
    client.enroll(&student, &course, &i128::MAX);
    assert_eq!(
        client
            .get_enrollment(&student, &student, &course)
            .unwrap()
            .enrollment_ref,
        String::from_str(&env, "43")
    );
    assert_eq!(client.get_total_active_enrollments(), 8);
    let another = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&another, &1_000_000_000);
    assert!(client
        .try_enroll(&another, &String::from_str(&env, "BUDGET-TWO"), &i128::MAX)
        .is_err());
    client.mark_completed(
        &admin,
        &student,
        &course,
        &Some(BytesN::from_array(&env, &[2; 32])),
    );
    assert_eq!(client.get_total_active_enrollments(), 7);
    client.enroll(&another, &String::from_str(&env, "BUDGET-TWO"), &i128::MAX);
    assert_eq!(client.get_total_active_enrollments(), 8);
    env.as_contract(&id, || {
        assert_eq!(
            env.storage()
                .persistent()
                .get::<_, u32>(&DataKey::TotalActiveEnrollments),
            Some(8)
        );
        assert_eq!(
            env.storage()
                .instance()
                .get::<_, u32>(&DataKey::TotalActiveEnrollments),
            None,
            "the first paid counter mutation removes the legacy instance value"
        );
        assert_eq!(
            env.storage()
                .persistent()
                .get::<_, u32>(&DataKey::EnrollmentRefCounter),
            Some(45)
        );
        assert_eq!(
            env.storage()
                .instance()
                .get::<_, u32>(&DataKey::EnrollmentRefCounter),
            Some(42)
        );
    });
}

#[test]
fn enrollment_counter_increments_on_multiple_students() {
    let (env, id, admin, student, token_id) = setup();
    let client = HamplardContractClient::new(&env, &id);
    let token_client = token::StellarAssetClient::new(&env, &token_id);
    
    client.set_platform_enrollment_cap(&admin, &Some(10));
    
    let course = String::from_str(&env, "BUDGET-ONE");
    client.enroll(&student, &course, &i128::MAX);
    assert_eq!(client.get_total_active_enrollments(), 1);
    
    let student2 = Address::generate(&env);
    token_client.mint(&student2, &1_000_000_000);
    client.enroll(&student2, &course, &i128::MAX);
    assert_eq!(client.get_total_active_enrollments(), 2);
    
    let student3 = Address::generate(&env);
    token_client.mint(&student3, &1_000_000_000);
    client.enroll(&student3, &String::from_str(&env, "BUDGET-TWO"), &i128::MAX);
    assert_eq!(client.get_total_active_enrollments(), 3);
}

#[test]
fn enrollment_counter_decrements_on_refund() {
    let (env, id, admin, student, token_id) = setup();
    let client = HamplardContractClient::new(&env, &id);
    let token_client = token::StellarAssetClient::new(&env, &token_id);
    
    client.set_platform_enrollment_cap(&admin, &Some(5));
    
    let course = String::from_str(&env, "BUDGET-ONE");
    client.enroll(&student, &course, &i128::MAX);
    assert_eq!(client.get_total_active_enrollments(), 1);
    
    // Fund refund reserve
    let treasury = Address::generate(&env);
    token_client.mint(&treasury, &100_000_000);
    client.fund_refund_reserve(&treasury, &token_id, &100_000_000);
    
    client.request_refund(&student, &course);
    client.process_refund(&admin, &student, &course, &true, &None);
    
    assert_eq!(client.get_total_active_enrollments(), 0);
}

#[test]
fn enrollment_cap_enforcement_at_boundary() {
    let (env, id, admin, student, token_id) = setup();
    let client = HamplardContractClient::new(&env, &id);
    let token_client = token::StellarAssetClient::new(&env, &token_id);
    
    client.set_platform_enrollment_cap(&admin, &Some(1));
    
    let course1 = String::from_str(&env, "BUDGET-ONE");
    client.enroll(&student, &course1, &i128::MAX);
    assert_eq!(client.get_total_active_enrollments(), 1);
    
    let student2 = Address::generate(&env);
    token_client.mint(&student2, &1_000_000_000);
    
    // Should fail because cap is reached
    let course2 = String::from_str(&env, "BUDGET-TWO");
    assert!(client.try_enroll(&student2, &course2, &i128::MAX).is_err());
    assert_eq!(client.get_total_active_enrollments(), 1);
}

#[test]
fn enrollment_counter_unaffected_by_certificate_issuance() {
    let (env, id, admin, student, _) = setup();
    let client = HamplardContractClient::new(&env, &id);
    
    client.set_platform_enrollment_cap(&admin, &Some(5));
    
    let course = String::from_str(&env, "BUDGET-ONE");
    client.enroll(&student, &course, &i128::MAX);
    assert_eq!(client.get_total_active_enrollments(), 1);
    
    // Mark completed
    let evidence = BytesN::from_array(&env, &[3; 32]);
    client.mark_completed(&admin, &student, &course, &Some(evidence));
    
    // Counter should decrement after completion
    assert_eq!(client.get_total_active_enrollments(), 0);
    
    // Issue certificate - should not affect counter
    let enrollment_ref = client
        .get_enrollment(&student, &student, &course)
        .unwrap()
        .enrollment_ref;
    client.issue_certificate(
        &admin,
        &student,
        &course,
        &String::from_str(&env, "CERT-BUDGET-ONE"),
        &String::from_str(&env, "Course Title"),
        &enrollment_ref,
        &None,
        &None,
    );
    assert_eq!(client.get_total_active_enrollments(), 0);
}

#[test]
fn enrollment_counter_persists_across_ledger_advances() {
    let (env, id, admin, student, token_id) = setup();
    let client = HamplardContractClient::new(&env, &id);
    let token_client = token::StellarAssetClient::new(&env, &token_id);
    
    client.set_platform_enrollment_cap(&admin, &Some(10));
    
    let course1 = String::from_str(&env, "BUDGET-ONE");
    client.enroll(&student, &course1, &i128::MAX);
    assert_eq!(client.get_total_active_enrollments(), 1);
    
    // Advance ledger significantly
    env.ledger().with_mut(|ledger| ledger.sequence_number += 10_000);
    
    // Counter should persist
    assert_eq!(client.get_total_active_enrollments(), 1);
    
    // New enrollment should still increment
    let student2 = Address::generate(&env);
    token_client.mint(&student2, &1_000_000_000);
    client.enroll(&student2, &String::from_str(&env, "BUDGET-TWO"), &i128::MAX);
    assert_eq!(client.get_total_active_enrollments(), 2);
}

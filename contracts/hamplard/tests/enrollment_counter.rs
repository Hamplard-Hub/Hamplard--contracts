use hamplard::{DataKey, HamplardContract, HamplardContractClient};
use soroban_sdk::{
    testutils::{storage::Instance, Address as _, Ledger},
    token, Address, BytesN, Env, String, Vec,
};

fn setup() -> (Env, Address, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let id = env.register(HamplardContract, ());
    let client = HamplardContractClient::new(&env, &id);
    let admin = Address::generate(&env);
    client.init(
        &admin,
        &Address::generate(&env),
        &Address::generate(&env),
        &20,
        &50,
        &1_000,
        &17_280,
    );
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
        client.enroll(&student, &String::from_str(&env, name));
        let cpu = env.cost_estimate().budget().cpu_instruction_cost();
        let memory = env.cost_estimate().budget().memory_bytes_cost();
        let after = env.as_contract(&id, || env.storage().instance().all());
        println!(
            "{name}: cpu={cpu}, memory={memory}, instance_unchanged={}",
            before == after
        );
        assert_eq!(before, after, "enrollment must not rewrite instance data");
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
    let course = String::from_str(&env, "BUDGET-ONE");
    client.enroll(&student, &course);
    assert_eq!(
        client
            .get_enrollment(&student, &student, &course)
            .unwrap()
            .enrollment_ref,
        String::from_str(&env, "42")
    );
    assert_eq!(client.get_total_active_enrollments(), 8);
    let another = Address::generate(&env);
    token::StellarAssetClient::new(&env, &token_id).mint(&another, &1_000_000_000);
    assert!(client
        .try_enroll(&another, &String::from_str(&env, "BUDGET-TWO"))
        .is_err());
    client.mark_completed(
        &admin,
        &student,
        &course,
        &Some(BytesN::from_array(&env, &[2; 32])),
    );
    assert_eq!(client.get_total_active_enrollments(), 7);
    client.enroll(&another, &String::from_str(&env, "BUDGET-TWO"));
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
            Some(7)
        );
        assert_eq!(
            env.storage()
                .persistent()
                .get::<_, u32>(&DataKey::EnrollmentRefCounter),
            Some(44)
        );
        assert_eq!(
            env.storage()
                .instance()
                .get::<_, u32>(&DataKey::EnrollmentRefCounter),
            Some(42)
        );
    });
}

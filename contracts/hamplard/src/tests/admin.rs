use crate::{setup_active_course, setup_contract, setup_contract_no_auth};
use hamplard_lib::{ContractError, DataKey};
use soroban_sdk::{testutils::Address as _, Address, Env, String};

#[test]
fn test_initialize_sets_admin_and_token() {
    let env = Env::default();
    env.mock_all_auths();

    let ctx = setup_contract(&env);
    let admin = ctx.admin.clone();
    let token = ctx.token.clone();

    let stored_admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .expect("admin should be set");
    assert_eq!(stored_admin, admin);

    let stored_token: Address = env
        .storage()
        .instance()
        .get(&DataKey::Token)
        .expect("token should be set");
    assert_eq!(stored_token, token);
}

#[test]
fn test_initialize_twice_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let ctx = setup_contract(&env);

    let result = ctx.client.try_initialize(&ctx.admin, &ctx.token);
    assert_eq!(result, Err(Ok(ContractError::AlreadyInitialized)));
}

#[test]
fn test_initialize_requires_admin_auth() {
    let env = Env::default();
    let ctx = setup_contract_no_auth(&env);

    let result = ctx.client.try_initialize(&ctx.admin, &ctx.token);
    assert!(result.is_err());
}

#[test]
fn test_admin_can_pause_and_unpause() {
    let env = Env::default();
    env.mock_all_auths();

    let ctx = setup_active_course(&env);

    ctx.client.pause(&ctx.admin);
    assert!(ctx.client.is_paused());

    ctx.client.unpause(&ctx.admin);
    assert!(!ctx.client.is_paused());
}

#[test]
fn test_non_admin_cannot_pause() {
    let env = Env::default();
    env.mock_all_auths();

    let ctx = setup_active_course(&env);
    let stranger = Address::generate(&env);

    let result = ctx.client.try_pause(&stranger);
    assert_eq!(result, Err(Ok(ContractError::Unauthorized)));
}

#[test]
fn test_admin_can_transfer_admin_role() {
    let env = Env::default();
    env.mock_all_auths();

    let ctx = setup_active_course(&env);
    let new_admin = Address::generate(&env);

    ctx.client.transfer_admin(&ctx.admin, &new_admin);

    let stored_admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .expect("admin should be set");
    assert_eq!(stored_admin, new_admin);
}

#[test]
fn test_non_admin_cannot_transfer_admin_role() {
    let env = Env::default();
    env.mock_all_auths();

    let ctx = setup_active_course(&env);
    let stranger = Address::generate(&env);
    let new_admin = Address::generate(&env);

    let result = ctx.client.try_transfer_admin(&stranger, &new_admin);
    assert_eq!(result, Err(Ok(ContractError::Unauthorized)));
}

#[test]
fn test_admin_can_set_course_fee() {
    let env = Env::default();
    env.mock_all_auths();

    let ctx = setup_active_course(&env);
    let new_fee: i128 = 500;

    ctx.client.set_course_fee(&ctx.admin, &new_fee);
    assert_eq!(ctx.client.get_course_fee(), new_fee);
}

#[test]
fn test_non_admin_cannot_set_course_fee() {
    let env = Env::default();
    env.mock_all_auths();

    let ctx = setup_active_course(&env);
    let stranger = Address::generate(&env);

    let result = ctx.client.try_set_course_fee(&stranger, &500);
    assert_eq!(result, Err(Ok(ContractError::Unauthorized)));
}

#[test]
fn test_admin_can_withdraw_funds() {
    let env = Env::default();
    env.mock_all_auths();

    let ctx = setup_active_course(&env);
    let recipient = Address::generate(&env);

    let balance_before = ctx.token_client.balance(&recipient);
    ctx.client.withdraw(&ctx.admin, &recipient, &100);
    let balance_after = ctx.token_client.balance(&recipient);

    assert_eq!(balance_after - balance_before, 100);
}

#[test]
fn test_non_admin_cannot_withdraw_funds() {
    let env = Env::default();
    env.mock_all_auths();

    let ctx = setup_active_course(&env);
    let stranger = Address::generate(&env);
    let recipient = Address::generate(&env);

    let result = ctx.client.try_withdraw(&stranger, &recipient, &100);
    assert_eq!(result, Err(Ok(ContractError::Unauthorized)));
}

#[test]
fn test_admin_can_register_course() {
    let env = Env::default();
    env.mock_all_auths();

    let ctx = setup_contract(&env);
    let title = String::from_str(&env, "Intro to Soroban");

    let course_id = ctx.client.register_course(&ctx.admin, &title, &100);
    assert_eq!(course_id, 0);
    assert_eq!(ctx.client.course_count(), 1);
}

#[test]
fn test_non_admin_cannot_register_course() {
    let env = Env::default();
    env.mock_all_auths();

    let ctx = setup_contract(&env);
    let stranger = Address::generate(&env);
    let title = String::from_str(&env, "Intro to Soroban");

    let result = ctx.client.try_register_course(&stranger, &title, &100);
    assert_eq!(result, Err(Ok(ContractError::Unauthorized)));
}

#[test]
fn test_admin_can_deactivate_course() {
    let env = Env::default();
    env.mock_all_auths();

    let ctx = setup_active_course(&env);

    ctx.client.deactivate_course(&ctx.admin, &ctx.course_id);
    assert!(!ctx.client.is_course_active(&ctx.course_id));
}

#[test]
fn test_non_admin_cannot_deactivate_course() {
    let env = Env::default();
    env.mock_all_auths();

    let ctx = setup_active_course(&env);
    let stranger = Address::generate(&env);

    let result = ctx.client.try_deactivate_course(&stranger, &ctx.course_id);
    assert_eq!(result, Err(Ok(ContractError::Unauthorized)));
}

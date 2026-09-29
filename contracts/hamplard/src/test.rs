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

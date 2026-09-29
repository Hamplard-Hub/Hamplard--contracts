// SPDX-License-Identifier: MIT
#![no_std]

use soroban_sdk::{contract, contractimpl, symbol, vec, Address, Env, Symbol, Vec};

pub struct HamplardContract;

// Storage keys
const INSTRUCTOR_EARNINGS: &str = "InstructorEarnings";
const ARBITRATION_FEES: &str = "ArbitrationFees";
const OWED_BALANCES: &str = "OwedBalances";
const ADMIN: &str = "Admin";

#[contract]
pub trait HamplardTrait {
    // Existing functions
    fn deposit_earnings(env: Env, instructor: Address, token: Address, amount: i128);
    fn withdraw_earnings(env: Env, instructor: Address, token: Address) -> i128;
    fn withdraw_tokens(env: Env, token: Address, to: Address, amount: i128);
    fn get_owed_balance(env: Env, token: Address) -> i128;
}

#[contractimpl]
impl HamplardTrait for HamplardContract {
    // Helper function to get owed balance for a token
    fn get_owed_balance(env: Env, token: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&(OWED_BALANCES, token))
            .unwrap_or(0)
    }

    // Updated deposit_earnings to track owed amounts
    fn deposit_earnings(env: Env, instructor: Address, token: Address, amount: i128) {
        // Existing logic to store earnings
        let key = (INSTRUCTOR_EARNINGS, instructor.clone(), token.clone());
        let current: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage().persistent().set(&key, &(current + amount));

        // Update owed balance
        let owed_key = (OWED_BALANCES, token.clone());
        let current_owed: i128 = env.storage().persistent().get(&owed_key).unwrap_or(0);
        env.storage()
            .persistent()
            .set(&owed_key, &(current_owed + amount));
    }

    // Updated withdraw_earnings to reduce owed amounts
    fn withdraw_earnings(env: Env, instructor: Address, token: Address) -> i128 {
        let key = (INSTRUCTOR_EARNINGS, instructor.clone(), token.clone());
        let amount: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage().persistent().remove(&key);

        // Reduce owed balance
        let owed_key = (OWED_BALANCES, token.clone());
        let current_owed: i128 = env.storage().persistent().get(&owed_key).unwrap_or(0);
        env.storage()
            .persistent()
            .set(&owed_key, &(current_owed - amount));

        amount
    }

    // Updated withdraw_tokens with validation
    fn withdraw_tokens(env: Env, token: Address, to: Address, amount: i128) {
        // Validate amount is positive
        if amount <= 0 {
            panic!("Amount must be positive");
        }

        // Get contract's balance for the token
        let contract_address = env.current_contract_address();
        let balance: i128 = env
            .token()
            .balance(&token, &contract_address);

        // Get total owed for this token
        let owed = Self::get_owed_balance(env.clone(), token.clone());

        // Calculate surplus
        let surplus = balance - owed;

        // Validate amount does not exceed surplus
        if amount > surplus {
            panic!("Amount exceeds available surplus");
        }

        // Perform the transfer
        env.token().transfer(&token, &contract_address, &to, &amount);
    }
}

// Tests would be in a separate test file, but including here for completeness
#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Address, Env};

    #[test]
    fn test_withdraw_tokens_validation() {
        let env = Env::default();
        let contract_id = env.register_contract(None, HamplardContract);
        let client = HamplardContractClient::new(&env, &contract_id);

        let admin = Address::random(&env);
        let token = Address::random(&env);
        let instructor = Address::random(&env);

        // Setup: mint tokens to contract
        env.token().mint(&token, &contract_id, &1000);

        // Deposit earnings (increases owed balance)
        client.deposit_earnings(&instructor, &token, &500);

        // Test 1: Cannot withdraw more than surplus (1000 - 500 = 500)
        let result = std::panic::catch_unwind(|| {
            client.withdraw_tokens(&token, &admin, &600);
        });
        assert!(result.is_err());

        // Test 2: Can withdraw surplus
        client.withdraw_tokens(&token, &admin, &500);
        assert_eq!(env.token().balance(&token, &contract_id), 500);

        // Test 3: Cannot withdraw zero or negative
        let result = std::panic::catch_unwind(|| {
            client.withdraw_tokens(&token, &admin, &0);
        });
        assert!(result.is_err());

        let result = std::panic::catch_unwind(|| {
            client.withdraw_tokens(&token, &admin, &-100);
        });
        assert!(result.is_err());

        // Test 4: Cannot withdraw instructor earnings
        let result = std::panic::catch_unwind(|| {
            client.withdraw_tokens(&token, &admin, &500);
        });
        assert!(result.is_err());
    }
}

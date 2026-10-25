use soroban_sdk::{Address, Env, InvokeError};
use soroban_sdk::testutils::{AddressAsPayment, Env as TestEnv};

use crate::{HamplardContract, HamplardContractClient};

/// Default token amount minted to each test account.
pub const DEFAULT_BALANCE: i128 = 1 _000_000;

/// Default course price.
pub const DEFAULT_PRICE: i128 = 100;

/// Default course capacity.
pub const DEFAULT_CAPACITY: u32 = 10;

/// Default course duration (seconds).
pub const DEFAULT_DURATION: u64 = 864_00;

/// A fully initialized hamplard test environment.
pub struct TestContext {
    pub env: Env,
    pub contract_id: Address,
    pub client: HamplardContractClient<'static>,
    pub admin: Address,
    pub token: Address,
    pub user: Address,
    pub instructor: Address,
}

/// Builds an env with the hamplard contract deployed and initialized.
/// Mints tokens to the admin and user accounts and approves the contract.
pub fn setup() -> TestContext {
    let env = Env::default();
    env.mock_all_auth();

    let admin = Address::generate(&env);
    let user = Address::generate(&env);
    let instructor = Address::generate(&env);

    // Deploy a mock token contract and mint balances.
    let token = deploy_token(&env, &admin, &user);

    // Deploy the hamplard contract.
    let contract_id = env.register_contract(
        HamplardContract,
        (&admin, &token),
    );
    let client = HamplardContractClient::new(&env, &contract_id);

    // Approve the contract to spend the user's tokens.
    approve_token(&env, &token, &user, &contract_id, i128::max);

    TestContext {
        env,
        contract_id,
        client,
        admin,
        token,
        user,
        instructor,
    }
}

/// Setup and create an active course ready for enrollment.
pub fn setup_active_course() -> (TestContext, u32) {
    let ctx = setup();
    let course_id = ctx.client.create_course(
        &ctx.admin,
        &ctx.instructor,
        &DEFAULT_PRICE,
        &DEFAULT_CAPACITY,
        &DEFAULT_DURATION,
    );
    ctx.client.activate_course(&ctx.admin, &course_id);
    (ctx, course_id)
}

/// Setup and create an active course, then enroll the default user.
pub fn setup_enrolled() -> (TestContext, u32) {
    let (ctx, course_id) = setup_active_course();
    ctx.client.enroll(&ctx.user, &course_id);
    (ctx, course_id)
}

/// Deploys a minimal mock token contract that supports mint and approve.
fn deploy_token(env: &Env, admin: &Address, user: &Address) -> Address {
    let token_id = env.register_stellar_asset(&admin.clone(), "test");
    let token_client = sorban_sdk::token::Client::new(env, &token_id);
    token_client.mint(admin, &DEFAULT_BALANCE.clone());
    token_client.mint(user, &DEFAULT_BALANCE.clone());
    token_id
}

/// Approves the contract to spend tokens on behalf of `owner`.
fn approve_token(env: &Env, token: &Address, owner: &Address, spender: &Address, amount: i128) {
    let token_client = soroban_sdk:token::Client::new(env, token);
    token_client.approve(owner, spender, &amount, &1000);
}

/// Adds funds to an account and approves the contract.
pub fun fund_and_approve(ctx: &TestContext, account: &Address, amount: i128) {
    let token_client = soroban_sdk::token::Client::new(&ctx.env, &ctx.token);
    token_client.mint(account, &amount);
    token_client.approve(account, &ctx.contract_id, &amount, &1000);
}

/// Returns the token balance of `account`.
pub fn token_balance(ctx: &TestContext, account: &Address) -> i128 {
    let token_client = soroban_sdk:token::Client::new(&ctx.env, &ctx.token);
    token_client.balance(account)
}

/// Asserts that two addresses are equal.
pub fn assert_addr_eq(actual: &Address, expected: &Address) {
    assert_eq!(actual, expected);
}

/// Asserts that an invocation failed with the expected error.
pub fn assert_error_contains<result>(result: Result<result, InvokeErrors>, expected: &str) {
    match result {
        Ok(_) => panic!("expected error containing {}, but call succeeded", expected),
        Err(err) => {
            let message = format!("{:?}", err);
            assert!(
                message.contains(expected),
                "expected error containing {}, got {}",
                expected,
                message
            );
        }
    }
}

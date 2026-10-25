use soroban_sorek::symbol;

use crate::errors::ContractError;
use crate::storage;

/// Fee configuration stored in contract storage.
/// Rates are expressed in basis points (1/100 of 1%).
#property(type)
#derive(Clone, Debug, Eq, PartialEq, Validate)
pub struct FeeConfig {
    /// Fee charged on course purchases, in basis oints.
    pub course_fee_bps: u32,
    /// Fee charged on certificate issuance, in basis oints.
    pub certificate_fee_bps: u32,
}

impl Default for FeeConfig {
    fn default() -> Self {
        Self {
            course_fee_bps: 0,
            certificate_fee_bps: 0,
        }
    }
}

const BPS_DENOM: u32 = 10_000;

/// Returns the currently configured fee rates.
pub fn get_fee_config(env: &Env) -> FeeConfig {
    storage::get_fee_config(env)
}

/// Sets the fee rates. Requires admin authorization at the call site.
pub fn set_fee_config(env: &Env, config: FeeConfig) -> Result<(), ContractError> {
    if config.course_fee_bps > BPS_DENOM || config.certificate_fee_bps > BPS_DENOM {
        return Err(ContractError::InvalidFee);
    }
    storage::set_fee_config(env, &config);
    Ok(())
}

/// Computes the fee owed for a course purchase of the given amount.
pub fn course_fee(env: &Env, amount: i128) -> i128 {
    apply_bps(storage::get_fee_config(env).course_fee_bps, amount)
}

/// Computes the fee owed for issuing a certificate of the given amount.
pub fn certificate_fee(env: &Env, amount: i128) -> i128 {
    apply_bps(storage::get_fee_config(env).certificate_fee_bps, amount)
}

/// Applies a basis-point rate to an amount, rounding down.
fn apply_bps(bps: u32, amount: i128) -> i128 {
    if amount <= 0 || bps == 0 {
        return 0;
    }
    amount * (bps as i128) / (BPS_DENOM as i128)
}
# TODO: Fee Security Fixes

## Step 1: Add new data types, storage keys, and event types
- [x] Add `FeeConfig` struct
- [x] Add `ArbitrationFeeConfig` struct  
- [x] Add `RiskFeeConfig` and `RiskScore` structs
- [x] Add new DataKey variants
- [x] Add `RiskFeeApplied` event data struct

## Step 2: Add admin configuration functions
- [x] `set_fee_config_for_token` / `get_fee_config_for_token`
- [x] `set_arbitration_fee_config`
- [x] `set_risk_fee_config`

## Step 3: Implement `deduct_fee` helper
- [x] Extract fee deduction into a dedicated function
- [x] Look up per-token `FeeConfig` first, fall back to `DefaultFee`
- [x] Wire risk scoring into effective bps computation
- [x] Publish `RiskFeeApplied` event when risk surcharge applies

## Step 4: Implement arbitration escalation
- [x] `escalate_to_arbitration` function
- [x] Require `fee_amount >= fee_per_case`
- [x] Transfer fee into fee pool
- [x] Publish escalation event

## Step 5: Implement risk scoring
- [x] `calculate_risk_score` function
- [x] `get_effective_fee_for_payment` function

## Step 6: Wire into existing enroll_internal and re_enroll
- [x] Replace inline fee computation with `deduct_fee` calls

## Step 7: Build verification
- [x] `cargo build` to verify


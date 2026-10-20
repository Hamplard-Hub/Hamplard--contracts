# Changelog

## Unreleased

### Breaking change
- `Enrollment.evidence_hash` and `mark_completed(..., evidence_hash)` now use `Option<BytesN<32>>` instead of `Option<String>`. Evidence is stored as a fixed 32-byte hash to reduce on-chain storage cost and keep verification deterministic.

### Tests
- Add targeted `mock_auths` coverage for privileged and fund-moving entrypoints, asserting exactly which signers each operation requires instead of relying on `mock_all_auths()`.

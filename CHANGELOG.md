# Changelog

## Unreleased

### Storage layout change

- `Course` gains two fields, `total_refunded: i128` and `refund_count: u32`, updated by both refund paths (`process_refund` and `archive_course`) so admins can read per-course refund rates on-chain via `get_course`. `Course` is a `#[contracttype]` struct, so courses stored before this change lack the new keys and will not deserialize against the new type; redeploy fresh, or migrate existing `Course` / `ArchivedCourse` entries before upgrading a live contract.
### Changed

- `set_risk_fee_config` now panics if any surcharge exceeds 10000 bps, or if the platform default fee plus all three surcharges exceeds 10000 bps. Previously such values were accepted and silently capped at payment time.

### Breaking change

- `Enrollment.evidence_hash` and `mark_completed(..., evidence_hash)` now use `Option<BytesN<32>>` instead of `Option<String>`. Evidence is stored as a fixed 32-byte hash to reduce on-chain storage cost and keep verification deterministic.
- `set_arbitration_fee_config` now takes a `token` argument, and `get_arbitration_fee_config` is keyed by token. Arbitration fees are stored under `DataKey2::ArbitrationFeeConfig(token)`.
- Contract failures now return stable `Error` codes instead of string panic messages. Clients should match on `HostError: Error(Contract, #N)` and the code table in `README.md`.

### Added
- Per-token arbitration fee reserve via `DataKey2::ArbitrationFeesHeld(token)` and `get_arbitration_fees_held()`.
- Escalation credits the reserve; resolution debits it before payout.
- `withdraw_tokens()` now excludes reserved arbitration fees from the withdrawable contract balance.
- Documentation for the actual revenue split, co-instructor limitation, security model, error codes, and current test suite.

### Changed
- Enrollment counter updates now use shared `inc_total_active` / `dec_total_active` helpers.

- `set_arbitration_fee_config` now takes a `token` argument, and `get_arbitration_fee_config` is keyed by token. Arbitration fees are stored under `DataKey2::ArbitrationFeeConfig(token)`.
- Contract failures now return stable `Error` codes instead of string panic messages. Clients should match on `HostError: Error(Contract, #N)` and the code table in `README.md`.

### Added
- Per-token arbitration fee reserve via `DataKey2::ArbitrationFeesHeld(token)` and `get_arbitration_fees_held()`.
- Escalation credits the reserve; resolution debits it before payout.
- `withdraw_tokens()` now excludes reserved arbitration fees from the withdrawable contract balance.
- Documentation for the actual revenue split, co-instructor limitation, security model, error codes, and current test suite.

### Changed
- Enrollment counter updates now use shared `inc_total_active` / `dec_total_active` helpers.


### Fixed
- Define `InstructorTotalEarnings` as lifetime net earnings: refunds reduce it, withdrawals do not, and the getter documents the distinction from the withdrawable per-token balance. (#344)
- Frozen instructors can no longer withdraw earnings until an admin unfreezes them. (#342)
- Correct the enrollment-expiry documentation: expiry does not remove the record or free a capacity slot. (#339)
- Allow `re_enroll()` to replace an expired enrollment, archiving the expired record to `EnrollmentHistory`. (#338)


### Tests
- Add targeted `mock_auths` coverage for privileged and fund-moving entrypoints, asserting exactly which signers each operation requires instead of relying on `mock_all_auths()`.

- `freeze_instructor(admin, instructor, reason)` and `block_student(admin, student, reason)` now require a non-empty reason of at most 256 bytes. Their events include the reason.
- `get_instructor_total_earnings(instructor)` is removed because it mixed balances denominated in different tokens. Use `get_instructor_lifetime_earnings(instructor, token)` instead.

### Added
- `get_enrollment_status(student, course_id)` returns `None`, `Active`, `Expired`, `Completed`, or `Refunded`, including refunds recovered from enrollment history.
- `get_instructor_freeze_reason(instructor)` and `get_student_block_reason(student)` expose stored moderation reasons.

### Fixed
- Free-course enrollments no longer consume the platform-wide paid-enrollment cap or decrement its counter on completion/refund.
- Instructor lifetime net earnings are tracked per token; withdrawals do not change them and refunds debit only the refunded token.

### Migration
- Existing `InstructorTotalEarnings` entries are no longer read. Historic cross-token totals cannot be decomposed into per-token amounts, so per-token lifetime counters start from zero after upgrade; refunds use saturating subtraction so pre-upgrade enrollments remain refundable.
- `enroll(student, course_id, max_price)`, `batch_enroll(student, course_ids, max_price)` and `re_enroll(student, course_id, max_price)` now take a `max_price` argument and revert when the course's current price exceeds it, so a price change between quote and execution can never charge a student more than they approved (#361).

- `enroll(student, course_id, max_price)`, `batch_enroll(student, course_ids, max_price)` and `re_enroll(student, course_id, max_price)` now take a `max_price` argument and revert when the course's current price exceeds it, so a price change between quote and execution can never charge a student more than they approved (#361).

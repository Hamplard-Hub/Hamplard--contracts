# Changelog

## Unreleased

### Breaking change
- `Enrollment.evidence_hash` and `mark_completed(..., evidence_hash)` now use `Option<BytesN<32>>` instead of `Option<String>`. Evidence is stored as a fixed 32-byte hash to reduce on-chain storage cost and keep verification deterministic.

### Fixed
- Define `InstructorTotalEarnings` as lifetime net earnings: refunds reduce it, withdrawals do not, and the getter documents the distinction from the withdrawable per-token balance. (#344)
- Frozen instructors can no longer withdraw earnings until an admin unfreezes them. (#342)
- Correct the enrollment-expiry documentation: expiry does not remove the record or free a capacity slot. (#339)
- Allow `re_enroll()` to replace an expired enrollment, archiving the expired record to `EnrollmentHistory`. (#338)

# Course Expiry Date Implementation

- [x] Step 1: Add `expires_at_ledger` field to `Course` struct in `lib.rs`
- [x] Step 2: Update `register_course` function to accept `expires_at_ledger: Option<u32>` parameter
- [x] Step 3: Update `validate_enrollment` to check course expiry
- [x] Step 4: Update `register_and_approve_course` test helper to pass `&None`
- [x] Step 5: Update all `register_course` calls in tests to pass `&None`
- [x] Step 6: Add new tests for course expiry behavior
- [x] Step 7: Run `cargo test` to verify


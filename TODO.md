# TODO: Implement Student Blocklist Check in `enroll()`

## Steps

### 1. Add `DataKey::StudentBlocked(Address)` storage key
- [x] In `lib.rs`, add `StudentBlocked(Address)` variant to `DataKey` enum

### 2. Add `is_student_blocked_internal()` helper
- [x] Similar to `is_instructor_frozen_internal()` but for students

### 3. Add public `is_student_blocked()` function
- [x] Query function to check if a student is blocked

### 4. Add `block_student()` admin function
- [x] Admin function to block a student (with event emission)

### 5. Add `unblock_student()` admin function
- [x] Admin function to unblock a student (with event emission)

### 6. Add student blocklist check in `validate_enrollment()`
- [x] Check if student is blocked before allowing enrollment

### 7. Add student blocklist check in `re_enroll()`
- [x] Same check for re-enrollment path

### 8. Run build to verify
- [ ] Run `cargo build` to verify compilation


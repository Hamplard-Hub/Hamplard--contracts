# Implementation Plan: Event Payload Consistency Bugfix

## Notes

Fix 19 broken `env.events().publish(...)` calls in `lib.rs` so every event conforms to the
canonical convention:

```
topics: (Symbol, primary_id)
data:   (primary_id, actor: Address, ledger_seq: u32, ...extra_fields)
```

Update test assertions in `test.rs` to match the new payloads. Write a conventions doc so
future events are authored correctly from the start.

## Task Dependency Graph

```json
{
  "waves": [
    {"wave": 1, "tasks": ["1.1", "2.1", "2.2", "2.3", "3.1", "3.2", "3.3", "4.1", "5.1", "5.2", "5.3", "5.4", "5.5", "5.6", "5.7", "6.1", "6.2", "6.3", "6.4", "6.5"]},
    {"wave": 2, "tasks": ["7.1", "7.2", "7.3", "7.4", "7.5", "7.6", "7.7", "7.8", "7.9", "7.10", "7.11", "7.12", "7.13"]},
    {"wave": 3, "tasks": ["8.1"]},
    {"wave": 4, "tasks": ["8.2", "8.3"]}
  ]
}
```

## Tasks

### Group 1 — Write the Event Conventions Document

## Overview

Fix 19 broken `env.events().publish(...)` calls in `lib.rs` so every event conforms to the
canonical convention:

```
topics: (Symbol, primary_id)
data:   (primary_id, actor: Address, ledger_seq: u32, ...extra_fields)
```

Update test assertions in `test.rs` to match the new payloads. Write a conventions doc so
future events are authored correctly from the start.

---

## Group 1 — Write the Event Conventions Document

- [x] 1.1 Create `docs/event-conventions.md`
  - Create the file at `docs/event-conventions.md` (new file; the `docs/` directory may need to be created).
  - Document the canonical schema: `topics = (event_name: Symbol, primary_id: T)`, `data = (primary_id: T, actor: Address, ledger_seq: u32, ...extra_fields)`.
  - Include a slot table: slot 0 = event name symbol, slot 1 = primary resource key; data slot 0 = repeat of primary ID, data slot 1 = actor Address, data slot 2 = ledger_seq u32, data slot 3+ = additional fields in stable order.
  - Include the complete reference table of all corrected events (copy the "Corrected data tuples" table from `design.md`).
  - Include a note on the `freeze_instructor` / `course_paused` exception: all code paths that emit `course_paused` must produce `(String, Address, u32)` in data — never a `Symbol` in the actor slot.
  - Include guidance for future event authors: (1) always capture `env.ledger().sequence()` into a local `let ledger_seq` before the publish call, (2) always repeat `topics.1` as `data.0`.
  - **Files:** `docs/event-conventions.md`
  - **Test:** `cargo doc` (or manual review) — no compilation gate needed; confirm the file is present and legible.

---

## Group 2 — Fix Course Events (course_registered, course_archived, course_paused freeze path)

- [x] 2.1 Fix `course_registered` event in `register_course` (Req 2.1)
  - Locate the `env.events().publish(...)` call near line 1140 of `lib.rs` inside `register_course`.
  - Current broken data: `course_id.clone()` (bare String — no actor, no ledger).
  - Change data argument from `course_id.clone()` to `(course_id.clone(), instructor.clone(), env.ledger().sequence())`.
  - `instructor` is already a parameter of `register_course`; no new variable is needed.
  - Topics are unchanged: `(Symbol::new(&env, "course_registered"), course_id.clone())`.
  - **Files:** `contracts/hamplard/src/lib.rs`
  - **Test:** `cargo test test_register_course_success` — should compile and pass.

- [-] 2.2 Fix `course_archived` event in `archive_course` (Req 2.2)
  - Locate the `env.events().publish(...)` call near line 1736 of `lib.rs` inside `archive_course`.
  - Current broken data: `(course_id.clone(), admin1.clone(), admin2.clone())` — missing ledger_seq.
  - Append `env.ledger().sequence()` as the fourth element: `(course_id.clone(), admin1.clone(), admin2.clone(), env.ledger().sequence())`.
  - Topics are unchanged.
  - **Files:** `contracts/hamplard/src/lib.rs`
  - **Test:** `cargo test test_multi_sig_admin_events_record_both_actors` — see task 3.2 for the required test update first.

- [-] 2.3 Fix `course_paused` event inside `freeze_instructor` — type fix (Req 2.19)
  - Locate the `env.events().publish(...)` call near line 4253 of `lib.rs` inside `freeze_instructor` (the loop over the instructor's Active courses).
  - Current broken data: `(course_id.clone(), Symbol::new(&env, "instructor_frozen"))` — Symbol in actor slot.
  - Replace entirely with: `(course_id.clone(), instructor.clone(), env.ledger().sequence())`.
  - `instructor` is the `freeze_instructor` function parameter, already in scope inside the loop.
  - Remove the `Symbol::new(&env, "instructor_frozen")` expression — context is already conveyed by the `instructor_frozen` event that fires after the loop.
  - Topics are unchanged: `(Symbol::new(&env, "course_paused"), course_id.clone())`.
  - **Files:** `contracts/hamplard/src/lib.rs`
  - **Test:** `cargo test test_events_emitted_for_admin_operations` — confirm no deserialization panic on `course_paused`.

---

## Group 3 — Fix Enrollment Events (batch_enrolled, course_completed, enrollment_start_set)

- [-] 3.1 Fix `batch_enrolled` event in `batch_enroll` (Req 2.3)
  - Locate the `env.events().publish(...)` call near line 2225 of `lib.rs` inside `batch_enroll`.
  - Current broken data: `(student, course_ids, total_paid)` — missing ledger_seq.
  - Append `env.ledger().sequence()` as the fourth element: `(student, course_ids, total_paid, env.ledger().sequence())`.
  - Topics are unchanged: `(Symbol::new(&env, "batch_enrolled"), student.clone())`.
  - Note: `student` is moved into the tuple; ensure `student.clone()` is used in topics and the original is used in data (or both are cloned) to avoid move-after-use.
  - **Files:** `contracts/hamplard/src/lib.rs`
  - **Test:** `cargo test test_batch_enroll_emits_event_for_each_enrollment` — see task 4.1 for required test update.

- [-] 3.2 Fix `course_completed` event in `mark_completed` (Req 2.4)
  - Locate the `env.events().publish(...)` call near line 3361 of `lib.rs` inside `mark_completed`.
  - Current broken data: `(student, admin, completion_ledger)` — `course_id` missing from position 0.
  - Prepend `course_id.clone()`: `(course_id.clone(), student, admin, completion_ledger)`.
  - `completion_ledger` is captured immediately above as `let completion_ledger = env.ledger().sequence();` — it satisfies the `ledger_seq` convention slot; no second call to `env.ledger().sequence()` is needed.
  - Topics are unchanged: `(Symbol::new(&env, "course_completed"), course_id.clone())`.
  - **Files:** `contracts/hamplard/src/lib.rs`
  - **Test:** `cargo test test_events_emitted_for_admin_operations` — see task 4.2 for required test update.

- [x] 3.3 Fix `enrollment_start_set` event in `set_enrollment_start_ledger` (Req 2.18)
  - Locate the `env.events().publish(...)` call near line 831 of `lib.rs` inside `set_enrollment_start_ledger`.
  - Current broken data: `(course_id, enrollment_start_ledger)` — missing actor and ledger_seq.
  - Replace with: `(course_id.clone(), caller.clone(), env.ledger().sequence(), enrollment_start_ledger)`.
  - `caller` is the function parameter already validated as admin or instructor before this point; use `caller.clone()` so `caller` is not moved.
  - `enrollment_start_ledger` remains as the last field (extra context after the required slots).
  - Topics are unchanged: `(Symbol::new(&env, "enrollment_start_set"), course_id.clone())`.
  - **Files:** `contracts/hamplard/src/lib.rs`
  - **Test:** `cargo test` — no existing test asserts on `enrollment_start_set` data; confirm no compile error and check that `test_enroll_rejects_before_enrollment_start_ledger` still passes.

---

## Group 4 — Fix Certificate Events (certificate_issued)

- [x] 4.1 Fix `certificate_issued` event in `issue_certificate` (Req 2.5)
  - Locate the `env.events().publish(...)` call near line 3543 of `lib.rs` inside `issue_certificate`.
  - Current broken data: `(student, course_id, admin, issued_at_ledger)` — `certificate_id` missing from position 0.
  - Prepend `certificate_id.clone()`: `(certificate_id.clone(), student, course_id, admin, issued_at_ledger)`.
  - `issued_at_ledger` is already `env.ledger().sequence()` captured earlier in the function — it satisfies the `ledger_seq` convention slot.
  - Topics are unchanged: `(Symbol::new(&env, "certificate_issued"), certificate_id.clone())`.
  - **Files:** `contracts/hamplard/src/lib.rs`
  - **Test:** `cargo test test_certificate_issued_event_contains_full_issuance_details` — see task 5.2 for required test update.

---

## Group 5 — Fix Platform Admin Events (platform_paused, platform_unpaused, tokens_withdrawn, admin_proposed, default_fee_updated, token_whitelisted, token_removed_from_whitelist)

- [x] 5.1 Fix `platform_paused` event in `pause_platform` (Req 2.6)
  - Locate the `env.events().publish(...)` call in `pause_platform` (search for `"platform_paused"` in `lib.rs`).
  - Current broken data: `admin` (bare Address, not a tuple).
  - Replace with tuple: `(admin.clone(), env.ledger().sequence())`.
  - Topics are unchanged: `(Symbol::new(&env, "platform_paused"), admin.clone())`.
  - **Files:** `contracts/hamplard/src/lib.rs`
  - **Test:** `cargo test test_events_emitted_for_admin_operations` — see task 6.1 for required test update.

- [-] 5.2 Fix `platform_unpaused` event in `unpause_platform` (Req 2.7)
  - Locate the `env.events().publish(...)` call near line 3831 of `lib.rs` inside `unpause_platform`.
  - Current broken data: `admin` (bare Address, not a tuple).
  - Replace with tuple: `(admin.clone(), env.ledger().sequence())`.
  - Topics are unchanged: `(Symbol::new(&env, "platform_unpaused"), admin.clone())`.
  - **Files:** `contracts/hamplard/src/lib.rs`
  - **Test:** `cargo test test_events_emitted_for_admin_operations` — see task 6.2 for required test update.

- [x] 5.3 Fix `tokens_withdrawn` event in `withdraw_tokens` (Req 2.8)
  - Locate the `env.events().publish(...)` call near line 3874 of `lib.rs` inside `withdraw_tokens`.
  - Current broken data: `(admin1, admin2, token, amount, destination)` — missing ledger_seq.
  - Append `env.ledger().sequence()`: `(admin1, admin2, token, amount, destination, env.ledger().sequence())`.
  - Topics are unchanged: `(Symbol::new(&env, "tokens_withdrawn"), admin1.clone())`.
  - **Files:** `contracts/hamplard/src/lib.rs`
  - **Test:** `cargo test test_events_emitted_for_admin_operations` — see task 6.3 for required test update.

- [x] 5.4 Fix `admin_proposed` event in `transfer_admin` (Req 2.9)
  - Locate the `env.events().publish(...)` call near line 3949 of `lib.rs` inside `transfer_admin`.
  - Current broken data: `(new_admin, admin1, admin2)` — missing ledger_seq.
  - Append `env.ledger().sequence()`: `(new_admin, admin1, admin2, env.ledger().sequence())`.
  - Topics are unchanged: `(Symbol::new(&env, "admin_proposed"), new_admin.clone())`.
  - **Files:** `contracts/hamplard/src/lib.rs`
  - **Test:** `cargo test test_multi_sig_admin_events_record_both_actors` — see task 6.4 for required test update.

- [x] 5.5 Fix `default_fee_updated` event in `update_default_fee` (Req 2.10)
  - Locate the `env.events().publish(...)` call near line 4114 of `lib.rs` inside `update_default_fee`.
  - Current broken data: `(admin, new_fee_pct)` — missing ledger_seq.
  - Insert `env.ledger().sequence()` between the admin and the fee: `(admin, env.ledger().sequence(), new_fee_pct)`.
  - Per design: `admin` occupies data slot 0 (it is also `topics.1`); `ledger_seq` occupies slot 1; `new_fee_pct` moves to slot 2.
  - Topics are unchanged: `(Symbol::new(&env, "default_fee_updated"), admin.clone())`.
  - **Files:** `contracts/hamplard/src/lib.rs`
  - **Test:** `cargo test test_events_emitted_for_admin_operations` — see task 6.5 for required test update.

- [ ] 5.6 Fix `token_whitelisted` event in `add_approved_token` (Req 2.11)
  - Locate the `env.events().publish(...)` call near line 4128 of `lib.rs` inside `add_approved_token`.
  - Current broken data: `(admin, token)` — missing ledger_seq.
  - Append `env.ledger().sequence()`: `(admin, token, env.ledger().sequence())`.
  - Topics are unchanged: `(Symbol::new(&env, "token_whitelisted"), admin.clone())`.
  - **Files:** `contracts/hamplard/src/lib.rs`
  - **Test:** `cargo test test_events_emitted_for_admin_operations` — see task 6.6 for required test update.

- [~] 5.7 Fix `token_removed_from_whitelist` event in `remove_approved_token` (Req 2.12)
  - Locate the `env.events().publish(...)` call near line 4186 of `lib.rs` inside `remove_approved_token`.
  - Current broken data: `(admin, token)` — missing ledger_seq.
  - Append `env.ledger().sequence()`: `(admin, token, env.ledger().sequence())`.
  - Topics are unchanged: `(Symbol::new(&env, "token_removed_from_whitelist"), admin.clone())`.
  - **Files:** `contracts/hamplard/src/lib.rs`
  - **Test:** `cargo test test_events_emitted_for_admin_operations` — see task 6.7 for required test update.

---

## Group 6 — Fix Student and Earnings Events (student_blocked, student_unblocked, earnings_withdrawn, refund_requested, refund_processed)

- [~] 6.1 Fix `student_blocked` event in `block_student` (Req 2.13)
  - Locate the `env.events().publish(...)` call in `block_student` (search for `"student_blocked"` in `lib.rs`).
  - Current broken data: `(student, admin)` — missing ledger_seq.
  - Append `env.ledger().sequence()`: `(student, admin, env.ledger().sequence())`.
  - Topics are unchanged: `(Symbol::new(&env, "student_blocked"), student.clone())`.
  - **Files:** `contracts/hamplard/src/lib.rs`
  - **Test:** `cargo test` — no existing test asserts on `student_blocked` data tuple; confirm compile success and that `test_block_student` still passes.

- [~] 6.2 Fix `student_unblocked` event in `unblock_student` (Req 2.14)
  - Locate the `env.events().publish(...)` call in `unblock_student` (search for `"student_unblocked"` in `lib.rs`).
  - Current broken data: `(student, admin)` — missing ledger_seq.
  - Append `env.ledger().sequence()`: `(student, admin, env.ledger().sequence())`.
  - Topics are unchanged: `(Symbol::new(&env, "student_unblocked"), student.clone())`.
  - **Files:** `contracts/hamplard/src/lib.rs`
  - **Test:** `cargo test` — no existing test asserts on `student_unblocked` data tuple; confirm compile success.

- [~] 6.3 Fix `earnings_withdrawn` event in `withdraw_earnings` (Req 2.15)
  - Locate the `env.events().publish(...)` call near line 3208 of `lib.rs` inside `withdraw_earnings`.
  - Current broken data: `(token, withdraw_amount, recipient)` — missing actor (instructor) at position 0 and missing ledger_seq.
  - Replace with: `(instructor.clone(), token, withdraw_amount, recipient, env.ledger().sequence())`.
  - `instructor` is the function parameter and is also `topics.1`; it must be cloned here so it is not moved before the topics tuple is constructed (or ensure the clone is placed correctly given Rust's evaluation order).
  - Topics are unchanged: `(Symbol::new(&env, "earnings_withdrawn"), instructor.clone())`.
  - **Files:** `contracts/hamplard/src/lib.rs`
  - **Test:** `cargo test test_withdraw_earnings_event_includes_destination` — see task 7.1 for required test update.

- [~] 6.4 Fix `refund_requested` event in `request_refund` (Req 2.16)
  - Locate the `env.events().publish(...)` call in `request_refund` (search for `"refund_requested"` in `lib.rs`).
  - Current broken data: `(student, course_id, enrollment.amount_paid)` — missing ledger_seq.
  - Append `env.ledger().sequence()`: `(student, course_id, enrollment.amount_paid, env.ledger().sequence())`.
  - Topics are unchanged: `(Symbol::new(&env, "refund_requested"), course_id.clone())`.
  - **Files:** `contracts/hamplard/src/lib.rs`
  - **Test:** `cargo test` — no existing test asserts on `refund_requested` data tuple directly; confirm compile success and that `test_process_refund_event_includes_admin` still passes (it calls `request_refund` as setup).

- [~] 6.5 Fix `refund_processed` event in `process_refund` (Req 2.17)
  - Locate the `env.events().publish(...)` call in `process_refund` (search for `"refund_processed"` in `lib.rs`).
  - Current broken data: `(student, course_id, approved, admin)` — missing ledger_seq.
  - Append `env.ledger().sequence()`: `(student, course_id, approved, admin, env.ledger().sequence())`.
  - Topics are unchanged: `(Symbol::new(&env, "refund_processed"), course_id.clone())`.
  - **Files:** `contracts/hamplard/src/lib.rs`
  - **Test:** `cargo test test_process_refund_event_includes_admin` — see task 7.2 for required test update.

---

## Group 7 — Update Test Assertions in `test.rs`

Each task here updates exactly one test's event-data destructuring to match the new tuple layout.

- [~] 7.1 Update `test_batch_enroll_emits_event_for_each_enrollment` for `batch_enrolled` (Req 2.3)
  - In `test_batch_enroll_emits_event_for_each_enrollment` in `test.rs`, locate the `batch_enrolled` branch (around line 2990).
  - Current destructuring: `let (event_student, event_course_ids, event_total_paid): (Address, soroban_sdk::Vec<String>, i128) = data.try_into_val(&env).unwrap();`
  - Replace with: `let (event_student, event_course_ids, event_total_paid, event_ledger): (Address, soroban_sdk::Vec<String>, i128, u32) = data.try_into_val(&env).unwrap();`
  - Add assertion: `assert!(event_ledger > 0);`
  - **Files:** `contracts/hamplard/src/test.rs`
  - **Test:** `cargo test test_batch_enroll_emits_event_for_each_enrollment`

- [~] 7.2 Update `test_events_emitted_for_admin_operations` for `course_completed` (Req 2.4)
  - In `test_events_emitted_for_admin_operations` in `test.rs`, locate the `course_completed` block (around line 5059).
  - Current destructuring: `let (event_student, event_admin, _event_ledger): (Address, Address, u32) = last_event_val(..., "course_completed")...`
  - Replace with: `let (event_course_id, event_student, event_admin, _event_ledger): (String, Address, Address, u32) = last_event_val(..., "course_completed")...`
  - Add assertion: `assert_eq!(event_course_id, course_id);`
  - **Files:** `contracts/hamplard/src/test.rs`
  - **Test:** `cargo test test_events_emitted_for_admin_operations`

- [~] 7.3 Update `test_certificate_issued_event_contains_full_issuance_details` and `test_events_emitted_for_admin_operations` for `certificate_issued` (Req 2.5)
  - **In `test_certificate_issued_event_contains_full_issuance_details`** (around line 1017): current destructuring `let (event_student, event_course_id, event_admin, event_ledger): (Address, String, Address, u32)` — replace with `let (event_cert_id, event_student, event_course_id, event_admin, event_ledger): (String, Address, String, Address, u32)`. Add assertion: `assert_eq!(event_cert_id, certificate_id);`
  - **In `test_events_emitted_for_admin_operations`** (around line 5082): current destructuring `let (event_student, event_course_id, event_admin, event_ledger): (Address, String, Address, u32)` — replace with `let (event_cert_id, event_student, event_course_id, event_admin, event_ledger): (String, Address, String, Address, u32)`. Add assertion: `assert_eq!(event_cert_id, cert_id);` (where `cert_id` is the `String::from_str(&env, "CERT-ATTR-001")` from that test).
  - **Files:** `contracts/hamplard/src/test.rs`
  - **Test:** `cargo test test_certificate_issued_event_contains_full_issuance_details` and `cargo test test_events_emitted_for_admin_operations`

- [~] 7.4 Update `test_events_emitted_for_admin_operations` for `platform_paused` (Req 2.6)
  - In `test_events_emitted_for_admin_operations`, locate the `platform_paused` block (around line 5094).
  - Current: `let event_admin: Address = last_event_val(&env, &contract_id, "platform_paused").try_into_val(&env).unwrap();`
  - Replace with: `let (event_admin, event_ledger): (Address, u32) = last_event_val(&env, &contract_id, "platform_paused").try_into_val(&env).unwrap();`
  - Keep existing `assert_eq!(event_admin, admin);`; add `assert!(event_ledger > 0);`
  - **Files:** `contracts/hamplard/src/test.rs`
  - **Test:** `cargo test test_events_emitted_for_admin_operations`

- [~] 7.5 Update `test_events_emitted_for_admin_operations` for `platform_unpaused` (Req 2.7)
  - In `test_events_emitted_for_admin_operations`, locate the `platform_unpaused` block (around line 5101).
  - Current: `let event_admin: Address = last_event_val(&env, &contract_id, "platform_unpaused").try_into_val(&env).unwrap();`
  - Replace with: `let (event_admin, event_ledger): (Address, u32) = last_event_val(&env, &contract_id, "platform_unpaused").try_into_val(&env).unwrap();`
  - Keep existing `assert_eq!(event_admin, admin);`; add `assert!(event_ledger > 0);`
  - **Files:** `contracts/hamplard/src/test.rs`
  - **Test:** `cargo test test_events_emitted_for_admin_operations`

- [~] 7.6 Update `test_events_emitted_for_admin_operations` for `tokens_withdrawn` (Req 2.8)
  - In `test_events_emitted_for_admin_operations`, locate the `tokens_withdrawn` block (around line 5186).
  - Current destructuring: `let (event_admin, event_admin2, event_token, event_amount, event_dest): (Address, Address, Address, i128, Address)`.
  - Replace with: `let (event_admin, event_admin2, event_token, event_amount, event_dest, event_ledger): (Address, Address, Address, i128, Address, u32)`.
  - Keep all existing assertions; add `assert!(event_ledger > 0);`
  - **Files:** `contracts/hamplard/src/test.rs`
  - **Test:** `cargo test test_events_emitted_for_admin_operations`

- [~] 7.7 Update `test_multi_sig_admin_events_record_both_actors` for `admin_proposed` (Req 2.9)
  - In `test_multi_sig_admin_events_record_both_actors`, locate the `admin_proposed` block (around line 5344).
  - Current destructuring: `let (event_new_admin, event_admin1, event_admin2): (Address, Address, Address)`.
  - Replace with: `let (event_new_admin, event_admin1, event_admin2, event_ledger): (Address, Address, Address, u32)`.
  - Keep all existing assertions; add `assert!(event_ledger > 0);`
  - **Files:** `contracts/hamplard/src/test.rs`
  - **Test:** `cargo test test_multi_sig_admin_events_record_both_actors`

- [~] 7.8 Update `test_events_emitted_for_admin_operations` for `default_fee_updated` (Req 2.10)
  - In `test_events_emitted_for_admin_operations`, locate the `default_fee_updated` block (around line 5108).
  - Current destructuring: `let (event_admin, event_fee): (Address, u32)`.
  - Replace with: `let (event_admin, event_ledger, event_fee): (Address, u32, u32)`.
  - Keep `assert_eq!(event_admin, admin);` and `assert_eq!(event_fee, 25u32);`; add `assert!(event_ledger > 0);`
  - **Files:** `contracts/hamplard/src/test.rs`
  - **Test:** `cargo test test_events_emitted_for_admin_operations`

- [~] 7.9 Update `test_events_emitted_for_admin_operations` for `token_whitelisted` (Req 2.11)
  - In `test_events_emitted_for_admin_operations`, locate the `token_whitelisted` block (around line 5118).
  - Current destructuring: `let (event_admin, event_token): (Address, Address)`.
  - Replace with: `let (event_admin, event_token, event_ledger): (Address, Address, u32)`.
  - Keep existing assertions; add `assert!(event_ledger > 0);`
  - **Files:** `contracts/hamplard/src/test.rs`
  - **Test:** `cargo test test_events_emitted_for_admin_operations`

- [~] 7.10 Update `test_events_emitted_for_admin_operations` for `token_removed_from_whitelist` (Req 2.12)
  - In `test_events_emitted_for_admin_operations`, locate the `token_removed_from_whitelist` block (around line 5126).
  - Current destructuring: `let (event_admin, event_token): (Address, Address)`.
  - Replace with: `let (event_admin, event_token, event_ledger): (Address, Address, u32)`.
  - Keep existing assertions; add `assert!(event_ledger > 0);`
  - **Files:** `contracts/hamplard/src/test.rs`
  - **Test:** `cargo test test_events_emitted_for_admin_operations`

- [~] 7.11 Update `test_multi_sig_admin_events_record_both_actors` for `course_archived` (Req 2.2)
  - In `test_multi_sig_admin_events_record_both_actors`, locate the `course_archived` block (around line 5389).
  - Current destructuring: `let (event_course_id, event_admin1, event_admin2): (String, Address, Address)`.
  - Replace with: `let (event_course_id, event_admin1, event_admin2, event_ledger): (String, Address, Address, u32)`.
  - Keep existing `assert_eq!(event_course_id, course_id);` and admin pair assertion; add `assert!(event_ledger > 0);`
  - **Files:** `contracts/hamplard/src/test.rs`
  - **Test:** `cargo test test_multi_sig_admin_events_record_both_actors`

- [~] 7.12 Update `test_process_refund_event_includes_admin` for `refund_processed` (Req 2.17)
  - In `test_process_refund_event_includes_admin`, locate the `refund_processed` destructuring (around line 5429).
  - Current destructuring: `let (event_student, event_course_id, event_approved, event_admin): (Address, String, bool, Address)`.
  - Replace with: `let (event_student, event_course_id, event_approved, event_admin, event_ledger): (Address, String, bool, Address, u32)`.
  - Keep all existing assertions; add `assert!(event_ledger > 0);`
  - **Files:** `contracts/hamplard/src/test.rs`
  - **Test:** `cargo test test_process_refund_event_includes_admin`

- [~] 7.13 Update `test_withdraw_earnings_event_includes_destination` for `earnings_withdrawn` (Req 2.15)
  - In `test_withdraw_earnings_event_includes_destination`, locate the `earnings_withdrawn` branch (around line 3419).
  - Current destructuring: `let (tok, amt, dest): (Address, i128, Address) = data.try_into_val(&env).unwrap();`
  - Replace with: `let (event_instructor, tok, amt, dest, event_ledger): (Address, Address, i128, Address, u32) = data.try_into_val(&env).unwrap();`
  - Keep `assert_eq!(tok, token_id);`, `assert_eq!(amt, share);`, `assert_eq!(dest, cold);`; add `assert_eq!(event_instructor, instructor);` and `assert!(event_ledger > 0);`
  - **Files:** `contracts/hamplard/src/test.rs`
  - **Test:** `cargo test test_withdraw_earnings_event_includes_destination`

---

## Group 8 — Run Full Test Suite and Verify No Regressions

- [~] 8.1 Compile the contract with `cargo build` to surface any type errors
  - From `contracts/hamplard/`, run `cargo build`.
  - There must be zero compilation errors. Common mistakes to check: (a) moved values — if a variable is used in both `topics` and `data`, ensure `.clone()` is used in one; (b) tuple arity mismatches — Soroban's XDR encoding is strict, so a 4-element tuple differs from a 5-element tuple.
  - Fix any compilation errors before proceeding.
  - **Files:** `contracts/hamplard/src/lib.rs`, `contracts/hamplard/src/test.rs`
  - **Test:** `cargo build` exits with code 0.

- [~] 8.2 Run the full test suite and confirm all tests pass
  - From `contracts/hamplard/`, run `cargo test 2>&1`.
  - All tests must pass. Zero failures, zero errors.
  - If the project uses snapshot testing with `UPDATE_SNAPSHOTS=1`, run `UPDATE_SNAPSHOTS=1 cargo test` once first, then `cargo test` again to confirm snapshots are stable. Event payload changes do not alter ledger storage or auth traces, so snapshot diffs are not expected.
  - **Files:** `contracts/hamplard/src/lib.rs`, `contracts/hamplard/src/test.rs`, `contracts/hamplard/test_snapshots/test/*.json`
  - **Test:** `cargo test` output shows no `FAILED` lines.

- [~] 8.3 Verify the 13 unchanged events have not regressed (Req 3.1–3.15)
  - Run the tests that cover the 13 already-correct events: `cargo test test_approve_course_event_details`, `cargo test test_event_certificate_revoked`, `cargo test test_bulk_revoke_emits_course_certificates_revoked_event`, `cargo test test_admin_transferred_event_emitted_once_with_full_schema`.
  - Confirm each passes without changes to its assertions.
  - Unchanged events: `course_approved`, `course_rejected`, `course_paused` (via `pause_course`), `course_unpaused`, `course_transferred`, `student_enrolled`, `student_re_enrolled`, `certificate_revoked`, `course_certificates_revoked`, `revocation_challenged`, `admin_transferred`, `treasury_updated`, `instructor_frozen`.
  - **Files:** `contracts/hamplard/src/test.rs`
  - **Test:** All four named tests pass; `cargo test` output shows no unexpected failures.

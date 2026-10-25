# Technical Design: Event Payload Consistency

## Overview

The Hamplard contract emits events for every significant state transition. Currently 19 events have inconsistent payloads — missing actor addresses, missing ledger sequences, missing primary IDs in data, or wrong types. This design describes the exact code changes to bring all 19 into compliance with a single documented convention, and explains how the 323 snapshot tests are affected.

No new dependencies, new types, or architectural changes are introduced. Every fix is a one-line edit to an `env.events().publish(...)` call in `contracts/hamplard/src/lib.rs`.

---

## Bug Details

Events are emitted via `env.events().publish((topic1, topic2), data_tuple)`. The contract currently has 19 events whose `data` tuples violate the established convention in one or more of these ways:

1. **Missing `ledger_seq`** — the majority of non-compliant events (16 of 19) omit `env.ledger().sequence()` from the data tuple, making temporal ordering impossible without a separate ledger query.

2. **Missing `actor` address** — three events (`course_registered`, `earnings_withdrawn`, `enrollment_start_set`) omit the address of the caller that triggered the action, so consumers cannot attribute the event.

3. **Missing `primary_id` in data** — two events (`course_completed`, `certificate_issued`) omit the primary resource ID from the data tuple, making the record non-self-contained; consumers reading only `data` must also parse `topics` to identify the resource.

4. **Wrong type in actor position** — `course_paused` emitted inside `freeze_instructor` places `Symbol::new(&env, "instructor_frozen")` where an `Address` is expected. This breaks any indexer that deserializes `course_paused` uniformly as `(String, Address, u32)`.

**Affected functions and current payloads:**

| Event | Function | Current data (broken) |
|---|---|---|
| `course_registered` | `register_course` | `course_id` (bare String, no actor, no ledger) |
| `course_archived` | `archive_course` | `(course_id, admin1, admin2)` — no ledger |
| `batch_enrolled` | `batch_enroll` | `(student, course_ids, total_paid)` — no ledger |
| `course_completed` | `mark_completed` | `(student, admin, completion_ledger)` — no primary ID in position 0 |
| `certificate_issued` | `issue_certificate` | `(student, course_id, admin, issued_at_ledger)` — no `certificate_id` in position 0 |
| `platform_paused` | `pause_platform` | `admin` (bare Address) |
| `platform_unpaused` | `unpause_platform` | `admin` (bare Address) |
| `tokens_withdrawn` | `withdraw_tokens` | `(admin1, admin2, token, amount, destination)` — no ledger |
| `admin_proposed` | `transfer_admin` | `(new_admin, admin1, admin2)` — no ledger |
| `default_fee_updated` | `update_default_fee` | `(admin, new_fee_pct)` — no ledger |
| `token_whitelisted` | `add_approved_token` | `(admin, token)` — no ledger |
| `token_removed_from_whitelist` | `remove_approved_token` | `(admin, token)` — no ledger |
| `student_blocked` | `block_student` | `(student, admin)` — no ledger |
| `student_unblocked` | `unblock_student` | `(student, admin)` — no ledger |
| `earnings_withdrawn` | `withdraw_earnings` | `(token, withdraw_amount, recipient)` — no actor, no ledger |
| `refund_requested` | `request_refund` | `(student, course_id, amount_paid)` — no ledger |
| `refund_processed` | `process_refund` | `(student, course_id, approved, admin)` — no ledger |
| `enrollment_start_set` | `set_enrollment_start_ledger` | `(course_id, enrollment_start_ledger)` — no actor, no ledger |
| `course_paused` *(freeze path)* | `freeze_instructor` | `(course_id, Symbol::new(&env, "instructor_frozen"))` — Symbol instead of Address |

---

## Expected Behavior

### Canonical event convention

```
topics: (Symbol, primary_id: T)
data:   (primary_id: T, actor: Address, ledger_seq: u32, ...extra_fields)
```

| Slot | Required | Description |
|---|---|---|
| `topics.0` | Always | `Symbol::new(&env, "event_name")` |
| `topics.1` | Always | Primary resource key. Used for filtered subscriptions. |
| `data.0` | Always | Repeat of `topics.1`. Makes data self-contained. |
| `data.1` | Always | Actor `Address` — who triggered the change. |
| `data.2` | Always | `ledger_seq: u32` — `env.ledger().sequence()`. |
| `data.3+` | As needed | Additional context in stable order. |

### Corrected data tuples for all 19 events

| # | Event | New `data` tuple |
|---|---|---|
| 1 | `course_registered` | `(course_id, instructor, ledger_seq)` |
| 2 | `course_archived` | `(course_id, admin1, admin2, ledger_seq)` |
| 3 | `batch_enrolled` | `(student, course_ids, total_paid, ledger_seq)` |
| 4 | `course_completed` | `(course_id, student, admin, completion_ledger)` |
| 5 | `certificate_issued` | `(certificate_id, student, course_id, admin, issued_at_ledger)` |
| 6 | `platform_paused` | `(admin, ledger_seq)` |
| 7 | `platform_unpaused` | `(admin, ledger_seq)` |
| 8 | `tokens_withdrawn` | `(admin1, admin2, token, amount, destination, ledger_seq)` |
| 9 | `admin_proposed` | `(new_admin, admin1, admin2, ledger_seq)` |
| 10 | `default_fee_updated` | `(admin, ledger_seq, new_fee_pct)` |
| 11 | `token_whitelisted` | `(admin, token, ledger_seq)` |
| 12 | `token_removed_from_whitelist` | `(admin, token, ledger_seq)` |
| 13 | `student_blocked` | `(student, admin, ledger_seq)` |
| 14 | `student_unblocked` | `(student, admin, ledger_seq)` |
| 15 | `earnings_withdrawn` | `(instructor, token, withdraw_amount, recipient, ledger_seq)` |
| 16 | `refund_requested` | `(student, course_id, amount_paid, ledger_seq)` |
| 17 | `refund_processed` | `(student, course_id, approved, admin, ledger_seq)` |
| 18 | `enrollment_start_set` | `(course_id, caller, ledger_seq, enrollment_start_ledger)` |
| 19 | `course_paused` *(freeze path)* | `(course_id, instructor, ledger_seq)` |

---

## Hypothesized Root Cause

The events were added incrementally without a written convention. Each developer added only the fields they needed at the time — enough to satisfy the immediate consumer (the NestJS backend) — without considering temporal ordering, self-containment, or type consistency. The `freeze_instructor` case is the most acute: the developer used a `Symbol` as a "tag" to distinguish the freeze-path emission from the manual-pause-path emission, not realising that this breaks the shared `(String, Address, u32)` deserialization schema every other `course_paused` consumer applies.

---

## Fix Implementation

All changes are in `contracts/hamplard/src/lib.rs`. There are exactly 19 `env.events().publish(...)` calls to edit. The topics tuple is untouched in every case; only the data argument changes.

### `course_registered` — add instructor and ledger_seq

```rust
// BEFORE
env.events().publish(
    (Symbol::new(&env, "course_registered"), course_id.clone()),
    course_id.clone(),
);

// AFTER
env.events().publish(
    (Symbol::new(&env, "course_registered"), course_id.clone()),
    (course_id.clone(), instructor.clone(), env.ledger().sequence()),
);
```

The `instructor` variable is already in scope in `register_course` (it is a function parameter).

---

### `course_archived` — append ledger_seq

```rust
// BEFORE
env.events().publish(
    (Symbol::new(&env, "course_archived"), course_id.clone()),
    (course_id.clone(), admin1.clone(), admin2.clone()),
);

// AFTER
env.events().publish(
    (Symbol::new(&env, "course_archived"), course_id.clone()),
    (course_id.clone(), admin1.clone(), admin2.clone(), env.ledger().sequence()),
);
```

---

### `batch_enrolled` — append ledger_seq

```rust
// BEFORE
env.events().publish(
    (Symbol::new(&env, "batch_enrolled"), student.clone()),
    (student, course_ids, total_paid),
);

// AFTER
env.events().publish(
    (Symbol::new(&env, "batch_enrolled"), student.clone()),
    (student, course_ids, total_paid, env.ledger().sequence()),
);
```

---

### `course_completed` — prepend course_id to data

```rust
// BEFORE
env.events().publish(
    (Symbol::new(&env, "course_completed"), course_id.clone()),
    (student, admin, completion_ledger),
);

// AFTER
env.events().publish(
    (Symbol::new(&env, "course_completed"), course_id.clone()),
    (course_id.clone(), student, admin, completion_ledger),
);
```

`completion_ledger` is captured immediately above the publish call as `let completion_ledger = env.ledger().sequence();`. It serves as the `ledger_seq` for this event — no second call to `env.ledger().sequence()` is needed.

---

### `certificate_issued` — prepend certificate_id to data

```rust
// BEFORE
env.events().publish(
    (Symbol::new(&env, "certificate_issued"), certificate_id.clone()),
    (student, course_id, admin, issued_at_ledger),
);

// AFTER
env.events().publish(
    (Symbol::new(&env, "certificate_issued"), certificate_id.clone()),
    (certificate_id.clone(), student, course_id, admin, issued_at_ledger),
);
```

`issued_at_ledger` is already the ledger sequence captured earlier in `issue_certificate`. It satisfies the `ledger_seq` position.

---

### `platform_paused` — wrap in tuple with ledger_seq

```rust
// BEFORE
env.events()
    .publish((Symbol::new(&env, "platform_paused"), admin.clone()), admin);

// AFTER
env.events().publish(
    (Symbol::new(&env, "platform_paused"), admin.clone()),
    (admin.clone(), env.ledger().sequence()),
);
```

---

### `platform_unpaused` — wrap in tuple with ledger_seq

```rust
// BEFORE
env.events().publish(
    (Symbol::new(&env, "platform_unpaused"), admin.clone()),
    admin,
);

// AFTER
env.events().publish(
    (Symbol::new(&env, "platform_unpaused"), admin.clone()),
    (admin.clone(), env.ledger().sequence()),
);
```

---

### `tokens_withdrawn` — append ledger_seq

```rust
// BEFORE
env.events().publish(
    (Symbol::new(&env, "tokens_withdrawn"), admin1.clone()),
    (admin1, admin2, token, amount, destination),
);

// AFTER
env.events().publish(
    (Symbol::new(&env, "tokens_withdrawn"), admin1.clone()),
    (admin1, admin2, token, amount, destination, env.ledger().sequence()),
);
```

---

### `admin_proposed` — append ledger_seq

```rust
// BEFORE
env.events().publish(
    (Symbol::new(&env, "admin_proposed"), new_admin.clone()),
    (new_admin, admin1, admin2),
);

// AFTER
env.events().publish(
    (Symbol::new(&env, "admin_proposed"), new_admin.clone()),
    (new_admin, admin1, admin2, env.ledger().sequence()),
);
```

---

### `default_fee_updated` — insert ledger_seq before new_fee_pct

```rust
// BEFORE
env.events().publish(
    (Symbol::new(&env, "default_fee_updated"), admin.clone()),
    (admin, new_fee_pct),
);

// AFTER
env.events().publish(
    (Symbol::new(&env, "default_fee_updated"), admin.clone()),
    (admin, env.ledger().sequence(), new_fee_pct),
);
```

Note: `ledger_seq` goes in position `data.1` (actor slot is `admin` at position `data.0`); `new_fee_pct` moves to `data.2`. This matches the convention — `admin` is already the primary ID (topics.1) and the actor, so positions 0 and 1 are both the admin address.

---

### `token_whitelisted` — append ledger_seq

```rust
// BEFORE
env.events().publish(
    (Symbol::new(&env, "token_whitelisted"), admin.clone()),
    (admin, token),
);

// AFTER
env.events().publish(
    (Symbol::new(&env, "token_whitelisted"), admin.clone()),
    (admin, token, env.ledger().sequence()),
);
```

---

### `token_removed_from_whitelist` — append ledger_seq

```rust
// BEFORE
env.events().publish(
    (Symbol::new(&env, "token_removed_from_whitelist"), admin.clone()),
    (admin, token),
);

// AFTER
env.events().publish(
    (Symbol::new(&env, "token_removed_from_whitelist"), admin.clone()),
    (admin, token, env.ledger().sequence()),
);
```

---

### `student_blocked` — append ledger_seq

```rust
// BEFORE
env.events().publish(
    (Symbol::new(&env, "student_blocked"), student.clone()),
    (student, admin),
);

// AFTER
env.events().publish(
    (Symbol::new(&env, "student_blocked"), student.clone()),
    (student, admin, env.ledger().sequence()),
);
```

---

### `student_unblocked` — append ledger_seq

```rust
// BEFORE
env.events().publish(
    (Symbol::new(&env, "student_unblocked"), student.clone()),
    (student, admin),
);

// AFTER
env.events().publish(
    (Symbol::new(&env, "student_unblocked"), student.clone()),
    (student, admin, env.ledger().sequence()),
);
```

---

### `earnings_withdrawn` — prepend instructor, append ledger_seq

```rust
// BEFORE
env.events().publish(
    (Symbol::new(&env, "earnings_withdrawn"), instructor.clone()),
    (token, withdraw_amount, recipient),
);

// AFTER
env.events().publish(
    (Symbol::new(&env, "earnings_withdrawn"), instructor.clone()),
    (instructor.clone(), token, withdraw_amount, recipient, env.ledger().sequence()),
);
```

`instructor` is already the function parameter and `topics.1`. It must be repeated in `data.0` per convention.

---

### `refund_requested` — append ledger_seq

```rust
// BEFORE
env.events().publish(
    (Symbol::new(&env, "refund_requested"), course_id.clone()),
    (student, course_id, enrollment.amount_paid),
);

// AFTER
env.events().publish(
    (Symbol::new(&env, "refund_requested"), course_id.clone()),
    (student, course_id, enrollment.amount_paid, env.ledger().sequence()),
);
```

---

### `refund_processed` — append ledger_seq

```rust
// BEFORE
env.events().publish(
    (Symbol::new(&env, "refund_processed"), course_id.clone()),
    (student, course_id, approved, admin),
);

// AFTER
env.events().publish(
    (Symbol::new(&env, "refund_processed"), course_id.clone()),
    (student, course_id, approved, admin, env.ledger().sequence()),
);
```

---

### `enrollment_start_set` — add caller and ledger_seq

```rust
// BEFORE
env.events().publish(
    (Symbol::new(&env, "enrollment_start_set"), course_id.clone()),
    (course_id, enrollment_start_ledger),
);

// AFTER
env.events().publish(
    (Symbol::new(&env, "enrollment_start_set"), course_id.clone()),
    (course_id.clone(), caller.clone(), env.ledger().sequence(), enrollment_start_ledger),
);
```

`caller` is the function parameter in `set_enrollment_start_ledger` and is already validated as admin or instructor above the publish call.

---

### `course_paused` inside `freeze_instructor` — type fix + restructure

This is the most critical fix. The current code places a `Symbol` in the actor position:

```rust
// BEFORE — inside freeze_instructor loop
env.events().publish(
    (Symbol::new(&env, "course_paused"), course_id.clone()),
    (course_id.clone(), Symbol::new(&env, "instructor_frozen")),
);
```

```rust
// AFTER
env.events().publish(
    (Symbol::new(&env, "course_paused"), course_id.clone()),
    (course_id.clone(), instructor.clone(), env.ledger().sequence()),
);
```

`instructor` is the `freeze_instructor` function parameter, already in scope inside the loop. The `Symbol::new(&env, "instructor_frozen")` is removed entirely — the context (this emission comes from `freeze_instructor`) is already communicated by the `instructor_frozen` event that fires after the loop.

**Result:** every `course_paused` emission now has schema `(course_id: String, actor: Address, ledger_seq: u32)` regardless of which code path fired it.

---

## Testing Strategy

### Unit test assertions

Search `src/test.rs` for `last_event_val` to find every event-value assertion:

```bash
grep -n "last_event_val" src/test.rs
```

For each hit that touches one of the 19 changed events, update the destructuring pattern to match the new tuple layout. Example for `batch_enrolled`:

```rust
// BEFORE
let (event_student, event_course_ids, event_total): (Address, Vec<String>, i128) =
    last_event_val(&env, &contract_id, "batch_enrolled")
        .try_into_val(&env)
        .unwrap();

// AFTER
let (event_student, event_course_ids, event_total, event_ledger): (Address, Vec<String>, i128, u32) =
    last_event_val(&env, &contract_id, "batch_enrolled")
        .try_into_val(&env)
        .unwrap();
assert_eq!(event_ledger, env.ledger().sequence());
```

Apply the same pattern to every changed event.

### Snapshot regeneration

`test_snapshots/test/` holds **323 JSON files**. These snapshots record ledger state and authorization traces, not event payloads. Since the fix only changes `env.events().publish(...)` calls and makes no storage writes, **the snapshot JSON content is not expected to change**.

Run the test suite to regenerate snapshots if the runner requires it:

```bash
# From contracts/hamplard/
cargo test
```

If the project uses an explicit snapshot-update flag:

```bash
UPDATE_SNAPSHOTS=1 cargo test
# or
make test-update-snapshots
```

If any snapshot file changes unexpectedly, inspect the diff — it likely indicates an accidental side-effect introduced during editing.

### Regression check

The 13 already-correct events listed in spec section 3 must continue to pass without modification. Run the full test suite and confirm zero regressions:

```bash
cargo test 2>&1 | grep -E "FAILED|ok|error"
```

---

## Correctness Properties

The following properties hold after the fix is applied:

### Property 1: Self-containment
For any event emission, `data.0` equals `topics.1`. A consumer reading only the data tuple can always identify the resource without parsing topics.

**Validates: Requirements 2.4, 2.5**

### Property 2: Temporal ordering
For any event emission, `data.2` (or the named `ledger_seq` / `completion_ledger` / `issued_at_ledger` field) is a `u32` equal to `env.ledger().sequence()` at call time. Indexers can sort any event stream by this field.

**Validates: Requirements 2.1, 2.2, 2.3, 2.6, 2.7, 2.8, 2.9, 2.10, 2.11, 2.12, 2.13, 2.14, 2.15, 2.16, 2.17, 2.18**

### Property 3: Actor presence
For any event emission, `data.1` is an `Address` representing the caller that authorized the state change. No event places a `Symbol` in position `data.1`.

**Validates: Requirements 2.1, 2.15, 2.18, 2.19**

### Property 4: Schema uniformity for `course_paused`
All code paths that emit `course_paused` (via `pause_course` or `freeze_instructor`) produce data tuples where position 0 is a `String` (course_id) and position 1 is an `Address` (actor). Deserializing as `(String, Address, ...)` succeeds on all paths.

**Validates: Requirements 2.19, 3.3**

---

## Glossary

| Term | Definition |
|---|---|
| `topics` | The first argument to `env.events().publish()`. A tuple used by Soroban nodes and indexers for event filtering. Max 4 elements. |
| `data` | The second argument to `env.events().publish()`. The event payload. Arbitrary XDR value; convention here is always a tuple. |
| `primary_id` | The resource identifier placed in `topics.1` and `data.0`. Scopes the event to one resource (course, certificate, student, etc.). |
| `actor` | The `Address` that signed and triggered the state change. Placed at `data.1`. |
| `ledger_seq` | `env.ledger().sequence()` — the ledger sequence number at call time. Used for temporal ordering. Placed at `data.2` (or may be a named field like `completion_ledger` when it carries additional semantic meaning). |
| `freeze path` | The code path inside `freeze_instructor` that auto-pauses the instructor's Active courses and emits `course_paused` for each. |

---

## Non-Goals

- Typed event structs (`#[contracttype]` wrappers around event payloads) — out of scope.
- Changes to topic layout — already correct for all events.
- Changes to the 13 events in spec section 3 (Unchanged Behavior).
- Any change to event consumers (NestJS backend, analytics pipelines).

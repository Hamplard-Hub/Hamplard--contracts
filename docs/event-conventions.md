# Hamplard Contract — Event Conventions

This document is the authoritative reference for how events are structured in
the Hamplard Soroban contract. All current events comply with the convention
described below. New events must also comply.

---

## Canonical Schema

Every event is published as:

```rust
env.events().publish(topics, data);
```

### Topics tuple

```
topics = (event_name: Symbol, primary_id: T)
```

| Slot | Type | Description |
|---|---|---|
| `topics.0` / slot 0 | `Symbol` | The event name, e.g. `Symbol::new(&env, "course_registered")`. |
| `topics.1` / slot 1 | `T` | The **primary resource key** scoping this event to one resource (course ID, certificate ID, student address, admin address, etc.). Used by indexers for filtered subscriptions. |

### Data tuple

```
data = (primary_id: T, actor: Address, ledger_seq: u32, ...extra_fields)
```

| Slot | Type | Description |
|---|---|---|
| `data.0` / slot 0 | `T` | **Repeat of `topics.1`** — the primary resource key. Makes the data tuple self-contained: a consumer reading only `data` can identify the resource without also parsing `topics`. |
| `data.1` / slot 1 | `Address` | **Actor** — the address that authorized and triggered the state change. Must always be an `Address`; never a `Symbol` or other type. |
| `data.2` / slot 2 | `u32` | **`ledger_seq`** — `env.ledger().sequence()` at call time. Enables indexers to sort or filter any event stream by ledger without a separate ledger query. |
| `data.3+` / slot 3+ | varies | Additional context fields in **stable, documented order**. See the reference table below. |

---

## Reference Table: All 19 Corrected Events

The table shows the exact `data` tuple for every event that was brought into
compliance. The slot layout follows the canonical schema above.

| # | Event | `data` tuple |
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

> **Note on `completion_ledger` / `issued_at_ledger`:** These named fields carry
> the same value as `ledger_seq` but retain a more specific name because they
> carry additional semantic meaning (the ledger at which completion or issuance
> was recorded). They satisfy the `data.2` ledger-sequence requirement.

---

## Special Case: `freeze_instructor` / `course_paused`

`course_paused` is emitted from two code paths:

1. **`pause_course`** — emitted with `(course_id, CourseStatus::Paused, caller, ledger_seq)`.
2. **`freeze_instructor`** — emitted inside a loop once per Active course
   belonging to the frozen instructor.

**All code paths that emit `course_paused` must produce data where `data.1` is
an `Address`.** The freeze path previously placed
`Symbol::new(&env, "instructor_frozen")` in the actor slot; this is incorrect
and has been fixed.

The corrected freeze-path emission is:

```rust
env.events().publish(
    (Symbol::new(&env, "course_paused"), course_id.clone()),
    (course_id.clone(), instructor.clone(), env.ledger().sequence()),
);
```

`instructor` is the `freeze_instructor` function parameter and is already in
scope inside the loop. The symbol tag is not needed — the context that a freeze
triggered this emission is already communicated by the `instructor_frozen`
event emitted after the loop.

**Invariant:** Deserializing any `course_paused` event as `(String, Address, u32)`
(or as the full pause-path tuple) must never fail. Never place a `Symbol` in
`data.1`.

---

## Guidance for Future Event Authors

Follow these two rules every time you add a new event:

### Rule 1 — Capture ledger sequence into a local before the publish call

```rust
// CORRECT — capture first, then use
let ledger_seq = env.ledger().sequence();
env.events().publish(
    (Symbol::new(&env, "my_event"), resource_id.clone()),
    (resource_id.clone(), actor.clone(), ledger_seq, extra_field),
);

// INCORRECT — do not inline env.ledger().sequence() multiple times
//             or forget it entirely
env.events().publish(
    (Symbol::new(&env, "my_event"), resource_id.clone()),
    (resource_id.clone(), actor.clone()),  // ← missing ledger_seq
);
```

Capturing into a local makes the value stable, readable, and easy to test. It
also prevents accidental double-calls if you need to reference the sequence
more than once in the same function.

### Rule 2 — Always repeat `topics.1` as `data.0`

```rust
// CORRECT — primary ID appears in both topics and data
env.events().publish(
    (Symbol::new(&env, "my_event"), resource_id.clone()),  // topics.1 = resource_id
    (resource_id.clone(), actor.clone(), ledger_seq),      // data.0   = resource_id
);

// INCORRECT — data starts with actor, primary ID missing from data
env.events().publish(
    (Symbol::new(&env, "my_event"), resource_id.clone()),
    (actor.clone(), ledger_seq),  // ← resource_id absent from data
);
```

A consumer that reads only the `data` tuple must be able to identify the
resource without also deserializing `topics`. Repeating the primary ID in
`data.0` guarantees this.

### Checklist before merging a new event

- [ ] `topics.0` is a `Symbol` with a descriptive, snake_case event name.
- [ ] `topics.1` is the primary resource identifier (course ID, certificate ID, address, …).
- [ ] `data.0` repeats `topics.1` exactly.
- [ ] `data.1` is an `Address` — never a `Symbol`, `String`, or integer.
- [ ] `data.2` is `ledger_seq: u32` (or a semantically equivalent named field like `completion_ledger`).
- [ ] Additional fields (`data.3+`) are in a stable, documented order.
- [ ] The new event is added to this reference table.

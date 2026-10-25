# Bugfix Requirements Document

## Introduction

The Hamplard contract emits events for every significant state transition so
that off-chain indexers (the NestJS backend, analytics pipelines, etc.) can
reconstruct platform state without polling. Currently the event payloads are
inconsistent: some events carry the actor address and ledger sequence in their
`data` tuple, others do not; some events repeat the primary ID in both topics
and data, others omit it from data entirely; one event (`course_paused` emitted
during `freeze_instructor`) puts a `Symbol` where an `Address` is expected.

This forces every indexer to maintain bespoke per-event parsing logic, making
it brittle to add new consumers or extend the platform. The fix establishes a
single documented convention and brings every event into compliance.

**Convention (topics / data):**
- **Topics**: `(event_name: Symbol, primary_id: T)` — the primary identifier
  that lets indexers subscribe to a filtered stream for one resource (e.g.
  one course, one certificate, one student).
- **Data**: always includes, at minimum, `(actor: Address, ledger_seq: u32)`.
  Additional contextual fields follow in a consistent order.  The primary ID
  **must** also appear in `data` so a consumer that only reads the data tuple
  gets a self-contained record.

---

## Bug Analysis

### Current Behavior (Defect)

1.1 WHEN `course_registered` is emitted THEN the system puts only `course_id` in `data` with no actor address and no ledger sequence, so indexers cannot determine who registered the course or when.

1.2 WHEN `course_archived` is emitted THEN the system includes both admin addresses and `course_id` in `data` but omits the ledger sequence, making it impossible to timestamp the archival event without a separate ledger query.

1.3 WHEN `batch_enrolled` is emitted THEN the system puts `(student, course_ids, total_paid)` in `data` with no ledger sequence, breaking temporal ordering for batch operations.

1.4 WHEN `course_completed` is emitted THEN the system puts `(student, admin, completion_ledger)` in `data` but does not include `course_id`, so the data tuple is not self-contained.

1.5 WHEN `certificate_issued` is emitted THEN the system puts `(student, course_id, admin, issued_at_ledger)` in `data` but does not include `certificate_id`, so the primary ID from topics is absent from data.

1.6 WHEN `platform_paused` is emitted THEN the system puts only `admin` in `data` with no ledger sequence.

1.7 WHEN `platform_unpaused` is emitted THEN the system puts only `admin` in `data` with no ledger sequence.

1.8 WHEN `tokens_withdrawn` is emitted THEN the system puts `(admin1, admin2, token, amount, destination)` in `data` with no ledger sequence.

1.9 WHEN `admin_proposed` is emitted THEN the system puts `(new_admin, admin1, admin2)` in `data` with no ledger sequence.

1.10 WHEN `default_fee_updated` is emitted THEN the system puts `(admin, new_fee_pct)` in `data` with no ledger sequence.

1.11 WHEN `token_whitelisted` is emitted THEN the system puts `(admin, token)` in `data` with no ledger sequence.

1.12 WHEN `token_removed_from_whitelist` is emitted THEN the system puts `(admin, token)` in `data` with no ledger sequence.

1.13 WHEN `student_blocked` is emitted THEN the system puts `(student, admin)` in `data` with no ledger sequence.

1.14 WHEN `student_unblocked` is emitted THEN the system puts `(student, admin)` in `data` with no ledger sequence.

1.15 WHEN `earnings_withdrawn` is emitted THEN the system puts `(token, withdraw_amount, recipient)` in `data` with no actor address and no ledger sequence.

1.16 WHEN `refund_requested` is emitted THEN the system puts `(student, course_id, amount_paid)` in `data` with no ledger sequence.

1.17 WHEN `refund_processed` is emitted THEN the system puts `(student, course_id, approved, admin)` in `data` with no ledger sequence.

1.18 WHEN `enrollment_start_set` is emitted THEN the system puts `(course_id, enrollment_start_ledger)` in `data` with no actor address and no ledger sequence.

1.19 WHEN `course_paused` is emitted during `freeze_instructor` THEN the system puts `(course_id, Symbol::new("instructor_frozen"))` in `data`, placing a `Symbol` in the actor field instead of an `Address`, causing deserialization failures for indexers expecting `(String, Address, u32)`.

### Expected Behavior (Correct)

2.1 WHEN `course_registered` is emitted THEN the system SHALL include `(course_id, instructor, ledger_seq)` in `data` so indexers know who registered the course and at which ledger.

2.2 WHEN `course_archived` is emitted THEN the system SHALL include `(course_id, admin1, admin2, ledger_seq)` in `data` so the archival timestamp is available in the data tuple.

2.3 WHEN `batch_enrolled` is emitted THEN the system SHALL include `(student, course_ids, total_paid, ledger_seq)` in `data` so batch operations are temporally anchored.

2.4 WHEN `course_completed` is emitted THEN the system SHALL include `(course_id, student, admin, completion_ledger)` in `data` so the data tuple is self-contained.

2.5 WHEN `certificate_issued` is emitted THEN the system SHALL include `(certificate_id, student, course_id, admin, issued_at_ledger)` in `data` so the primary ID is present in data.

2.6 WHEN `platform_paused` is emitted THEN the system SHALL include `(admin, ledger_seq)` in `data`.

2.7 WHEN `platform_unpaused` is emitted THEN the system SHALL include `(admin, ledger_seq)` in `data`.

2.8 WHEN `tokens_withdrawn` is emitted THEN the system SHALL include `(admin1, admin2, token, amount, destination, ledger_seq)` in `data`.

2.9 WHEN `admin_proposed` is emitted THEN the system SHALL include `(new_admin, admin1, admin2, ledger_seq)` in `data`.

2.10 WHEN `default_fee_updated` is emitted THEN the system SHALL include `(admin, new_fee_pct, ledger_seq)` in `data`.

2.11 WHEN `token_whitelisted` is emitted THEN the system SHALL include `(admin, token, ledger_seq)` in `data`.

2.12 WHEN `token_removed_from_whitelist` is emitted THEN the system SHALL include `(admin, token, ledger_seq)` in `data`.

2.13 WHEN `student_blocked` is emitted THEN the system SHALL include `(student, admin, ledger_seq)` in `data`.

2.14 WHEN `student_unblocked` is emitted THEN the system SHALL include `(student, admin, ledger_seq)` in `data`.

2.15 WHEN `earnings_withdrawn` is emitted THEN the system SHALL include `(instructor, token, withdraw_amount, recipient, ledger_seq)` in `data` so the actor (instructor) and timestamp are present.

2.16 WHEN `refund_requested` is emitted THEN the system SHALL include `(student, course_id, amount_paid, ledger_seq)` in `data`.

2.17 WHEN `refund_processed` is emitted THEN the system SHALL include `(student, course_id, approved, admin, ledger_seq)` in `data`.

2.18 WHEN `enrollment_start_set` is emitted THEN the system SHALL include `(course_id, caller, enrollment_start_ledger, ledger_seq)` in `data` so the actor and timestamp are present.

2.19 WHEN `course_paused` is emitted during `freeze_instructor` THEN the system SHALL include `(course_id, instructor, ledger_seq)` in `data`, using the instructor's `Address` as the actor rather than a `Symbol`, so the data schema is identical to all other `course_paused` emissions.

### Unchanged Behavior (Regression Prevention)

3.1 WHEN `course_approved` is emitted THEN the system SHALL CONTINUE TO include `(course_id, instructor, caller, ledger_seq)` in `data` as it already does.

3.2 WHEN `course_rejected` is emitted THEN the system SHALL CONTINUE TO include `(course_id, instructor, caller, reason, ledger_seq)` in `data` as it already does.

3.3 WHEN `course_paused` is emitted via `pause_course` THEN the system SHALL CONTINUE TO include `(course_id, CourseStatus::Paused, caller, ledger_seq)` in `data` as it already does.

3.4 WHEN `course_unpaused` is emitted THEN the system SHALL CONTINUE TO include `(course_id, CourseStatus::Active, caller, ledger_seq)` in `data` as it already does.

3.5 WHEN `course_transferred` is emitted THEN the system SHALL CONTINUE TO include `(course_id, previous_instructor, new_instructor, admin, ledger_seq)` in `data` as it already does.

3.6 WHEN `student_enrolled` is emitted THEN the system SHALL CONTINUE TO include `(student, course_id, amount, platform_fee, instructor_fee, ledger_seq)` in `data` as it already does.

3.7 WHEN `student_re_enrolled` is emitted THEN the system SHALL CONTINUE TO include `(student, course_id, price, platform_amount, instructor_amount, ledger_seq)` in `data` as it already does.

3.8 WHEN `certificate_revoked` is emitted THEN the system SHALL CONTINUE TO include `(admin, certificate_id, student, course_id, reason, ledger_seq, deadline)` in `data` as it already does.

3.9 WHEN `course_certificates_revoked` is emitted THEN the system SHALL CONTINUE TO include `(admin, course_id, revoked_count, ledger_seq)` in `data` as it already does.

3.10 WHEN `revocation_challenged` is emitted THEN the system SHALL CONTINUE TO include `(student, certificate_id, ledger_seq)` in `data` as it already does.

3.11 WHEN `admin_transferred` is emitted THEN the system SHALL CONTINUE TO include `(previous_admin, new_admin, ledger_seq)` in `data` as it already does.

3.12 WHEN `treasury_updated` is emitted THEN the system SHALL CONTINUE TO include `(old_treasury, new_treasury, admin1, admin2, ledger_seq, effective_ledger)` in `data` as it already does.

3.13 WHEN `instructor_frozen` is emitted THEN the system SHALL CONTINUE TO include `(instructor, admin)` in `data` as it already does.

3.14 WHEN a course_registered, course_approved, course_paused, or any other course-scoped event is emitted THEN the system SHALL CONTINUE TO place `course_id` as the second topic element so filtered subscriptions by course_id continue to work.

3.15 WHEN a certificate_issued or certificate_revoked event is emitted THEN the system SHALL CONTINUE TO place `certificate_id` as the second topic element so filtered subscriptions by certificate_id continue to work.

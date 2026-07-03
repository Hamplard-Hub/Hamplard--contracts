# Hamplard Event Emission Testing Guide

## Overview

This document describes the comprehensive event emission system implemented in the Hamplard smart contract, including testing strategies, event schemas, and off-chain indexer integration patterns.

## Executive Summary

The Hamplard contract emits Soroban events for all state-changing operations, providing complete on-chain audit trails. Each event includes:
- **Actor**: The address that triggered the operation
- **Operation Data**: Relevant IDs, amounts, and status changes
- **Ledger Sequence**: The exact block height when the event occurred
- **Event Type**: A symbolic identifier for easy filtering

## State-Changing Functions & Their Events

### 1. Platform & Contract Management

#### `init(admin, secondary_admin, treasury, default_fee_pct)`
- **Event Type**: `platform_initialized`
- **Event Data**:
  - Actor: `admin` address
  - Secondary Admin: `secondary_admin` address
  - Treasury: `treasury` address
  - Default Fee: `default_fee_pct` (0-100)
  - Ledger Sequence: Block height
- **Audit Use**: Verify initial platform configuration and admins
- **Test**: Verify event contains all 5 parameters

#### `pause_platform(admin)`
- **Event Type**: `platform_paused`
- **Event Data**:
  - Actor: `admin` address
  - Ledger Sequence: Block height
- **Audit Use**: Track platform-wide operational pauses
- **Test**: Verify admin is recorded and ledger sequence is current

#### `unpause_platform(admin)`
- **Event Type**: `platform_unpaused`
- **Event Data**:
  - Actor: `admin` address
  - Ledger Sequence: Block height
- **Audit Use**: Track platform resumption
- **Test**: Verify admin is recorded and ledger sequence is current

---

### 2. Course Management

#### `register_course(instructor, course_id, price, token, platform_fee_pct)`
- **Event Type**: `course_registered`
- **Event Data**:
  - Actor: `instructor` address (who triggered the operation)
  - Course ID: Unique identifier
  - Instructor: `instructor` address (recipient of revenue)
  - Price: Amount in stroops (7 decimals)
  - Token: Token contract address
  - Fee Percent: Platform fee percentage
  - Ledger Sequence: Block height
- **Audit Use**: Track course creation and initial pricing
- **Test**: Verify all 7 fields, especially price accuracy

#### `approve_course(admin, course_id)`
- **Event Type**: `course_approved`
- **Event Data**:
  - Actor: `admin` address
  - Course ID: Approved course identifier
  - Ledger Sequence: Block height
- **Audit Use**: Track course approval decisions and timing
- **Test**: Verify admin and course_id, check ledger sequence progression

#### `pause_course(caller, course_id)`
- **Event Type**: `course_paused`
- **Event Data**:
  - Actor: `caller` address (admin or instructor)
  - Course ID: Paused course identifier
  - Ledger Sequence: Block height
- **Audit Use**: Track course availability changes
- **Test**: Verify caller can be instructor or admin

#### `unpause_course(caller, course_id)`
- **Event Type**: `course_unpaused`
- **Event Data**:
  - Actor: `caller` address
  - Course ID: Course identifier
  - Ledger Sequence: Block height
- **Audit Use**: Track course reactivation
- **Test**: Verify correct actor and course restoration

#### `archive_course(admin1, admin2, course_id, students_to_refund)`
- **Event Type**: `course_archived`
- **Event Data**:
  - Actor 1: First admin address
  - Actor 2: Second admin address (multi-sig)
  - Course ID: Archived course identifier
  - Refund Count: Number of students refunded
  - Total Refunded: Total amount refunded in stroops
  - Ledger Sequence: Block height
- **Audit Use**: Track course removal and refund operations
- **Test**: Verify both admins, refund amounts match calculations
- **Critical**: Total refunded must equal sum of individual refunds

---

### 3. Enrollment & Payment

#### `enroll(student, course_id)`
- **Event Type**: `student_enrolled`
- **Event Data**:
  - Actor: `student` address
  - Course ID: Course identifier
  - Amount Paid: Full payment amount in stroops
  - Platform Fee: Amount sent to treasury
  - Instructor Fee: Amount sent to instructor
  - Ledger Sequence: Block height
- **Payment Verification**:
  - `Platform Fee` = `(Amount Paid * Platform Fee Percent) / 100`
  - `Instructor Fee` = `Amount Paid - Platform Fee`
  - `Platform Fee + Instructor Fee` = `Amount Paid`
- **Audit Use**: Complete enrollment and payment audit trail
- **Test**: Verify all fees calculated correctly
- **Critical**: Validate payment split totals

#### `mark_completed(admin, student, course_id, evidence_hash)`
- **Event Type**: `course_completed`
- **Event Data**:
  - Actor: `admin` address
  - Student: `student` address
  - Course ID: Course identifier
  - Has Evidence: Boolean (true if evidence_hash provided)
  - Ledger Sequence: Block height
- **Audit Use**: Track course completions with or without evidence
- **Test**: Verify has_evidence field matches presence of hash

---

### 4. Certificates

#### `issue_certificate(admin, certificate_id, student, course_id, course_title)`
- **Event Type**: `certificate_issued`
- **Event Data**:
  - Actor: `admin` address (issuer)
  - Certificate ID: Unique certificate identifier
  - Student: `student` address (recipient)
  - Course ID: Course identifier
  - Course Title: Short course name
  - Ledger Sequence: Block height
- **Audit Use**: Complete certificate issuance audit trail
- **Test**: Verify all identifiers and student address

#### `revoke_certificate(admin, certificate_id, reason)`
- **Event Type**: `certificate_revoked`
- **Event Data**:
  - Actor: `admin` address (revoker)
  - Certificate ID: Revoked certificate identifier
  - Student: `student` address (original recipient)
  - Course ID: Course identifier
  - Reason: Revocation reason code
  - Ledger Sequence: Block height
- **Audit Use**: Complete revocation audit trail
- **Test**: Verify reason is recorded accurately

---

### 5. Admin Management

#### `transfer_admin(admin1, admin2, new_admin, new_secondary_admin)`
- **Event Type**: `admin_transfer_proposed`
- **Event Data**:
  - Proposer 1: First current admin address
  - Proposer 2: Second current admin address
  - New Admin: Proposed new primary admin
  - New Secondary Admin: Proposed new secondary admin
  - Ledger Sequence: Block height
- **Audit Use**: Track admin change proposals
- **Test**: Verify both proposers and both new admins

#### `accept_admin(new_admin, new_secondary_admin)`
- **Event Type**: `admin_transfer_accepted`
- **Event Data**:
  - New Admin: Accepted primary admin address
  - New Secondary Admin: Accepted secondary admin address
  - Ledger Sequence: Block height
- **Audit Use**: Track admin change acceptance/completion
- **Test**: Verify new admins match proposal

#### `update_treasury(admin1, admin2, new_treasury)`
- **Event Type**: `treasury_updated`
- **Event Data**:
  - Admin 1: First approving admin
  - Admin 2: Second approving admin
  - New Treasury: Treasury address
  - Effective Ledger: When the update takes effect (current + 100)
  - Ledger Sequence: Event emission block height
- **Audit Use**: Track treasury changes with scheduled activation
- **Test**: Verify effective ledger is current + 100

#### `update_default_fee(admin, new_fee_pct)`
- **Event Type**: `default_fee_updated`
- **Event Data**:
  - Actor: `admin` address
  - New Fee Percent: New fee percentage (0-100)
  - Ledger Sequence: Block height
- **Audit Use**: Track fee policy changes
- **Test**: Verify new fee percentage

#### `add_approved_token(admin, token)`
- **Event Type**: `token_whitelisted`
- **Event Data**:
  - Actor: `admin` address
  - Token: Token contract address
  - Ledger Sequence: Block height
- **Audit Use**: Track payment token additions
- **Test**: Verify token address recorded

#### `remove_approved_token(admin, token)`
- **Event Type**: `token_removed_from_whitelist`
- **Event Data**:
  - Actor: `admin` address
  - Token: Token contract address
  - Ledger Sequence: Block height
- **Audit Use**: Track payment token removals
- **Test**: Verify token address recorded

#### `withdraw_tokens(admin, token, amount, destination)`
- **Event Type**: `tokens_withdrawn`
- **Event Data**:
  - Actor: `admin` address (approver)
  - Token: Token contract address
  - Amount: Amount withdrawn in stroops
  - Destination: Recipient address
  - Ledger Sequence: Block height
- **Audit Use**: Complete token withdrawal audit trail
- **Test**: Verify amount and destination addresses

---

## Event Structure Format

### Soroban Event Topics (Signature)
Each event uses a tuple for topics:
```rust
(Symbol, Data)
```

- **Symbol**: Event type (e.g., "course_registered")
- **Data**: Primary identifier (e.g., course_id, certificate_id, or "system")

### Soroban Event Data (Payload)
Events are published with a tuple payload containing:
- Ledger sequence as the last field (for temporal ordering)
- All relevant identifiers (addresses, IDs, amounts)
- Status information and operation details

### Event Query Pattern
Off-chain indexers can query events by:
```
Topic 0: Event type (Symbol)
Topic 1: Primary identifier (course_id, certificate_id, etc.)
```

---

## Testing Strategy

### Test Categories

#### 1. Event Existence Tests
- Verify that calling a state-changing function emits exactly one event
- Verify event has correct type/symbol
- Verify event is indexed with correct primary identifier

#### 2. Event Data Accuracy Tests
- Verify all fields are present in event data
- Verify field values match function parameters
- Verify calculated fields are correct
- Verify actor addresses are correct

#### 3. Ledger Sequence Tests
- Verify ledger sequence in event matches env.ledger().sequence()
- Verify ledger sequence increments across multiple operations
- Verify events from same transaction have same ledger sequence

#### 4. Integration Tests
- Verify event chains (e.g., register → approve → enroll → complete → issue_certificate)
- Verify multiple events in same transaction are all emitted
- Verify event sequence reflects operation ordering

#### 5. Edge Case Tests
- Verify no events for read-only operations
- Verify events for failed operations (e.g., unauthorized access attempts)
- Verify events for operations that trigger refunds/multi-step transactions

### Test Verification Functions

Each test should verify:

```rust
// 1. Check event was emitted
let events = env.events().all();
assert!(!events.is_empty(), "No events emitted");

// 2. Verify event type
let (topics, data) = &events[0];
assert_eq!(topics[0], Symbol::new(&env, "event_type"));

// 3. Verify primary identifier
assert_eq!(topics[1], course_id);

// 4. Verify data fields (depends on event type)
// For enrollment event:
// Event data tuple: (student, course_id, amount_paid, platform_fee, instructor_fee, ledger_seq)
assert_eq!(data.student, expected_student);
assert_eq!(data.platform_fee + data.instructor_fee, data.amount_paid);
```

---

## Off-Chain Indexer Integration

### Event Query Examples

#### Query All Enrollment Events for a Student
```sql
SELECT * FROM events 
WHERE event_type = 'student_enrolled' 
AND event_data.student = $student_address
ORDER BY ledger_sequence DESC
```

#### Query All Certificate Issuances
```sql
SELECT * FROM events 
WHERE event_type = 'certificate_issued'
ORDER BY ledger_sequence DESC
```

#### Query Certificate Revocations with Reasons
```sql
SELECT * FROM events 
WHERE event_type = 'certificate_revoked'
AND event_data.reason LIKE 'ACADEMIC_DISHONESTY'
```

#### Query Platform Fee Changes
```sql
SELECT * FROM events 
WHERE event_type = 'default_fee_updated'
ORDER BY ledger_sequence DESC
```

#### Reconstruct Course State
```sql
-- Track all course state changes
SELECT * FROM events 
WHERE (
  event_type IN ('course_registered', 'course_approved', 'course_paused', 
                 'course_unpaused', 'course_archived')
  AND event_data.course_id = $course_id
)
ORDER BY ledger_sequence ASC
```

#### Audit Admin Changes
```sql
SELECT * FROM events 
WHERE event_type IN ('admin_transfer_proposed', 'admin_transfer_accepted')
ORDER BY ledger_sequence DESC
```

#### Calculate Revenue by Course
```sql
SELECT course_id, 
       SUM(platform_fee) as total_platform_fees,
       SUM(instructor_fee) as total_instructor_fees,
       COUNT(*) as total_enrollments
FROM events 
WHERE event_type = 'student_enrolled'
GROUP BY course_id
```

---

## Event Schema Definitions

### Certificate Issued Event
```json
{
  "type": "certificate_issued",
  "topics": {
    "0": "certificate_issued",
    "1": "certificate_id"
  },
  "data": {
    "admin": "Address",
    "certificate_id": "String",
    "student": "Address",
    "course_id": "String",
    "course_title": "String",
    "ledger_sequence": "u32"
  }
}
```

### Student Enrolled Event
```json
{
  "type": "student_enrolled",
  "topics": {
    "0": "student_enrolled",
    "1": "course_id"
  },
  "data": {
    "student": "Address",
    "course_id": "String",
    "amount_paid": "i128",
    "platform_fee": "i128",
    "instructor_fee": "i128",
    "ledger_sequence": "u32"
  },
  "invariants": [
    "platform_fee + instructor_fee == amount_paid",
    "platform_fee == (amount_paid * platform_fee_percent) / 100",
    "instructor_fee == amount_paid - platform_fee"
  ]
}
```

### Course Archived Event
```json
{
  "type": "course_archived",
  "topics": {
    "0": "course_archived",
    "1": "course_id"
  },
  "data": {
    "admin1": "Address",
    "admin2": "Address",
    "course_id": "String",
    "refund_count": "u32",
    "total_refunded": "i128",
    "ledger_sequence": "u32"
  },
  "invariants": [
    "total_refunded >= 0",
    "refund_count <= total_active_enrollments"
  ]
}
```

---

## Critical Invariants to Verify

1. **Payment Conservation**: For every enrollment, `platform_fee + instructor_fee == amount_paid`
2. **Ledger Monotonicity**: Events from later operations have higher ledger sequences
3. **Actor Consistency**: The actor in an event matches the caller of the function
4. **Identifier Uniqueness**: Course IDs, certificate IDs, and tokens are unique across events
5. **State Transitions**: Course status changes follow valid state machine transitions
6. **Refund Accuracy**: Refund counts and amounts in archive events match actual refunds

---

## Acceptance Criteria Checklist

- [ ] All 20+ state-changing functions emit events
- [ ] Events contain actor address for authorization audit
- [ ] Events contain operation data (IDs, amounts, status)
- [ ] Events contain ledger sequence for temporal ordering
- [ ] Unit tests verify event emission for each function
- [ ] Unit tests verify event data accuracy
- [ ] Unit tests verify ledger sequence inclusion
- [ ] Integration tests verify event chains
- [ ] Edge case tests for error scenarios
- [ ] Off-chain indexer can consume and query events
- [ ] Event schema documented for indexer integration
- [ ] Payment split invariants verified in tests
- [ ] Admin multi-sig recorded in events
- [ ] Refund operations fully auditable


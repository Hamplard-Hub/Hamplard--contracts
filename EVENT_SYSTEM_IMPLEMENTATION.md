# Hamplard Smart Contract - Comprehensive Event Emission System

## Overview

A complete event audit trail system has been implemented for the Hamplard smart contract, providing full transparency and traceability for all state-changing operations. The system uses snake_case naming conventions and includes structured event data with ledger sequences, actor information, and operation-specific details.

## Architecture

### Events Module
- **Location**: `src/lib.rs` lines 13-303
- **Type**: Centralized private module with public helper functions
- **Pattern**: Each event type has a dedicated function ensuring consistency
- **Ledger Tracking**: Every event includes the current ledger sequence for chronological ordering

## Event Types Implemented

### 1. Platform Initialization
**Event Name**: `platform_initialized`
- **Emitted by**: `init()`
- **Data**:
  - Actor (deployer/admin)
  - Secondary admin
  - Treasury address
  - Default fee percentage
  - Ledger sequence

**Use Case**: Full audit trail of platform setup with initial configuration

---

### 2. Course Management Events

#### 2.1 Course Registration
**Event Name**: `course_registered`
- **Emitted by**: `register_course()`
- **Data**:
  - Actor (instructor)
  - Course ID
  - Instructor address
  - Price (in stroops)
  - Token contract address
  - Platform fee percentage
  - Ledger sequence

**Use Case**: Track all new course submissions with pricing details

#### 2.2 Course Approval
**Event Name**: `course_approved`
- **Emitted by**: `approve_course()`
- **Data**:
  - Admin address
  - Course ID
  - Ledger sequence

**Use Case**: Audit admin approval decisions

#### 2.3 Course Pause
**Event Name**: `course_paused`
- **Emitted by**: `pause_course()`
- **Data**:
  - Caller (admin or instructor)
  - Course ID
  - Ledger sequence

**Use Case**: Track course availability changes

#### 2.4 Course Unpause
**Event Name**: `course_unpaused`
- **Emitted by**: `unpause_course()`
- **Data**:
  - Caller (admin or instructor)
  - Course ID
  - Ledger sequence

**Use Case**: Track course restoration to active status

#### 2.5 Course Archive
**Event Name**: `course_archived`
- **Emitted by**: `archive_course()`
- **Data**:
  - Admin 1 address
  - Admin 2 address
  - Course ID
  - **Refund count** (number of students refunded)
  - **Total refunded amount** (in stroops)
  - Ledger sequence

**Use Case**: Critical for compliance tracking - records all refunds issued during course archival

---

### 3. Enrollment & Payment Events

#### 3.1 Student Enrollment
**Event Name**: `student_enrolled`
- **Emitted by**: `enroll()`
- **Data**:
  - Student address
  - Course ID
  - **Amount paid** (total payment in stroops)
  - **Platform fee** (amount sent to treasury)
  - **Instructor fee** (amount sent to instructor)
  - Ledger sequence

**Use Case**: Complete payment split visibility for financial audits

---

### 4. Certificate Events

#### 4.1 Course Completion
**Event Name**: `course_completed`
- **Emitted by**: `mark_completed()`
- **Data**:
  - Admin address
  - Student address
  - Course ID
  - **Has evidence** (boolean - whether evidence hash was provided)
  - Ledger sequence

**Use Case**: Track completion verification method (admin-only vs. evidence-based)

#### 4.2 Certificate Issuance
**Event Name**: `certificate_issued`
- **Emitted by**: `issue_certificate()`
- **Data**:
  - Admin address
  - Certificate ID
  - Student address
  - Course ID
  - Course title
  - Ledger sequence

**Use Case**: Verifiable proof of certificate creation

#### 4.3 Certificate Revocation
**Event Name**: `certificate_revoked`
- **Emitted by**: `revoke_certificate()`
- **Data**:
  - Admin address
  - Certificate ID
  - Student address
  - Course ID
  - **Revocation reason** (e.g., "ACADEMIC_DISHONESTY", "ISSUED_IN_ERROR")
  - Ledger sequence

**Use Case**: Complete audit trail of certificate invalidations with reason codes

---

### 5. Platform Control Events

#### 5.1 Platform Pause
**Event Name**: `platform_paused`
- **Emitted by**: `pause_platform()`
- **Data**:
  - Admin address
  - Ledger sequence

**Use Case**: Track emergency platform pauses

#### 5.2 Platform Unpause
**Event Name**: `platform_unpaused`
- **Emitted by**: `unpause_platform()`
- **Data**:
  - Admin address
  - Ledger sequence

**Use Case**: Track platform restoration

---

### 6. Treasury & Payment Events

#### 6.1 Token Withdrawal
**Event Name**: `tokens_withdrawn`
- **Emitted by**: `withdraw_tokens()`
- **Data**:
  - Admin address
  - Token contract address
  - **Amount withdrawn** (in stroops)
  - Destination address
  - Ledger sequence

**Use Case**: Complete treasury operations audit trail

#### 6.2 Treasury Update
**Event Name**: `treasury_updated`
- **Emitted by**: `update_treasury()`
- **Data**:
  - Admin 1 address
  - Admin 2 address
  - New treasury address
  - **Effective ledger** (when the change takes effect)
  - Ledger sequence

**Use Case**: Track treasury address changes with activation timing

#### 6.3 Default Fee Update
**Event Name**: `default_fee_updated`
- **Emitted by**: `update_default_fee()`
- **Data**:
  - Admin address
  - New fee percentage
  - Ledger sequence

**Use Case**: Audit platform fee configuration changes

---

### 7. Token Management Events

#### 7.1 Token Whitelisted
**Event Name**: `token_whitelisted`
- **Emitted by**: `add_approved_token()`
- **Data**:
  - Admin address
  - Token contract address
  - Ledger sequence

**Use Case**: Track approved payment tokens

#### 7.2 Token Removed from Whitelist
**Event Name**: `token_removed_from_whitelist`
- **Emitted by**: `remove_approved_token()`
- **Data**:
  - Admin address
  - Token contract address
  - Ledger sequence

**Use Case**: Track token acceptance changes

---

### 8. Admin Management Events

#### 8.1 Admin Transfer Proposal
**Event Name**: `admin_transfer_proposed`
- **Emitted by**: `transfer_admin()`
- **Data**:
  - Proposer 1 (current admin)
  - Proposer 2 (current secondary admin)
  - New admin address (nominee)
  - New secondary admin address (nominee)
  - Ledger sequence

**Use Case**: First step of two-step admin transfer

#### 8.2 Admin Transfer Acceptance
**Event Name**: `admin_transfer_accepted`
- **Emitted by**: `accept_admin()`
- **Data**:
  - New admin address
  - New secondary admin address
  - Ledger sequence

**Use Case**: Confirms completion of admin transfer

---

## Implementation Details

### Event Data Structure Pattern

Each event consists of:
1. **Event Key**: Tuple of (Symbol with event name, identifier)
   - Example: `(Symbol::new(env, "course_registered"), course_id.clone())`
2. **Event Data**: Tuple or struct containing all relevant details
   - Includes actor/caller for authorization tracking
   - Includes ledger sequence for chronological ordering
   - Includes operation-specific data (amounts, statuses, reasons, etc.)

### Ledger Sequence Tracking

Every event includes `env.ledger().sequence()` to provide:
- Chronological ordering of events
- Correlation with Stellar ledger state
- Immutable timestamp (block-like ordering)

### Payment Split Transparency

Enrollment events explicitly break down payment distribution:
```rust
Events::student_enrolled(
    &env,
    &student,
    &course_id,
    course.price,           // Total payment
    platform_amount,        // Fee to treasury
    instructor_amount       // Fee to instructor
);
```

This enables:
- Financial reconciliation
- Revenue verification
- Fee dispute resolution

### Refund Audit Trail

Archive operation tracks:
- Number of students refunded
- Total amount refunded
- Both admin signers
- Course ID

This enables compliance reporting for:
- Tax purposes
- Consumer protection audits
- Financial statements

---

## State-Changing Functions Updated

✅ All 18 state-changing functions now emit comprehensive events:

1. ✅ `init` - Platform initialization
2. ✅ `register_course` - Course registration
3. ✅ `approve_course` - Course approval
4. ✅ `pause_course` - Course pause
5. ✅ `unpause_course` - Course unpause
6. ✅ `archive_course` - Course archive with refund tracking
7. ✅ `enroll` - Enrollment with payment split
8. ✅ `mark_completed` - Completion marking with evidence flag
9. ✅ `issue_certificate` - Certificate creation
10. ✅ `revoke_certificate` - Certificate revocation with reason
11. ✅ `pause_platform` - Platform pause
12. ✅ `unpause_platform` - Platform unpause
13. ✅ `withdraw_tokens` - Token withdrawal
14. ✅ `transfer_admin` - Admin transfer proposal
15. ✅ `accept_admin` - Admin transfer acceptance
16. ✅ `update_treasury` - Treasury update with effective ledger
17. ✅ `update_default_fee` - Fee configuration update
18. ✅ `add_approved_token` - Token whitelist addition
19. ✅ `remove_approved_token` - Token whitelist removal

---

## Event Naming Convention

All events follow **snake_case** naming with descriptive verbs:
- `platform_initialized`
- `course_registered`
- `student_enrolled`
- `certificate_issued`
- `tokens_withdrawn`
- etc.

This maintains consistency across the contract and aligns with Soroban SDK conventions.

---

## Usage Examples

### Listening for Enrollment Events
```rust
// Event structure for student_enrolled:
// (student, course_id, amount_paid, platform_fee, instructor_fee, ledger_sequence)

// Query: Get all enrollments for a student
events.filter(|e| e.event_name == "student_enrolled" && e.data.0 == student_address)
```

### Tracking Refunds
```rust
// Event structure for course_archived:
// (admin1, admin2, course_id, refund_count, total_refunded, ledger_sequence)

// Query: Sum all refunds issued
events
    .filter(|e| e.event_name == "course_archived")
    .map(|e| e.data.4)  // total_refunded
    .sum()
```

### Certificate Revocation Audit
```rust
// Event structure for certificate_revoked:
// (admin, certificate_id, student, course_id, reason, ledger_sequence)

// Query: All revocations by reason
events
    .filter(|e| e.event_name == "certificate_revoked")
    .group_by(|e| e.data.4)  // reason
```

---

## Compliance & Audit Benefits

1. **Full Traceability**: Every operation traced to actor and ledger
2. **Payment Verification**: Complete split breakdown for every enrollment
3. **Refund Tracking**: Quantifiable refund records with admin signatures
4. **Certificate Audit**: Reason-coded revocations for dispute resolution
5. **Multi-sig Verification**: Both admins visible in critical operations
6. **Temporal Ordering**: Ledger sequences enable chronological reconstruction
7. **Token Management**: Complete history of approved payment methods
8. **Treasury Operations**: All fund movements recorded

---

## Compilation Status

✅ **Build Status**: Successfully compiles without errors or warnings
- Cargo version: Works with standard Rust/Soroban build pipeline
- No unsafe code
- Follows Rust best practices

---

## Future Enhancements

1. **Event Indexing**: Add off-chain indexing for fast event queries
2. **Event Filtering**: Query events by actor, date range, or operation type
3. **Dashboard Integration**: Real-time event streaming to admin dashboard
4. **Alerting System**: Trigger alerts on suspicious event patterns
5. **Event Archive**: Export events for compliance reporting

---

## Summary

The Hamplard contract now provides comprehensive event emission for complete audit trail support. Every state-changing operation generates rich events with:
- Full actor identification
- Ledger sequence timestamps
- Operation-specific details (amounts, fees, refunds, reasons)
- Payment split transparency
- Multi-sig verification for critical operations

This enables compliance with financial regulations, dispute resolution, and transparent platform operations.

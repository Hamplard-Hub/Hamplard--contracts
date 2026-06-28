# Hamplard Event System - Quick Reference Guide

## Event Emission Reference

### Platform Initialization
```
Event Name: platform_initialized
Function: init()
Data: (admin, secondary_admin, treasury, default_fee_pct, ledger_sequence)
```

### Course Management

```
Event Name: course_registered
Function: register_course()
Data: (actor/instructor, course_id, instructor, price, token, fee_percent, ledger_sequence)

Event Name: course_approved
Function: approve_course()
Data: (admin, course_id, ledger_sequence)

Event Name: course_paused
Function: pause_course()
Data: (caller, course_id, ledger_sequence)

Event Name: course_unpaused
Function: unpause_course()
Data: (caller, course_id, ledger_sequence)

Event Name: course_archived
Function: archive_course()
Data: (admin1, admin2, course_id, refund_count, total_refunded, ledger_sequence)
```

### Enrollment & Payment

```
Event Name: student_enrolled
Function: enroll()
Data: (student, course_id, amount_paid, platform_fee, instructor_fee, ledger_sequence)
```

### Certificates

```
Event Name: course_completed
Function: mark_completed()
Data: (admin, student, course_id, has_evidence, ledger_sequence)

Event Name: certificate_issued
Function: issue_certificate()
Data: (admin, certificate_id, student, course_id, course_title, ledger_sequence)

Event Name: certificate_revoked
Function: revoke_certificate()
Data: (admin, certificate_id, student, course_id, reason, ledger_sequence)
```

### Platform Control

```
Event Name: platform_paused
Function: pause_platform()
Data: (admin, ledger_sequence)

Event Name: platform_unpaused
Function: unpause_platform()
Data: (admin, ledger_sequence)
```

### Treasury & Tokens

```
Event Name: tokens_withdrawn
Function: withdraw_tokens()
Data: (admin, token, amount, destination, ledger_sequence)

Event Name: treasury_updated
Function: update_treasury()
Data: (admin1, admin2, new_treasury, effective_ledger, ledger_sequence)

Event Name: default_fee_updated
Function: update_default_fee()
Data: (admin, new_fee_pct, ledger_sequence)

Event Name: token_whitelisted
Function: add_approved_token()
Data: (admin, token, ledger_sequence)

Event Name: token_removed_from_whitelist
Function: remove_approved_token()
Data: (admin, token, ledger_sequence)
```

### Admin Management

```
Event Name: admin_transfer_proposed
Function: transfer_admin()
Data: (admin1, admin2, new_admin, new_secondary_admin, ledger_sequence)

Event Name: admin_transfer_accepted
Function: accept_admin()
Data: (new_admin, new_secondary_admin, ledger_sequence)
```

---

## Event Count Summary

| Category | Count |
|----------|-------|
| Platform Initialization | 1 |
| Course Management | 5 |
| Enrollment & Payment | 1 |
| Certificates | 3 |
| Platform Control | 2 |
| Treasury & Tokens | 5 |
| Admin Management | 2 |
| **Total Events** | **19** |

---

## Critical Compliance Events

### For Financial Audits
- `student_enrolled` - Payment split tracking
- `tokens_withdrawn` - Fund movements
- `course_archived` - Refund tracking (includes refund count and total amount)

### For Certificate Management
- `certificate_issued` - Creation record
- `certificate_revoked` - Revocation with reason code
- `course_completed` - Completion verification method

### For Admin Oversight
- `admin_transfer_proposed` - Track admin changes
- `admin_transfer_accepted` - Confirm handover
- `treasury_updated` - Track treasury changes with effective date

---

## Key Fields in Events

### Common to All Events
- `ledger_sequence`: The ledger sequence when event was emitted

### Common to Actor-Driven Events
- `actor/admin/caller`: Address of the person performing the action

### Financial Events Include
- Amounts (in stroops)
- Fee breakdown (platform vs instructor)
- Destination addresses

### Multi-Admin Events Include
- Both admin addresses for transparency

---

## Event Query Examples

### Find all course approvals by admin
```
Filter: event_name == "course_approved" AND admin == admin_address
Extract: course_id, ledger_sequence
```

### Calculate total platform fees collected
```
Filter: event_name == "student_enrolled"
Extract: platform_fee from each event
Sum: All platform_fee amounts
```

### Find all certificate revocations with reason
```
Filter: event_name == "certificate_revoked"
Extract: certificate_id, student, course_id, reason, ledger_sequence
```

### Track refunds by course
```
Filter: event_name == "course_archived"
Extract: course_id, total_refunded, refund_count
Group by: course_id
```

### Admin activity audit trail
```
Filter: admin == admin_address (check all event types with admin field)
Extract: operation, timestamp (via ledger_sequence), affected_entity
```

---

## Implementation Notes

1. All events use snake_case naming
2. All events include ledger_sequence for chronological ordering
3. Actor/caller is always present in state-changing operations
4. Financial events break down payment splits explicitly
5. Multi-sig operations include both signers
6. Refund tracking includes count AND amount
7. Certificate revocations include reason code

---

## Testing Event Emissions

All events are tested in the existing test suite. Event emissions are verified in snapshots:
- Check `test_snapshots/test/*.json` files
- Each test verifies event structure and data
- Events are part of the contract's public interface

---

## Performance Considerations

- Events are published to Soroban event ledger
- No gas cost for event storage (handled by network)
- Recommended to index events off-chain for queries
- Events are immutable once published
- Ledger sequence provides natural ordering without additional storage

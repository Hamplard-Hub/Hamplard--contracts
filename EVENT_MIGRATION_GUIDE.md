# Hamplard Event Emission - Migration Guide

This guide explains how to migrate existing systems to use the new comprehensive event system for audit trails and forensic analysis.

## Overview

The Hamplard smart contract now emits comprehensive events for all state-changing operations. This enables:

- Complete audit trail with actor identification
- Financial transparency through payment split tracking
- Compliance reporting with ledger sequence ordering
- Off-chain indexing for analytics and monitoring
- Forensic analysis of contract state changes

## What Changed

### Before
- Minimal event emissions (only a few basic events)
- Inconsistent event data structure
- No actor identification in events
- No payment split transparency
- Limited audit trail capability

### After
- **19 comprehensive event types** covering all state-changing operations
- **Consistent event structure** with ledger sequences and actor information
- **Complete actor tracking** for authorization and compliance
- **Payment split transparency** showing platform vs instructor fees
- **Rich audit trail** with operation-specific details (refund counts, reason codes, etc.)

## Migration Steps

### Phase 1: Update Event Infrastructure (Week 1)

#### 1. Deploy Updated Contract

```bash
# Build the new contract
cd contracts/hamplard
cargo build --release

# Deploy to testnet first
soroban contract deploy \
  --network testnet \
  --source account-name \
  ./target/wasm32-unknown-unknown/release/hamplard.wasm
```

**Pre-deployment Checklist:**
- [ ] All tests pass locally (`cargo test --release`)
- [ ] Code compiles without warnings
- [ ] Reviewed event implementations
- [ ] Tested on Futurenet first

#### 2. Update Event Listener Configuration

Update your event listener to subscribe to new events:

```javascript
// Old configuration (minimal events)
const eventFilters = [
  'course_approved',
  'student_enrolled',
  'certificate_issued'
];

// New configuration (comprehensive events)
const eventFilters = [
  // Platform & Admin
  'platform_initialized',
  'admin_transfer_proposed',
  'admin_transfer_accepted',
  'platform_paused',
  'platform_unpaused',
  
  // Course Management
  'course_registered',
  'course_approved',
  'course_paused',
  'course_unpaused',
  'course_archived',
  
  // Enrollment & Payment
  'student_enrolled',
  
  // Certificates
  'course_completed',
  'certificate_issued',
  'certificate_revoked',
  
  // Treasury & Tokens
  'tokens_withdrawn',
  'treasury_updated',
  'default_fee_updated',
  'token_whitelisted',
  'token_removed_from_whitelist'
];
```

### Phase 2: Update Event Processing (Week 2-3)

#### 1. Create New Event Handlers

For each event type, create a handler:

```python
class EventHandler:
    def handle_platform_initialized(self, event):
        """Platform initialization event"""
        admin, secondary_admin, treasury, default_fee_pct, ledger_seq = event['data']
        self.db.store_platform_init({
            'admin': admin,
            'secondary_admin': secondary_admin,
            'treasury': treasury,
            'default_fee_pct': default_fee_pct,
            'ledger_sequence': ledger_seq
        })
    
    def handle_student_enrolled(self, event):
        """Student enrollment with payment split"""
        student, course_id, amount_paid, platform_fee, instructor_fee, ledger_seq = event['data']
        self.db.store_enrollment({
            'student': student,
            'course_id': course_id,
            'amount_paid': amount_paid,
            'platform_fee': platform_fee,
            'instructor_fee': instructor_fee,
            'ledger_sequence': ledger_seq,
            'timestamp': self.ledger_to_timestamp(ledger_seq)
        })
        
        # Update financial metrics
        self.db.update_metrics({
            'platform_collected': platform_fee,
            'instructor_earned': instructor_fee
        })
    
    def handle_course_archived(self, event):
        """Course archive with refund tracking"""
        admin1, admin2, course_id, refund_count, total_refunded, ledger_seq = event['data']
        self.db.store_archive_event({
            'course_id': course_id,
            'admin1': admin1,
            'admin2': admin2,
            'refund_count': refund_count,
            'total_refunded': total_refunded,
            'ledger_sequence': ledger_seq
        })
        
        # Important: Document refund for compliance
        self.compliance_log.record_refund({
            'course_id': course_id,
            'count': refund_count,
            'amount': total_refunded,
            'admins': [admin1, admin2],
            'ledger': ledger_seq
        })
    
    def handle_certificate_revoked(self, event):
        """Certificate revocation with reason"""
        admin, cert_id, student, course_id, reason, ledger_seq = event['data']
        self.db.mark_certificate_revoked({
            'certificate_id': cert_id,
            'student': student,
            'course_id': course_id,
            'revoked_by': admin,
            'reason': reason,
            'revoked_at': ledger_seq
        })
        
        # Log for dispute resolution
        self.dispute_log.record_revocation({
            'certificate': cert_id,
            'reason': reason,
            'admin': admin,
            'ledger': ledger_seq
        })
```

#### 2. Create Event Dispatcher

```python
class HamplardEventProcessor:
    def __init__(self, db):
        self.db = db
        self.handlers = {
            'platform_initialized': self.handle_platform_initialized,
            'course_registered': self.handle_course_registered,
            'course_approved': self.handle_course_approved,
            'course_paused': self.handle_course_paused,
            'course_unpaused': self.handle_course_unpaused,
            'course_archived': self.handle_course_archived,
            'student_enrolled': self.handle_student_enrolled,
            'course_completed': self.handle_course_completed,
            'certificate_issued': self.handle_certificate_issued,
            'certificate_revoked': self.handle_certificate_revoked,
            'platform_paused': self.handle_platform_paused,
            'platform_unpaused': self.handle_platform_unpaused,
            'tokens_withdrawn': self.handle_tokens_withdrawn,
            'treasury_updated': self.handle_treasury_updated,
            'default_fee_updated': self.handle_default_fee_updated,
            'admin_transfer_proposed': self.handle_admin_transfer_proposed,
            'admin_transfer_accepted': self.handle_admin_transfer_accepted,
            'token_whitelisted': self.handle_token_whitelisted,
            'token_removed_from_whitelist': self.handle_token_removed,
        }
    
    async def process_event(self, event):
        """Main event processing entry point"""
        event_name = event['topic'][0]
        
        # Store raw event for audit trail
        self.db.store_raw_event(event)
        
        # Dispatch to specific handler
        handler = self.handlers.get(event_name)
        if handler:
            try:
                await handler(event)
            except Exception as e:
                self.logger.error(f"Error processing {event_name}: {e}")
                self.db.mark_event_error(event, e)
        else:
            self.logger.warning(f"No handler for event: {event_name}")
```

### Phase 3: Update Database Schema (Week 3-4)

#### 1. Add Event Tables

```sql
-- Create event audit trail table
CREATE TABLE event_audit_trail (
    id SERIAL PRIMARY KEY,
    event_name VARCHAR(100) NOT NULL,
    contract_id VARCHAR(100) NOT NULL,
    actor_address VARCHAR(100),
    course_id VARCHAR(256),
    student_address VARCHAR(100),
    certificate_id VARCHAR(256),
    operation_type VARCHAR(100),
    amount_involved BIGINT,
    ledger_sequence INTEGER NOT NULL,
    event_timestamp TIMESTAMP NOT NULL,
    raw_event_data JSONB,
    indexed_at TIMESTAMP DEFAULT NOW(),
    
    INDEX idx_event_name (event_name),
    INDEX idx_actor (actor_address),
    INDEX idx_ledger (ledger_sequence),
    INDEX idx_timestamp (event_timestamp),
    INDEX idx_course (course_id),
    INDEX idx_student (student_address)
);

-- Create compliance tracking table
CREATE TABLE compliance_log (
    id SERIAL PRIMARY KEY,
    log_type VARCHAR(50),  -- 'refund', 'revocation', 'admin_transfer', etc.
    reference_id VARCHAR(256),
    actor_addresses TEXT[],
    amount BIGINT,
    reason VARCHAR(512),
    ledger_sequence INTEGER,
    logged_at TIMESTAMP,
    details JSONB,
    
    INDEX idx_log_type (log_type),
    INDEX idx_ledger (ledger_sequence)
);

-- Create metrics table for analytics
CREATE TABLE contract_metrics (
    id SERIAL PRIMARY KEY,
    metric_date DATE,
    total_enrolled INTEGER,
    total_completed INTEGER,
    total_refunded BIGINT,
    platform_fees_collected BIGINT,
    instructor_fees_collected BIGINT,
    active_courses INTEGER,
    certificates_issued INTEGER,
    certificates_revoked INTEGER,
    
    UNIQUE(metric_date),
    INDEX idx_date (metric_date)
);
```

#### 2. Migrate Historical Data

```python
def migrate_historical_events():
    """
    For events already emitted before migration:
    1. Re-index from Soroban events ledger
    2. Or: Manually insert summary records if indexing not possible
    """
    
    # Query historical enrollments from contract state
    enrollments = contract_client.get_all_enrollments()
    
    for enrollment in enrollments:
        # Create synthetic event record for historical data
        db.store_enrollment({
            'student': enrollment['student'],
            'course_id': enrollment['course_id'],
            'amount_paid': enrollment['amount_paid'],
            'platform_fee': calculate_fee(enrollment),
            'instructor_fee': enrollment['amount_paid'] - calculate_fee(enrollment),
            'ledger_sequence': enrollment['enrolled_at_ledger'],
            'historical': True  # Mark as migrated, not live event
        })
```

### Phase 4: Testing & Validation (Week 4-5)

#### 1. Test Event Emission

```bash
# Run all contract tests
cd contracts/hamplard
cargo test --release

# All 41 tests should pass
# Test output: "test result: ok. 41 passed; 0 failed"
```

#### 2. Validate Event Structure

```python
def validate_events():
    """Validate all emitted events match expected schema"""
    
    test_cases = [
        {
            'event': 'student_enrolled',
            'expected_fields': ['student', 'course_id', 'amount_paid', 'platform_fee', 'instructor_fee', 'ledger_sequence'],
            'validations': [
                lambda e: is_valid_address(e['student']),
                lambda e: is_valid_string(e['course_id']),
                lambda e: e['amount_paid'] == e['platform_fee'] + e['instructor_fee'],
                lambda e: e['platform_fee'] >= 0,
                lambda e: e['instructor_fee'] >= 0,
            ]
        },
        {
            'event': 'course_archived',
            'expected_fields': ['admin1', 'admin2', 'course_id', 'refund_count', 'total_refunded', 'ledger_sequence'],
            'validations': [
                lambda e: is_valid_address(e['admin1']),
                lambda e: is_valid_address(e['admin2']),
                lambda e: e['refund_count'] >= 0,
                lambda e: e['total_refunded'] >= 0,
            ]
        },
        # ... test other event types
    ]
    
    for test in test_cases:
        events = query_events_by_name(test['event'])
        for event in events:
            for field in test['expected_fields']:
                assert field in event, f"Missing field {field}"
            
            for validator in test['validations']:
                assert validator(event), f"Validation failed for {test['event']}"
```

#### 3. Performance Testing

```python
async def load_test_events():
    """Test indexing performance with large event volumes"""
    
    # Simulate 10,000 events
    events = generate_test_events(count=10000)
    
    start = time.time()
    for event in events:
        await processor.process_event(event)
    elapsed = time.time() - start
    
    # Should process at least 1000 events/second
    assert elapsed < 10, f"Too slow: {len(events)/elapsed} events/sec"
    
    # Verify data integrity
    stored_count = db.count_events()
    assert stored_count == 10000, f"Lost events: {stored_count} != 10000"
```

### Phase 5: Deployment to Production (Week 5-6)

#### 1. Pre-deployment Checklist

- [ ] All tests pass (locally and in CI/CD)
- [ ] Event handlers tested with all event types
- [ ] Database schema updated
- [ ] Historical data migrated (if needed)
- [ ] Performance validated
- [ ] Compliance logs configured
- [ ] Alerting for suspicious patterns set up
- [ ] Documentation updated
- [ ] Team trained on new event system

#### 2. Staged Rollout

```bash
# Stage 1: Mainnet testnet (Futurenet)
# - Deploy contract
# - Enable event indexing for 1 week
# - Validate all events flowing correctly

# Stage 2: Mainnet - Read-only
# - Deploy contract
# - Index events to read-only database
# - Validate 1 week without using indexed data

# Stage 3: Mainnet - Full deployment
# - Switch queries to use indexed events
# - Monitor for issues
# - Keep old system running for 2 weeks

# Stage 4: Cleanup
# - Decommission old event system
# - Archive old data for compliance
```

#### 3. Monitoring & Alerting

```python
class EventMonitor:
    def __init__(self):
        self.alerts = []
    
    def monitor_events(self):
        """Monitor event system health"""
        
        # Check: Events flowing in real-time
        recent_count = db.count_events(last_n_minutes=5)
        if recent_count == 0:
            self.alerts.append("No events in last 5 minutes")
        
        # Check: Refund tracking
        refunds = db.get_course_archives()
        for archive in refunds:
            if archive['total_refunded'] > 0:
                self.log_compliance_event("REFUND", archive)
        
        # Check: Certificate revocations
        revocations = db.get_certificate_revocations()
        for revocation in revocations:
            self.log_compliance_event("REVOCATION", revocation)
        
        # Check: Admin transfers
        transfers = db.get_admin_transfers()
        for transfer in transfers:
            self.alerts.append(f"Admin transfer: {transfer['old_admin']} -> {transfer['new_admin']}")
        
        return self.alerts
```

## Backwards Compatibility

### Old Code Still Works

Existing code querying contract state directly continues to work:

```javascript
// Old way - still works
const course = await client.getCourse(courseId);
const enrollment = await client.getEnrollment(student, courseId);

// New way - use events for audit trail
const enrollmentEvents = await eventIndexer.query({
  event_name: 'student_enrolled',
  course_id: courseId,
  student: student
});
```

### Migration Path

You can run both systems in parallel:

1. **Phase 1-2:** Old system continues, new events indexed separately
2. **Phase 3:** Queries start using event data for analytics
3. **Phase 4:** Complete migration, old system becomes backup
4. **Phase 5:** Old system decommissioned

## Compliance Implications

### Financial Audit

The new event system provides:

```
✅ Complete payment split tracking
✅ Verifiable refund records (with admin signatures)
✅ Immutable audit trail (ledger sequence ordering)
✅ Actor identification for all operations
```

### Regulatory Reporting

```python
# Generate monthly compliance report
def generate_monthly_report(year, month):
    report = {
        'period': f"{year}-{month}",
        'total_enrollments': count_enrollments(year, month),
        'total_fees_collected': sum_platform_fees(year, month),
        'total_refunded': sum_refunds(year, month),
        'certificates_issued': count_certs_issued(year, month),
        'certificates_revoked': count_certs_revoked(year, month),
        'refund_detail': get_all_refunds(year, month),
        'revocation_detail': get_all_revocations(year, month),
        'admin_actions': get_admin_actions(year, month)
    }
    return report
```

### Audit Trail

All state changes are now traceable:

```
Event: student_enrolled
  - Student: [address]
  - Course: [course_id]
  - Amount: [stroops]
  - Platform Fee: [stroops]
  - Instructor Fee: [stroops]
  - Ledger: [sequence] (immutable timestamp)

Event: certificate_revoked
  - Certificate: [id]
  - Student: [address]
  - Reason: [code]
  - Revoked By: [admin]
  - Ledger: [sequence]
```

## Support & Troubleshooting

### Common Issues

#### Events not being emitted

```
Problem: No events appearing in indexer
Solution:
1. Verify contract deployed with new code: soroban contract inspect
2. Check event listener is subscribed to correct events
3. Verify listener connected to correct Soroban network
4. Check contract logs for errors
```

#### Database schema mismatch

```
Problem: Events stored but queries fail
Solution:
1. Run schema migration: psql -f migration_schema.sql
2. Verify all columns exist: SELECT * FROM event_audit_trail LIMIT 1
3. Check for NULL values in required fields
```

#### Performance degradation

```
Problem: Event indexing is slow
Solution:
1. Add database indexes: CREATE INDEX idx_ledger ON events(ledger_sequence)
2. Batch process events instead of one-by-one
3. Archive old events to separate table
4. Monitor query performance: EXPLAIN ANALYZE
```

## Rollback Plan

If issues occur:

```bash
# Immediate: Revert to previous contract version
soroban contract deploy \
  --network [network] \
  ./target/wasm32-unknown-unknown/release/hamplard_old.wasm

# Keep event data for audit trail
# Resume old event processing system

# Investigate issue with new contract offline
# Fix and re-test before re-deploying
```

## Questions?

Refer to:
- `EVENT_REFERENCE_GUIDE.md` - Event schema reference
- `EVENT_INDEXER_INTEGRATION.md` - Implementation guide
- `EVENT_SYSTEM_IMPLEMENTATION.md` - Detailed design
- Test snapshots in `contracts/hamplard/test_snapshots/test/`

# Hamplard Event System - Integration Checklist

**Purpose**: Step-by-step guide for teams integrating the Hamplard event system into their applications.

---

## Phase 1: Understanding the Event System

### Conceptual Understanding
- [ ] Read `EVENT_REFERENCE_GUIDE.md` - Quick overview of all 19 events
- [ ] Read `EVENT_SYSTEM_IMPLEMENTATION.md` - Detailed architecture
- [ ] Understand the event structure: (Topic, Data) format
- [ ] Understand ledger sequences for chronological ordering

### Key Concepts to Master
- [ ] Events are immutable once published
- [ ] Ledger sequence provides deterministic ordering
- [ ] Payment splits enable financial reconciliation
- [ ] Multi-admin events ensure governance transparency
- [ ] Refund tracking captures compliance-critical data

---

## Phase 2: Development Environment Setup

### Prerequisites
- [ ] Rust toolchain installed (1.70+)
- [ ] Soroban SDK installed
- [ ] Node.js (14+) if using JavaScript indexer
- [ ] PostgreSQL or MongoDB for event storage
- [ ] Git for version control

### Build & Verify
- [ ] Clone Hamplard contracts repository
- [ ] Run `cargo build` - verify compilation succeeds
- [ ] Run `cargo test --lib` - verify all 41 tests pass
- [ ] Review test snapshots in `test_snapshots/test/` directory

---

## Phase 3: Event Monitoring Implementation

### 3.1 Soroban Event Streaming

#### Using Soroban RPC
- [ ] Setup connection to Soroban RPC endpoint
- [ ] Configure event subscription for contract ID
- [ ] Implement event handler function
- [ ] Test event streaming with test transactions

**Example (Node.js)**:
```javascript
const { Server } = require('soroban-client');

const server = new Server('https://soroban-rpc.mainnet.sorobanrpc.com');
const contractId = 'HAMPLARD_CONTRACT_ID';

server.on('contract:event', async (event) => {
  if (event.contractId === contractId) {
    console.log('Event received:', event.eventData);
  }
});
```

#### Using Stellar Horizon (Alternative)
- [ ] Subscribe to Soroban ledger operations
- [ ] Filter for HamplardContract events
- [ ] Extract event data from operation results
- [ ] Parse event topics and data tuples

### 3.2 Event Validation
- [ ] Implement schema validation for each event type
- [ ] Validate actor/admin addresses format
- [ ] Validate amount fields (positive, reasonable bounds)
- [ ] Validate course_id and certificate_id strings
- [ ] Check ledger sequence monotonicity

---

## Phase 4: Database Design

### 4.1 Event Storage Schema

#### Create Events Table
```sql
CREATE TABLE events (
    id BIGSERIAL PRIMARY KEY,
    event_name VARCHAR(100) NOT NULL,
    contract_id VARCHAR(100) NOT NULL,
    ledger_sequence INTEGER NOT NULL,
    ledger_timestamp TIMESTAMP NOT NULL,
    event_data JSONB NOT NULL,
    received_at TIMESTAMP DEFAULT NOW(),
    processed_at TIMESTAMP,
    INDEX idx_event_name (event_name),
    INDEX idx_ledger_sequence (ledger_sequence),
    INDEX idx_timestamp (ledger_timestamp),
    CONSTRAINT unique_event UNIQUE (contract_id, ledger_sequence, event_name)
);
```

#### Create Indexed Tables for Analysis
```sql
-- Enrollments
CREATE TABLE enrollments (
    id BIGSERIAL PRIMARY KEY,
    student_address VARCHAR(100) NOT NULL,
    course_id VARCHAR(256) NOT NULL,
    amount_paid BIGINT NOT NULL,
    platform_fee BIGINT NOT NULL,
    instructor_fee BIGINT NOT NULL,
    ledger_sequence INTEGER NOT NULL,
    enrolled_at TIMESTAMP NOT NULL,
    UNIQUE(student_address, course_id),
    INDEX idx_student (student_address),
    INDEX idx_course (course_id),
    INDEX idx_ledger (ledger_sequence)
);

-- Certificates
CREATE TABLE certificates (
    id BIGSERIAL PRIMARY KEY,
    certificate_id VARCHAR(256) UNIQUE NOT NULL,
    student_address VARCHAR(100) NOT NULL,
    course_id VARCHAR(256) NOT NULL,
    course_title VARCHAR(512),
    issued_by VARCHAR(100),
    issued_at_ledger INTEGER,
    is_revoked BOOLEAN DEFAULT FALSE,
    revoked_by VARCHAR(100),
    revocation_reason VARCHAR(256),
    revoked_at_ledger INTEGER,
    INDEX idx_student (student_address),
    INDEX idx_course (course_id),
    INDEX idx_issued (issued_at_ledger)
);

-- Courses
CREATE TABLE courses (
    id BIGSERIAL PRIMARY KEY,
    course_id VARCHAR(256) UNIQUE NOT NULL,
    instructor VARCHAR(100) NOT NULL,
    price BIGINT NOT NULL,
    token_address VARCHAR(100),
    platform_fee_percent SMALLINT,
    status VARCHAR(20),
    registered_at_ledger INTEGER,
    total_enrollments INTEGER DEFAULT 0,
    total_earned BIGINT DEFAULT 0,
    INDEX idx_instructor (instructor),
    INDEX idx_status (status)
);

-- Treasury Operations
CREATE TABLE treasury_operations (
    id BIGSERIAL PRIMARY KEY,
    operation_type VARCHAR(50),
    admin_address VARCHAR(100),
    token_address VARCHAR(100),
    amount BIGINT,
    destination VARCHAR(100),
    effective_ledger INTEGER,
    ledger_sequence INTEGER,
    operation_at TIMESTAMP,
    INDEX idx_admin (admin_address),
    INDEX idx_ledger (ledger_sequence),
    INDEX idx_type (operation_type)
);
```

- [ ] Create events table with proper indexes
- [ ] Create domain-specific tables (enrollments, certificates, courses, treasury)
- [ ] Add constraints and unique indexes
- [ ] Plan backup and retention policy

### 4.2 Sync State Tracking
```sql
CREATE TABLE sync_state (
    contract_id VARCHAR(100) PRIMARY KEY,
    last_processed_ledger INTEGER,
    last_processed_at TIMESTAMP,
    status VARCHAR(20),  -- 'synced', 'catching_up', 'behind'
    error_message TEXT,
    updated_at TIMESTAMP DEFAULT NOW()
);
```

- [ ] Create sync_state table
- [ ] Initialize with starting ledger
- [ ] Implement error tracking

---

## Phase 5: Event Processing Pipeline

### 5.1 Event Handler Implementation

#### Python Example
```python
class HamplardEventProcessor:
    def __init__(self, db):
        self.db = db
        self.handlers = {
            'student_enrolled': self.handle_enrollment,
            'certificate_issued': self.handle_cert_issued,
            'course_archived': self.handle_archive,
            'tokens_withdrawn': self.handle_withdrawal,
            'admin_transfer_proposed': self.handle_admin_transfer,
            # ... more handlers
        }
    
    def process_event(self, event):
        try:
            event_name = event['topic'][0]
            handler = self.handlers.get(event_name)
            
            if handler:
                handler(event)
            
            # Store raw event
            self.db.store_raw_event(event)
            
        except Exception as e:
            self.db.log_error(e, event)
            raise
    
    def handle_enrollment(self, event):
        data = event['data']
        enrollment = {
            'student': data[0],
            'course_id': data[1],
            'amount_paid': data[2],
            'platform_fee': data[3],
            'instructor_fee': data[4],
            'ledger_sequence': data[5]
        }
        self.db.store_enrollment(enrollment)
    
    def handle_cert_issued(self, event):
        data = event['data']
        cert = {
            'certificate_id': data[1],
            'student': data[2],
            'course_id': data[3],
            'course_title': data[4],
            'issued_by': data[0],
            'ledger_sequence': data[5]
        }
        self.db.store_certificate(cert)
    
    def handle_archive(self, event):
        data = event['data']
        archive = {
            'admin1': data[0],
            'admin2': data[1],
            'course_id': data[2],
            'refund_count': data[3],
            'total_refunded': data[4],
            'ledger_sequence': data[5]
        }
        self.db.store_archive(archive)
```

- [ ] Implement event handler for each event type
- [ ] Add error handling and logging
- [ ] Validate event data before storage
- [ ] Implement transaction management

### 5.2 Incremental Sync
- [ ] Query last_processed_ledger from sync_state
- [ ] Fetch events since last_processed_ledger
- [ ] Process events in ledger sequence order
- [ ] Update sync_state with new ledger
- [ ] Implement retry logic for failures

### 5.3 Reorg Handling
- [ ] Store ledger_hash with each event
- [ ] Detect reorg when new ledger_hash differs at same sequence
- [ ] Implement rollback logic
- [ ] Re-process events after reorg

---

## Phase 6: Query Implementation

### 6.1 Financial Analytics

#### Platform Revenue Calculation
```sql
SELECT 
    DATE_TRUNC('month', enrolled_at) as month,
    SUM(platform_fee) as platform_revenue,
    SUM(instructor_fee) as instructor_payouts,
    COUNT(*) as enrollment_count
FROM enrollments
GROUP BY DATE_TRUNC('month', enrolled_at)
ORDER BY month DESC;
```

- [ ] Implement platform revenue query
- [ ] Implement instructor earnings query
- [ ] Implement course-specific revenue query

#### Refund Tracking
```sql
SELECT 
    course_id,
    SUM(refund_count) as total_students_refunded,
    SUM(total_refunded) as total_refund_amount
FROM archive_events
GROUP BY course_id;
```

- [ ] Implement refund summary query
- [ ] Implement refund by course query
- [ ] Implement refund timeline query

### 6.2 Course Analytics

#### Enrollment Funnel
```sql
SELECT 
    c.course_id,
    c.instructor,
    COUNT(DISTINCT e.student_address) as enrollments,
    COUNT(DISTINCT cc.student_address) as completions,
    COUNT(DISTINCT cert.certificate_id) as certificates,
    ROUND(100.0 * COUNT(DISTINCT cc.student_address) / 
          NULLIF(COUNT(DISTINCT e.student_address), 0)) as completion_rate
FROM courses c
LEFT JOIN enrollments e ON c.course_id = e.course_id
LEFT JOIN completion_events cc ON c.course_id = cc.course_id
LEFT JOIN certificates cert ON c.course_id = cert.course_id
GROUP BY c.course_id, c.instructor
ORDER BY enrollments DESC;
```

- [ ] Implement course performance query
- [ ] Implement completion rate query
- [ ] Implement student dropout analysis

### 6.3 Compliance Reporting

#### Certificate Revocation Audit
```sql
SELECT 
    certificate_id,
    student_address,
    course_id,
    revocation_reason,
    revoked_by,
    revoked_at_ledger
FROM certificates
WHERE is_revoked = TRUE
ORDER BY revoked_at_ledger DESC;
```

- [ ] Implement revocation audit query
- [ ] Implement admin action log query
- [ ] Implement treasury operations query

---

## Phase 7: API Development

### 7.1 Event API Endpoints

#### Enumerator Events by Type
```
GET /api/events?type=student_enrolled&limit=100&offset=0
Response: {events: [...], total: N, next_offset: N}
```

- [ ] Implement event list endpoint
- [ ] Implement event filter parameters
- [ ] Implement pagination

#### Get Event Details
```
GET /api/events/{ledger_sequence}
Response: {event_name, data, timestamp}
```

- [ ] Implement event detail endpoint
- [ ] Implement event search

### 7.2 Analytics Endpoints

#### Revenue Endpoints
```
GET /api/analytics/revenue?month=2026-06
Response: {platform_revenue, instructor_payouts, enrollment_count}
```

- [ ] Implement revenue endpoint
- [ ] Implement course revenue endpoint
- [ ] Implement instructor earnings endpoint

#### Certificate Endpoints
```
GET /api/certificates/{certificate_id}
Response: {student, course_id, course_title, status, revocation_reason}
```

- [ ] Implement certificate lookup
- [ ] Implement student certificates query
- [ ] Implement certificate verification endpoint

#### Course Analytics
```
GET /api/courses/{course_id}/analytics
Response: {enrollments, completions, revenue, completion_rate}
```

- [ ] Implement course analytics endpoint
- [ ] Implement enrollment timeline
- [ ] Implement revenue timeline

---

## Phase 8: Monitoring & Alerting

### 8.1 Event Processing Monitoring
- [ ] Monitor events/second rate
- [ ] Monitor processing latency
- [ ] Monitor error rate
- [ ] Alert on processing failures

### 8.2 Financial Monitoring
- [ ] Monitor total platform revenue
- [ ] Monitor average enrollment amount
- [ ] Alert on suspicious refund patterns
- [ ] Alert on unusual transaction amounts

### 8.3 Operational Monitoring
- [ ] Monitor admin action frequency
- [ ] Alert on platform pauses
- [ ] Alert on certificate revocations
- [ ] Alert on treasury changes

---

## Phase 9: Testing & Validation

### 9.1 Unit Tests
- [ ] Test event validation logic
- [ ] Test database storage functions
- [ ] Test query accuracy
- [ ] Test error handling

### 9.2 Integration Tests
- [ ] Test end-to-end event processing
- [ ] Test database consistency
- [ ] Test API endpoints
- [ ] Test analytics calculations

### 9.3 Test Coverage
- [ ] Verify enrollment flow events
- [ ] Verify certificate issuance flow
- [ ] Verify archive with refunds
- [ ] Verify admin transfers
- [ ] Verify edge cases

### 9.4 Validation Against Test Snapshots
- [ ] Review `test_snapshots/` directory
- [ ] Compare your event parsing against snapshots
- [ ] Validate payment splits match
- [ ] Validate refund calculations

---

## Phase 10: Documentation & Deployment

### 10.1 Documentation
- [ ] Document event schema for your team
- [ ] Document API endpoints
- [ ] Document database schema
- [ ] Create runbook for operations
- [ ] Create troubleshooting guide

### 10.2 Deployment Planning
- [ ] Plan database migrations
- [ ] Plan indexer deployment
- [ ] Plan monitoring setup
- [ ] Plan disaster recovery
- [ ] Plan scaling strategy

### 10.3 Pre-Production Testing
- [ ] Test against testnet
- [ ] Load test indexer
- [ ] Test failover scenarios
- [ ] Test backup/restore procedures
- [ ] Security audit

### 10.4 Production Deployment
- [ ] Deploy in staging first
- [ ] Monitor for 24 hours
- [ ] Deploy to production
- [ ] Monitor closely first week
- [ ] Gradual rollout if applicable

---

## Phase 11: Ongoing Operations

### 11.1 Daily Operations
- [ ] Monitor event processing lag
- [ ] Check for errors in logs
- [ ] Verify analytics accuracy
- [ ] Monitor system performance

### 11.2 Weekly Tasks
- [ ] Review compliance reports
- [ ] Verify refund tracking accuracy
- [ ] Check admin action audit log
- [ ] Backup databases

### 11.3 Monthly Tasks
- [ ] Generate compliance reports
- [ ] Review certificate revocations
- [ ] Analyze course performance
- [ ] Plan enhancements

---

## Phase 12: Enhancement Roadmap

### Short Term (1-3 months)
- [ ] Implement real-time dashboards
- [ ] Add alert system
- [ ] Build compliance report generator
- [ ] Create certificate lookup service

### Medium Term (3-6 months)
- [ ] Implement fraud detection
- [ ] Add predictive analytics
- [ ] Build admin portal
- [ ] Create public certificate verification

### Long Term (6+ months)
- [ ] Advanced analytics platform
- [ ] Event-driven workflows
- [ ] Machine learning models
- [ ] Blockchain explorer integration

---

## Quick Reference: Event Data Types

| Event Name | Key Data |
|------------|----------|
| `student_enrolled` | student, course_id, amount_paid, platform_fee, instructor_fee |
| `certificate_issued` | certificate_id, student, course_id, course_title |
| `certificate_revoked` | certificate_id, reason, revoked_by |
| `course_archived` | course_id, refund_count, total_refunded |
| `course_approved` | course_id, admin |
| `admin_transfer_proposed` | new_admin, new_secondary_admin |
| `tokens_withdrawn` | amount, destination, admin |
| `treasury_updated` | new_treasury, effective_ledger |

---

## Troubleshooting Guide

### Issue: Events not received
- [ ] Check contract address is correct
- [ ] Verify Soroban RPC endpoint is accessible
- [ ] Check network connectivity
- [ ] Verify event subscription is active

### Issue: Events delayed
- [ ] Check database performance
- [ ] Monitor Soroban network latency
- [ ] Check for reorg handling
- [ ] Verify sync_state is updating

### Issue: Analytics inaccurate
- [ ] Verify event validation logic
- [ ] Check database constraints
- [ ] Re-process events from backup
- [ ] Compare against test snapshots

### Issue: Missing events
- [ ] Check sync_state for gaps
- [ ] Re-fetch from last good ledger
- [ ] Compare against archival backup
- [ ] Review error logs

---

## Support & Resources

- **Event Documentation**: See `EVENT_*.md` files
- **Contract Source**: `contracts/hamplard/src/lib.rs`
- **Test Snapshots**: `contracts/hamplard/test_snapshots/test/*.json`
- **Test Suite**: `contracts/hamplard/src/test.rs` (41 tests)

---

## Sign-Off Checklist

- [ ] All phases completed
- [ ] All tests passing
- [ ] Documentation complete
- [ ] Monitoring active
- [ ] Ready for production
- [ ] Team trained
- [ ] Runbook available
- [ ] Backup procedures verified

---

**Prepared**: June 27, 2026  
**Version**: 1.0  
**Status**: Ready for implementation


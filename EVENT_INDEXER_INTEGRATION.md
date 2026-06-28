# Hamplard Event Indexer Integration Guide

This guide provides everything needed for off-chain services to index and query Hamplard contract events for audit trails, analytics, and compliance reporting.

## Quick Start

### 1. Subscribe to Contract Events

Connect to the Soroban event stream for the Hamplard contract and listen for events:

```javascript
// Example using soroban-js
const HamplardContractId = "CBIGP4IMVP3SLMJX32XVKIVB3WNXR6T4S3LSQPHDQ7IF6LQ6W5D7T7";

// Subscribe to all Hamplard events
sorobanEvents.on('contract:event', async (event) => {
  if (event.contractId === HamplardContractId) {
    const eventName = event.eventData[0].sym();  // First element is event name
    const eventData = event.eventData[1];         // Second element is data tuple
    
    // Process based on event type
    handleEvent(eventName, eventData);
  }
});
```

### 2. Event Structure

All events follow this pattern:

```
Event Topic: (Symbol: event_name, Identifier: string/symbol)
Event Data: (actor, data1, data2, ..., ledger_sequence)
```

**Example - student_enrolled event:**
```
Topic: ("student_enrolled", course_id)
Data: (student_addr, course_id, amount_paid, platform_fee, instructor_fee, ledger_seq)
```

## Event Schemas

### Platform Initialization

**Event Name:** `platform_initialized`
```
Topic: ("platform_initialized", admin_address)
Data: (admin, secondary_admin, treasury, default_fee_pct, ledger_sequence)

Data Types:
- admin: Address
- secondary_admin: Address
- treasury: Address
- default_fee_pct: u32 (percentage, 0-100)
- ledger_sequence: u32

Usage: Track platform setup and initial configuration
```

### Course Management

**Event Name:** `course_registered`
```
Topic: ("course_registered", course_id)
Data: (instructor, course_id, instructor, price, token, fee_percent, ledger_sequence)

Data Types:
- instructor: Address (actor)
- course_id: String
- instructor: Address (course instructor)
- price: i128 (in stroops)
- token: Address (payment token)
- fee_percent: u32 (platform fee 0-100)
- ledger_sequence: u32

Query: Find all courses by instructor
```

**Event Name:** `course_approved`
```
Topic: ("course_approved", course_id)
Data: (admin, course_id, ledger_sequence)

Data Types:
- admin: Address
- course_id: String
- ledger_sequence: u32

Compliance: Track admin approval decisions
```

**Event Name:** `course_paused`
```
Topic: ("course_paused", course_id)
Data: (caller, course_id, ledger_sequence)

Data Types:
- caller: Address (admin or instructor)
- course_id: String
- ledger_sequence: u32
```

**Event Name:** `course_unpaused`
```
Topic: ("course_unpaused", course_id)
Data: (caller, course_id, ledger_sequence)

Data Types:
- caller: Address
- course_id: String
- ledger_sequence: u32
```

**Event Name:** `course_archived`
```
Topic: ("course_archived", course_id)
Data: (admin1, admin2, course_id, refund_count, total_refunded, ledger_sequence)

Data Types:
- admin1: Address (first signer)
- admin2: Address (second signer)
- course_id: String
- refund_count: u32 (number of students refunded)
- total_refunded: i128 (total amount in stroops)
- ledger_sequence: u32

Critical: Document all refunds for compliance and accounting
```

### Enrollment & Payment

**Event Name:** `student_enrolled`
```
Topic: ("student_enrolled", course_id)
Data: (student, course_id, amount_paid, platform_fee, instructor_fee, ledger_sequence)

Data Types:
- student: Address
- course_id: String
- amount_paid: i128 (in stroops, total payment)
- platform_fee: i128 (portion to treasury in stroops)
- instructor_fee: i128 (portion to instructor in stroops)
- ledger_sequence: u32

Analytics:
- Total platform fees: SUM(platform_fee)
- Total instructor earnings: SUM(instructor_fee)
- Course revenue: SUM(amount_paid) GROUP BY course_id
- Student enrollment count: COUNT(DISTINCT student)

Note: amount_paid = platform_fee + instructor_fee (always)
```

### Certificate Management

**Event Name:** `course_completed`
```
Topic: ("course_completed", course_id)
Data: (admin, student, course_id, has_evidence, ledger_sequence)

Data Types:
- admin: Address
- student: Address
- course_id: String
- has_evidence: bool (evidence hash was provided)
- ledger_sequence: u32

Verification: Track completion verification method
```

**Event Name:** `certificate_issued`
```
Topic: ("certificate_issued", certificate_id)
Data: (admin, certificate_id, student, course_id, course_title, ledger_sequence)

Data Types:
- admin: Address
- certificate_id: String
- student: Address
- course_id: String
- course_title: String (on-chain title for verification)
- ledger_sequence: u32

Indexing: Create certificate lookup by student or course
```

**Event Name:** `certificate_revoked`
```
Topic: ("certificate_revoked", certificate_id)
Data: (admin, certificate_id, student, course_id, reason, ledger_sequence)

Data Types:
- admin: Address (revoking admin)
- certificate_id: String
- student: Address
- course_id: String
- reason: String (reason code: "ACADEMIC_DISHONESTY", "ISSUED_IN_ERROR", etc.)
- ledger_sequence: u32

Compliance: Document all revocations with reason for audit
```

### Platform Control

**Event Name:** `platform_paused`
```
Topic: ("platform_paused", "system")
Data: (admin, ledger_sequence)

Data Types:
- admin: Address
- ledger_sequence: u32

Operations: Track emergency pauses
```

**Event Name:** `platform_unpaused`
```
Topic: ("platform_unpaused", "system")
Data: (admin, ledger_sequence)

Data Types:
- admin: Address
- ledger_sequence: u32
```

### Treasury & Tokens

**Event Name:** `tokens_withdrawn`
```
Topic: ("tokens_withdrawn", token)
Data: (admin, token, amount, destination, ledger_sequence)

Data Types:
- admin: Address
- token: Address
- amount: i128 (in stroops)
- destination: Address
- ledger_sequence: u32

Auditing: Track all treasury fund movements
```

**Event Name:** `treasury_updated`
```
Topic: ("treasury_updated", new_treasury)
Data: (admin1, admin2, new_treasury, effective_ledger, ledger_sequence)

Data Types:
- admin1: Address
- admin2: Address
- new_treasury: Address
- effective_ledger: u32 (when change becomes effective)
- ledger_sequence: u32

Configuration: Track treasury address changes with activation timing
```

**Event Name:** `default_fee_updated`
```
Topic: ("default_fee_updated", "system")
Data: (admin, new_fee_pct, ledger_sequence)

Data Types:
- admin: Address
- new_fee_pct: u32 (0-100)
- ledger_sequence: u32

Configuration: Track platform fee percentage changes
```

**Event Name:** `token_whitelisted`
```
Topic: ("token_whitelisted", token)
Data: (admin, token, ledger_sequence)

Data Types:
- admin: Address
- token: Address
- ledger_sequence: u32

Configuration: Track approved payment tokens
```

**Event Name:** `token_removed_from_whitelist`
```
Topic: ("token_removed_from_whitelist", token)
Data: (admin, token, ledger_sequence)

Data Types:
- admin: Address
- token: Address
- ledger_sequence: u32

Configuration: Track removed payment tokens
```

### Admin Management

**Event Name:** `admin_transfer_proposed`
```
Topic: ("admin_transfer_proposed", new_admin)
Data: (admin1, admin2, new_admin, new_secondary_admin, ledger_sequence)

Data Types:
- admin1: Address (current admin)
- admin2: Address (current secondary admin)
- new_admin: Address (proposed new admin)
- new_secondary_admin: Address (proposed new secondary admin)
- ledger_sequence: u32

Governance: First step of admin transfer
```

**Event Name:** `admin_transfer_accepted`
```
Topic: ("admin_transfer_accepted", new_admin)
Data: (new_admin, new_secondary_admin, ledger_sequence)

Data Types:
- new_admin: Address
- new_secondary_admin: Address
- ledger_sequence: u32

Governance: Confirms admin transfer completion
```

## Query Examples

### Financial Analytics

```sql
-- Total platform fees collected (in stroops)
SELECT SUM(platform_fee) FROM events 
WHERE event_name = 'student_enrolled'

-- Revenue by instructor
SELECT 
  (event_data).2 as instructor,  -- instructor address from course_registered
  SUM((event_data).3) as total_revenue  -- price from course_registered
FROM events 
WHERE event_name = 'student_enrolled'
GROUP BY instructor

-- Platform fee breakdown by month
SELECT 
  DATE_TRUNC('month', ledger_date) as month,
  SUM(platform_fee) as platform_fees,
  SUM(instructor_fee) as instructor_fees
FROM events 
WHERE event_name = 'student_enrolled'
GROUP BY DATE_TRUNC('month', ledger_date)

-- Total refunds issued
SELECT SUM(total_refunded) FROM events 
WHERE event_name = 'course_archived'
```

### Enrollment Analytics

```sql
-- Students enrolled in each course
SELECT 
  course_id,
  COUNT(DISTINCT student) as student_count
FROM events 
WHERE event_name = 'student_enrolled'
GROUP BY course_id

-- Course completion rate
SELECT 
  course_id,
  COUNT(DISTINCT student) as completed_count,
  (SELECT COUNT(DISTINCT student) 
   FROM events e2 
   WHERE e2.event_name = 'student_enrolled' 
   AND e2.course_id = e1.course_id) as enrolled_count
FROM events e1
WHERE event_name = 'course_completed'
GROUP BY course_id
```

### Compliance Reporting

```sql
-- Certificate revocations audit trail
SELECT 
  certificate_id,
  student,
  course_id,
  reason,
  admin,
  ledger_sequence
FROM events 
WHERE event_name = 'certificate_revoked'
ORDER BY ledger_sequence DESC

-- Admin actions log
SELECT 
  event_name,
  admin,
  course_id,
  ledger_sequence,
  ledger_timestamp
FROM events 
WHERE event_name IN (
  'admin_transfer_proposed',
  'admin_transfer_accepted',
  'course_approved',
  'course_archived',
  'certificate_revoked'
)
ORDER BY ledger_sequence DESC

-- All treasury operations
SELECT 
  event_name,
  admin,
  amount,
  destination,
  ledger_sequence
FROM events 
WHERE event_name IN (
  'tokens_withdrawn',
  'treasury_updated'
)
ORDER BY ledger_sequence DESC
```

## Implementation Patterns

### Event Processing Pipeline

```python
class HamplardEventIndexer:
    def __init__(self, db):
        self.db = db
        self.event_handlers = {
            'student_enrolled': self.handle_enrollment,
            'certificate_issued': self.handle_cert_issued,
            'course_archived': self.handle_archive,
            # ... map all event types to handlers
        }
    
    def process_event(self, event):
        """Process incoming Soroban event"""
        event_name = event['topic'][0]
        handler = self.event_handlers.get(event_name)
        
        if handler:
            handler(event)
        
        # Always store raw event for audit trail
        self.db.store_raw_event(event)
    
    def handle_enrollment(self, event):
        """Extract and store enrollment data"""
        data = event['data']
        enrollment = {
            'student': data[0],
            'course_id': data[1],
            'amount_paid': data[2],
            'platform_fee': data[3],
            'instructor_fee': data[4],
            'ledger_sequence': data[5],
            'timestamp': self.ledger_to_timestamp(data[5])
        }
        self.db.store_enrollment(enrollment)
    
    def handle_cert_issued(self, event):
        """Extract and store certificate data"""
        data = event['data']
        certificate = {
            'certificate_id': data[1],
            'student': data[2],
            'course_id': data[3],
            'course_title': data[4],
            'issued_by': data[0],
            'ledger_sequence': data[5]
        }
        self.db.store_certificate(certificate)
```

### Database Schema

```sql
CREATE TABLE events_raw (
    id SERIAL PRIMARY KEY,
    event_name VARCHAR(100),
    contract_id VARCHAR(100),
    ledger_sequence INTEGER,
    event_data JSONB,
    received_at TIMESTAMP DEFAULT NOW(),
    INDEX idx_event_name (event_name),
    INDEX idx_ledger_sequence (ledger_sequence)
);

CREATE TABLE enrollments (
    id SERIAL PRIMARY KEY,
    student_address VARCHAR(100),
    course_id VARCHAR(256),
    amount_paid BIGINT,
    platform_fee BIGINT,
    instructor_fee BIGINT,
    ledger_sequence INTEGER,
    enrolled_at TIMESTAMP,
    UNIQUE(student_address, course_id, ledger_sequence),
    INDEX idx_student (student_address),
    INDEX idx_course (course_id),
    INDEX idx_ledger (ledger_sequence)
);

CREATE TABLE certificates (
    id SERIAL PRIMARY KEY,
    certificate_id VARCHAR(256),
    student_address VARCHAR(100),
    course_id VARCHAR(256),
    course_title VARCHAR(512),
    issued_by VARCHAR(100),
    issued_at_ledger INTEGER,
    is_revoked BOOLEAN DEFAULT FALSE,
    revoked_by VARCHAR(100),
    revocation_reason VARCHAR(256),
    revoked_at_ledger INTEGER,
    UNIQUE(certificate_id),
    INDEX idx_student (student_address),
    INDEX idx_course (course_id),
    INDEX idx_issued (issued_at_ledger)
);

CREATE TABLE treasury_operations (
    id SERIAL PRIMARY KEY,
    operation_type VARCHAR(50),  -- 'withdrawal' or 'update'
    admin_address VARCHAR(100),
    token_address VARCHAR(100),
    amount BIGINT,
    destination VARCHAR(100),
    effective_ledger INTEGER,
    ledger_sequence INTEGER,
    operation_at TIMESTAMP,
    INDEX idx_admin (admin_address),
    INDEX idx_ledger (ledger_sequence)
);
```

## Error Handling

### Invalid Events

Some events may have incomplete or malformed data. Always validate:

```javascript
function validateEvent(event) {
  const [eventName, identifier] = event.topic;
  const data = event.data;
  
  // Validate event-specific schema
  switch(eventName) {
    case 'student_enrolled':
      if (data.length !== 6) throw new Error('Invalid enrollment event');
      if (!isValidAddress(data[0])) throw new Error('Invalid student address');
      if (!isValidString(data[1])) throw new Error('Invalid course_id');
      if (typeof data[2] !== 'number') throw new Error('Invalid amount_paid');
      break;
    
    // ... validate other event types
  }
  
  return true;
}
```

### Handling Reorgs

Stellar ledgers can be reorged. Handle this by:

1. Store `ledger_sequence` and `ledger_hash` for each event
2. When reorg detected (new ledger_hash at same sequence), delete and re-process events
3. Use ledger_sequence as primary ordering, not timestamp

## Performance Optimization

### Batch Processing

```javascript
async function batchProcessEvents(events, batchSize = 100) {
  for (let i = 0; i < events.length; i += batchSize) {
    const batch = events.slice(i, i + batchSize);
    await Promise.all(batch.map(e => processEvent(e)));
  }
}
```

### Incremental Sync

```sql
-- Store last processed ledger
CREATE TABLE sync_state (
    contract_id VARCHAR(100) PRIMARY KEY,
    last_processed_ledger INTEGER,
    last_processed_at TIMESTAMP
);

-- Only fetch new events
SELECT * FROM soroban_events 
WHERE ledger_sequence > (
  SELECT last_processed_ledger FROM sync_state 
  WHERE contract_id = ?
)
ORDER BY ledger_sequence ASC
```

## Event Retention

Soroban events are retained on-chain according to the node's configuration:

- **Typical retention**: Up to 30 days of events
- **Long-term storage**: Must implement off-chain indexing for historical queries
- **Archive strategy**: Export events to permanent storage (database, S3, etc.) daily

## Testing Event Indexing

Use the test snapshots to validate your indexer:

```bash
# View sample events from tests
ls contracts/hamplard/test_snapshots/test/*.json

# Each JSON file contains the events emitted during that test
# Use these to validate your event parsing and storage logic
```

## Support

For questions about event schemas or integration, refer to:
- `EVENT_REFERENCE_GUIDE.md` - Quick reference of all events
- `EVENT_SYSTEM_IMPLEMENTATION.md` - Detailed implementation notes
- `contracts/hamplard/src/lib.rs` - Source code with event definitions

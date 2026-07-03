# Hamplard Indexer Integration Guide

## Overview

This guide provides off-chain indexers and backend systems with everything needed to consume, parse, and store Hamplard contract events for audit trails and analytics.

## Event Source Configuration

### Connect to Stellar RPC

```javascript
// Using Stellar JavaScript SDK
import { Keypair, SorobanRpc } from "@stellar/js-sdk";

const rpcUrl = "https://soroban-testnet.stellar.org";
const server = new SorobanRpc.Server(rpcUrl);
const contractId = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4";
```

### Listen for Events

```javascript
async function listenToContractEvents() {
  // Get the latest ledger
  let lastLedger = await server.getLatestLedger();
  
  while (true) {
    // Query events from the last ledger
    const events = await server.getEvents({
      filters: [
        {
          type: "contract",
          contractIds: [contractId],
        },
      ],
      startLedger: lastLedger.sequence,
    });

    // Process each event
    for (const event of events.events) {
      await processEvent(event);
    }

    lastLedger = events.latestLedger;
    
    // Poll every 5 seconds
    await new Promise(resolve => setTimeout(resolve, 5000));
  }
}
```

## Event Parsing

### Event Structure

Events returned from RPC have this structure:

```typescript
{
  type: "contract",
  ledger: 1000,
  ledgerCloseTime: "2024-06-28T12:00:00Z",
  contractId: "CAAAA...",
  id: "0001000000000001",
  pagingToken: "1000-0",
  topics: [
    Symbol | String,  // Topic 0: Event type
    String,          // Topic 1: Primary identifier (course_id, cert_id, etc.)
  ],
  data: {
    xdr: "AAAADgAAA...",  // XDR-encoded data
  }
}
```

### Decode Event Topics

```javascript
function decodeEventTopics(topics) {
  const eventType = topics[0].value || topics[0];
  const primaryId = topics[1].value || topics[1];
  
  return {
    eventType: String(eventType),
    primaryId: String(primaryId),
  };
}
```

### Parse XDR Data

```javascript
import { xdr } from "@stellar/js-sdk";

function parseEventData(eventXdr) {
  // Decode the XDR bytes
  const buffer = Buffer.from(eventXdr, "base64");
  const val = xdr.ScVal.fromXDR(buffer);
  
  // Extract individual fields (depends on event type)
  const fields = [];
  if (val.type() === xdr.ScValType.scvTypeVec()) {
    for (const item of val.vec().scVec()) {
      fields.push(scValToNative(item));
    }
  }
  
  return fields;
}

function scValToNative(scVal) {
  switch (scVal.type()) {
    case xdr.ScValType.scvTypeU32:
      return scVal.u32().low;
    case xdr.ScValType.scvTypeI128:
      return scVal.i128();
    case xdr.ScValType.scvTypeAddress:
      return scVal.address().contractId().toString();
    case xdr.ScValType.scvTypeSymbol:
      return scVal.sym().toString();
    default:
      return scVal;
  }
}
```

## Event Type Handlers

### Handle Enrollment Events

```javascript
async function handleStudentEnrolled(event, db) {
  const { student, courseId, amountPaid, platformFee, instructorFee, ledgerSeq } = event.data;
  
  // Validate payment split invariant
  if (platformFee + instructorFee !== amountPaid) {
    console.error("Payment split invariant violated!");
    return;
  }
  
  // Store in database
  await db.query(
    `INSERT INTO enrollments (student, course_id, amount_paid, platform_fee, instructor_fee, ledger_seq, created_at)
     VALUES ($1, $2, $3, $4, $5, $6, $7)`,
    [student, courseId, amountPaid, platformFee, instructorFee, ledgerSeq, new Date()]
  );
  
  // Update course stats
  await db.query(
    `UPDATE courses SET total_enrollments = total_enrollments + 1, total_earned = total_earned + $1
     WHERE course_id = $2`,
    [amountPaid, courseId]
  );
}
```

### Handle Certificate Issuance

```javascript
async function handleCertificateIssued(event, db) {
  const { admin, certificateId, student, courseId, courseTitle, ledgerSeq } = event.data;
  
  // Store certificate
  await db.query(
    `INSERT INTO certificates (certificate_id, student, course_id, course_title, issued_by, ledger_seq, issued_at)
     VALUES ($1, $2, $3, $4, $5, $6, $7)`,
    [certificateId, student, courseId, courseTitle, admin, ledgerSeq, new Date()]
  );
  
  // Emit notification
  await notificationService.send({
    type: 'certificate_issued',
    recipient: student,
    courseId,
    certificateId,
  });
}
```

### Handle Certificate Revocation

```javascript
async function handleCertificateRevoked(event, db) {
  const { admin, certificateId, student, courseId, reason, ledgerSeq } = event.data;
  
  // Mark as revoked
  await db.query(
    `UPDATE certificates 
     SET revoked = true, revoked_by = $1, revoked_reason = $2, revoked_at_ledger = $3
     WHERE certificate_id = $4`,
    [admin, reason, ledgerSeq, certificateId]
  );
  
  // Log audit trail
  await auditLog.record({
    action: 'certificate_revoked',
    actor: admin,
    certificateId,
    student,
    reason,
    timestamp: new Date(),
    ledgerSequence: ledgerSeq,
  });
}
```

### Handle Admin Transfer

```javascript
async function handleAdminTransferProposed(event, db) {
  const { proposer1, proposer2, newAdmin, newSecondaryAdmin, ledgerSeq } = event.data;
  
  // Record proposal
  await db.query(
    `INSERT INTO admin_transfers (status, proposed_by_1, proposed_by_2, new_admin, new_secondary_admin, proposed_ledger)
     VALUES ('pending', $1, $2, $3, $4, $5)`,
    [proposer1, proposer2, newAdmin, newSecondaryAdmin, ledgerSeq]
  );
}

async function handleAdminTransferAccepted(event, db) {
  const { newAdmin, newSecondaryAdmin, ledgerSeq } = event.data;
  
  // Mark transfer complete
  await db.query(
    `UPDATE admin_transfers 
     SET status = 'accepted', accepted_ledger = $1
     WHERE new_admin = $2 AND new_secondary_admin = $3`,
    [ledgerSeq, newAdmin, newSecondaryAdmin]
  );
  
  // Update admin records
  await db.query(
    `UPDATE contract_config 
     SET primary_admin = $1, secondary_admin = $2, admin_updated_at = $3
     WHERE id = 1`,
    [newAdmin, newSecondaryAdmin, new Date()]
  );
}
```

### Handle Archive (Complex Multi-Event)

```javascript
async function handleCourseArchived(event, db) {
  const { admin1, admin2, courseId, refundCount, totalRefunded, ledgerSeq } = event.data;
  
  // Update course status
  await db.query(
    `UPDATE courses 
     SET status = 'archived', archived_by_1 = $1, archived_by_2 = $2, 
         archived_ledger = $3, refund_count = $4, total_refunded = $5
     WHERE course_id = $6`,
    [admin1, admin2, ledgerSeq, refundCount, totalRefunded, courseId]
  );
  
  // Mark all enrollments as refunded
  await db.query(
    `UPDATE enrollments 
     SET refunded = true, refunded_ledger = $1
     WHERE course_id = $2 AND refunded = false`,
    [ledgerSeq, courseId]
  );
}
```

## Database Schema

### Enrollments Table

```sql
CREATE TABLE enrollments (
  id SERIAL PRIMARY KEY,
  student VARCHAR(56) NOT NULL,
  course_id VARCHAR(256) NOT NULL,
  amount_paid BIGINT NOT NULL,
  platform_fee BIGINT NOT NULL,
  instructor_fee BIGINT NOT NULL,
  ledger_seq INTEGER NOT NULL,
  created_at TIMESTAMP NOT NULL,
  completed BOOLEAN DEFAULT false,
  completed_at TIMESTAMP,
  certificate_issued BOOLEAN DEFAULT false,
  refunded BOOLEAN DEFAULT false,
  
  UNIQUE(student, course_id),
  FOREIGN KEY(course_id) REFERENCES courses(course_id),
  INDEX(student),
  INDEX(course_id),
  INDEX(ledger_seq)
);
```

### Certificates Table

```sql
CREATE TABLE certificates (
  certificate_id VARCHAR(256) PRIMARY KEY,
  student VARCHAR(56) NOT NULL,
  course_id VARCHAR(256) NOT NULL,
  course_title VARCHAR(512),
  issued_by VARCHAR(56) NOT NULL,
  issued_at TIMESTAMP NOT NULL,
  ledger_seq INTEGER NOT NULL,
  revoked BOOLEAN DEFAULT false,
  revoked_by VARCHAR(56),
  revoked_reason VARCHAR(255),
  revoked_at_ledger INTEGER,
  
  FOREIGN KEY(course_id) REFERENCES courses(course_id),
  FOREIGN KEY(student) REFERENCES enrollments(student),
  INDEX(student),
  INDEX(course_id),
  INDEX(ledger_seq),
  INDEX(revoked)
);
```

### Courses Table

```sql
CREATE TABLE courses (
  course_id VARCHAR(256) PRIMARY KEY,
  instructor VARCHAR(56) NOT NULL,
  price BIGINT NOT NULL,
  platform_fee_percent INTEGER NOT NULL,
  token_address VARCHAR(56) NOT NULL,
  status VARCHAR(20) NOT NULL,
  total_enrollments INTEGER DEFAULT 0,
  total_earned BIGINT DEFAULT 0,
  created_ledger INTEGER NOT NULL,
  approved_ledger INTEGER,
  archived_ledger INTEGER,
  
  INDEX(instructor),
  INDEX(status),
  INDEX(created_ledger)
);
```

### Audit Log Table

```sql
CREATE TABLE audit_log (
  id SERIAL PRIMARY KEY,
  action VARCHAR(50) NOT NULL,
  actor VARCHAR(56),
  course_id VARCHAR(256),
  certificate_id VARCHAR(256),
  student VARCHAR(56),
  amount_change BIGINT,
  reason VARCHAR(255),
  ledger_sequence INTEGER NOT NULL,
  created_at TIMESTAMP NOT NULL,
  
  INDEX(actor),
  INDEX(ledger_sequence),
  INDEX(created_at)
);
```

## Analytics Queries

### Revenue by Course

```sql
SELECT 
  course_id,
  SUM(amount_paid) as total_revenue,
  SUM(platform_fee) as platform_revenue,
  SUM(instructor_fee) as instructor_revenue,
  COUNT(*) as total_enrollments,
  COUNT(DISTINCT student) as unique_students,
  MAX(created_at) as last_enrollment
FROM enrollments
WHERE completed = true
GROUP BY course_id
ORDER BY total_revenue DESC;
```

### Certificate Metrics

```sql
SELECT 
  course_id,
  COUNT(*) as certificates_issued,
  SUM(CASE WHEN revoked THEN 1 ELSE 0 END) as revoked_count,
  SUM(CASE WHEN revoked THEN 0 ELSE 1 END) as valid_count,
  COUNT(DISTINCT revoked_reason) as unique_revocation_reasons
FROM certificates
GROUP BY course_id;
```

### Payment Analysis

```sql
-- Find payment anomalies
SELECT 
  e.student,
  e.course_id,
  e.amount_paid,
  e.platform_fee,
  e.instructor_fee,
  (e.platform_fee + e.instructor_fee) as sum_fees,
  CASE 
    WHEN (e.platform_fee + e.instructor_fee) != e.amount_paid THEN 'INVARIANT_VIOLATION'
    ELSE 'OK'
  END as status
FROM enrollments e
HAVING (e.platform_fee + e.instructor_fee) != e.amount_paid;
```

### Admin Activity Timeline

```sql
SELECT 
  ledger_sequence,
  action,
  actor,
  reason,
  created_at
FROM audit_log
WHERE action IN ('admin_transfer_proposed', 'admin_transfer_accepted', 'certificate_revoked')
ORDER BY ledger_sequence DESC
LIMIT 100;
```

## Error Handling

```javascript
async function processEvent(event) {
  try {
    const { eventType, primaryId } = decodeEventTopics(event.topics);
    const data = parseEventData(event.data);
    
    switch (eventType) {
      case 'student_enrolled':
        await handleStudentEnrolled({ ...data }, db);
        break;
      case 'certificate_issued':
        await handleCertificateIssued({ ...data }, db);
        break;
      case 'certificate_revoked':
        await handleCertificateRevoked({ ...data }, db);
        break;
      // ... other event types
      default:
        console.warn(`Unknown event type: ${eventType}`);
    }
    
    // Record successful processing
    await db.query(
      `INSERT INTO processed_events (ledger_seq, event_type, status) VALUES ($1, $2, $3)`,
      [event.ledger, eventType, 'success']
    );
    
  } catch (error) {
    console.error(`Error processing event: ${error.message}`);
    
    // Record failed processing
    await db.query(
      `INSERT INTO processed_events (ledger_seq, event_type, status, error) VALUES ($1, $2, $3, $4)`,
      [event.ledger, eventType, 'failed', error.message]
    );
    
    // Implement retry logic or alerting
    await alerting.sendAlert({
      severity: 'high',
      message: `Failed to process event at ledger ${event.ledger}`,
      error: error.message,
    });
  }
}
```

## Best Practices

### 1. Ledger Sequence Ordering
- Always process events in ascending ledger sequence order
- Use `ledger_seq` as the primary ordering key
- This ensures causality and enables timeline reconstruction

### 2. Idempotency
- Store processed event IDs to avoid duplicate processing
- Use `(ledger_seq, event_index)` as unique identifier
- Re-entrancy on the same event should be no-op

### 3. Invariant Validation
- Always verify payment splits: `platform_fee + instructor_fee == amount_paid`
- Validate ledger sequence progression
- Check for orphaned records (e.g., enrollment without corresponding course)

### 4. Data Consistency
- Use transactions when updating related records
- Example: updating enrollments must also update course stats atomically
- Implement foreign key constraints

### 5. Backfill & Recovery
- Store the last processed ledger sequence
- On restart, query events from (lastProcessed + 1)
- Implement full backfill capability for debugging

### 6. Monitoring
- Alert if event processing falls behind (>5 blocks behind current)
- Monitor for invariant violations
- Track processing errors and retry failures
- Dashboard for event processing metrics

## Performance Optimization

### Batch Processing
```javascript
async function batchProcessEvents(events, batchSize = 100) {
  for (let i = 0; i < events.length; i += batchSize) {
    const batch = events.slice(i, i + batchSize);
    
    // Process in parallel
    await Promise.all(batch.map(event => processEvent(event)));
  }
}
```

### Caching
```javascript
const courseCache = new Map();

async function getCourseInfo(courseId) {
  if (courseCache.has(courseId)) {
    return courseCache.get(courseId);
  }
  
  const course = await db.query('SELECT * FROM courses WHERE course_id = $1', [courseId]);
  courseCache.set(courseId, course);
  return course;
}
```

### Connection Pooling
```javascript
const pool = new Pool({
  user: 'postgres',
  password: process.env.DB_PASSWORD,
  host: 'localhost',
  port: 5432,
  database: 'hamplard',
  max: 20,  // Maximum connections
  idleTimeoutMillis: 30000,
  connectionTimeoutMillis: 2000,
});
```

## Testing Your Indexer

### Mock Events for Testing
```javascript
function createMockEnrollmentEvent(student, courseId, amount) {
  return {
    type: 'contract',
    ledger: 1000,
    topics: [
      Symbol('student_enrolled'),
      courseId,
    ],
    data: {
      student,
      courseId,
      amount_paid: amount,
      platform_fee: Math.floor(amount * 0.2),
      instructor_fee: Math.floor(amount * 0.8),
      ledger_seq: 1000,
    },
  };
}

async function testEnrollmentHandling() {
  const event = createMockEnrollmentEvent(
    'GAAAA...',
    'COURSE-001',
    1000000000
  );
  
  await handleStudentEnrolled(event.data, testDb);
  
  const enrollment = await testDb.query(
    'SELECT * FROM enrollments WHERE course_id = $1',
    ['COURSE-001']
  );
  
  assert(enrollment.amount_paid === 1000000000);
  assert(enrollment.platform_fee === 200000000);
}
```

## Support & Debugging

### Common Issues

**Issue**: Events not appearing
- **Solution**: Verify contract is emitting events with `env.events().publish()`
- Check RPC endpoint and contract ID

**Issue**: Payment split doesn't match**
- **Solution**: Verify percentage calculation: `platform_fee = (amount * pct) / 100`
- Check for rounding issues in integer division

**Issue**: Duplicate event processing**
- **Solution**: Implement idempotency check on `(ledger_seq, event_index)`
- Store processed event IDs

**Issue**: Falling behind on event processing**
- **Solution**: Batch process events
- Use connection pooling
- Implement pagination for large result sets

## Additional Resources

- Stellar Documentation: https://developers.stellar.org
- Soroban Events: https://developers.stellar.org/docs/learn/soroban/events
- RPC Spec: https://developers.stellar.org/docs/soroban/rpc


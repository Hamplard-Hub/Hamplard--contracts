# Hamplard — Contract Repo

> **On-chain course enrollment, payments, and certificate issuance on Stellar Soroban**

Hamplard is an online learning platform for practical vocational skills — tailoring, makeup artistry, baking, photography, hairstyling, nail technology, fashion design, and more. This Soroban smart contract handles the trustless financial and credential layer of the platform: course enrollment payments, on-chain instructor earnings, refunds, and verifiable certificates of completion.

## Short version

Hamplard Contracts is the Soroban smart contract layer for the Hamplard platform. It manages course lifecycle state, enrollment payments, pull-based instructor earnings, refunds, dispute fees, certificate issuance, and revocation in a trustless and auditable way.

The contract provides the core rules for:

- registering and approving courses
- pausing and unpausing courses
- archiving courses permanently
- enrolling students and splitting payments automatically
- marking enrollments complete
- issuing and revoking certificates
- emitting events for off-chain indexing and backend synchronization

### Main contract concepts

- Courses move through a lifecycle from Pending to Active, Paused, and Archived.
- Enrollments trigger automatic platform and instructor payment splits.
- Certificates can be issued after completion and later revoked with metadata captured on-chain.

### Key features

- multi-admin authorization for sensitive operations
- course status enforcement and lifecycle rules
- instructor earnings accounting, including co-instructor revenue shares
- refund requests with proportional debits across every revenue recipient
- certificate issuance, revocation challenges, and expiry tracking
- arbitration cases, course waitlists, and platform-level safety controls
- Soroban event emission for off-chain systems and indexers

The detailed documentation below expands on the architecture, data model, contract functions, and deployment flow.

This is **Repo 1 of 3** in the Hamplard project:

| Repo                               | Description                           |
| ---------------------------------- | ------------------------------------- |
| `hamplard-contract` ← you are here | Soroban smart contract (Rust)         |
| `hamplard-backend`                 | NestJS REST API + content management  |
| `hamplard-frontend`                | Next.js student and instructor portal |

---

## Table of Contents

- [What the Contract Does](#what-the-contract-does)
- [What Lives Off-Chain](#what-lives-off-chain)
- [Architecture](#architecture)
- [Data Structures](#data-structures)
- [Contract Functions](#contract-functions)
- [Revenue Split Logic](#revenue-split-logic)
- [Refunds](#refunds)
- [Certificate Verification](#certificate-verification)
- [Events](#events)
- [Error Codes](#error-codes)
- [Project Structure](#project-structure)
- [Prerequisites](#prerequisites)
- [Setup & Installation](#setup--installation)
- [Running Tests](#running-tests)
- [Building](#building)
- [Deploying to Testnet](#deploying-to-testnet)
- [Security Considerations](#security-considerations)
- [Roadmap](#roadmap)

---

## What the Contract Does

The Hamplard contract is the trust layer for the platform. It handles four responsibilities:

**1. Course Registry**
Instructors register courses on-chain with a price and fee structure. Each course starts as `Pending` — an admin must approve it before students can enroll. This gives the platform moderation control while payment rules remain enforced by the contract.

**2. Enrollment Payments**
When a student enrolls, the contract charges the course price and splits it in a single transaction:
- Platform fee → treasury address immediately
- Instructor share → credited to withdrawable instructor earnings (including co-instructor shares)

The instructor share stays in the contract until each recipient calls `withdraw_earnings()`, which is what lets refunds debit the exact balances that were credited. The contract records the enrollment so no student can enroll twice.

Enrollment is **price-protected**: `enroll()`, `batch_enroll()` and `re_enroll()` take a `max_price` argument, and the call reverts if the course's current price is higher. A student therefore can never be charged more than the price they saw when they signed, even if an instructor changes the price between the quote and execution.

**3. On-Chain Certificates**
After a student completes a course (verified off-chain by the backend), the admin issues an on-chain certificate. Each certificate is a permanent, verifiable record tied to the student's Stellar address and the course. Anyone can verify a certificate by querying `verify_certificate(certificate_id)` — no trusted middleman needed. Certificates can be revoked by the admin if issued in error, subject to a challenge window.

**4. Refunds**
Both admins can jointly issue refunds by archiving a course and specifying which students to refund. Refunds return the full enrollment amount to students with incomplete enrollments. This provides dispute resolution and quality control mechanisms without requiring a time window.

---

## What Lives Off-Chain

The contract intentionally stores only what is necessary for payments and verification. Everything else lives in the backend database:

| Off-chain (backend)    | On-chain (contract)                  |
| ---------------------- | ------------------------------------ |
| Video lessons          | Course ID, price, instructor address |
| Course descriptions    | Total enrollments, total earned      |
| Assignments            | Enrollment records                   |
| Student progress %     | Completion status                    |
| Downloadable resources | Certificate with course title        |
| User profiles          | —                                    |
| Instructor analytics   | —                                    |

This keeps the contract small, cheap to call, and focused on what blockchains are actually good at: verifiable ownership, trustless payments, and permanent records.

---

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                Hamplard Contract (Soroban)                    │
│                                                              │
│  ┌──────────────┐   ┌──────────────────┐   ┌─────────────┐  │
│  │   Course     │   │   Enrollment     │   │ Certificate │  │
│  │   Registry   │   │   + Payment      │   │   Issuance  │  │
│  │  (Pending →  │   │  Fee + earnings  │   │  (on-chain  │  │
│  │   Active)    │   │  credit          │   │   NFT-like) │  │
│  └──────────────┘   └──────────────────┘   └─────────────┘  │
└──────────────────────────────────────────────────────────────┘
         ↑                    ↑                    ↑
    Instructor           Student pays         Admin issues
    registers            at enroll           after backend
    course               (fee + credit)      verifies completion
```

### Roles

| Role | Address | Permissions |
|------|---------|-------------|
| **Admin** | Set at `init` | Single-admin configuration and moderation; co-signs every multi-sig operation |
| **Secondary Admin** | Set at `init` | Must co-sign multi-sig operations: archive, treasury update, admin transfer, token sweep, admin expiry, and upgrades |
| **Approver** | Granted by an admin | Approve or reject pending courses |
| **Instructor** | Set per course (`Course.instructor`) | Register courses, manage their own courses, withdraw earnings, co-sign course transfers |
| **Co-instructor** | `Course.co_instructors` | Passive recipient of a configured revenue share; no privileged entrypoints |
| **Student** | Any address | Enroll, request refunds, challenge revocations, escalate arbitration, join waitlists |
| **Treasury** | Set at `init` | Receives the platform fee share (passive recipient only) |

---

## Data Structures

> The listings below mirror the `#[contracttype]` definitions in
> [`contracts/hamplard/src/lib.rs`](contracts/hamplard/src/lib.rs), which is the
> authoritative interface. Regenerate client bindings from that source rather
> than copying these snippets by hand.

### `Course`

```rust
pub struct Course {
    pub id: String,                              // matches backend DB record ID
    pub instructor: Address,                     // lead instructor
    pub price: i128,                             // enrollment price in token stroops
    pub platform_fee_percent: u32,               // 0-100 snapshot; 0 follows live fee config
    pub token: Address,                          // approved Stellar Asset Contract
    pub total_enrollments: u32,                  // lifetime enrollments
    pub active_enrollments: u32,                 // enrolled but not completed/refunded
    pub total_earned: i128,                      // gross earnings, net of refunds
    pub total_refunded: i128,                    // cumulative amount refunded (process_refund + archive_course)
    pub refund_count: u32,                       // number of refunded enrollments
    pub status: CourseStatus,                    // Pending | Active | Paused | Rejected | Archived
    pub created_at_ledger: u32,
    pub max_capacity: Option<u32>,
    pub enrollment_start_ledger: Option<u32>,
    pub enrollment_expiry_ledgers: Option<u32>,
    pub min_completion_ledgers: u32,
    pub version: u32,                            // incremented on material updates
    pub last_updated_ledger: u32,
    pub expires_at_ledger: Option<u32>,
    pub content_hash: BytesN<32>,                // off-chain content commitment
    pub max_certificates: Option<u32>,
    pub certificates_issued: u32,
    pub prerequisite_course_ids: Vec<String>,
    pub archived_at_ledger: Option<u32>,
    pub categories: Vec<String>,
    pub co_instructors: Vec<CoInstructor>,
}
```

### `CoInstructor`

```rust
pub struct CoInstructor {
    pub address: Address,
    pub share_bps: u32,                          // 0-10_000 bps of the instructor share
}
```

Co-instructor shares are taken from the instructor portion of each enrollment
(after the platform fee). Shares must total at most 10 000 bps; any remainder
accrues to the lead instructor. The split is configured with
`set_co_instructors()` and locked once the course receives its first enrollment.

### `Enrollment`

```rust
pub struct Enrollment {
    pub student: Address,
    pub course_id: String,
    pub amount_paid: i128,
    pub enrolled_at_ledger: u32,
    pub completed: bool,
    pub certificate_issued: bool,
    pub certificate_id: Option<String>,
    pub evidence_hash: Option<BytesN<32>>,       // fixed-size completion evidence
    pub is_refunded: bool,
    pub course_version: u32,                     // course version at enrollment time
    pub platform_amount: i128,                   // platform share actually charged
    pub instructor_amount: i128,                 // instructor share actually credited
    pub enrollment_ref: String,                  // unique on-chain reference
    pub completed_at_ledger: Option<u32>,
}
```

### `Certificate`

```rust
pub struct Certificate {
    pub id: String,                              // unique certificate ID
    pub student: Address,
    pub course_id: String,
    pub course_title: String,
    pub enrollment_reference: String,
    pub instructor: Address,
    pub issued_by: Address,                      // contract deployment that issued it
    pub issued_at_ledger: u32,
    pub revoked: bool,
    pub revoked_by: Option<Address>,
    pub revocation_ledger: Option<u32>,
    pub revocation_reason: Option<String>,
    pub expires_at_ledger: Option<u32>,
    pub instructor_signature: Option<BytesN<64>>,
    pub revocation_deadline: Option<u32>,        // pending revocation is challengeable until this ledger
}
```

### Supporting types

| Type | Purpose |
|---|---|
| `RefundRequest` | Pending/approved/rejected refund request with optional approved amount |
| `FeeConfig` | Per-token platform fee in basis points |
| `RiskFeeConfig` / `RiskScore` | Risk-based surcharge configuration and computed score |
| `ArbitrationCase` | On-chain dispute record with fee, token, and resolution status |
| `TreasuryUpdate` | Pending treasury address plus its effective ledger |
| `PendingUpgrade` | Proposed Wasm hash and upgrade time-lock ledger |
| `InstructorStats` | Aggregate students, completions, and certificates per instructor |

### `CourseStatus` state machine

```
Pending ── approve_course() ──→ Active
Pending ── reject_course()  ──→ Rejected
Active  ── pause_course()   ──→ Paused
Paused  ── unpause_course() ──→ Active
Paused  ── archive_course() ──→ Archived (multi-sig, permanent)
Pending ── archive_course() ──→ Archived (no enrollments exist)
Rejected ─ archive_course() ──→ Archived (no enrollments exist)
```

Archiving requires the course to be in `Paused`, `Pending`, or `Rejected` status — an `Active` course cannot be archived directly. The operation also requires both admin addresses (multi-sig). `Archived` is a terminal state; no further transitions are possible.

---

## Contract Functions

### `init`

```rust
init(admin, secondary_admin, treasury, default_fee_pct,
     max_courses_per_instructor, refund_window_ledgers,
     revocation_challenge_period)
```

Initialises the contract once. The admin must sign.

### Course management

| Function | Signer | Description |
|---|---|---|
| `register_course(instructor, course_id, price, token, platform_fee_pct, max_capacity, content_hash, categories)` | Instructor | Register a course — starts as Pending |
| `approve_course(caller, course_id)` | Admin or Approver | Move Pending → Active; high-value courses need two distinct approvals |
| `reject_course(caller, course_id, reason)` | Admin or Approver | Move Pending → Rejected |
| `pause_course(caller, course_id)` / `unpause_course(caller, course_id)` | Lead instructor or Admin | Move Active ↔ Paused |
| `update_course(caller, course_id, new_price, new_max_capacity)` | Lead instructor or Admin | Update price/capacity and increment the course version |
| `set_co_instructors(caller, course_id, addresses, shares_bps)` | Lead instructor or Admin | Configure co-instructors before the first enrollment |
| `set_enrollment_start_ledger` / `set_enrollment_expiry` / `set_course_expiry` | Lead instructor or Admin | Configure enrollment windows |
| `set_prerequisite_courses` / `set_max_certificates` / `update_content_hash` | Lead instructor or Admin | Configure prerequisites, certificate cap, and content commitment |
| `archive_course(admin1, admin2, course_id, students_to_refund)` | Both admins | Permanently archive; optionally refund students |

### Enrollment and payments

### Enrollment, refunds, and earnings

| Function | Signer | Description |
|---|---|---|
| `enroll(student, course_id)` | Student | Pay course price; platform fee is forwarded and instructor earnings are credited |
| `batch_enroll(student, course_ids)` | Student | Enroll in up to 50 courses in one transaction |
| `re_enroll(student, course_id)` | Student | Re-enroll after a completed course, archiving the previous enrollment |
| `withdraw_earnings(instructor, token, amount, destination)` | Instructor | Withdraw accumulated earnings (`amount = 0` withdraws the full balance) |
| `request_refund(student, course_id)` | Student | Open a refund request within the configured window |
| `process_refund(admin, student, course_id, approved, refund_amount)` | Admin | Approve or reject a refund and debit each revenue recipient proportionally |

### Completion and certificates

| Function | Signer | Description |
|---|---|---|
| `mark_completed(admin, student, course_id, evidence_hash)` | Admin (+ student when `evidence_hash` is `None`) | Mark an enrollment completed |
| `issue_certificate(admin, student, course_id, certificate_id, course_title, enrollment_reference, expires_at_ledger, instructor_signature)` | Admin | Issue an on-chain certificate after completion |
| `revoke_certificate(admin, certificate_id, reason)` | Admin | Start a revocation; it becomes permanent after the challenge period |
| `challenge_revocation(student, certificate_id)` | Certificate holder | Cancel a pending revocation before its deadline |
| `bulk_revoke_course_certificates(admin, course_id)` | Admin | Revoke every certificate of a course |

### Arbitration, waitlists, and risk configuration

| Function | Signer | Description |
|---|---|---|
| `escalate_to_arbitration(caller, course_id)` | Student | Open an arbitration case and pay the configured fee |
| `resolve_arbitration(admin, caller, course_id, for_student)` | Admin | Resolve an open case for the student or instructor |
| `join_waitlist(student, course_id)` / `leave_waitlist(student, course_id)` | Student | Manage a capacity waitlist |
| `set_fee_config(admin, token, fee_bps)` | Admin | Per-token platform fee |
| `set_arbitration_fee_config(admin, fee_per_case)` | Admin | Arbitration fee |
| `set_risk_config_enabled(admin, enabled)` / `set_risk_fee_config(...)` | Admin | Risk-surcharge pricing |

### Governance, safety, and upgrades

| Function | Caller | Description |
|---|---|---|
| `transfer_admin(admin1, admin2, new_admin, new_secondary_admin)` | Both admins | Propose a new admin pair |
| `accept_admin(new_admin, new_secondary_admin)` | Both new admins | Accept the proposed admin pair |
| `update_treasury(admin1, admin2, new_treasury)` | Both admins | Schedule a treasury update |
| `set_admin_expiry(admin1, admin2, expires_at)` | Both admins | Set or clear the admin-role expiry ledger |
| `withdraw_tokens(admin1, admin2, token, amount, destination)` | Both admins | Emergency token sweep |
| `pause_platform(admin)` / `unpause_platform(admin)` | Admin | Halt or restore enrollments |
| `update_default_fee`, `update_max_courses_limit`, `update_refund_window` | Admin | Global parameters |
| `block_student` / `unblock_student`, `freeze_instructor` / `unfreeze_instructor` | Admin | Moderation controls |
| `add_approver` / `remove_approver`, `add_approved_token` / `remove_approved_token` | Admin | Manage approvers and accepted tokens |

### Read-only queries

| Function | Returns |
|---|---|
| `get_course(course_id)` | Full `Course` struct |
| `get_enrollment(caller, student, course_id)` | `Enrollment` (student, course instructor, or admin only) |
| `quote_enrollment(course_id, student)` | `(price, platform_fee, instructor_share)` using the current enrollment fee calculation |
| `get_certificate(caller, certificate_id)` | `Certificate` (student, course instructor, or admin only) |
| `is_enrolled(student, course_id)` | `bool` |
| `has_completed(student, course_id)` | `Option<bool>` |
| `verify_certificate(certificate_id)` | `bool` — true if it exists, is not permanently revoked, and has not expired |
| `get_platform_fee(admin)` | `u32` — current default fee percentage (admin only) |
| `get_instructor_earnings(instructor, token)` | `i128` — withdrawable balance |

> The full, generated interface includes many more configuration and query
> entrypoints. See the `#[contractimpl]` block in `lib.rs` for the exhaustive
> list and exact argument types.

---

## Revenue Split Logic

When a student calls `enroll()`, the contract charges the full course price, sends
the platform share to the treasury, and credits the instructor share to
withdrawable balances inside the contract:

```
price = 100 USDC
platform fee = 20%

platform_amount   = 100 × 20 / 100 = 20 USDC → treasury
instructor_amount = 100 - 20       = 80 USDC → credited to instructor earnings
```

The instructor share is not pushed to the instructor at enrollment time. It is
recorded in `InstructorEarnings(instructor, token)` and paid out when the
instructor calls `withdraw_earnings()`. This pull-based model lets a later
refund debit the same balance that was credited earlier.

If the course has co-instructors, the instructor share is split by their
configured basis-point shares (configured with `set_co_instructors()` before the
first enrollment). Any remainder goes to the lead instructor:

```
instructor_amount = 80 USDC
co-instructor A = 30% of 80 = 24 USDC
co-instructor B = 20% of 80 = 16 USDC
lead instructor = 80 - 24 - 16 = 40 USDC
```

Refunds debit the lead instructor and every co-instructor in the same
proportions, so no recipient keeps revenue for an enrollment that was refunded.

The platform fee percentage can be:

- **Global default** (set at `init`, updatable by admin)
- **Per-token configuration** via `set_fee_config(token, fee_bps)`
- **Per-course override** captured at registration as a floor on the live fee

`quote_enrollment(course_id, student)` returns the current price and exact split that enrollment will use, including the course's fee override and applicable risk settings.

---

## Refunds

Hamplard supports **admin-initiated refunds** through course archival. This is the only refund mechanism in the contract.

### How Refunds Work

When both admins call `archive_course()` with a `students_to_refund` list, the contract refunds each listed student their full enrollment payment:

```
Original enrollment: 100 USDC
platform_fee_percent: 20

Platform fee (from treasury):     20 USDC → student
Instructor share (from instructor): 80 USDC → student
Total refunded:                  100 USDC
```

Refunds are only applied to **incomplete** enrollments (where `completed == false`). Students who have already completed the course are not refunded.

### Refund Window

There is no time-based refund window. Refunds can be initiated at any point:
- During the course (if students want to drop out)
- After the course ends (if issues with content or instruction)
- The course must be **paused first**, then archived with the refund list

This gives admins maximum flexibility to handle dispute resolution and quality control issues.

### Multi-Admin Authorization

**Both admins must sign the `archive_course()` transaction.** This is a safety mechanism that prevents a single compromised or rogue admin from unilaterally draining instructor funds through unauthorized refunds.

The two-signature requirement ensures:
- Refunds are intentional, not accidental
- Bad actors cannot act alone
- The platform maintains instructor trust

### Example Refund Scenario

1. Course `COURSE-BAKING-101` has 5 enrolled students, 3 completed
2. Instructor disappears mid-course; students complain
3. Both admins agree to refund the 2 incomplete students
4. Admin calls:
   ```
   pause_course(COURSE-BAKING-101)
   archive_course(admin1, admin2, COURSE-BAKING-101, [student_a, student_b])
   ```
5. Students A and B each receive their full 100 USDC back
6. The 3 completed students keep their certificates (already issued) but their enrollments are deleted

---

## Certificate Verification

Any third party — an employer, a client, a government agency — can verify a Hamplard certificate without trusting the platform:

```bash
stellar contract invoke \
  --id <CONTRACT_ID> \
  --network testnet \
  -- verify_certificate \
  --certificate_id "CERT-12345-TAILORING"
```

Returns `true` if the certificate exists, has not expired, and has not been permanently revoked. A certificate with a pending revocation remains verifiable until its challenge deadline passes. The `get_certificate()` function returns the full record including the student's Stellar address, course title, instructor, and issuance ledger — all permanently recorded and independently auditable.

---

## Events

| Event name                    | Payload                                     | When                                      |
| ----------------------------- | ------------------------------------------- | ----------------------------------------- |
| `course_registered`           | `course_id`                                 | Instructor registers a course             |
| `course_approved`             | `course_id`                                 | Admin approves a course                   |
| `course_paused`               | `course_id`                                 | Course paused                             |
| `course_unpaused`             | `course_id`                                 | Course unpaused                           |
| `course_archived`             | `course_id`                                 | Course archived                           |
| `student_enrolled`            | `(course_id, student, amount_paid)`         | Student enrolls and pays                  |
| `course_completed`            | `(course_id, student)`                      | Completion marked by admin                |
| `certificate_issued`          | `(certificate_id, student, course_id)`      | Certificate issued                        |
| `certificate_revoked`         | `certificate_id`                            | Certificate revoked                       |
| `course_certificates_revoked` | `(admin, course_id, revoked_count, ledger)` | All certificates of a course bulk-revoked |
| `admin_transferred`           | `new_admin`                                 | Admin role transferred                    |

See `contracts/hamplard/src/lib.rs` for the complete event list, including waitlist, freeze, blocklist, approver, and configuration events.

---

## Error Codes

Every contract failure is reported as a stable numeric `Error` code. Soroban surfaces these as:

```
HostError: Error(Contract, #N)
```

Clients should match on `N`; the numeric values are part of the public interface and do not change once assigned.

| Code | Name | Meaning |
|---:|---|---|
| 1 | `AlreadyInitialized` | `init` has already been called |
| 2 | `Unauthorized` | Caller is missing the required role or authorization |
| 3 | `AdminExpired` | The admin role has reached its configured expiry |
| 4 | `InvalidAdminConfig` | Invalid admin pair, proposal, or transfer state |
| 5 | `InvalidFeeConfig` | Fee rate, bps value, or arbitration fee is invalid |
| 6 | `InvalidTreasury` | Treasury address is invalid or not configured |
| 7 | `CourseNotFound` | No active or archived course exists for the ID |
| 8 | `InstructorFrozen` | The instructor is frozen |
| 9 | `InstructorState` | Invalid freeze/unfreeze state transition |
| 10 | `ArchivedCourseImmutable` | An archived course cannot be modified |
| 11 | `InvalidCourseInput` | Malformed course ID, capacity, categories, or hash |
| 12 | `InvalidPrice` | Price is outside the allowed precision range |
| 13 | `CourseAlreadyRegistered` | Course ID is already in use |
| 14 | `CourseIdInCooldown` | Archived course ID is still in its re-registration cooldown |
| 15 | `InstructorCourseLimitReached` | Instructor has reached the course limit |
| 16 | `GovernanceWindowNotElapsed` | Required approval/review delay has not elapsed |
| 17 | `CourseNotPending` | Operation requires a pending course |
| 18 | `HighValueApprovalConflict` | The same approver cannot provide both high-value approvals |
| 19 | `CounterUnderflow` | A counter would be decremented below zero |
| 20 | `CourseNotActive` | Operation requires an active course |
| 21 | `CourseNotPaused` | Operation requires a paused course |
| 22 | `InvalidCourseTransfer` | Invalid course ownership transfer |
| 23 | `CourseMustBePausedToArchive` | Active courses must be paused before archiving |
| 24 | `TooManyRefundStudents` | Refund list exceeds the maximum size |
| 25 | `ArithmeticOverflow` | Checked arithmetic overflow |
| 26 | `InvalidPrerequisite` | Prerequisite is invalid, missing, or incomplete |
| 27 | `EmptyBatch` | Batch enrollment list is empty |
| 28 | `BatchTooLarge` | Batch or list exceeds the maximum size |
| 29 | `DuplicateCourseInBatch` | Batch contains duplicate course IDs |
| 30 | `PlatformPaused` | Platform enrollment is paused or pause state is invalid |
| 31 | `StudentBlocked` | Student is blocked or block state is invalid |
| 32 | `EntryRestricted` | Address is not allowed to enroll or join a waitlist |
| 33 | `InstructorReferenceInvalid` | Invalid instructor reference |
| 34 | `CourseNotAvailable` | Course is not enrollable (status, timing, capacity, token, or cap) |
| 35 | `AlreadyEnrolled` | Student is already enrolled |
| 36 | `CircuitBreakerTripped` | Repeated payment failures auto-paused the course |
| 37 | `TransferAmountMismatch` | Token transfer delivered an unexpected amount |
| 38 | `EnrollmentNotFound` | No enrollment exists for the student/course pair |
| 39 | `EnrollmentStateError` | Completion or enrollment-state transition is invalid |
| 40 | `InvalidWithdrawal` | Withdrawal amount, destination, or balance is invalid |
| 41 | `InvalidCertificateInput` | Malformed certificate field |
| 42 | `CertificateAlreadyIssued` | Certificate already exists or was already issued |
| 43 | `MaxCertificatesReached` | Course certificate cap reached |
| 44 | `CertificateError` | Revocation, challenge, or certificate lookup failed |
| 45 | `RefundError` | Refund window, request state, or amount is invalid |
| 46 | `ApproverState` | Approver add/remove operation conflicts with current state |
| 47 | `ArbitrationError` | Arbitration config or case state is invalid |
| 48 | `WaitlistError` | Waitlist operation is invalid |
| 49 | `UpgradeError` | Upgrade proposal, timelock, or execution state is invalid |
| 50 | `InconsistentState` | Internal invariant or reserved-balance check failed |

---

## Project Structure

```
hamplard-contract/
├── Cargo.toml                         ← Rust workspace config
├── Cargo.lock
├── .gitignore
├── README.md
└── contracts/
    └── hamplard/
        ├── Cargo.toml                 ← Contract package config
        ├── Makefile                   ← Build / deploy shortcuts
        └── src/
            ├── lib.rs                 ← Full contract logic
            └── test.rs                ← unit and integration tests
```

---

## Prerequisites

```bash
# 1. Install rustup (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 2. The correct Rust toolchain and wasm32v1-none target are declared in
#    rust-toolchain.toml at the repo root. rustup picks them up automatically
#    on the first cargo command — no manual rustup target add needed.
#
#    Pinned toolchain: 1.96.0  (builds the locked soroban-sdk =22.0.11 dependency tree)
#    Target:           wasm32v1-none

# 3. Stellar CLI
cargo install --locked stellar-cli --features opt

# 4. Testnet account
stellar keys generate --global my-account --network testnet
stellar keys fund my-account --network testnet
```

> **Toolchain pinning** — `rust-toolchain.toml` pins Rust to `1.96.0` and
> `soroban-sdk` is pinned to `=22.0.11` in `Cargo.toml`. Together with
> `Cargo.lock` this guarantees identical builds across machines and CI.
> When upgrading the SDK, update both files together.

---

## Setup & Installation

```bash
git clone https://github.com/Hamplard-Hub/Hamplard--contracts.git
cd Hamplard--contracts
cargo check
```

---

## Running Tests

```bash
cargo test
cargo test -- --nocapture          # with output
cargo test test_full_lifecycle     # single test
```

Expected: the full suite passes. `cargo test` prints one `ok` line per test
and a final `test result: ok.` summary (300+ tests).

---

## Building

The Makefile lives in `contracts/hamplard`:

```bash
cd contracts/hamplard
make build      # → target/wasm32v1-none/release/hamplard.wasm
make optimize   # → target/wasm32v1-none/release/hamplard.optimized.wasm
```

---

## Deploying to Testnet

```bash
cd contracts/hamplard
export STELLAR_ACCOUNT=my-account
export ADMIN_ADDRESS=<ADMIN_ADDRESS>
export SECONDARY_ADMIN_ADDRESS=<SECONDARY_ADMIN_ADDRESS>
export TREASURY_ADDRESS=<TREASURY_ADDRESS>
export DEFAULT_FEE_PCT=20
export MAX_COURSES_PER_INSTRUCTOR=10
export REFUND_WINDOW_LEDGERS=17280
export REVOCATION_CHALLENGE_PERIOD=17280
make -C contracts/hamplard deploy-and-init-testnet
# Prints the initialized contract ID and configuration
```

### Initialize after deployment

Run the following from `contracts/hamplard`:

```bash
stellar contract invoke \
  --id <CONTRACT_ID> \
  --source my-account \
  --network testnet \
  -- init \
  --admin <ADMIN_ADDRESS> \
  --secondary_admin <SECONDARY_ADMIN_ADDRESS> \
  --treasury <TREASURY_ADDRESS> \
  --default_fee_pct 20 \
  --max_courses_per_instructor 50 \
  --refund_window_ledgers 17280 \
  --revocation_challenge_period 17280
```

### Register and approve a test course

```bash
# Register
stellar contract invoke \
  --id <CONTRACT_ID> \
  --source instructor-account \
  --network testnet \
  -- register_course \
  --instructor <INSTRUCTOR_ADDRESS> \
  --course_id "COURSE-TAILORING-001" \
  --price 500000000 \
  --token <USDC_SAC_ADDRESS> \
  --platform_fee_pct 0 \
  --max_capacity 100 \
  --content_hash <32_BYTE_HEX> \
  --categories '["tailoring"]'

# Approve
stellar contract invoke \
  --id <CONTRACT_ID> \
  --source my-account \
  --network testnet \
  -- approve_course \
  --caller <ADMIN_ADDRESS> \
  --course_id "COURSE-TAILORING-001"
```

> **USDC SAC on Testnet:** `CBIELTK6YBZJU5UP2WWQEUCYKLPU6AUNZ2BQ4WWFEIE3USCIHMXQDAMA`

---

## Security Considerations

- **Authorization**: Every write function calls `require_auth()` on the relevant party. Instructors cannot approve their own courses. Students cannot issue their own certificates. Only admins can revoke certificates.
- **Dual-admin model**: The contract is initialised with two distinct admin addresses — `admin` and `secondary_admin` — so there is no single-address point of failure. Sensitive operations require **both** admin signatures (`require_multi_admin`): `archive_course`, `withdraw_tokens`, `update_treasury`, `transfer_admin`, `set_admin_expiry`, and the contract upgrade functions (`propose_upgrade`, `cancel_upgrade`, `upgrade_contract`, `set_upgrade_timelock`). Routine operations (approving courses, issuing certificates, fee configuration) need only one admin.
- **Two-step admin transfer with cooldown**: `transfer_admin(admin1, admin2, new_admin, new_secondary_admin)` only *proposes* the new pair; the new admins must call `accept_admin` to complete the rotation. A minimum cooldown between consecutive transfers prevents rapid re-rotation during incident response. `init` also enforces that admin, secondary admin, and treasury are all distinct addresses.
- **Admin expiry**: `set_admin_expiry(admin1, admin2, expires_at)` sets a ledger sequence after which every admin operation panics with "admin role has expired" — a dead-man's switch that can be read via `get_admin_expiry()` and cleared by passing `None`.
- **Scoped approvers**: Admins can delegate course moderation via `add_approver` / `remove_approver`. Approvers may approve and reject courses, but have no access to treasury, fee, or admin-transfer functions — and unlike the admin role, approver authority does not expire.
- **Instructor freezes**: `freeze_instructor` blocks an instructor and automatically pauses all of their Active courses; `unfreeze_instructor` reverses it. Frozen instructors cannot operate while the platform keeps student funds safe.
- **Student blocklist**: `block_student` bans an address from enrolling in any course; `unblock_student` lifts the ban.
- **No double enrollment**: The contract checks for an existing enrollment record before processing payment. A student cannot be charged twice for the same course.
- **Price protection**: `enroll()`, `batch_enroll()` and `re_enroll()` require a `max_price` and revert if the course's current price exceeds it, so a price change between a student's quote and execution cannot charge them more than they approved.
- **Atomic payment split**: Both the platform and instructor transfers happen in the same transaction. There is no intermediate escrow that could be exploited.
- **Certificate immutability**: Revoked certificates are flagged — not deleted. This preserves the audit trail and prevents a revocation from being hidden. Revocation is permanent only after the challenge period elapses.
- **Fee validation**: Platform fee percentage is validated to be ≤ 100 at both `init` and `register_course` time.
- **Event schema stability**: Event topics and data layouts are part of the public interface. Off-chain indexers must be updated in lockstep with any contract change to the events table above.

---

## Roadmap

- [x] Course registry with approval and rejection workflow
- [x] Enrollment payment with platform fee + instructor earnings credit
- [x] Instructor earnings withdrawal tracking
- [x] Batch enrollment and re-enrollment
- [x] On-chain certificate issuance, revocation, and challenge window
- [x] Course pause/unpause and permanent archive
- [x] Refund requests with admin approval
- [x] Waitlists, prerequisites, capacity limits, and enrollment windows
- [x] Arbitration fee configuration and per-token fee reserves
- [x] Two-admin governance, approvers, admin expiry, blocklist, and instructor freeze
- [x] Time-locked contract upgrade mechanism
- [x] 313 unit tests
- [ ] Discount codes / coupon system (Soroban-based)
- [ ] Subscription enrollment model (monthly access)
- [ ] Co-instructor revenue shares and payouts
- [ ] Mainnet deployment

---

## License

MIT

> **Note:** The events table above is the source of truth for off-chain indexers. When adding or changing an event in `contracts/hamplard/src/lib.rs`, update this table in the same PR.

//! # Hamplard — On-chain course enrollment, payments, and certificates
//!
//! Hamplard is a Soroban contract for course registration, enrollment payments,
//! certificates, and refund processing. Course records and payment state live
//! on-chain; course content and student progress remain off-chain.
//!
//! ## Roles and authorization
//!
//! - **Admins:** administer courses, platform configuration, certificates,
//!   refunds, treasury updates, and upgrades. Operations marked as multi-sig
//!   require both the primary and secondary admins.
//! - **Approvers and moderators:** perform the approval and moderation actions
//!   granted to them; they cannot use admin-only fund or configuration methods.
//! - **Instructors:** register and manage their courses and withdraw their
//!   accumulated earnings.
//! - **Students:** enroll, request or cancel refunds, claim eligible refunds,
//!   and use other student self-service methods.
//! - **Treasury:** receives platform fees and is otherwise a passive recipient.
//!
//! ## Enrollment payments and instructor earnings
//!
//! On enrollment, the contract transfers the course price from the student,
//! sends the platform-fee share to the effective treasury, and credits the
//! remaining instructor share to on-chain instructor earnings. Instructor
//! revenue is pull-based: an instructor must call `withdraw_earnings` or
//! `withdraw_all_earnings` to transfer accumulated earnings. Co-instructor
//! shares are recorded and accounted for from that instructor portion.
//!
//! ## Refund lifecycle
//!
//! Refunds have three related paths:
//!
//! 1. `request_refund` lets an eligible student create a pending request
//!    within the configured refund window.
//! 2. `process_refund` lets an admin approve or reject a pending request.
//!    Approval applies the refund; rejection moves the request to `Rejected`
//!    without moving funds.
//! 3. `claim_refund` lets the requesting student approve their own pending
//!    request after its recorded processing deadline has elapsed.
//!
//! `archive_course` is a separate multi-admin operation. When archiving a
//! paused course, admins may provide a list of incomplete enrollments to refund.
//! It can therefore issue refunds without a preceding request, while the
//! request-based paths remain available for ordinary student refunds. Refunds
//! use the enrollment's recorded payment split; the platform portion is funded
//! from the refund reserve and the instructor portion from contract-held
//! instructor earnings. Completed enrollments are not refundable.
//!
//! ## Treasury updates
//!
//! `update_treasury` schedules a new treasury address, which becomes effective
//! after `TREASURY_UPDATE_DELAY_LEDGERS` (17,280 ledgers, approximately one
//! day at five seconds per ledger). `finalize_treasury_update` is
//! permissionless and applies a matured update on-chain.
//!
//! ## Contract metadata
//!
//! The deployed contract includes its version and this repository URL in
//! `contractmeta!` metadata so operators can identify the source of a Wasm
//! build.
//!
//! ## Public limitations
//!
//! Course content delivery, course quality, token price risk, and other
//! off-chain concerns are not enforced by this contract.
#![allow(clippy::too_many_arguments)]
#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, token, Address, Bytes, BytesN, Env,
    IntoVal, Map, String, Symbol, Val, Vec,
};

/// Stable contract error codes returned by every failure path.
///
/// Codes are part of the public interface: clients and indexers should
/// match on the numeric value, which never changes once assigned.
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    Unauthorized = 2,
    AdminExpired = 3,
    InvalidAdminConfig = 4,
    InvalidFeeConfig = 5,
    InvalidTreasury = 6,
    CourseNotFound = 7,
    InstructorFrozen = 8,
    InstructorState = 9,
    ArchivedCourseImmutable = 10,
    InvalidCourseInput = 11,
    InvalidPrice = 12,
    CourseAlreadyRegistered = 13,
    CourseIdInCooldown = 14,
    InstructorCourseLimitReached = 15,
    GovernanceWindowNotElapsed = 16,
    CourseNotPending = 17,
    HighValueApprovalConflict = 18,
    CounterUnderflow = 19,
    CourseNotActive = 20,
    CourseNotPaused = 21,
    InvalidCourseTransfer = 22,
    CourseMustBePausedToArchive = 23,
    TooManyRefundStudents = 24,
    ArithmeticOverflow = 25,
    InvalidPrerequisite = 26,
    EmptyBatch = 27,
    BatchTooLarge = 28,
    DuplicateCourseInBatch = 29,
    PlatformPaused = 30,
    StudentBlocked = 31,
    EntryRestricted = 32,
    InstructorReferenceInvalid = 33,
    CourseNotAvailable = 34,
    AlreadyEnrolled = 35,
    CircuitBreakerTripped = 36,
    TransferAmountMismatch = 37,
    EnrollmentNotFound = 38,
    EnrollmentStateError = 39,
    InvalidWithdrawal = 40,
    InvalidCertificateInput = 41,
    CertificateAlreadyIssued = 42,
    MaxCertificatesReached = 43,
    CertificateError = 44,
    RefundError = 45,
    ApproverState = 46,
    ArbitrationError = 47,
    WaitlistError = 48,
    UpgradeError = 49,
    InconsistentState = 50,
    InvalidMaxCoursesLimit = 51,
    /// set_platform_enrollment_cap() received a cap below the current
    /// TotalActiveEnrollments count. Lowering the cap below the live count
    /// would prevent all new enrollments without expiring existing ones.
    InvalidCapBelowActiveEnrollments = 52,
    /// update_refund_window() or init() received a refund_window_ledgers value
    /// that exceeds MAX_REFUND_WINDOW_LEDGERS.
    InvalidRefundWindow = 53,
    /// An approved token reported a precision other than 7 decimals (#705).
    InvalidTokenDecimals = 54,
}

impl From<Error> for soroban_sdk::Error {
    fn from(e: Error) -> Self {
        soroban_sdk::Error::from_contract_error(e as u32)
    }
}

// Embed version and source metadata into the deployed Wasm so operators can
// identify which build is live after an upgrade.
soroban_sdk::contractmeta!(key = "version", val = "0.1.0");
soroban_sdk::contractmeta!(key = "repo", val = "https://github.com/Hamplard-Hub/Hamplard--contracts");

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum HamplardError {
    CourseNotActive = 1,
    TokenNotApproved = 2,
}
/// Number of ledgers to extend the instance TTL by on user activity.
const INSTANCE_TTL_EXTEND: u32 = 100_000;
/// Threshold below which the instance TTL is extended.
const INSTANCE_TTL_THRESHOLD: u32 = 50_000;

// ============================================================
// FEE & RISK DATA TYPES
// ============================================================

/// Per-token fee configuration — allows the admin to configure different
/// platform fee rates for different approved tokens.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct FeeConfig {
    /// Basis points (0-10000) charged as platform fee for this token.
    /// E.g. 2000 = 20%, 500 = 5%, 0 = free.
    pub fee_bps: u32,
}

/// Configuration for arbitration fee per case.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ArbitrationFeeConfig {
    /// Minimum fee required to escalate a dispute to arbitration,
    /// denominated in the settlement token's stroops.
    pub fee_per_case: i128,
}

/// Composite storage key for an arbitration case (caller + course_id).
/// Used as a direct storage key to avoid exceeding DataKey's 50-variant XDR limit.
#[contracttype]
#[derive(Clone, Debug)]
pub struct ArbitrationCaseKey {
    pub caller: Address,
    pub course_id: String,
}

/// The outcome / lifecycle status of an arbitration case.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum DisputeStatus {
    /// Awaiting admin resolution.
    Open,
    /// Admin ruled in favour of the student (fee refunded to caller).
    ResolvedForStudent,
    /// Admin ruled in favour of the instructor (fee paid to instructor).
    ResolvedForInstructor,
}

/// A persistent record of an arbitration case created by `escalate_to_arbitration`.
#[contracttype]
#[derive(Clone, Debug)]
pub struct ArbitrationCase {
    /// Unique case identifier (student address + course_id).
    pub case_id: String,
    /// The student / caller who escalated the dispute.
    pub caller: Address,
    /// The course this dispute concerns.
    pub course_id: String,
    /// Instructor of the course at the time the case was opened.
    pub instructor: Address,
    /// Token used for the course (arbitration fee was collected in this token).
    pub token: Address,
    /// Arbitration fee collected at escalation time.
    pub fee_paid: i128,
    /// Ledger sequence when the case was opened.
    pub opened_at_ledger: u32,
    /// Current status of the case.
    pub status: DisputeStatus,
}

/// Configuration for risk-based fee surcharges.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RiskFeeConfig {
    /// Extra basis points added for large payments above `large_payment_threshold`
    pub large_payment_surcharge_bps: u32,
    /// Threshold in stroops above which a payment is considered "large"
    pub large_payment_threshold: i128,
    /// Extra basis points added for new customers (first enrollment)
    pub new_customer_surcharge_bps: u32,
    /// Extra basis points for BTC/ETH currency (higher volatility)
    pub btc_eth_surcharge_bps: u32,
}

/// A computed risk score with the associated surcharge.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RiskScore {
    /// The computed risk score (0-100, arbitrary scale)
    pub score: u32,
    /// Total surcharge basis points to add to base fee
    pub surcharge_bps: u32,
}

/// Emitted when a risk-adjusted fee is applied to a payment.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RiskFeeApplied {
    pub payment_amount: i128,
    pub base_fee_bps: u32,
    pub risk_surcharge_bps: u32,
    pub effective_fee_bps: u32,
    pub platform_fee: i128,
}

/// Emitted when an enrollment is rejected due to course status.
/// Includes course_id, student address, course status, and ledger sequence
/// so rejection reasons are auditable off-chain.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct EnrollmentRejected {
    pub course_id: String,
    pub student: Address,
    pub status: CourseStatus,
    pub ledger_sequence: u32,
}

/// Emitted when an enrollment payment fails, along with the scoped reason code.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct EnrollmentFailed {
    pub course_id: String,
    pub student: Address,
    pub reason_code: String,
    pub ledger_sequence: u32,
}

// ============================================================
// DATA TYPES
// ============================================================

/// A co-instructor of a course with a revenue share in basis points.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct CoInstructor {
    /// Address of the co-instructor.
    pub address: Address,
    /// Revenue share in basis points (0-10000).
    pub share_bps: u32,
}

/// The status of a course listing
#[contracttype]
#[derive(Clone, PartialEq, Debug)]
pub enum CourseStatus {
    /// Submitted by instructor — awaiting admin approval
    Pending,
    /// Approved by admin — visible and enrollable
    Active,
    /// Paused by instructor or admin — not enrollable
    Paused,
    /// Permanently removed from the platform
    Archived,
    /// Rejected by admin — not approved for the platform
    Rejected,
}

/// Controls how `max_capacity` is enforced for a course.
///
/// - `Lifetime`   — the default; `max_capacity` caps the all-time total number
///   of enrollments (existing behaviour, including completed and refunded seats).
/// - `Concurrent` — `max_capacity` caps the number of *active* (i.e. enrolled
///   but not yet completed or refunded) seats at any one time.  A seat is freed
///   when `mark_completed()` or a refund path decrements `active_enrollments`,
///   allowing a new student to take that seat.
#[contracttype]
#[derive(Clone, PartialEq, Debug)]
pub enum CapacityMode {
    /// Cap total lifetime enrollments (default).
    Lifetime,
    /// Cap concurrent active seats — seats freed on completion or refund.
    Concurrent,
}

/// Identifies which role or system state paused a course.
#[contracttype]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PausedBy {
    None,
    Instructor,
    Admin,
    Moderator,
    /// Automatically paused while the instructor is frozen.
    InstructorFreeze,
}

/// A course listing stored on-chain
/// Full content (videos, materials, descriptions) lives off-chain in the backend.
/// The contract stores only what is needed to enforce payments and certificates.
#[contracttype]
#[derive(Clone)]
pub struct Course {
    /// Unique course ID — must match the backend DB record
    pub id: String,
    /// Instructor's Stellar address — receives their revenue share
    pub instructor: Address,
    /// USDC price per enrollment (in stroops, 7 decimal places).
    /// Must be either exactly 0 (free course) or within
    /// `[MIN_COURSE_PRICE_STROOPS, MAX_COURSE_PRICE_STROOPS]`
    /// (0.01 USDC to 100,000 USDC) — enforced at registration to catch
    /// prices accidentally entered in the wrong unit.
    pub price: i128,
    /// Platform fee percentage (0-100). Remainder goes to instructor.
    /// e.g. platform_fee_percent = 20 → instructor gets 80%
    pub platform_fee_percent: u32,
    /// USDC token contract address (Stellar Asset Contract)
    pub token: Address,
    /// Total number of non-refunded enrollments (incremented on each enroll
    /// and re-enroll, decremented when an enrollment is refunded). `max_capacity`
    /// is a lifetime cap on this counter, so a refund releases a seat.
    pub total_enrollments: u32,
    /// Total active enrollments (enrolled but not completed)
    pub active_enrollments: u32,
    /// Total USDC earned across all enrollments, net of refunds (in stroops).
    /// Incremented by `course.price` on enrollment and decremented by the
    /// refunded `amount_paid` on every refund path (process_refund, archive_course).
    pub total_earned: i128,
    /// Cumulative USDC refunded to students for this course (in stroops).
    /// Incremented on every refund path: by the actual refunded amount in
    /// `process_refund` (partial refunds count only what was paid back) and by
    /// `platform_amount + instructor_amount` per student in `archive_course`.
    /// Combined with `refund_count` this gives an on-chain refund rate.
    ///
    /// Storage layout: added after earlier deployments. Courses persisted
    /// before this field existed lack the key and must be migrated.
    pub total_refunded: i128,
    /// Number of enrollments refunded for this course (one per refunded
    /// enrollment, via either `process_refund` or `archive_course`).
    /// See `total_refunded` for the storage layout note.
    pub refund_count: u32,
    /// Course status
    pub status: CourseStatus,
    /// Ledger sequence when the course was registered
    pub created_at_ledger: u32,
    pub max_capacity: Option<u32>,
    /// Whether `max_capacity` is enforced as a lifetime total or a concurrent
    /// active-seat cap. Defaults to `CapacityMode::Lifetime` so existing
    /// behaviour is unchanged when `capacity_mode` is not explicitly set.
    pub capacity_mode: CapacityMode,
    /// Optional ledger sequence before which new enrollments are rejected.
    /// Allows instructors/admins to create a grace period after approval before
    /// student intake begins.
    pub enrollment_start_ledger: Option<u32>,
    /// Optional enrollment expiry duration in ledger sequences.
    /// If set, an enrollment is considered inactive (expired) after
    /// `enrolled_at_ledger + enrollment_expiry_ledgers` ledgers.
    /// Expired enrollments cannot be marked completed and `has_completed()`
    /// returns `Some(false)` for them.
    ///
    /// Expiry alone does not remove the enrollment record or free a
    /// `max_capacity` slot: capacity is checked against `total_enrollments`.
    /// A student with an expired enrollment can start a new attempt through
    /// `re_enroll()`, which archives the expired record.
    pub enrollment_expiry_ledgers: Option<u32>,
    /// Minimum ledger sequences that must elapse between a student's
    /// enroll() and mark_completed() for this course. Captured from the
    /// platform's `DefaultMinCompletionLedgers` at registration time.
    pub min_completion_ledgers: u32,
    /// Incremental version counter tracking course metadata updates
    pub version: u32,
    /// Ledger sequence when the course was last updated
    pub last_updated_ledger: u32,
    /// Optional ledger sequence when the course expires (no further enrollments)
    pub expires_at_ledger: Option<u32>,
    /// SHA-256 (or equivalent 32-byte) hash of the off-chain course content
    /// (syllabus, video manifest, etc.) at registration time.
    /// Allows students and auditors to verify that off-chain materials have not
    /// been silently changed after enrollment by comparing against the stored
    /// commitment. Updated only by the instructor or admin via
    /// `update_content_hash()`.
    pub content_hash: BytesN<32>,
    /// Optional maximum number of certificates that can be issued for this course.
    /// When set, the cap applies to *live* certificates: `certificates_issued - certificates_revoked`.
    /// Revoking a certificate therefore frees one slot so it can be reissued to another student.
    pub max_certificates: Option<u32>,
    /// Gross number of certificates ever issued for this course (never decremented).
    pub certificates_issued: u32,
    /// Number of those certificates that have since been revoked (via `revoke_certificate`
    /// or `bulk_revoke_course_certificates`). The live (net-active) count is
    /// `certificates_issued - certificates_revoked`.
    pub certificates_revoked: u32,
    /// Course IDs that must be completed (with an issued, non-revoked
    /// certificate) before a student may enroll in this course. Empty
    /// means no prerequisites. Configured via `set_prerequisite_courses`.
    pub prerequisite_course_ids: Vec<String>,
    /// Ledger sequence when the course was archived. Used to enforce
    /// a cooldown period before the same course ID can be re-registered.
    /// This prevents confusion where a new course inherits the identity
    /// of an archived one, potentially misleading students with historical
    /// enrollment records.
    pub archived_at_ledger: Option<u32>,
    /// Category tags for the course (e.g. "programming", "design") (#165)
    pub categories: Vec<String>,
    /// Co-instructors and their revenue shares in basis points.
    /// Shares are taken from the instructor portion of each enrollment and
    /// must total at most 10 000 bps; any remainder goes to the lead
    /// instructor. Configured via `set_co_instructors()` and locked once the
    /// course has received its first enrollment so the split cannot be
    /// changed retroactively.
    pub co_instructors: Vec<CoInstructor>,
    /// Role that paused the course via `pause_course`, if currently paused.
    pub paused_by: PausedBy,
}

/// An enrollment record — one per student per course
#[contracttype]
#[derive(Clone)]
pub struct Enrollment {
    /// The student's Stellar address
    pub student: Address,
    /// The course ID this enrollment belongs to
    pub course_id: String,
    /// Amount paid at enrollment (in stroops)
    pub amount_paid: i128,
    /// Ledger sequence when the student enrolled
    pub enrolled_at_ledger: u32,
    /// Whether the student has completed the course
    pub completed: bool,
    /// Whether a certificate has been issued on-chain
    pub certificate_issued: bool,
    /// The ID of the certificate issued, if any
    pub certificate_id: Option<String>,
    /// Optional fixed-size proof of completion evidence.
    /// Stored as a 32-byte hash to keep on-chain verification compact and deterministic.
    pub evidence_hash: Option<BytesN<32>>,
    /// Whether this enrollment has been refunded
    pub is_refunded: bool,
    /// Course version active at the time of enrollment
    pub course_version: u32,
    /// Hash of the course content at the time of enrollment (#416)
    pub content_hash: BytesN<32>,
    /// Platform's share of `amount_paid`, as actually deducted at
    /// enroll()/re_enroll() time via `deduct_fee()`. Persisted so that a
    /// later refund splits funds in the same proportion that was actually
    /// collected, even if `FeeConfig(token)` or `course.platform_fee_percent`
    /// changes afterward.
    pub platform_amount: i128,
    /// Instructor's share of `amount_paid`, as actually credited to
    /// `InstructorEarnings` at enroll()/re_enroll() time.
    pub instructor_amount: i128,
    /// Unique generated reference ID for this enrollment.
    pub enrollment_ref: String,
    /// Ledger sequence when the enrollment was marked completed, if any (#166)
    pub completed_at_ledger: Option<u32>,
    /// Snapshot of the course's `enrollment_expiry_ledgers` at enrollment time (#340)
    pub enrollment_expiry_ledgers: Option<u32>,
}

/// Compact snapshot of a course's terms at a specific version (#371).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct CourseTerms {
    /// Course price at this version (in stroops)
    pub price: i128,
    /// Platform fee percentage at this version (0-100)
    pub platform_fee_percent: u32,
    /// Content commitment hash active at this version
    pub content_hash: BytesN<32>,
    /// Optional maximum capacity at this version
    pub max_capacity: Option<u32>,
    /// Capacity enforcement mode at this version
    pub capacity_mode: CapacityMode,
}

/// An on-chain certificate of completion
/// Acts as a lightweight NFT — a verifiable proof of skill attainment.
#[contracttype]
#[derive(Clone)]
pub struct Certificate {
    /// Unique certificate ID
    pub id: String,
    /// The student's Stellar address
    pub student: Address,
    /// The course ID completed
    pub course_id: String,
    /// Short course title stored on-chain for easy verification
    pub course_title: String,
    /// Course version the student completed (snapshot from the enrollment, #416)
    pub course_version: u32,
    /// Course content hash the student completed (snapshot from the enrollment, #416)
    pub content_hash: BytesN<32>,
    /// Reference back to the enrollment record (e.g. backend ID)
    pub enrollment_reference: String,
    /// Instructor's address (for attribution)
    pub instructor: Address,
    /// The contract address that issued this certificate.
    /// Allows external verifiers to distinguish certificates issued by
    /// different deployments (testnet, mainnet, upgraded versions) without
    /// relying on off-chain metadata.
    pub issued_by: Address,
    /// Ledger sequence when the certificate was issued
    pub issued_at_ledger: u32,
    /// Whether this certificate has been revoked (e.g. cheating).
    /// One-way and append-only: once `true`, no function may set this back
    /// to `false`. See `revoke_certificate` / `bulk_revoke_course_certificates`
    /// for the write paths (both go through `apply_revocation`).
    pub revoked: bool,
    /// Admin address that performed the revocation, if revoked
    pub revoked_by: Option<Address>,
    /// Ledger sequence when the revocation occurred, if revoked
    pub revocation_ledger: Option<u32>,
    /// Reason code supplied by the revoking admin, if revoked
    pub revocation_reason: Option<String>,
    /// Optional ledger sequence when the certificate expires
    pub expires_at_ledger: Option<u32>,
    /// Optional bytes supplied by the caller at issuance, intended to carry
    /// an instructor-produced Ed25519 signature over the certificate data.
    ///
    /// **This field is NOT verified on-chain.** The contract stores whatever
    /// 64 bytes the admin supplies without checking them against the
    /// instructor's key.  On-chain verification is not possible because
    /// `Course.instructor` is a Soroban `Address`, not a raw 32-byte Ed25519
    /// public key; there is no protocol primitive to extract a raw key from
    /// an arbitrary Soroban address.
    ///
    /// External verifiers MUST validate this signature themselves using the
    /// instructor's known public key and the canonical signed payload
    /// (SHA-256 of `certificate_id || student || course_id || issued_at_ledger`).
    /// Treat the field as unverified attestation data, not a proof of
    /// endorsement.
    ///
    /// **Limitation:** only Ed25519 Stellar accounts are supported as signers;
    /// multisig and contract-based accounts cannot produce a usable signature
    /// for this field.
    pub instructor_attestation: Option<BytesN<64>>,
    /// Ledger sequence after which a pending revocation becomes permanent.
    /// `Some(seq)` means revocation is pending and challengeable until
    /// the current ledger reaches `seq`. `None` means no pending revocation.
    pub revocation_deadline: Option<u32>,
    /// ID of the replacement certificate issued after this one was revoked
    /// for `ISSUED_IN_ERROR`.  Set by `issue_certificate` when it creates a
    /// corrected certificate against the same enrollment.  The old revoked
    /// certificate remains on-chain; this field lets verifiers find the
    /// replacement without an off-chain index.
    pub superseded_by: Option<String>,
}

/// Verification status of an on-chain certificate returned by `get_certificate_status`.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CertificateStatus {
    /// No certificate exists with the given ID.
    NotFound,
    /// Certificate exists, is not permanently revoked, and has not expired.
    Valid,
    /// Certificate has been permanently revoked.
    Revoked,
    /// Certificate has passed its `expires_at_ledger` sequence.
    Expired,
}

/// Pending platform treasury update with effective ledger sequence
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct TreasuryUpdate {
    pub address: Address,
    pub effective_ledger: u32,
}

/// The status of a refund request
#[contracttype]
#[derive(Clone, PartialEq, Debug)]
pub enum RefundStatus {
    Pending,
    Approved,
    Rejected,
    /// Withdrawn by the student before any admin decision. A cancelled
    /// request no longer blocks completion or a fresh request in-window.
    Cancelled,
}

/// A refund request record
#[contracttype]
#[derive(Clone)]
pub struct RefundRequest {
    pub student: Address,
    pub course_id: String,
    pub requested_at_ledger: u32,
    /// Ledger sequence by which an admin must process this request.
    /// After it elapses the student may self-approve the refund via
    /// `claim_refund` without admin action. Snapshotted at request time
    /// as `requested_at_ledger + refund_processing_deadline`, so later
    /// config changes do not affect requests already pending.
    pub deadline_ledger: u32,
    pub status: RefundStatus,
    pub refund_amount: Option<i128>,
}

struct RefundBreakdown {
    refund_amount: i128,
    platform_refund: i128,
    instructor_refund: i128,
}

/// Aggregate on-chain reputation stats for an instructor, accumulated
/// across all of their courses. Gives students an on-chain signal of an
/// instructor's track record without relying on off-chain review systems.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct InstructorStats {
    /// Total number of enrollments across all of the instructor's courses
    /// (incremented on `enroll`, `batch_enroll`, and `re_enroll`).
    pub total_students: u32,
    /// Total number of enrollments marked completed via `mark_completed`.
    pub total_completions: u32,
    /// Number of certificates issued via `issue_certificate` that are not currently revoked.
    pub total_certificates: u32,
}

/// Hot enrollment and revenue counters for a course, stored separately from
/// the static `Course` metadata so enrollments do not rewrite the full
/// `Course` entry.
/// Lifecycle state of a student's enrollment in a course.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum EnrollmentStatus {
    /// No enrollment record exists for this student/course pair.
    None,
    /// Enrolled and currently within the enrollment window.
    Active,
    /// Enrolled, not completed, and past the course's enrollment expiry.
    Expired,
    /// Enrollment has been marked completed.
    Completed,
    /// Enrollment was refunded and archived to enrollment history.
    Refunded,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct CourseStats {
    /// Total number of non-refunded enrollments.
    pub total_enrollments: u32,
    /// Total active enrollments (enrolled but not completed or refunded).
    pub active_enrollments: u32,
    /// Total USDC earned across all enrollments, net of refunds (in stroops).
    pub total_earned: i128,
}

// ============================================================
// STORAGE KEYS
// ============================================================

#[contracttype]
pub enum DataKey {
    /// Course record by course ID
    Course(String),
    /// Enrollment record by (student_address, course_id)
    Enrollment(Address, String),
    /// Certificate record by certificate ID
    Certificate(String),
    /// Admin address — set at init, can approve courses and issue certificates
    Admin,
    /// Moderator address — set by admin, can perform day-to-day moderation
    Moderator,
    /// Secondary admin address for multi-sig operations
    SecondaryAdmin,
    /// Optional ledger sequence when the admin role expires
    AdminExpiresAt,
    /// Platform treasury address — receives the platform fee share
    Treasury,
    /// Platform default fee percentage (overrideable per course)
    DefaultFee,
    /// Pending platform treasury address and effective ledger sequence
    PendingTreasury,
    /// Whitelisted token contract address (used to validate course tokens)
    ApprovedToken(Address),
    /// Pending new admin address — must call accept_admin() to take effect
    PendingAdmin,
    /// Pending new secondary admin address
    PendingSecondaryAdmin,
    /// Platform paused state flag
    PlatformPaused,
    /// Withdrawable instructor earnings per (instructor, token) pair (in stroops)
    InstructorEarnings(Address, Address),
    /// Legacy per-instructor lifetime earnings accumulator. Retained for
    /// storage-layout compatibility only; lifetime earnings are tracked
    /// token-scoped in `InstructorLifetimeEarnings` (#645).
    InstructorTotalEarnings(Address),
    /// Lifetime net earnings per (instructor, token) pair (in stroops).
    /// Withdrawals do not reduce this counter; refunds do. Keeping the key
    /// token-scoped prevents mixing amounts denominated in different tokens.
    InstructorLifetimeEarnings(Address, Address),
    /// Number of courses registered by an instructor
    InstructorCourseCount(Address),
    /// Number of currently pending courses for an instructor
    InstructorPendingCourseCount(Address),
    /// Ordered list of course IDs registered by a specific instructor
    /// (append-only — course status changes never remove an entry).
    InstructorCourseList(Address),
    /// Maximum number of courses an instructor can register
    MaxCoursesPerInstructor,
    /// Minimum ledger sequences required between course registration and approval
    MinReviewDelay,
    /// Refund window delay in ledger sequences
    RefundWindow,
    /// Admin-configurable processing deadline (in ledger sequences) for a
    /// pending refund request. Once it elapses the student may self-approve
    /// the refund via `claim_refund` without admin action.
    RefundProcessingDeadline,
    /// Refund request record by (student_address, course_id)
    RefundRequest(Address, String),
    /// Blocklist of instructor addresses who are frozen
    InstructorBlocked(Address),
    /// Ordered list of all registered course IDs (on-chain catalog)
    CourseList,
    /// Archived past `Enrollment` records for a (student, course_id) pair,
    /// preserved when a completed or expired student re-enrolls via
    /// `re_enroll()`.
    EnrollmentHistory(Address, String),
    /// Aggregate reputation stats for an instructor (total students,
    /// completions, certificates issued) — see `InstructorStats`.
    InstructorStats(Address),
    /// Blocklist of student addresses who are banned from the platform
    StudentBlocked(Address),
    /// Per-token fee configuration (maps token address → FeeConfig)
    FeeConfig(Address),
    /// Risk fee configuration for surcharge pricing
    RiskFeeConfig,
    /// Flag indicating whether risk-based fee pricing is enabled
    RiskConfigEnabled,
    /// Persistent total active enrollments; legacy deployments used instance storage.
    /// Total active paid enrollments across all courses platform-wide.
    /// Free-course enrollments are excluded so they cannot consume paid
    /// platform capacity.
    TotalActiveEnrollments,
    /// Maximum total active paid enrollments allowed platform-wide.
    /// Free-course enrollments do not count against this cap.
    PlatformEnrollmentCap,
    /// A proposed contract Wasm upgrade awaiting its time-lock, if any.
    PendingUpgrade,
    /// Configurable time-lock (in ledger sequences) that must elapse
    /// between an upgrade proposal and its execution.
    UpgradeTimelock,
    /// Waitlist for a course - ordered list of student addresses waiting for enrollment
    CourseWaitlist(String),
    /// Global registry of all instructor addresses who have registered courses
    InstructorRegistry,
    /// Archived course records by course ID, stored to enforce cooldown
    /// period before the same course ID can be re-registered.
    ArchivedCourse(String),
    /// Configurable challenge period (in ledger sequences) after which
    /// a pending certificate revocation becomes permanent.
    RevocationChallengePeriod,
    /// Ordered (append-only) list of certificate IDs issued for a course.
    /// Populated by `issue_certificate` and consumed by course-wide bulk
    /// operations such as `bulk_revoke_course_certificates()`.
    CourseCertificates(String),
    /// Legacy reverse-lookup key, retained for storage-key compatibility.
    /// New enrollments do not write this key; `issue_certificate` accepts the
    /// student and course ID directly.
    EnrollmentByRef(String),
    /// Counter for generating unique enrollment references
    EnrollmentRefCounter,
    /// Global registry of approver addresses with course approval authority
    Approver(Address),
    /// Ledger sequence when init() was called — used to enforce
    /// a governance window before approve_course() can be called.
    InitLedger,
    /// Ledger sequence of the last successful transfer_admin() call.
    /// Used to enforce a minimum cooldown between consecutive admin key
    /// rotations, preventing rapid-rotation attacks after a compromise.
    LastAdminTransferLedger,
    /// Per-course consecutive enrollment failure counter.
    /// Incremented each time enroll_internal() panics due to a token
    /// transfer failure for a specific course. Reset to 0 on a successful
    /// enrollment. When the count reaches CIRCUIT_BREAKER_THRESHOLD the
    /// course is automatically paused.
    CourseFailureCount(String),
}

/// Overflow keys split out to avoid the #[contracttype] variant-count limit.
#[contracttype]
pub enum DataKey2 {
    /// Maximum pending courses per instructor limit
    MaxPendingCoursesPerInstructor,
    /// Default minimum ledger sequences required between enroll() and
    /// mark_completed() for newly-registered courses (admin-configurable).
    DefaultMinCompletionLedgers,
    /// Tokens classified as BTC/ETH-denominated for risk-surcharge pricing.
    BtcEthToken(Address),
    /// Ordered list of all whitelisted token addresses.
    /// Maintained in sync with `ApprovedToken(Address)` entries so that
    /// `get_approved_tokens()` can enumerate them without an index scan.
    ApprovedTokenList,
    /// Lifetime enrollment count per student (new-customer risk flag).
    StudentEnrollmentCount(Address),
    /// Legacy instance-storage key for the per-instructor course count.
    /// Retained only so the migration helper can read and clear the old
    /// instance entry; new writes go to `InstructorCourseCountPersistent`.
    InstructorCourseCountLegacy(Address),
    /// Legacy instance-storage key for the per-instructor pending count.
    /// Retained only so the migration helper can read and clear the old
    /// instance entry; new writes go to `InstructorPendingCountPersistent`.
    InstructorPendingCountLegacy(Address),
    /// Persistent per-instructor course count (migrated out of instance
    /// storage so instance size no longer grows with the instructor count).
    InstructorCourseCountPersistent(Address),
    /// Persistent per-instructor pending course count (migrated out of
    /// instance storage so instance size no longer grows with the instructor count).
    InstructorPendingCountPersistent(Address),
    /// Threshold price (in stroops) above which a course is "high-value"
    /// and requires co-signature from two different admins to approve
    HighValueCourseThreshold,
    /// Stores the first approver's address for a high-value course pending co-signature
    HighValueCourseApproval(String),
    /// Total outstanding un-withdrawn instructor earnings for a token (in stroops)
    TotalInstructorEarnings(Address),
    /// Total outstanding arbitration fees held for a token (in stroops)
    TotalArbitrationFees(Address),
    /// Arbitration fee configuration, keyed by the settlement token.
    ArbitrationFeeConfig(Address),
    /// Arbitration fees currently held by the contract for a given token.
    /// These funds are reserved for pending arbitration cases and are not
    /// available for instructor withdrawals or emergency sweeps.
    ArbitrationFeesHeld(Address),
    /// Bounded admin-supplied reason for freezing an instructor.
    InstructorBlockReason(Address),
    /// Bounded admin-supplied reason for blocking a student.
    StudentBlockReason(Address),
    /// Contract-held reserve, per token, that funds the platform-fee share of
    /// refunds so they never need a signature from the treasury account
    RefundReserve(Address),
    /// Snapshot of course terms keyed by `(course_id, version)` (#371).
    CourseTerms(String, u32),
    /// Per-instructor set of token addresses in which the instructor currently
    /// holds a non-zero withdrawable earnings balance.
    InstructorEarningTokens(Address),
    /// Net cumulative platform fees collected for a token (incremented on
    /// enrollment fee collection, decremented on refunds).
    PlatformFeesCollected(Address),
    /// Ordered (append-only) list of certificate IDs issued to a student (#414).
    /// Populated by `issue_certificate` and read by `get_certificates_by_student`.
    StudentCertificates(Address),
    /// Accumulated in-contract platform fees awaiting sweep.
    PlatformFeesAccumulated(Address),
    /// Pending co-instructor revenue share keyed by (course_id, address).
    PendingCoInstructor(String, Address),
    /// List of pending co-instructor addresses for a course.
    PendingCoInstructorList(String),
    /// Hot enrollment and earnings counters for a course.
    CourseStats(String),
    /// Ledger sequence at which a pending admin transfer proposal expires (#427).
    PendingAdminExpiresAt,
    /// Token precision captured once when the token is approved (#705).
    ApprovedTokenDecimals(Address),
}

/// A proposed contract code upgrade awaiting its governance time-lock.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PendingUpgrade {
    /// Hash of the new Wasm code, previously uploaded via
    /// `env.deployer().upload_contract_wasm()`.
    pub new_wasm_hash: BytesN<32>,
    /// Ledger sequence when the upgrade was proposed.
    pub proposed_at_ledger: u32,
    /// Ledger sequence at or after which `upgrade_contract` may execute.
    pub effective_ledger: u32,
}

// ============================================================
// CONTRACT
// ============================================================

/// Pure surcharge rules shared by risk previews and payment fee deduction.
/// Missing configuration or disabled risk pricing yields no score or surcharge.
fn risk_surcharge(
    enabled: bool,
    config: Option<&RiskFeeConfig>,
    payment_amount: i128,
    is_new_customer: bool,
    is_btc_eth: bool,
) -> (u32, u32) {
    let config = match config {
        Some(config) if enabled => config,
        _ => {
            return (0, 0);
        }
    };

    let mut score: u32 = 0;
    let mut surcharge_bps: u32 = 0;

    // Large payment surcharge
    if payment_amount > config.large_payment_threshold {
        score = score.saturating_add(30);
        surcharge_bps = surcharge_bps.saturating_add(config.large_payment_surcharge_bps);
    }

    // New customer surcharge
    if is_new_customer {
        score = score.saturating_add(40);
        surcharge_bps = surcharge_bps.saturating_add(config.new_customer_surcharge_bps);
    }

    // BTC/ETH surcharge
    if is_btc_eth {
        score = score.saturating_add(30);
        surcharge_bps = surcharge_bps.saturating_add(config.btc_eth_surcharge_bps);
    }

    (score, surcharge_bps)
}

#[contract]
pub struct HamplardContract;

#[contractimpl]
impl HamplardContract {
    /// Minimum ledgers before instance storage TTL extension is triggered (~1 year)
    const INSTANCE_TTL_THRESHOLD: u32 = 6_000_000;
    const INSTANCE_TTL_EXTEND_TO: u32 = 6_300_000;
    /// Minimum ledgers before persistent storage TTL extension is triggered (~1 year)
    pub(crate) const PERSISTENT_TTL_THRESHOLD: u32 = 6_000_000;
    pub(crate) const PERSISTENT_TTL_EXTEND_TO: u32 = 6_300_000;
    const MAX_COURSE_ID_LEN: u32 = 256;
    const MAX_COURSE_TITLE_LEN: u32 = 512;
    /// Maximum length, in bytes, of an admin-supplied moderation reason.
    pub const MAX_MODERATION_REASON_LEN: u32 = 256;
    /// Minimum non-zero course price accepted at registration, denominated
    /// in stroops at the expected 7-decimal-place precision (0.01 USDC).
    /// Catches an instructor accidentally entering a price in whole-dollar
    /// units instead of stroops (e.g. typing `50` meaning $50, instead of
    /// the correct `500_000_000`).
    const MIN_COURSE_PRICE_STROOPS: i128 = 100_000;
    /// Maximum course price accepted at registration, denominated in
    /// stroops at the expected 7-decimal-place precision (100,000 USDC).
    /// Catches an accidental extra digit turning a reasonable price into
    /// an absurd one.
    const MAX_COURSE_PRICE_STROOPS: i128 = 1_000_000_000_000;
    /// Default governance time-lock for contract upgrades, in ledger
    /// sequences (~17,280 ledgers ≈ 1 day at 5s/ledger). Used when the
    /// admin has not configured a custom value via `set_upgrade_timelock`.
    const DEFAULT_UPGRADE_TIMELOCK_LEDGERS: u32 = 17_280;
    /// Cooldown period in ledger sequences after a course is archived
    /// before the same course ID can be re-registered. This prevents
    /// confusion where a new course inherits the identity of an archived
    /// one, potentially misleading students with historical enrollment
    /// records. (~172,800 ledgers ≈ 10 days at 5s/ledger)
    const ARCHIVE_COOLDOWN_LEDGERS: u32 = 172_800;
    /// Default revocation challenge period in ledger sequences
    /// (~17,280 ledgers ≈ 1 day at 5s/ledger).
    const DEFAULT_REVOCATION_CHALLENGE_PERIOD: u32 = 17_280;

    /// Maximum number of addresses that may be on a course waitlist at once.
    const MAX_WAITLIST_ENTRIES: u32 = 500;

    /// Default refund processing deadline in ledger sequences
    /// (~17,280 ledgers ≈ 1 day at 5s/ledger). After this many ledgers
    /// elapse from a refund request, the student may self-approve it via
    /// `claim_refund` without admin action.
    const DEFAULT_REFUND_PROCESSING_DEADLINE: u32 = 17_280;
    /// Bounds for the admin-configurable refund processing deadline. The
    /// floor gives the admin a real window to act before a student can
    /// self-approve; the ceiling stops the feature being silently disabled
    /// with an absurdly large value.
    const MIN_REFUND_PROCESSING_DEADLINE: u32 = 10;
    const MAX_REFUND_PROCESSING_DEADLINE: u32 = 1_000_000;
    /// Maximum allowed refund window in ledger sequences (~172,800 ledgers
    /// ≈ 10 days at 5s/ledger). Bounds `update_refund_window()` so an
    /// excessively long window cannot leave instructor earnings exposed
    /// indefinitely or allow refunds long after a course was consumed.
    /// `init()` and `update_refund_window()` must reject values above this.
    pub(crate) const MAX_REFUND_WINDOW_LEDGERS: u32 = 172_800;
    /// Default refund window used when storage has no value (~1 day at 5s/ledger).
    pub(crate) const DEFAULT_REFUND_WINDOW_LEDGERS: u32 = 17_280;
    /// Maximum number of course IDs allowed in a single `batch_enroll()` call.
    pub(crate) const MAX_BATCH_SIZE: u32 = 50;
    /// Maximum number of course IDs returned by a single `list_courses()` call.
    /// Larger catalogs must be walked with successive `offset` values so one
    /// invocation cannot exceed the transaction CPU/instruction budget.
    pub(crate) const MAX_PAGE_SIZE: u32 = 50;
    /// Maximum number of students that may be refunded in a single
    /// `archive_course()` call. Large courses must be refunded in multiple
    /// transactions.
    pub(crate) const MAX_STUDENTS_TO_REFUND: u32 = 100;
    /// Minimum ledger sequences that must elapse between consecutive
    /// `transfer_admin()` calls. Prevents rapid admin key rotation
    /// after a compromise (~17,280 ledgers ≈ 1 day at 5s/ledger). (#176)
    const MIN_ADMIN_TRANSFER_COOLDOWN: u32 = 17_280;
    /// How long a pending admin transfer stays acceptable (~17,280 ledgers ≈ 1 day
    /// at 5s/ledger). After this window `accept_admin()` rejects the stale
    /// proposal and clears it, so compromised keys nominated long ago cannot be
    /// activated later. (#427)
    pub const ADMIN_PROPOSAL_VALIDITY_LEDGERS: u32 = 17_280;
    /// Time-lock delay in ledger sequences before a proposed treasury address
    /// change takes effect (~17,280 ledgers ≈ 1 day at 5s/ledger).
    /// Gives monitors and admins sufficient time to detect and react to a
    /// malicious treasury change.
    pub const TREASURY_UPDATE_DELAY_LEDGERS: u32 = 17_280;
    /// Number of consecutive enrollment failures (token-transfer errors)
    /// after which a course is automatically paused as a circuit breaker.
    /// Reset to 0 on a successful enrollment. (#178)
    const CIRCUIT_BREAKER_THRESHOLD: u32 = 5;
    /// Upper bound for the platform-wide minimum completion duration.
    /// At 5 seconds per ledger this is roughly 58 days. Without an upper
    /// bound an extreme value would make every newly registered course
    /// impossible to complete. (#444)
    pub(crate) const MAX_MIN_COMPLETION_LEDGERS: u32 = 1_000_000;
    /// Maximum number of categories a course can have (#165)
    const MAX_CATEGORIES: u32 = 10;
    /// Maximum length of a category name in characters (#165)
    const MAX_CATEGORY_LEN: u32 = 64;
    /// Maximum number of co-instructors that can be attached to one course.
    const MAX_CO_INSTRUCTORS: u32 = 10;
    /// Revenue shares are expressed in basis points (100% = 10 000 bps).
    const MAX_SHARE_BPS: u32 = 10_000;
    /// Token precision required for every approved token (#705).
    const EXPECTED_TOKEN_DECIMALS: u32 = 7;
    /// Maximum number of prerequisite courses stored on a course. The list is
    /// walked on every enrollment, so it must remain bounded (#699).
    const MAX_PREREQUISITE_COURSES: u32 = 10;
    /// Maximum length of a certificate revocation reason (#411)
    const MAX_REVOCATION_REASON_LEN: u32 = 64;
    /// 100% in basis points: upper bound for any single fee surcharge and for
    /// the platform default fee plus all risk surcharges combined
    const MAX_FEE_BPS: u32 = 10_000;

    // ----------------------------------------------------------
    // INIT
    // ----------------------------------------------------------

    /// Returns the crate version of this contract build as a string.
    /// Useful for verifying which implementation is live after an upgrade.
    pub fn version(env: Env) -> String {
        String::from_str(&env, env!("CARGO_PKG_VERSION"))
    }

    /// Initialise the contract atomically at deployment time.
    /// Runs as the Soroban `__constructor`, preventing any front-running
    /// between contract deployment and initialisation.
    /// Emits `initialized` so indexers can discover the initial configuration.
    ///
    /// # Arguments
    /// - `admin`                       — admin address (approves courses, issues certificates)
    /// - `secondary_admin`             — secondary admin address for multi-sig operations
    /// - `treasury`                    — platform treasury address (receives platform fee share)
    /// - `default_fee_pct`             — default platform fee percentage (e.g. 20 = 20%)
    /// - `max_courses_per_instructor`  — max active courses per instructor
    /// - `refund_window_ledgers`       — number of ledger sequences after enrollment during which a
    ///                                   refund request is accepted; requests after this window are
    ///                                   automatically rejected (e.g. 17_280 ≈ 1 day at 5s/ledger)
    /// - `revocation_challenge_period` — ledger sequences during which a certificate revocation
    ///                                   can be challenged
    pub fn __constructor(
        env: Env,
        admin: Address,
        secondary_admin: Address,
        treasury: Address,
        default_fee_pct: u32,
        max_courses_per_instructor: u32,
        refund_window_ledgers: u32,
        revocation_challenge_period: u32,
    ) {
        admin.require_auth();
        secondary_admin.require_auth();

        if env.storage().instance().has(&DataKey::Admin) {
            soroban_sdk::panic_with_error!(&env, Error::AlreadyInitialized);
        }

        if max_courses_per_instructor == 0 {
            soroban_sdk::panic_with_error!(&env, Error::InvalidMaxCoursesLimit);
        }

        if default_fee_pct > 100 {
            soroban_sdk::panic_with_error!(&env, Error::InvalidFeeConfig);
        }

        if refund_window_ledgers > Self::MAX_REFUND_WINDOW_LEDGERS {
            soroban_sdk::panic_with_error!(&env, Error::InvalidRefundWindow);
        }

        if treasury == env.current_contract_address() {
            soroban_sdk::panic_with_error!(&env, Error::InvalidTreasury);
        }

        if admin == treasury {
            soroban_sdk::panic_with_error!(&env, Error::InvalidAdminConfig);
        }

        if secondary_admin == treasury {
            soroban_sdk::panic_with_error!(&env, Error::InvalidAdminConfig);
        }

        if admin == secondary_admin {
            soroban_sdk::panic_with_error!(&env, Error::InvalidAdminConfig);
        }

        if revocation_challenge_period == 0 || revocation_challenge_period > 1_209_600 {
            soroban_sdk::panic_with_error!(&env, Error::InvalidFeeConfig);
        }

        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Moderator, &admin);
        env.storage()
            .instance()
            .set(&DataKey::SecondaryAdmin, &secondary_admin);
        env.storage().instance().set(&DataKey::Treasury, &treasury);
        env.storage()
            .instance()
            .set(&DataKey::PlatformPaused, &false);
        env.storage()
            .instance()
            .set(&DataKey::DefaultFee, &default_fee_pct);
        env.storage().instance().set(
            &DataKey::MaxCoursesPerInstructor,
            &max_courses_per_instructor,
        );
        env.storage()
            .instance()
            .set(&DataKey::RefundWindow, &refund_window_ledgers);
        env.storage().instance().set(
            &DataKey::RevocationChallengePeriod,
            &revocation_challenge_period,
        );
        env.storage()
            .instance()
            .set(&DataKey::InitLedger, &env.ledger().sequence());

        env.events().publish(
            (Symbol::new(&env, "initialized"), admin.clone()),
            (
                admin,
                secondary_admin,
                treasury,
                default_fee_pct,
                max_courses_per_instructor,
                refund_window_ledgers,
            ),
        );
    }

    pub fn init(
        env: Env,
        admin: Address,
        secondary_admin: Address,
        treasury: Address,
        default_fee_pct: u32,
        max_courses_per_instructor: u32,
        refund_window_ledgers: u32,
        revocation_challenge_period: u32,
    ) {
        Self::__constructor(
            env,
            admin,
            secondary_admin,
            treasury,
            default_fee_pct,
            max_courses_per_instructor,
            refund_window_ledgers,
            revocation_challenge_period,
        );
    }

    /// Admin sets (or rotates) the moderator address.
    ///
    /// The moderator is a hot key permitted to perform day-to-day moderation
    /// (approve/reject/pause/unpause courses, block/unblock students, mark completions)
    /// without holding the admin key that can move funds or change config.
    ///
    /// # Arguments
    /// - `admin`     — current admin (must sign)
    /// - `moderator` — new moderator address
    pub fn set_moderator(env: Env, admin: Address, moderator: Address) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "set_moderator");
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        if moderator == env.current_contract_address() {
            panic!("moderator cannot be the contract address");
        }

        env.storage()
            .instance()
            .set(&DataKey::Moderator, &moderator);

        env.events().publish(
            (Symbol::new(&env, "moderator_set"), admin.clone()),
            moderator,
        );
    }

    /// Returns the current moderator address, if one has been configured.
    pub fn get_moderator(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Moderator)
    }

    /// Instructor or admin configures an optional ledger sequence when enrollment opens.
    ///
    /// Set to `None` to allow enrollment immediately after the course becomes Active.
    /// When set, `enroll()` rejects students until the current ledger sequence is
    /// greater than or equal to `enrollment_start_ledger`.
    ///
    /// # Arguments
    /// - `caller`                  — must be the course instructor or admin
    /// - `course_id`               — the course to update
    /// - `enrollment_start_ledger` — optional ledger sequence when enrollment opens
    pub fn set_enrollment_start_ledger(
        env: Env,
        caller: Address,
        course_id: String,
        enrollment_start_ledger: Option<u32>,
    ) {
        caller.require_auth();

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CourseNotFound));

        let is_admin = Self::is_admin(&env, &caller);
        let is_instructor = caller == course.instructor;

        if !is_admin && !is_instructor {
            soroban_sdk::panic_with_error!(&env, Error::Unauthorized);
        }

        if !is_admin && Self::is_instructor_frozen_internal(&env, &course.instructor) {
            soroban_sdk::panic_with_error!(&env, Error::InstructorFrozen);
        }

        if course.status == CourseStatus::Archived {
            soroban_sdk::panic_with_error!(&env, Error::ArchivedCourseImmutable);
        }
        if course.status == CourseStatus::Rejected {
            soroban_sdk::panic_with_error!(&env, Error::CourseNotAvailable);
        }

        course.enrollment_start_ledger = enrollment_start_ledger;
        course.last_updated_ledger = env.ledger().sequence();
        Self::persist_course(&env, &course_id, &course);

        env.events().publish(
            (Symbol::new(&env, "enrollment_start_set"), course_id.clone()),
            (
                course_id.clone(),
                caller.clone(),
                env.ledger().sequence(),
                enrollment_start_ledger,
            ),
        );
    }

    // ----------------------------------------------------------
    // COURSE MANAGEMENT
    // ----------------------------------------------------------

    /// Instructor registers a new course on-chain.
    /// The course starts in Pending status — an admin must approve it
    /// before students can enroll.
    ///
    /// # Arguments
    /// - `instructor`       — instructor's Stellar address (must sign)
    /// - `course_id`        — unique ID matching the backend DB record
    /// - `price`            — enrollment price in USDC stroops
    /// - `token`            — USDC Stellar Asset Contract address; must already
    ///   be on the enrollment whitelist (`add_approved_token`), otherwise
    ///   registration reverts
    /// - `platform_fee_pct` — optional fee override; pass 0 to follow the live
    ///   per-token fee (`FeeConfig(token)` / `DefaultFee`). A non-zero value
    ///   must be at least the token's current fee and is charged at enrollment
    ///   as a floor on the live per-token fee.
    /// - `content_hash`     — 32-byte hash of the off-chain course content at registration time
    #[allow(dead_code)]
    fn normalize_course_id(env: &Env, input: &String) -> String {
        let len = input.len() as usize;
        if len == 0 {
            panic!("course_id must not be empty");
        }
        let mut buf = [0u8; 128];
        if len > buf.len() {
            panic!("course_id exceeds maximum length");
        }
        input.copy_into_slice(&mut buf[..len]);
        for b in &mut buf[..len] {
            if *b == b' ' || *b == b'\t' || *b == b'\n' || *b == b'\r' {
                panic!("course_id must not contain whitespace");
            }
            if b'A' <= *b && *b <= b'Z' {
                *b += 32;
            }
        }
        String::from_bytes(env, &buf[..len])
    }

    pub fn register_course(
        env: Env,
        instructor: Address,
        course_id: String,
        price: i128,
        token: Address,
        platform_fee_pct: u32,
        max_capacity: Option<u32>,
        content_hash: BytesN<32>,
        categories: Vec<String>,
    ) -> String {
        instructor.require_auth();

        // #391 - Check the whitelist BEFORE any cross-contract call, so an
        // attacker-supplied token contract never executes inside registration.
        if !env
            .storage()
            .instance()
            .has(&DataKey::ApprovedToken(token.clone()))
        {
            soroban_sdk::panic_with_error!(&env, Error::CourseNotAvailable);
        }

        // #705 — Precision was validated and cached when the token was
        // approved, so registration never calls out to the token contract.
        let token_decimals: u32 = env
            .storage()
            .instance()
            .get(&DataKey2::ApprovedTokenDecimals(token.clone()))
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::InvalidTokenDecimals));
        if token_decimals != Self::EXPECTED_TOKEN_DECIMALS {
            soroban_sdk::panic_with_error!(&env, Error::InvalidTokenDecimals);
        }
        if Self::is_instructor_frozen_internal(&env, &instructor) {
            soroban_sdk::panic_with_error!(&env, Error::InstructorFrozen);
        }

        if course_id.is_empty() {
            soroban_sdk::panic_with_error!(&env, Error::InvalidCourseInput);
        }

        if course_id.len() > Self::MAX_COURSE_ID_LEN {
            soroban_sdk::panic_with_error!(&env, Error::InvalidCourseInput);
        }

        // `Some(0)` would make the course permanently unenrollable
        // (`total_enrollments >= 0` is always true); use `None` for unlimited.
        if max_capacity == Some(0) {
            soroban_sdk::panic_with_error!(&env, Error::InvalidCourseInput);
        }

        // Validate course ID contains only allowed characters.
        // Allowed: printable ASCII (0x20-0x7E) excluding backtick and tilde
        // which can cause issues in some off-chain parsers.
        // This prevents null bytes, control characters, and problematic
        // Unicode that could break off-chain parsers or create unreproducible
        // storage keys.
        // `String` only exposes its bytes through `copy_into_slice()`, which
        // requires an exactly-sized buffer. The length was already bounded by
        // the MAX_COURSE_ID_LEN check above, so a fixed buffer is safe.
        let mut id_bytes = [0u8; Self::MAX_COURSE_ID_LEN as usize];
        let id_bytes = &mut id_bytes[..course_id.len() as usize];
        course_id.copy_into_slice(id_bytes);

        for byte in id_bytes.iter() {
            // Allow printable ASCII: space (0x20) through tilde (0x7E)
            // Exclude null (0x00) and other control chars (0x01-0x1F, 0x7F)
            if *byte < 0x20 || *byte > 0x7E {
                soroban_sdk::panic_with_error!(&env, Error::InvalidCourseInput);
            }
        }

        // Validate categories (#165)
        if categories.len() > Self::MAX_CATEGORIES {
            soroban_sdk::panic_with_error!(&env, Error::InvalidCourseInput);
        }
        for cat in categories.iter() {
            if cat.len() > Self::MAX_CATEGORY_LEN {
                soroban_sdk::panic_with_error!(&env, Error::InvalidCourseInput);
            }
        }

        if price < 0 {
            soroban_sdk::panic_with_error!(&env, Error::InvalidPrice);
        }

        if content_hash.to_array().iter().all(|&b| b == 0) {
            soroban_sdk::panic_with_error!(&env, Error::InvalidCourseInput);
        }

        // A price of exactly 0 is a valid free course. Any non-zero price
        // must be denominated in stroops at the token's expected 7-decimal
        // precision — reject values so small or so large that they signal
        // the price was entered in the wrong unit.
        if price != 0
            && (price < Self::MIN_COURSE_PRICE_STROOPS || price > Self::MAX_COURSE_PRICE_STROOPS)
        {
            soroban_sdk::panic_with_error!(&env, Error::InvalidPrice);
        }

        if env
            .storage()
            .persistent()
            .has(&DataKey::Course(course_id.clone()))
        {
            soroban_sdk::panic_with_error!(&env, Error::CourseAlreadyRegistered);
        }

        // Check if this course ID was previously used and is still within
        // the archival cooldown period. This prevents confusion where a new
        // course inherits the identity of an archived one.
        if let Some(old_course) = env
            .storage()
            .persistent()
            .get::<DataKey, Course>(&DataKey::ArchivedCourse(course_id.clone()))
        {
            if let Some(archived_ledger) = old_course.archived_at_ledger {
                let current_ledger = env.ledger().sequence();
                if current_ledger < archived_ledger + Self::ARCHIVE_COOLDOWN_LEDGERS {
                    soroban_sdk::panic_with_error!(&env, Error::CourseIdInCooldown);
                }
            }
        }

        let max_courses: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MaxCoursesPerInstructor)
            .unwrap_or(50);

        let course_count_key = DataKey2::InstructorCourseCountPersistent(instructor.clone());
        let current_count: u32 = Self::read_instructor_course_count(&env, &instructor);
        // A stored limit of 0 would block every instructor from registering
        // (current_count >= 0 is always true). Reject it defensively so a
        // misconfigured update_max_courses_limit(0) cannot silently freeze
        // all registrations; use a positive limit instead.
        if max_courses == 0 {
            panic!("max courses per instructor must be greater than zero");
        }

        if current_count >= max_courses {
            soroban_sdk::panic_with_error!(&env, Error::InstructorCourseLimitReached);
        }

        let pending_count_key = DataKey2::InstructorPendingCountPersistent(instructor.clone());
        let current_pending_count = Self::read_instructor_pending_course_count(&env, &instructor);

        if current_pending_count >= Self::effective_max_pending_courses(&env, max_courses) {
            soroban_sdk::panic_with_error!(&env, Error::InstructorCourseLimitReached);
        }

        // Validate against the same fee source-of-truth that deduct_fee()
        // uses at enrollment: the per-token `FeeConfig(token)`, falling back
        // to `DefaultFee`. Validating against `DefaultFee` alone would accept
        // overrides below a higher per-token rate (or reject overrides above
        // a lower one) that enrollment would never actually charge.
        let token_fee_bps = Self::get_fee_config(env.clone(), token.clone()).fee_bps;

        let fee = if platform_fee_pct == 0 {
            // Snapshot of the live token rate, rounded up to whole percent.
            // Informational only — with no override, enrollment always
            // follows the live FeeConfig(token) / DefaultFee.
            token_fee_bps.div_ceil(100)
        } else {
            if platform_fee_pct > 100 {
                soroban_sdk::panic_with_error!(&env, Error::InvalidFeeConfig);
            }
            if platform_fee_pct * 100 < token_fee_bps {
                soroban_sdk::panic_with_error!(&env, Error::InvalidFeeConfig);
            }
            platform_fee_pct
        };

        let min_completion_ledgers: u32 = env
            .storage()
            .instance()
            .get(&DataKey2::DefaultMinCompletionLedgers)
            .unwrap_or(0);

        let course = Course {
            id: course_id.clone(),
            instructor: instructor.clone(),
            price,
            platform_fee_percent: fee,
            token,
            total_enrollments: 0,
            active_enrollments: 0,
            total_earned: 0,
            total_refunded: 0,
            refund_count: 0,
            status: CourseStatus::Pending,
            created_at_ledger: env.ledger().sequence(),
            max_capacity,
            capacity_mode: CapacityMode::Lifetime,
            enrollment_start_ledger: None,
            enrollment_expiry_ledgers: None,
            min_completion_ledgers,
            version: 1,
            last_updated_ledger: env.ledger().sequence(),
            expires_at_ledger: None,
            content_hash,
            max_certificates: None,
            certificates_issued: 0,
            certificates_revoked: 0,
            prerequisite_course_ids: Vec::new(&env),
            archived_at_ledger: None,
            categories,
            co_instructors: Vec::new(&env),
            paused_by: PausedBy::None,
        };

        Self::persist(&env, &DataKey::Course(course_id.clone()), &course);
        Self::save_course_terms(&env, &course);

        // Append to on-chain course catalog
        let mut catalog: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::CourseList)
            .unwrap_or_else(|| Vec::new(&env));
        catalog.push_back(course_id.clone());
        Self::persist(&env, &DataKey::CourseList, &catalog);

        env.storage()
            .persistent()
            .set(&course_count_key, &(current_count + 1));
        env.storage().persistent().extend_ttl(
            &course_count_key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );
        env.storage()
            .persistent()
            .set(&pending_count_key, &(current_pending_count + 1));
        env.storage().persistent().extend_ttl(
            &pending_count_key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        // Append to the per-instructor course list
        let instructor_list_key = DataKey::InstructorCourseList(instructor.clone());
        let mut instructor_courses: Vec<String> = env
            .storage()
            .persistent()
            .get(&instructor_list_key)
            .unwrap_or_else(|| Vec::new(&env));
        instructor_courses.push_back(course_id.clone());
        Self::persist(&env, &instructor_list_key, &instructor_courses);

        // Add instructor to global registry if first course
        let mut registry: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::InstructorRegistry)
            .unwrap_or_else(|| Vec::new(&env));

        // Check if instructor already exists in registry
        let mut exists = false;
        for i in 0..registry.len() {
            if registry.get(i).unwrap() == instructor {
                exists = true;
                break;
            }
        }

        if !exists {
            registry.push_back(instructor.clone());
            Self::persist(&env, &DataKey::InstructorRegistry, &registry);
        }

        env.events().publish(
            (Symbol::new(&env, "course_registered"), course_id.clone()),
            (
                course_id.clone(),
                instructor.clone(),
                env.ledger().sequence(),
            ),
        );

        course_id
    }

    /// Admin or approved approver approves a Pending course, making it Active and enrollable.
    ///
    /// # Arguments
    /// - `caller`    — must be admin or an approved approver
    /// - `course_id` — the course to approve
    pub fn approve_course(env: Env, caller: Address, course_id: String) {
        caller.require_auth();
        Self::require_admin_or_approver(&env, &caller, "approve_course");
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CourseNotFound));

        if course.status != CourseStatus::Pending {
            soroban_sdk::panic_with_error!(&env, Error::CourseNotPending);
        }

        // #380 — Reject approval if the course's token is no longer (or never
        // was) on the whitelist.  Without this guard a course could become
        // Active while its token is unapproved, making every subsequent
        // enrollment fail with "course token is not approved".
        if !env
            .storage()
            .instance()
            .has(&DataKey::ApprovedToken(course.token.clone()))
        {
            soroban_sdk::panic_with_error!(&env, Error::CourseNotAvailable);
        }

        // Enforce governance window: approve_course cannot be called
        // immediately after init(). This ensures there's a review period
        // where the community can observe admin actions before the platform
        // starts accepting courses.
        if let Some(init_ledger) = env
            .storage()
            .instance()
            .get::<DataKey, u32>(&DataKey::InitLedger)
        {
            let min_delay = env
                .storage()
                .instance()
                .get::<DataKey, u32>(&DataKey::MinReviewDelay)
                .unwrap_or(0);
            let elapsed = env
                .ledger()
                .sequence()
                .checked_sub(init_ledger)
                .unwrap_or(0);
            if elapsed < min_delay {
                soroban_sdk::panic_with_error!(&env, Error::GovernanceWindowNotElapsed);
            }
        }

        let delay = env
            .storage()
            .instance()
            .get::<DataKey, u32>(&DataKey::MinReviewDelay)
            .unwrap_or(0);

        let elapsed = env
            .ledger()
            .sequence()
            .checked_sub(course.created_at_ledger)
            .unwrap_or(0);

        if elapsed < delay {
            soroban_sdk::panic_with_error!(&env, Error::GovernanceWindowNotElapsed);
        }

        // Issue 167: High-value course co-signature
        let threshold: Option<i128> = env
            .storage()
            .instance()
            .get(&DataKey2::HighValueCourseThreshold);
        if let Some(threshold_val) = threshold {
            if course.price >= threshold_val {
                let approval_key = DataKey2::HighValueCourseApproval(course_id.clone());
                let prior_approver: Option<Address> = env.storage().persistent().get(&approval_key);
                match prior_approver {
                    None => {
                        // Record first approval and return — need a second admin.
                        // Use persistent storage so the pending approval survives instance TTL bumps.
                        env.storage().persistent().set(&approval_key, &caller);
                        env.storage().persistent().extend_ttl(
                            &approval_key,
                            Self::PERSISTENT_TTL_THRESHOLD,
                            Self::PERSISTENT_TTL_EXTEND_TO,
                        );
                        return;
                    }
                    Some(ref prior) if *prior == caller => {
                        soroban_sdk::panic_with_error!(&env, Error::HighValueApprovalConflict);
                    }
                    Some(_) => {
                        // Different admin — proceed and clear the approval key.
                        env.storage().persistent().remove(&approval_key);
                    }
                }
            }
        }

        course.status = CourseStatus::Active;
        course.last_updated_ledger = env.ledger().sequence();
        Self::persist_course(&env, &course_id, &course);

        let pending_key = DataKey2::InstructorPendingCountPersistent(course.instructor.clone());
        let pending_count = Self::read_instructor_pending_course_count(&env, &course.instructor);
        let new_pending_count = pending_count
            .checked_sub(1)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CounterUnderflow));
        env.storage()
            .persistent()
            .set(&pending_key, &new_pending_count);
        env.storage().persistent().extend_ttl(
            &pending_key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        env.events().publish(
            (Symbol::new(&env, "course_approved"), course_id.clone()),
            (
                course_id,
                course.instructor,
                caller,
                env.ledger().sequence(),
            ),
        );
    }

    /// Admin or approved approver rejects a Pending course, transitioning it to Rejected status.
    ///
    /// # Arguments
    /// - `caller`    — must be admin or an approved approver
    /// - `course_id` — the course to reject
    /// - `reason`    — rejection reason (e.g., "CONTENT_POLICY_VIOLATION", "DUPLICATE_COURSE")
    /// Sets the price threshold above which a course requires two different admins
    /// to approve (co-signature). Issue #167.
    pub fn set_high_value_course_threshold(env: Env, admin: Address, threshold: i128) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "set_high_value_course_threshold");
        if threshold <= 0 {
            soroban_sdk::panic_with_error!(&env, Error::InvalidFeeConfig);
        }
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);
        env.storage()
            .instance()
            .set(&DataKey2::HighValueCourseThreshold, &threshold);

        env.events().publish(
            (Symbol::new(&env, "high_value_threshold_set"), admin.clone()),
            (admin, threshold, env.ledger().sequence()),
        );
    }

    /// Returns the high-value course threshold, if set. Issue #167.
    pub fn get_high_value_course_threshold(env: Env) -> Option<i128> {
        env.storage()
            .instance()
            .get(&DataKey2::HighValueCourseThreshold)
    }

    pub fn reject_course(env: Env, caller: Address, course_id: String, reason: String) {
        caller.require_auth();
        Self::require_admin_or_approver(&env, &caller, "reject_course");
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CourseNotFound));

        if course.status != CourseStatus::Pending {
            soroban_sdk::panic_with_error!(&env, Error::CourseNotPending);
        }

        course.status = CourseStatus::Rejected;
        course.last_updated_ledger = env.ledger().sequence();
        Self::persist_course(&env, &course_id, &course);

        // Clear any pending high-value approval so a later re-registration
        // cannot inherit a stale first-approval from the rejected state.
        env.storage()
            .persistent()
            .remove(&DataKey2::HighValueCourseApproval(course_id.clone()));

        // #376 — Decrement the total course count so a rejected course does
        // not permanently occupy a slot against MaxCoursesPerInstructor.
        let count_key = DataKey2::InstructorCourseCountPersistent(course.instructor.clone());
        let count = Self::read_instructor_course_count(&env, &course.instructor);
        let new_count = count
            .checked_sub(1)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CounterUnderflow));
        env.storage().persistent().set(&count_key, &new_count);
        env.storage().persistent().extend_ttl(
            &count_key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        // The course was Pending when rejected, so also release the pending slot.
        let pending_key = DataKey2::InstructorPendingCountPersistent(course.instructor.clone());
        let pending_count = Self::read_instructor_pending_course_count(&env, &course.instructor);
        let new_pending_count = pending_count
            .checked_sub(1)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CounterUnderflow));
        env.storage()
            .persistent()
            .set(&pending_key, &new_pending_count);
        env.storage().persistent().extend_ttl(
            &pending_key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        env.events().publish(
            (Symbol::new(&env, "course_rejected"), course_id.clone()),
            (
                course_id,
                course.instructor,
                caller,
                reason,
                env.ledger().sequence(),
            ),
        );
    }

    /// Instructor resubmits a Rejected course for review, moving it back to
    /// Pending and resetting the governance review clock.
    ///
    /// This avoids burning the original course ID: the instructor keeps their
    /// existing backend record and simply re-enters the approval queue.
    ///
    /// # Arguments
    /// - `instructor` — must be the course owner and must sign
    /// - `course_id`  — the previously-rejected course to resubmit
    pub fn resubmit_course(env: Env, instructor: Address, course_id: String) {
        instructor.require_auth();
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        if Self::is_instructor_frozen_internal(&env, &instructor) {
            soroban_sdk::panic_with_error!(&env, Error::InstructorFrozen);
        }

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CourseNotFound));

        if course.instructor != instructor {
            soroban_sdk::panic_with_error!(&env, Error::Unauthorized);
        }

        if course.status != CourseStatus::Rejected {
            soroban_sdk::panic_with_error!(&env, Error::CourseNotPending);
        }

        // Check the pending slot limit before re-entering the queue.
        let max_courses: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MaxCoursesPerInstructor)
            .unwrap_or(50);
        let current_pending = Self::read_instructor_pending_course_count(&env, &instructor);
        if current_pending >= max_courses {
            soroban_sdk::panic_with_error!(&env, Error::InstructorCourseLimitReached);
        }

        // Also reclaim the total-count slot (rejected courses don't consume a
        // slot against MaxCoursesPerInstructor, so this re-registers the course
        // in the live count).
        let current_count = Self::read_instructor_course_count(&env, &instructor);
        if current_count >= max_courses {
            soroban_sdk::panic_with_error!(&env, Error::InstructorCourseLimitReached);
        }

        // Reset the review clock so the MinReviewDelay governance window is
        // measured from the resubmission, not the original registration.
        course.status = CourseStatus::Pending;
        course.created_at_ledger = env.ledger().sequence();
        course.last_updated_ledger = env.ledger().sequence();
        Self::persist(&env, &DataKey::Course(course_id.clone()), &course);

        // Increment both the total and pending counters.
        let count_key = DataKey2::InstructorCourseCountPersistent(instructor.clone());
        env.storage()
            .persistent()
            .set(&count_key, &(current_count + 1));
        env.storage().persistent().extend_ttl(
            &count_key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        let pending_key = DataKey2::InstructorPendingCountPersistent(instructor.clone());
        env.storage()
            .persistent()
            .set(&pending_key, &(current_pending + 1));
        env.storage().persistent().extend_ttl(
            &pending_key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        env.events().publish(
            (Symbol::new(&env, "course_resubmitted"), course_id.clone()),
            (course_id, instructor, env.ledger().sequence()),
        );
    }

    /// Instructor, admin, or moderator pauses a course.
    /// Existing enrollments are unaffected — students can still access content.
    /// New enrollments are blocked until the course is unpaused.
    pub fn pause_course(env: Env, caller: Address, course_id: String) {
        caller.require_auth();

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CourseNotFound));

        let is_admin = Self::is_admin(&env, &caller);
        let is_instructor = caller == course.instructor;
        let is_moderator = Self::is_moderator(&env, &caller);

        if !is_admin && !is_instructor && !is_moderator {
            soroban_sdk::panic_with_error!(&env, Error::Unauthorized);
        }
        if is_admin {
            Self::require_admin(&env, &caller, "pause_course");
        }

        if !is_admin
            && is_instructor
            && Self::is_instructor_frozen_internal(&env, &course.instructor)
        {
            soroban_sdk::panic_with_error!(&env, Error::InstructorFrozen);
        }

        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        if course.status != CourseStatus::Active {
            soroban_sdk::panic_with_error!(&env, Error::CourseNotActive);
        }

        course.status = CourseStatus::Paused;
        course.paused_by = if is_admin {
            PausedBy::Admin
        } else if is_moderator {
            PausedBy::Moderator
        } else {
            PausedBy::Instructor
        };
        course.last_updated_ledger = env.ledger().sequence();
        Self::persist_course(&env, &course_id, &course);

        env.events().publish(
            (Symbol::new(&env, "course_paused"), course_id.clone()),
            (
                course_id,
                CourseStatus::Paused,
                caller,
                env.ledger().sequence(),
            ),
        );
    }

    /// Instructor, admin, or moderator unpauses a Paused course, restoring it to Active.
    pub fn unpause_course(env: Env, caller: Address, course_id: String) {
        caller.require_auth();

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CourseNotFound));

        let is_admin = Self::is_admin(&env, &caller);
        let is_instructor = caller == course.instructor;
        let is_moderator = Self::is_moderator(&env, &caller);

        if !is_admin && !is_instructor && !is_moderator {
            soroban_sdk::panic_with_error!(&env, Error::Unauthorized);
        }

        if is_admin {
            Self::require_admin(&env, &caller, "unpause_course");
        }

        // A frozen instructor must not be able to undo the pause themselves.
        if !is_admin
            && is_instructor
            && Self::is_instructor_frozen_internal(&env, &course.instructor)
        {
            soroban_sdk::panic_with_error!(&env, Error::InstructorFrozen);
        }

        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        if course.status != CourseStatus::Paused {
            soroban_sdk::panic_with_error!(&env, Error::CourseNotPaused);
        }

        if (course.paused_by == PausedBy::Admin || course.paused_by == PausedBy::Moderator)
            && !is_admin
            && !is_moderator
        {
            soroban_sdk::panic_with_error!(&env, Error::Unauthorized);
        }

        course.status = CourseStatus::Active;
        course.paused_by = PausedBy::None;
        course.last_updated_ledger = env.ledger().sequence();
        Self::persist_course(&env, &course_id, &course);

        env.events().publish(
            (Symbol::new(&env, "course_unpaused"), course_id.clone()),
            (
                course_id,
                CourseStatus::Active,
                caller,
                env.ledger().sequence(),
            ),
        );
    }

    /// Transfer course ownership from the current instructor to a new instructor.
    ///
    /// Only the current instructor may initiate the transfer, and the platform
    /// admin must co-approve in the same invocation. The course record is
    /// updated in place: enrollment history, earnings already credited to the
    /// previous instructor, and all other course state are left untouched.
    /// The course is neither archived nor re-registered.
    ///
    /// # Arguments
    /// - `instructor`     — current course instructor (must sign)
    /// - `admin`          — platform admin (must sign; co-approval)
    /// - `course_id`      — the course to transfer
    /// - `new_instructor` — address that will become the course instructor
    pub fn transfer_course(
        env: Env,
        instructor: Address,
        admin: Address,
        course_id: String,
        new_instructor: Address,
    ) {
        instructor.require_auth();
        admin.require_auth();
        new_instructor.require_auth();

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CourseNotFound));

        if instructor != course.instructor {
            soroban_sdk::panic_with_error!(&env, Error::Unauthorized);
        }

        Self::require_admin(&env, &admin, "transfer_course");
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        if course.status == CourseStatus::Archived {
            soroban_sdk::panic_with_error!(&env, Error::ArchivedCourseImmutable);
        }

        if new_instructor == course.instructor {
            soroban_sdk::panic_with_error!(&env, Error::InvalidCourseTransfer);
        }

        // Reject addresses that must not become instructor: admin, treasury,
        // or any existing co-instructor (who would otherwise be paid twice).
        if Self::is_admin(&env, &new_instructor) {
            soroban_sdk::panic_with_error!(&env, Error::InvalidCourseTransfer);
        }
        let treasury = Self::read_treasury(&env);
        if new_instructor == treasury {
            soroban_sdk::panic_with_error!(&env, Error::InvalidCourseTransfer);
        }
        for co in course.co_instructors.iter() {
            if co.address == new_instructor {
                soroban_sdk::panic_with_error!(&env, Error::InvalidCourseTransfer);
            }
        }

        if Self::is_instructor_frozen_internal(&env, &instructor) {
            soroban_sdk::panic_with_error!(&env, Error::InstructorFrozen);
        }

        if Self::is_instructor_frozen_internal(&env, &new_instructor) {
            soroban_sdk::panic_with_error!(&env, Error::InstructorFrozen);
        }

        let max_courses: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MaxCoursesPerInstructor)
            .unwrap_or(50);

        let new_count_key = DataKey2::InstructorCourseCountPersistent(new_instructor.clone());
        let new_count = Self::read_instructor_course_count(&env, &new_instructor);
        if new_count >= max_courses {
            soroban_sdk::panic_with_error!(&env, Error::InstructorCourseLimitReached);
        }

        if course.status == CourseStatus::Pending {
            let new_pending_key =
                DataKey2::InstructorPendingCountPersistent(new_instructor.clone());
            let new_pending = Self::read_instructor_pending_course_count(&env, &new_instructor);
            if new_pending >= Self::effective_max_pending_courses(&env, max_courses) {
                soroban_sdk::panic_with_error!(&env, Error::InstructorCourseLimitReached);
            }
        }

        let previous_instructor = course.instructor.clone();

        course.instructor = new_instructor.clone();
        course.last_updated_ledger = env.ledger().sequence();
        Self::persist_course(&env, &course_id, &course);

        let old_count_key = DataKey2::InstructorCourseCountPersistent(previous_instructor.clone());
        let old_count = Self::read_instructor_course_count(&env, &previous_instructor);
        let decremented_old = old_count
            .checked_sub(1)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CounterUnderflow));
        env.storage()
            .persistent()
            .set(&old_count_key, &decremented_old);
        env.storage().persistent().extend_ttl(
            &old_count_key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );
        env.storage()
            .persistent()
            .set(&new_count_key, &(new_count + 1));
        env.storage().persistent().extend_ttl(
            &new_count_key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        if course.status == CourseStatus::Pending {
            let old_pending_key =
                DataKey2::InstructorPendingCountPersistent(previous_instructor.clone());
            let old_pending =
                Self::read_instructor_pending_course_count(&env, &previous_instructor);
            let decremented_pending = old_pending
                .checked_sub(1)
                .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CounterUnderflow));
            env.storage()
                .persistent()
                .set(&old_pending_key, &decremented_pending);
            env.storage().persistent().extend_ttl(
                &old_pending_key,
                Self::PERSISTENT_TTL_THRESHOLD,
                Self::PERSISTENT_TTL_EXTEND_TO,
            );

            let new_pending_key =
                DataKey2::InstructorPendingCountPersistent(new_instructor.clone());
            let new_pending = Self::read_instructor_pending_course_count(&env, &new_instructor);
            env.storage()
                .persistent()
                .set(&new_pending_key, &(new_pending + 1));
            env.storage().persistent().extend_ttl(
                &new_pending_key,
                Self::PERSISTENT_TTL_THRESHOLD,
                Self::PERSISTENT_TTL_EXTEND_TO,
            );
        }

        // InstructorCourseList is append-only for the previous instructor
        // (it records every course they ever registered). Append the course
        // to the new instructor's list so they can query it going forward.
        let new_list_key = DataKey::InstructorCourseList(new_instructor.clone());
        let mut new_instructor_courses: Vec<String> = env
            .storage()
            .persistent()
            .get(&new_list_key)
            .unwrap_or_else(|| Vec::new(&env));
        new_instructor_courses.push_back(course_id.clone());
        Self::persist(&env, &new_list_key, &new_instructor_courses);

        env.events().publish(
            (Symbol::new(&env, "course_transferred"), course_id.clone()),
            (
                course_id,
                previous_instructor,
                new_instructor,
                admin,
                env.ledger().sequence(),
            ),
        );
    }

    /// Admin archives a course permanently and optionally issues refunds.
    /// This is the primary refund mechanism in Hamplard — only admins can initiate refunds.
    ///
    /// **Refund Behavior:**
    /// - If `students_to_refund` is provided, the contract refunds each listed student
    ///   their full enrollment amount (both platform fee and instructor share)
    /// - Only incomplete enrollments are refunded (where `completed == false`)
    /// - Refunds are proportionally sourced from the platform treasury and instructor account
    /// - There is no time window restriction — refunds can be issued at any point
    ///   so long as the course is paused before archiving
    ///
    /// **Multi-admin Authorization:**
    /// Both `admin1` and `admin2` must sign this transaction (multi-sig requirement).
    /// This prevents a single compromised admin from unilaterally refunding or archiving courses.
    ///
    /// **Course State Requirement:**
    /// The course must be in `Paused` status before archiving. This is a safety mechanism
    /// to prevent accidental archive of active courses. Pause first, then archive.
    ///
    /// # Arguments
    /// - `admin1`              — primary admin address (must sign)
    /// - `admin2`              — secondary admin address (must sign)
    /// - `course_id`           — the course to archive
    /// - `students_to_refund`  — optional list of student addresses to refund
    pub fn archive_course(
        env: Env,
        admin1: Address,
        admin2: Address,
        course_id: String,
        students_to_refund: Option<Vec<Address>>,
    ) {
        admin1.require_auth();
        admin2.require_auth();
        Self::require_multi_admin(&env, &admin1, &admin2);
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CourseNotFound));

        if !matches!(
            &course.status,
            &CourseStatus::Paused | &CourseStatus::Pending | &CourseStatus::Rejected
        ) {
            soroban_sdk::panic_with_error!(&env, Error::CourseMustBePausedToArchive);
        }

        if let Some(ref students) = students_to_refund {
            if students.len() > Self::MAX_STUDENTS_TO_REFUND {
                soroban_sdk::panic_with_error!(&env, Error::TooManyRefundStudents);
            }
        }

        let mut refund_count = 0u32;
        let mut total_refunded = 0i128;

        if let Some(ref students) = students_to_refund {
            for student in students.iter() {
                let enrollment_key = DataKey::Enrollment(student.clone(), course_id.clone());
                if env.storage().persistent().has(&enrollment_key) {
                    let enrollment: Enrollment =
                        env.storage().persistent().get(&enrollment_key).unwrap();

                    if !enrollment.completed && !enrollment.is_refunded {
                        let breakdown = Self::refund_enrollment(&env, &mut course, &student, None);

                        refund_count = refund_count.checked_add(1).unwrap_or_else(|| {
                            soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow)
                        });
                        total_refunded = total_refunded
                            .checked_add(breakdown.refund_amount)
                            .unwrap_or_else(|| {
                                soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow)
                            });

                        let mut stats = Self::read_course_stats(&env, &course_id, &course);
                        // Capacity is a lifetime cap on non-refunded
                        // enrollments, so a refund releases the seat. (#324)
                        if stats.total_enrollments > 0 {
                            stats.total_enrollments -= 1;
                        }
                        Self::write_course_stats(&env, &course_id, &stats);
                        course.active_enrollments = stats.active_enrollments;
                        course.total_enrollments = stats.total_enrollments;

                        // Decrement total_students in InstructorStats so
                        // archive-refunded enrollments no longer count toward
                        // the instructor's reputation. (#323)
                        Self::update_instructor_stats(&env, &course.instructor, |s| {
                            s.total_students = s.total_students.saturating_sub(1);
                        });

                        Self::emit_refund_issued(
                            &env,
                            &course_id,
                            &student,
                            breakdown.refund_amount,
                            breakdown.platform_refund,
                            breakdown.instructor_refund,
                        );

                        // Resolve any pending refund request so it cannot get stuck
                        // in Pending after archive_course() refunds and removes the
                        // enrollment. (#321)
                        let refund_req_key =
                            DataKey::RefundRequest(student.clone(), course_id.clone());
                        if let Some(mut refund_req) = env
                            .storage()
                            .persistent()
                            .get::<DataKey, RefundRequest>(&refund_req_key)
                        {
                            if refund_req.status == RefundStatus::Pending {
                                refund_req.status = RefundStatus::Approved;
                                refund_req.refund_amount = Some(breakdown.refund_amount);
                                Self::persist(&env, &refund_req_key, &refund_req);
                            }
                        }

                        Self::promote_from_waitlist(&env, &course_id);
                    }
                }
            }
        }

        // Record this batch's refunds in the course's cumulative stats.
        if refund_count > 0 {
            course.refund_count = course
                .refund_count
                .checked_add(refund_count)
                .unwrap_or_else(|| panic!("course refund_count overflow"));
            course.total_refunded = course
                .total_refunded
                .checked_add(total_refunded)
                .unwrap_or_else(|| panic!("course total_refunded overflow"));
        }

        // Only archive when there are no remaining active enrollments.
        // If students remain enrolled, persist the decremented counts so
        // follow-up refund batches observe the updated course state.
        if course.active_enrollments > 0 {
            Self::persist_course(&env, &course_id, &course);
            return;
        }

        // #376 — Capture the status before we overwrite it; we need to know
        // whether to also release the pending slot.
        let pre_archive_status = course.status.clone();

        course.status = CourseStatus::Archived;
        course.archived_at_ledger = Some(env.ledger().sequence());
        course.last_updated_ledger = env.ledger().sequence();

        // Clear any pending high-value approval; the course is immutable after archiving.
        env.storage()
            .persistent()
            .remove(&DataKey2::HighValueCourseApproval(course_id.clone()));

        Self::persist(&env, &DataKey::ArchivedCourse(course_id.clone()), &course);

        env.storage()
            .persistent()
            .remove(&DataKey::Course(course_id.clone()));

        // #376 — Decrement the total course count so an archived course does
        // not permanently occupy a slot against MaxCoursesPerInstructor.
        let count_key = DataKey2::InstructorCourseCountPersistent(course.instructor.clone());
        let count = Self::read_instructor_course_count(&env, &course.instructor);
        let new_count = count
            .checked_sub(1)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CounterUnderflow));
        env.storage().persistent().set(&count_key, &new_count);
        env.storage().persistent().extend_ttl(
            &count_key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        // If the archived course was still Pending, release the pending slot too.
        if pre_archive_status == CourseStatus::Pending {
            let pending_key = DataKey2::InstructorPendingCountPersistent(course.instructor.clone());
            let pending_count =
                Self::read_instructor_pending_course_count(&env, &course.instructor);
            let new_pending_count = pending_count
                .checked_sub(1)
                .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CounterUnderflow));
            env.storage()
                .persistent()
                .set(&pending_key, &new_pending_count);
            env.storage().persistent().extend_ttl(
                &pending_key,
                Self::PERSISTENT_TTL_THRESHOLD,
                Self::PERSISTENT_TTL_EXTEND_TO,
            );
        }
        // #697 — Remove per-course storage entries that are no longer needed
        // once a course is archived to avoid persistent rent leakage.
        env.storage()
            .persistent()
            .remove(&DataKey::CourseWaitlist(course_id.clone()));
        env.storage()
            .persistent()
            .remove(&DataKey::CourseFailureCount(course_id.clone()));
        env.storage()
            .instance()
            .remove(&DataKey2::HighValueCourseApproval(course_id.clone()));

        // Include refund accounting in the archive event so indexers can reconcile
        // the operation without replaying every enrollment history entry.
        env.events().publish(
            (Symbol::new(&env, "course_archived"), course_id.clone()),
            (
                course_id.clone(),
                admin1.clone(),
                admin2.clone(),
                refund_count,
                total_refunded,
            ),
        );
    }

    /// Cancel a pending treasury update before it takes effect.
    ///
    /// Requires both admin signatures, mirroring `update_treasury`. Once
    /// cancelled, the current `Treasury` address remains in effect and all
    /// future enrollments continue routing platform fees to it. Panics if
    /// there is no pending treasury update to cancel.
    ///
    /// # Arguments
    /// - `admin1` — first admin (must sign)
    /// - `admin2` — second admin (must sign)
    pub fn cancel_treasury_update(env: Env, admin1: Address, admin2: Address) {
        admin1.require_auth();
        admin2.require_auth();
        Self::require_multi_admin(&env, &admin1, &admin2);
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        let pending: TreasuryUpdate = env
            .storage()
            .instance()
            .get(&DataKey::PendingTreasury)
            .unwrap_or_else(|| panic!("no pending treasury update"));

        env.storage().instance().remove(&DataKey::PendingTreasury);

        env.events().publish(
            (
                Symbol::new(&env, "treasury_update_cancelled"),
                admin1.clone(),
            ),
            (pending.address, pending.effective_ledger, admin1, admin2),
        );
    }

    /// Instructor or admin updates course details (e.g. price, capacity).
    /// Increments the course version so prior student enrollments remain bound to
    /// their original enrollment terms.
    pub fn update_course(
        env: Env,
        caller: Address,
        course_id: String,
        new_price: Option<i128>,
        new_max_capacity: Option<Option<u32>>,
        new_capacity_mode: Option<CapacityMode>,
    ) -> u32 {
        caller.require_auth();

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CourseNotFound));

        let is_admin = Self::is_admin(&env, &caller);
        let is_instructor = caller == course.instructor;

        if !is_admin && !is_instructor {
            soroban_sdk::panic_with_error!(&env, Error::Unauthorized);
        }

        if !is_admin && Self::is_instructor_frozen_internal(&env, &course.instructor) {
            soroban_sdk::panic_with_error!(&env, Error::InstructorFrozen);
        }

        if course.status == CourseStatus::Archived {
            soroban_sdk::panic_with_error!(&env, Error::ArchivedCourseImmutable);
        }
        // A rejected course has no live listing to update; changing its terms
        // or bumping its version would emit a misleading course_updated event.
        // Re-submission creates a new course record instead (#386).
        if course.status == CourseStatus::Rejected {
            soroban_sdk::panic_with_error!(&env, Error::CourseNotActive);
        }

        // Snapshot the values that may change so the event can carry a diff.
        let old_price = course.price;
        let old_max_capacity = course.max_capacity;

        let mut modified = false;

        if let Some(price) = new_price {
            if price < 0 {
                soroban_sdk::panic_with_error!(&env, Error::InvalidPrice);
            }
            if price != 0
                && (price < Self::MIN_COURSE_PRICE_STROOPS
                    || price > Self::MAX_COURSE_PRICE_STROOPS)
            {
                soroban_sdk::panic_with_error!(&env, Error::InvalidPrice);
            }
            course.price = price;
            modified = true;
            // A price change may move the course across the high-value threshold;
            // clear any pending first-approval so it cannot be inherited at the new price.
            env.storage()
                .persistent()
                .remove(&DataKey2::HighValueCourseApproval(course_id.clone()));
        }

        if let Some(mode) = new_capacity_mode {
            course.capacity_mode = mode;
            modified = true;
            // #702 — switching mode redefines which counter max_capacity caps.
            // Validate against the existing cap when no new cap is supplied.
            if new_max_capacity.is_none() {
                if let Some(cap) = course.max_capacity {
                    let current_enrollments = match course.capacity_mode {
                        CapacityMode::Lifetime => course.total_enrollments,
                        CapacityMode::Concurrent => course.active_enrollments,
                    };
                    if cap < current_enrollments {
                        soroban_sdk::panic_with_error!(&env, Error::InvalidCourseInput);
                    }
                }
            }
        }

        if let Some(capacity) = new_max_capacity {
            if capacity == Some(0) {
                soroban_sdk::panic_with_error!(&env, Error::InvalidCourseInput);
            }
            if let Some(cap) = capacity {
                let current_enrollments = match course.capacity_mode {
                    CapacityMode::Lifetime => course.total_enrollments,
                    CapacityMode::Concurrent => course.active_enrollments,
                };
                if cap < current_enrollments {
                    soroban_sdk::panic_with_error!(&env, Error::InvalidCourseInput);
                }
            }
            course.max_capacity = capacity;
            modified = true;
        }

        if !modified {
            return course.version;
        }

        course.version = course
            .version
            .checked_add(1)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow));
        course.last_updated_ledger = env.ledger().sequence();

        Self::persist(&env, &DataKey::Course(course_id.clone()), &course);
        Self::save_course_terms(&env, &course);

        env.events().publish(
            (Symbol::new(&env, "course_updated"), course_id.clone()),
            (
                course_id,
                course.version,
                caller,
                old_price,
                course.price,
                old_max_capacity,
                course.max_capacity,
            ),
        );

        course.version
    }

    /// Remove the capacity cap on a course, making it unlimited.
    /// Can be called by the course instructor or an admin.
    pub fn clear_course_max_capacity(env: Env, caller: Address, course_id: String) {
        caller.require_auth();

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CourseNotFound));

        let is_admin = Self::is_admin(&env, &caller);
        let is_instructor = caller == course.instructor;

        if !is_admin && !is_instructor {
            soroban_sdk::panic_with_error!(&env, Error::Unauthorized);
        }

        if !is_admin && Self::is_instructor_frozen_internal(&env, &course.instructor) {
            soroban_sdk::panic_with_error!(&env, Error::InstructorFrozen);
        }

        if course.status == CourseStatus::Archived {
            soroban_sdk::panic_with_error!(&env, Error::ArchivedCourseImmutable);
        }

        let old_max_capacity = course.max_capacity;
        let old_price = course.price;

        course.max_capacity = None;
        course.version = course
            .version
            .checked_add(1)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow));
        course.last_updated_ledger = env.ledger().sequence();

        Self::persist(&env, &DataKey::Course(course_id.clone()), &course);
        Self::save_course_terms(&env, &course);

        env.events().publish(
            (Symbol::new(&env, "course_updated"), course_id.clone()),
            (
                course_id,
                course.version,
                caller,
                old_price,
                course.price,
                old_max_capacity,
                course.max_capacity,
            ),
        );
    }

    /// Instructor configures an optional enrollment expiry for a course.
    ///
    /// After `expiry_ledgers` ledger sequences from the moment a student
    /// enrolls, that enrollment is considered expired and cannot be marked
    /// as completed. Set to `None` to remove the expiry.
    ///
    /// # Arguments
    /// - `caller`         — must be the course instructor or admin
    /// - `course_id`      — the course to update
    /// - `expiry_ledgers` — optional expiry duration in ledger sequences
    pub fn set_enrollment_expiry(
        env: Env,
        caller: Address,
        course_id: String,
        expiry_ledgers: Option<u32>,
    ) {
        caller.require_auth();

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CourseNotFound));

        // One authorization path and one frozen-instructor guard (#639).
        Self::require_admin_or_instructor(&env, &caller, &course);
        if !Self::is_admin(&env, &caller)
            && Self::is_instructor_frozen_internal(&env, &course.instructor)
        {
            soroban_sdk::panic_with_error!(&env, Error::InstructorFrozen);
        }

        course.enrollment_expiry_ledgers = expiry_ledgers;
        course.last_updated_ledger = env.ledger().sequence();
        Self::persist_course(&env, &course_id, &course);

        env.events().publish(
            (
                Symbol::new(&env, "enrollment_expiry_set"),
                course_id.clone(),
            ),
            (course_id, expiry_ledgers),
        );
    }

    /// Instructor or admin sets or clears the course expiry ledger.
    ///
    /// A configured expiry must be strictly after the current ledger sequence
    /// and strictly after `enrollment_start_ledger` (when set), so setting it
    /// can never make an otherwise enrollable course immediately unusable.
    /// Passing `None` clears the expiry (#698).
    ///
    /// # Arguments
    /// - `caller`              — must be the course instructor or admin
    /// - `course_id`           — the course to update
    /// - `expires_at_ledger`   — optional ledger sequence when course expires (no further enrollments)
    pub fn set_course_expiry(
        env: Env,
        caller: Address,
        course_id: String,
        expires_at_ledger: Option<u32>,
    ) {
        caller.require_auth();

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CourseNotFound));

        let is_admin = Self::is_admin(&env, &caller);
        let is_instructor = caller == course.instructor;

        if !is_admin && !is_instructor {
            soroban_sdk::panic_with_error!(&env, Error::Unauthorized);
        }

        if !is_admin && Self::is_instructor_frozen_internal(&env, &course.instructor) {
            soroban_sdk::panic_with_error!(&env, Error::InstructorFrozen);
        }

        if course.status == CourseStatus::Archived {
            soroban_sdk::panic_with_error!(&env, Error::ArchivedCourseImmutable);
        }

        // #698 — An expiry at or before "now", or one that closes enrollment
        // before it even opens, silently makes the course unenrollable.
        if let Some(expiry) = expires_at_ledger {
            if expiry <= env.ledger().sequence() {
                soroban_sdk::panic_with_error!(&env, Error::InvalidCourseInput);
            }
            if let Some(start) = course.enrollment_start_ledger {
                if expiry <= start {
                    soroban_sdk::panic_with_error!(&env, Error::InvalidCourseInput);
                }
            }
        }

        course.expires_at_ledger = expires_at_ledger;
        course.last_updated_ledger = env.ledger().sequence();
        Self::persist_course(&env, &course_id, &course);

        env.events().publish(
            (Symbol::new(&env, "course_expiry_set"), course_id.clone()),
            (course_id, expires_at_ledger),
        );
    }

    /// Instructor or admin configures the list of prerequisite course IDs
    /// that a student must have completed — with an issued, non-revoked
    /// certificate — before they may enroll in this course.
    ///
    /// Pass an empty list to remove all prerequisites. The list is bounded to
    /// `MAX_PREREQUISITE_COURSES`, de-duplicated, restricted to Active courses,
    /// and may not form a direct cycle with another course (#699).
    ///
    /// # Arguments
    /// - `caller`                  — must be the course instructor or admin
    /// - `course_id`               — the course to update
    /// - `prerequisite_course_ids` — course IDs that must be completed first
    pub fn set_prerequisite_courses(
        env: Env,
        caller: Address,
        course_id: String,
        prerequisite_course_ids: Vec<String>,
    ) {
        caller.require_auth();

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CourseNotFound));

        // One authorization path and one frozen-instructor guard (#639).
        Self::require_admin_or_instructor(&env, &caller, &course);
        if !Self::is_admin(&env, &caller)
            && Self::is_instructor_frozen_internal(&env, &course.instructor)
        {
            soroban_sdk::panic_with_error!(&env, Error::InstructorFrozen);
        }

        if course.status == CourseStatus::Archived {
            soroban_sdk::panic_with_error!(&env, Error::ArchivedCourseImmutable);
        }

        // #699 — Bound the list because it is walked on every enrollment.
        if prerequisite_course_ids.len() > Self::MAX_PREREQUISITE_COURSES {
            soroban_sdk::panic_with_error!(&env, Error::InvalidPrerequisite);
        }

        let mut unique_prerequisites: Vec<String> = Vec::new(&env);
        for i in 0..prerequisite_course_ids.len() {
            let prereq_id = prerequisite_course_ids.get(i).unwrap();

            if prereq_id == course_id {
                soroban_sdk::panic_with_error!(&env, Error::InvalidPrerequisite);
            }

            let mut duplicate = false;
            for existing in unique_prerequisites.iter() {
                if existing == prereq_id {
                    duplicate = true;
                    break;
                }
            }
            if duplicate {
                continue;
            }

            let prereq = Self::get_course_internal(&env, &prereq_id).unwrap_or_else(|| {
                soroban_sdk::panic_with_error!(&env, Error::InvalidPrerequisite)
            });
            if prereq.status != CourseStatus::Active {
                soroban_sdk::panic_with_error!(&env, Error::InvalidPrerequisite);
            }
            for other in prereq.prerequisite_course_ids.iter() {
                if other == course_id.clone() {
                    soroban_sdk::panic_with_error!(&env, Error::InvalidPrerequisite);
                }
            }

            unique_prerequisites.push_back(prereq_id);
        }

        course.prerequisite_course_ids = unique_prerequisites.clone();
        course.last_updated_ledger = env.ledger().sequence();
        Self::persist_course(&env, &course_id, &course);

        env.events().publish(
            (
                Symbol::new(&env, "prerequisite_courses_set"),
                course_id.clone(),
            ),
            (course_id, unique_prerequisites),
        );
    }

    /// Update the content hash commitment for a course.
    ///
    /// Only the course instructor or the platform admin may call this.
    /// The course must not be Archived — content updates on a retired course
    /// have no practical effect and are rejected to avoid misleading auditors.
    ///
    /// # Arguments
    /// - `caller`       — instructor or admin address (must sign)
    /// - `course_id`    — the course whose hash is being updated
    /// - `content_hash` — new 32-byte hash of the off-chain course content
    pub fn update_content_hash(
        env: Env,
        caller: Address,
        course_id: String,
        content_hash: BytesN<32>,
    ) {
        caller.require_auth();

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CourseNotFound));

        // One authorization path and one frozen-instructor guard (#639).
        // A frozen instructor may not alter the content commitment, but an
        // admin may still update it as an administrative intervention.
        Self::require_admin_or_instructor(&env, &caller, &course);
        if !Self::is_admin(&env, &caller)
            && Self::is_instructor_frozen_internal(&env, &course.instructor)
        {
            soroban_sdk::panic_with_error!(&env, Error::InstructorFrozen);
        }

        if course.status == CourseStatus::Archived {
            soroban_sdk::panic_with_error!(&env, Error::ArchivedCourseImmutable);
        }

        course.content_hash = content_hash.clone();
        course.last_updated_ledger = env.ledger().sequence();
        Self::persist_course(&env, &course_id, &course);

        env.events().publish(
            (Symbol::new(&env, "content_hash_updated"), course_id.clone()),
            (course_id, caller, content_hash),
        );
    }

    /// Set or update the maximum number of certificates for a course.
    ///
    /// Enforces a hard limit on certificate issuance to prevent credential dilution.
    /// Once the course reaches this limit, issue_certificate() will reject further
    /// issuance attempts.
    ///
    /// Only the course instructor or the platform admin may call this.
    /// The course must not be Archived.
    ///
    /// # Arguments
    /// - `caller`       — instructor or admin address (must sign)
    /// - `course_id`    — the course whose certificate limit is being set
    /// - `max_certs`    — optional maximum number of certificates. Pass None to remove the limit
    pub fn set_max_certificates(
        env: Env,
        caller: Address,
        course_id: String,
        max_certs: Option<u32>,
    ) {
        caller.require_auth();

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CourseNotFound));

        let is_admin = Self::is_admin(&env, &caller);
        let is_instructor = caller == course.instructor;

        if !is_admin && !is_instructor {
            soroban_sdk::panic_with_error!(&env, Error::Unauthorized);
        }

        if !is_admin && Self::is_instructor_frozen_internal(&env, &course.instructor) {
            soroban_sdk::panic_with_error!(&env, Error::InstructorFrozen);
        }

        if course.status == CourseStatus::Archived {
            soroban_sdk::panic_with_error!(&env, Error::ArchivedCourseImmutable);
        }

        // Validate: if setting a limit, it must be at least as high as the current
        // live (net-active) certificate count — issued minus already-revoked.
        if let Some(limit) = max_certs {
            let live_certs = course
                .certificates_issued
                .saturating_sub(course.certificates_revoked);
            if limit < live_certs {
                soroban_sdk::panic_with_error!(&env, Error::InvalidCourseInput);
            }
        }

        course.max_certificates = max_certs;
        course.last_updated_ledger = env.ledger().sequence();
        Self::persist_course(&env, &course_id, &course);

        env.events().publish(
            (Symbol::new(&env, "max_certificates_set"), course_id.clone()),
            (course_id, caller, max_certs),
        );
    }

    /// Configure the co-instructors of a course and their revenue shares.
    ///
    /// This is the first step of a two-step consent flow.  `set_co_instructors`
    /// records the proposed addresses and their shares as *pending invitations*;
    /// no address becomes active until it calls `accept_co_instructor()`.
    ///
    /// This design prevents arbitrary addresses (including exchange or contract
    /// addresses that cannot withdraw) from being attached to a course and
    /// credited earnings without their knowledge.
    ///
    /// `co_instructor_addresses` and `co_instructor_shares_bps` must have the
    /// same length. Shares are basis points of the instructor portion of each
    /// enrollment and must total at most 10 000 bps; any remainder is paid to
    /// the lead instructor. Once the course has received its first enrollment
    /// the split is locked so it cannot be changed retroactively.
    ///
    /// Calling `set_co_instructors` again replaces the entire pending list,
    /// clearing unaccepted invitations.  Already-accepted co-instructors in
    /// `course.co_instructors` are cleared and must re-accept from the new
    /// proposal.
    ///
    /// # Arguments
    /// - `caller`                    — course instructor or admin (must sign)
    /// - `course_id`                 — the course to configure
    /// - `co_instructor_addresses`   — co-instructor addresses to invite
    /// - `co_instructor_shares_bps`  — matching revenue shares in basis points
    pub fn set_co_instructors(
        env: Env,
        caller: Address,
        course_id: String,
        co_instructor_addresses: Vec<Address>,
        co_instructor_shares_bps: Vec<u32>,
    ) {
        caller.require_auth();

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| panic!("course not found"));

        let is_admin = Self::is_admin(&env, &caller);
        if !is_admin && caller != course.instructor {
            panic!("unauthorized");
        }

        if !is_admin && Self::is_instructor_frozen_internal(&env, &course.instructor) {
            panic!("instructor is frozen");
        }

        if course.status == CourseStatus::Archived {
            panic!("cannot update archived course");
        }

        // Lock only while non-refunded enrollments exist; once every
        // enrollment has been refunded, co-instructors can be reconfigured.
        if course.total_enrollments > 0 {
            panic!("co-instructors are locked after the first enrollment");
        }

        if co_instructor_addresses.len() != co_instructor_shares_bps.len() {
            panic!("co-instructor addresses and shares must have the same length");
        }

        if co_instructor_addresses.len() > Self::MAX_CO_INSTRUCTORS {
            panic!("too many co-instructors (maximum is 10)");
        }

        let admin: Option<Address> = env.storage().instance().get(&DataKey::Admin);
        let secondary_admin: Option<Address> =
            env.storage().instance().get(&DataKey::SecondaryAdmin);
        let treasury = Self::read_treasury(&env);

        let mut co_instructors: Vec<CoInstructor> = Vec::new(&env);
        let mut total_share_bps: u32 = 0;

        for i in 0..co_instructor_addresses.len() {
            let address = co_instructor_addresses.get(i).unwrap();
            let share_bps = co_instructor_shares_bps.get(i).unwrap();

            if share_bps > Self::MAX_SHARE_BPS {
                panic!("co-instructor share cannot exceed 10000 bps");
            }

            if address == course.instructor {
                panic!("lead instructor cannot be a co-instructor");
            }

            if address == env.current_contract_address() {
                panic!("contract address cannot be a co-instructor");
            }

            if admin.as_ref() == Some(&address) {
                panic!("admin cannot be a co-instructor");
            }

            if secondary_admin.as_ref() == Some(&address) {
                panic!("secondary admin cannot be a co-instructor");
            }

            if address == treasury {
                panic!("treasury cannot be a co-instructor");
            }

            for existing in co_instructors.iter() {
                if existing.address == address {
                    panic!("duplicate co-instructor address");
                }
            }

            total_share_bps = total_share_bps
                .checked_add(share_bps)
                .unwrap_or_else(|| panic!("co-instructor share overflow"));

            co_instructors.push_back(CoInstructor { address, share_bps });
        }

        if total_share_bps > Self::MAX_SHARE_BPS {
            panic!("total co-instructor shares cannot exceed 10000 bps");
        }

        // Clear the live co-instructor list — new assignments are not active
        // until each address calls accept_co_instructor().
        course.co_instructors = Vec::new(&env);
        course.last_updated_ledger = env.ledger().sequence();
        Self::persist_course(&env, &course_id, &course);

        // Evict any existing pending invitations for this course before
        // writing the new ones, so a re-proposal starts from a clean slate.
        let pending_list_key = DataKey2::PendingCoInstructorList(course_id.clone());
        let old_pending: Vec<Address> = env
            .storage()
            .persistent()
            .get(&pending_list_key)
            .unwrap_or_else(|| Vec::new(&env));
        for i in 0..old_pending.len() {
            let addr = old_pending.get(i).unwrap();
            env.storage()
                .persistent()
                .remove(&DataKey2::PendingCoInstructor(course_id.clone(), addr));
        }

        // Write new pending invitations.
        let mut pending_addresses: Vec<Address> = Vec::new(&env);
        for i in 0..co_instructors.len() {
            let co = co_instructors.get(i).unwrap();
            let inv_key = DataKey2::PendingCoInstructor(course_id.clone(), co.address.clone());
            env.storage().persistent().set(&inv_key, &co.share_bps);
            env.storage().persistent().extend_ttl(
                &inv_key,
                Self::PERSISTENT_TTL_THRESHOLD,
                Self::PERSISTENT_TTL_EXTEND_TO,
            );
            pending_addresses.push_back(co.address.clone());
        }
        env.storage()
            .persistent()
            .set(&pending_list_key, &pending_addresses);
        env.storage().persistent().extend_ttl(
            &pending_list_key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        env.events().publish(
            (
                Symbol::new(&env, "co_instructors_proposed"),
                course_id.clone(),
            ),
            (course_id, co_instructors, env.ledger().sequence()),
        );
    }

    /// Accept a co-instructor invitation for a course.
    ///
    /// After `set_co_instructors` proposes a list, each invited address must
    /// call this function to consent to the role.  Only then does the address
    /// appear in `course.co_instructors` and start receiving earnings from
    /// new enrollments.
    ///
    /// A co-instructor who does not accept is never credited earnings, so
    /// exchange addresses or contract addresses that cannot withdraw are
    /// never trapped with unreachable funds.
    ///
    /// # Arguments
    /// - `co_instructor` — the invited address (must sign)
    /// - `course_id`     — the course the invitation is for
    ///
    /// # Panics
    /// - If there is no pending invitation for `(course_id, co_instructor)`.
    pub fn accept_co_instructor(env: Env, co_instructor: Address, course_id: String) {
        co_instructor.require_auth();

        let inv_key = DataKey2::PendingCoInstructor(course_id.clone(), co_instructor.clone());

        let share_bps: u32 = env
            .storage()
            .persistent()
            .get(&inv_key)
            .unwrap_or_else(|| panic!("no pending co-instructor invitation"));

        // Remove the pending invitation entry.
        env.storage().persistent().remove(&inv_key);

        // Remove from the pending list.
        let list_key = DataKey2::PendingCoInstructorList(course_id.clone());
        let old_list: Vec<Address> = env
            .storage()
            .persistent()
            .get(&list_key)
            .unwrap_or_else(|| Vec::new(&env));
        let mut new_list: Vec<Address> = Vec::new(&env);
        for i in 0..old_list.len() {
            let addr = old_list.get(i).unwrap();
            if addr != co_instructor {
                new_list.push_back(addr);
            }
        }
        if new_list.is_empty() {
            env.storage().persistent().remove(&list_key);
        } else {
            env.storage().persistent().set(&list_key, &new_list);
            env.storage().persistent().extend_ttl(
                &list_key,
                Self::PERSISTENT_TTL_THRESHOLD,
                Self::PERSISTENT_TTL_EXTEND_TO,
            );
        }

        // Add to the live co-instructor list on the course.
        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| panic!("course not found"));
        course.co_instructors.push_back(CoInstructor {
            address: co_instructor.clone(),
            share_bps,
        });
        course.last_updated_ledger = env.ledger().sequence();
        Self::persist(&env, &DataKey::Course(course_id.clone()), &course);

        env.events().publish(
            (
                Symbol::new(&env, "co_instructor_accepted"),
                course_id.clone(),
            ),
            (co_instructor, course_id, share_bps, env.ledger().sequence()),
        );
    }

    // ----------------------------------------------------------
    // ENROLLMENT & PAYMENT
    // ----------------------------------------------------------

    /// Quote the payment split for enrolling a student in a course.
    ///
    /// Returns `(price, platform_fee, instructor_share)` using the course's
    /// token fee configuration, registration-time fee floor, and current risk
    /// settings.
    pub fn quote_enrollment(env: Env, course_id: String, student: Address) -> (i128, i128, i128) {
        let course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| panic!("course not found"));
        let (is_new_customer, is_btc_eth) = Self::resolve_risk_flags(&env, &student, &course.token);
        let (instructor_share, platform_fee, _) = Self::calculate_enrollment_split(
            &env,
            &course.token,
            course.price,
            Self::course_fee_override_bps(&env, &course_id),
            is_new_customer,
            is_btc_eth,
        );

        (course.price, platform_fee, instructor_share)
    }

    /// Student enrolls in a course and pays the fee.
    ///
    /// The payment is split automatically:
    ///   - Platform fee  → treasury address
    ///   - Instructor fee → credited to instructor earnings (withdraw via withdraw_earnings)
    ///
    /// A student cannot enroll in the same course twice.
    ///
    /// # Arguments
    /// - `student`   — student's Stellar address (must sign)
    /// - `course_id` — the course to enroll in
    pub fn enroll(env: Env, student: Address, course_id: String, max_price: i128) {
        student.require_auth();
        Self::enroll_internal(&env, &student, &course_id, max_price);
    }

    /// Enroll a student in multiple courses atomically.
    /// The entire batch succeeds or the entire batch fails — no partial state.
    ///
    /// # Arguments
    /// - `student`    — student's Stellar address (must sign)
    /// - `course_ids` — list of course IDs to enroll in
    ///
    /// # Returns
    /// The exact total amount paid across all enrollments in the batch.
    ///
    /// # Panics
    /// - If `course_ids` is empty
    /// - If `course_ids` contains duplicates
    /// - If `course_ids.len()` exceeds `MAX_BATCH_SIZE`
    /// - If any course fails validation
    /// Enroll a student in multiple courses atomically.
    ///
    /// `max_price` is applied per course (each course price must be <= `max_price`).
    pub fn batch_enroll(
        env: Env,
        student: Address,
        course_ids: Vec<String>,
        max_price: i128,
    ) -> i128 {
        student.require_auth();

        if course_ids.is_empty() {
            soroban_sdk::panic_with_error!(&env, Error::EmptyBatch);
        }

        if course_ids.len() > Self::MAX_BATCH_SIZE {
            soroban_sdk::panic_with_error!(&env, Error::BatchTooLarge);
        }

        // Reject duplicate course IDs in linear time via a host map so a
        // MAX_BATCH_SIZE input cannot burn quadratic CPU budget.
        let mut seen: Map<String, bool> = Map::new(&env);
        for i in 0..course_ids.len() {
            let id = course_ids.get(i).unwrap();
            if seen.contains_key(id.clone()) {
                soroban_sdk::panic_with_error!(&env, Error::DuplicateCourseInBatch);
            }
            seen.set(id, true);
        }

        // Validate every course before any mutation, reading each record once (#706)
        for i in 0..course_ids.len() {
            let course_id = course_ids.get(i).unwrap();
            let course = Self::get_course_internal(&env, &course_id)
                .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CourseNotFound));
            Self::validate_enrollment(&env, &student, &course_id, &course);
            // #361 — price guard for every course before any payment moves.
            Self::assert_price_within_limit(course.price, max_price);
        }

        // All validations passed — enroll atomically. Use the same internal
        // enrollment path as single enroll so risk flags, circuit breaker,
        // and student enrollment counts stay consistent. (#704)
        let mut total_paid = 0i128;
        for i in 0..course_ids.len() {
            let course_id = course_ids.get(i).unwrap();
            Self::enroll_internal(&env, &student, &course_id, max_price);
            total_paid = total_paid
                .checked_add(
                    Self::get_course_internal(&env, &course_id)
                        .map(|c| c.price)
                        .unwrap_or(0),
                )
                .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow));
        }

        env.events().publish(
            (Symbol::new(&env, "batch_enrolled"), student.clone()),
            (student, course_ids, total_paid, env.ledger().sequence()),
        );

        total_paid
    }


    /// #706 — Callers read the `Course` once and pass it in so validation does
    /// not load the same record again.
    fn validate_enrollment(env: &Env, student: &Address, course_id: &String, course: &Course) {
        Self::validate_enrollment_for(env, student, course_id, course, true);
    }

    /// The shared enrollment guard chain.
    ///
    /// `enroll()` and `re_enroll()` both run it, so the two paid entry points
    /// can never drift apart again: every guard added here is enforced by both.
    ///
    /// `require_not_enrolled` is the only genuine difference between them — a
    /// first enrollment must not find an existing record, while a re-enrollment
    /// requires one and therefore skips that single check.
    fn validate_enrollment_for(
        env: &Env,
        student: &Address,
        course_id: &String,
        course: &Course,
        require_not_enrolled: bool,
    ) {
        if env
            .storage()
            .instance()
            .get(&DataKey::PlatformPaused)
            .unwrap_or(false)
        {
            Self::emit_enrollment_failed(env, student, course_id, "PLATFORM_PAUSED");
            soroban_sdk::panic_with_error!(&env, Error::PlatformPaused);
        }

        // #706 — `course` is supplied by the caller; do not read it again.

        if Self::is_student_blocked_internal(env, student) {
            Self::emit_enrollment_failed(env, student, course_id, "STUDENT_BLOCKED");
            panic!("student is blocked");
        }

        if Self::is_any_course_instructor_frozen(env, &course) {
            Self::emit_enrollment_failed(env, student, course_id, "INSTRUCTOR_FROZEN");
            panic!("instructor is frozen");
        }

        if Self::is_admin(env, student) {
            Self::emit_enrollment_failed(env, student, course_id, "ADMIN_SELF_ENROLLMENT");
            soroban_sdk::panic_with_error!(&env, Error::StudentBlocked);
        }

        if Self::is_instructor_frozen_internal(env, &course.instructor) {
            soroban_sdk::panic_with_error!(&env, Error::InstructorFrozen);
        }

        if Self::is_admin(env, student) {
            soroban_sdk::panic_with_error!(&env, Error::EntryRestricted);
        }

        // Issue 168: Self-enrollment guard
        if *student == env.current_contract_address() {
            Self::emit_enrollment_failed(env, student, course_id, "ADMIN_SELF_ENROLLMENT");
            soroban_sdk::panic_with_error!(&env, Error::EntryRestricted);
        }

        if *student == course.instructor {
            Self::emit_enrollment_failed(env, student, course_id, "INSTRUCTOR_SELF_ENROLLMENT");
            soroban_sdk::panic_with_error!(&env, Error::EntryRestricted);
        }

        if course.status != CourseStatus::Active {
            // Emit rejection event with current status before panicking
            // so off-chain systems can distinguish status-based rejections
            // from other panic causes.
            env.events().publish(
                (Symbol::new(env, "enrollment_rejected"), course_id.clone()),
                EnrollmentRejected {
                    course_id: course_id.clone(),
                    student: student.clone(),
                    status: course.status.clone(),
                    ledger_sequence: env.ledger().sequence(),
                },
            );
            soroban_sdk::panic_with_error!(&env, Error::CourseNotAvailable);
        }

        if env.ledger().sequence() <= course.created_at_ledger {
            soroban_sdk::panic_with_error!(&env, Error::CourseNotAvailable);
        }

        if let Some(enrollment_start_ledger) = course.enrollment_start_ledger {
            if env.ledger().sequence() < enrollment_start_ledger {
                Self::emit_enrollment_failed(env, student, course_id, "ENROLLMENT_NOT_STARTED");
                soroban_sdk::panic_with_error!(&env, Error::CourseNotAvailable);
            }
        }

        if require_not_enrolled
            && env
                .storage()
                .persistent()
                .has(&DataKey::Enrollment(student.clone(), course_id.clone()))
        {
            Self::emit_enrollment_failed(env, student, course_id, "ALREADY_ENROLLED");
            panic!("already enrolled in this course");
        }
        if let Some(cap) = course.max_capacity {
            let count = match course.capacity_mode {
                CapacityMode::Lifetime => course.total_enrollments,
                CapacityMode::Concurrent => course.active_enrollments,
            };
            if count >= cap {
                soroban_sdk::panic_with_error!(&env, Error::CourseNotAvailable);
            }
        }

        // Check the platform-wide cap for paid courses only. Free courses do
        // not consume platform capacity and must remain enrollable even when
        // the paid-enrollment cap is full.
        if course.price > 0 {
            if let Some(platform_cap) = env
                .storage()
                .instance()
                .get::<DataKey, u32>(&DataKey::PlatformEnrollmentCap)
            {
                let total_active: u32 = Self::read_total_active_enrollments(env);
                if total_active >= platform_cap {
                    Self::emit_enrollment_failed(env, student, course_id, "PLATFORM_CAP_EXCEEDED");
                    soroban_sdk::panic_with_error!(&env, Error::CourseNotAvailable);
                }
            }
        }

        if let Some(expiry) = course.expires_at_ledger {
            if env.ledger().sequence() >= expiry {
                Self::emit_enrollment_failed(env, student, course_id, "COURSE_EXPIRED");
                soroban_sdk::panic_with_error!(&env, Error::CourseNotAvailable);
            }
        }

        if !env
            .storage()
            .instance()
            .has(&DataKey::ApprovedToken(course.token.clone()))
        {
            Self::emit_enrollment_failed(env, student, course_id, "UNAPPROVED_TOKEN");
            soroban_sdk::panic_with_error!(&env, Error::CourseNotAvailable);
        }

        // #705 — Token precision is validated once by add_approved_token and
        // cached at approval time. Only whitelisted tokens reach this point,
        // so enrollment no longer performs a cross-contract decimals() call.

        // Every prerequisite course must have an issued, non-revoked
        // certificate on record for this student before enrollment proceeds.
        for i in 0..course.prerequisite_course_ids.len() {
            let prereq_id = course.prerequisite_course_ids.get(i).unwrap();

            let prereq_enrollment: Option<Enrollment> = env
                .storage()
                .persistent()
                .get(&DataKey::Enrollment(student.clone(), prereq_id.clone()));

            let has_valid_certificate = match prereq_enrollment {
                Some(e) if e.certificate_issued => match e.certificate_id {
                    Some(cert_id) => env
                        .storage()
                        .persistent()
                        .get::<DataKey, Certificate>(&DataKey::Certificate(cert_id))
                        .map(|c| !c.revoked)
                        .unwrap_or(false),
                    None => false,
                },
                _ => false,
            };

            if !has_valid_certificate {
                Self::emit_enrollment_failed(env, student, course_id, "PREREQUISITE_NOT_MET");
                soroban_sdk::panic_with_error!(&env, Error::InvalidPrerequisite);
            }
        }

        // Check platform-wide enrollment cap
        if let Some(platform_cap) = env
            .storage()
            .instance()
            .get::<DataKey, u32>(&DataKey::PlatformEnrollmentCap)
        {
            let total_active: u32 = Self::read_total_active_enrollments(env);
            if total_active >= platform_cap {
                Self::emit_enrollment_failed(env, student, course_id, "PLATFORM_CAP_EXCEEDED");
                soroban_sdk::panic_with_error!(&env, Error::CourseNotAvailable);
            }
        }
    }

    /// Reverts when the course's current price exceeds the maximum the student
    /// is willing to pay. Guards against an instructor raising the price
    /// between the student's price quote and transaction execution (#361).
    fn assert_price_within_limit(price: i128, max_price: i128) {
        if price > max_price {
            panic!(
                "course price {} exceeds maximum accepted price {}",
                price, max_price
            );
        }
    }

    fn emit_enrollment_failed(env: &Env, student: &Address, course_id: &String, reason_code: &str) {
        env.events().publish(
            (Symbol::new(env, "enrollment_failed"), course_id.clone()),
            EnrollmentFailed {
                course_id: course_id.clone(),
                student: student.clone(),
                reason_code: String::from_str(env, reason_code),
                ledger_sequence: env.ledger().sequence(),
            },
        );
    }

    fn enroll_internal(env: &Env, student: &Address, course_id: &String, max_price: i128) {
        // #706 — One course read per enrollment, passed through validation
        // and the payment path instead of being fetched three times.
        let mut course = Self::get_course_internal(env, course_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CourseNotFound));
        Self::validate_enrollment(env, student, course_id, &course);

        // #361 — Guard against a price change between the price the student
        // approved and execution: revert before any payment is attempted when
        // the current price exceeds the student's stated maximum.
        Self::assert_price_within_limit(course.price, max_price);

        let token_client = token::Client::new(env, &course.token);

        // #178 — Circuit breaker early-exit: if a prior failed invocation has already
        // pushed the per-course failure counter to the threshold, auto-pause the
        // course now (before any token interaction) and abort. This prevents the
        // contract from repeatedly attempting a doomed transfer on a broken token
        // contract and wasting student gas fees.
        let failure_key = DataKey::CourseFailureCount(course_id.clone());
        let current_failures: u32 = env.storage().persistent().get(&failure_key).unwrap_or(0);
        if current_failures >= Self::CIRCUIT_BREAKER_THRESHOLD
            && course.status == CourseStatus::Active
        {
            course.status = CourseStatus::Paused;
            course.last_updated_ledger = env.ledger().sequence();
            Self::persist_course(&env, &course_id, &course);
            env.events().publish(
                (
                    Symbol::new(env, "course_circuit_breaker_tripped"),
                    course_id.clone(),
                ),
                (course_id.clone(), current_failures, env.ledger().sequence()),
            );
            soroban_sdk::panic_with_error!(&env, Error::CircuitBreakerTripped);
        }

        // Atomicity guarantee: Soroban executes a single contract invocation
        // (and everything it calls) as one atomic unit — no other
        // transaction can observe or mutate this course's state between the
        // status check in validate_enrollment() above and the token
        // transfer below. This re-check exists anyway, immediately before
        // payment, so that if a future refactor ever separates
        // course-fetching from payment (e.g. introduces an async step),
        // the status is still re-verified at the tightest possible point
        // rather than relying solely on the earlier check.
        if course.status != CourseStatus::Active {
            // Emit rejection event with current status before panicking
            // so off-chain systems can distinguish status-based rejections
            // from other panic causes.
            env.events().publish(
                (Symbol::new(env, "enrollment_rejected"), course_id.clone()),
                EnrollmentRejected {
                    course_id: course_id.clone(),
                    student: student.clone(),
                    status: course.status.clone(),
                    ledger_sequence: env.ledger().sequence(),
                },
            );
            soroban_sdk::panic_with_error!(&env, Error::CourseNotAvailable);
        }

        // Use the centralized fee deduction function that supports:
        // 1. Per-token fee configuration (map token → FeeConfig)
        // 2. Risk-based surcharges for large payments, new customers, and BTC/ETH
        // 3. Publishes RiskFeeApplied event when surcharge applies
        // Quote and enrollment share the same risk flags and course-aware fee split.
        let (is_new_customer, is_btc_eth) = Self::resolve_risk_flags(env, student, &course.token);
        let course_fee_override_bps = Self::course_fee_override_bps(env, &course.id);
        let (instructor_amount, platform_amount) = Self::deduct_fee(
            env,
            &course.token,
            course.price,
            course_fee_override_bps,
            is_new_customer,
            is_btc_eth,
        );

        // Fetch treasury, applying any pending treasury update if effective
        let treasury = Self::effective_treasury(env);
        let mut treasury: Address = env
            .storage()
            .instance()
            .get(&DataKey::Treasury)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::InvalidTreasury));

        if let Some(pending) = env
            .storage()
            .instance()
            .get::<DataKey, TreasuryUpdate>(&DataKey::PendingTreasury)
        {
            if env.ledger().sequence() >= pending.effective_ledger {
                treasury = pending.address.clone();
                env.storage().instance().set(&DataKey::Treasury, &treasury);
                env.storage().instance().remove(&DataKey::PendingTreasury);
            }
        }
        let treasury: Address = Self::read_treasury(&env);

        // Transfer full price from student to contract, then distribute platform fee.
        //
        // #178 — Circuit breaker: if the token transfer panics (e.g. broken token
        // contract, zero balance on the contract side, or a token-level pause),
        // the Soroban runtime will revert the entire transaction so we never reach
        // the failure-counter increment. Instead we track the number of *successful*
        // path executions vs panics externally by using a per-course failure counter
        // stored in persistent storage. Because Soroban reverts all state on panic,
        // we must increment the failure counter BEFORE attempting the transfer and
        // then clear/decrement it AFTER a successful transfer.
        let failure_key = DataKey::CourseFailureCount(course_id.clone());
        let failure_count: u32 = env.storage().persistent().get(&failure_key).unwrap_or(0);

        // Pre-increment: if the transfer below panics, this increment is committed
        // because Soroban reverts the *caller's* transaction, not the storage writes
        // that already landed in a prior successful invocation. The counter therefore
        // accumulates across failed calls to the same course.
        let new_failure_count = failure_count
            .checked_add(1)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow));
        Self::persist(&env, &failure_key, &new_failure_count);

        let actual_amount_paid = if course.price > 0 {
            // Verify actual transfer amount by checking balance before and after.
            // If the token rejects the payment, emit the failure trace and exit
            // without mutating enrollment state.
            let balance_before = token_client.balance(&env.current_contract_address());
            if token_client
                .try_transfer(student, &env.current_contract_address(), &course.price)
                .is_err()
            {
                Self::emit_enrollment_failed(env, student, course_id, "INSUFFICIENT_BALANCE");
                panic!("insufficient balance");
            }
            let balance_after = token_client.balance(&env.current_contract_address());

            let actual_received = balance_after
                .checked_sub(balance_before)
                .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow));

            if actual_received != course.price {
                Self::emit_enrollment_failed(env, student, course_id, "TRANSFER_AMOUNT_MISMATCH");
                panic!("token transfer amount mismatch");
            }

            // #368 — Accumulate the platform fee in-contract instead of
            // pushing it to the treasury on every enrollment.  The fee is
            // swept to the treasury in a single call to sweep_platform_fees().
            if platform_amount > 0 {
                let acc_key = DataKey2::PlatformFeesAccumulated(course.token.clone());
                let current: i128 = env.storage().instance().get(&acc_key).unwrap_or(0);
                let new_total = current.checked_add(platform_amount).unwrap_or_else(|| {
                    soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow)
                });
                env.storage().instance().set(&acc_key, &new_total);
                Self::increment_platform_fees_collected(env, &course.token, platform_amount);
                env.events().publish(
                    (Symbol::new(&env, "platform_fee_accrued"), course_id.clone()),
                    (
                        course.token.clone(),
                        platform_amount,
                        new_total,
                        env.ledger().sequence(),
                    ),
                );
            }

            // Credit instructor earnings — pull-based withdrawal model.
            // Co-instructors receive their configured share first; the lead
            // instructor receives the remainder.
            if instructor_amount > 0 {
                Self::credit_instructor_shares(env, &course, instructor_amount);
            }

            actual_received
        } else {
            0
        };

        // Transfer succeeded — reset the circuit-breaker failure counter.
        // Also check whether the pre-incremented count crossed the threshold:
        // if CIRCUIT_BREAKER_THRESHOLD consecutive calls all failed before this
        // success, the course will have been paused by a prior invocation, so
        // this success can still proceed (the course status check in
        // validate_enrollment would have rejected it if it were Paused).
        Self::persist(&env, &failure_key, &0u32);

        // If the pre-incremented failure count hit the threshold (meaning this
        // successful call was preceded by CIRCUIT_BREAKER_THRESHOLD-1 failures),
        // check whether we should auto-pause. In practice the course would already
        // be Paused from the last failing invocation reaching the threshold, but
        // we guard here for completeness. The real auto-pause path fires in the
        // failure branch below — reached only when the transfer above panics and
        // Soroban reverts this transaction, leaving the incremented counter in
        // storage. On the *next* call, new_failure_count will equal the threshold
        // and we auto-pause before even attempting a transfer.
        if new_failure_count >= Self::CIRCUIT_BREAKER_THRESHOLD
            && course.status == CourseStatus::Active
        {
            course.status = CourseStatus::Paused;
            course.last_updated_ledger = env.ledger().sequence();
            Self::persist_course(&env, &course_id, &course);
            env.events().publish(
                (
                    Symbol::new(&env, "course_circuit_breaker_tripped"),
                    course_id.clone(),
                ),
                (
                    course_id.clone(),
                    new_failure_count,
                    env.ledger().sequence(),
                ),
            );
        }

        // Record enrollment
        let enrollment_ref = Self::generate_enrollment_ref(&env);
        let enrollment = Enrollment {
            student: student.clone(),
            course_id: course_id.clone(),
            amount_paid: actual_amount_paid,
            enrolled_at_ledger: env.ledger().sequence(),
            completed: false,
            certificate_issued: false,
            certificate_id: None,
            evidence_hash: None,
            is_refunded: false,
            course_version: course.version,
            content_hash: course.content_hash.clone(),
            platform_amount,
            instructor_amount,
            enrollment_ref: enrollment_ref.clone(),
            completed_at_ledger: None,
            enrollment_expiry_ledgers: course.enrollment_expiry_ledgers,
        };

        Self::persist(
            &env,
            &DataKey::Enrollment(student.clone(), course_id.clone()),
            &enrollment,
        );

        // Update course stats without rewriting the static Course entry.
        let mut stats = Self::read_course_stats(&env, &course_id, &course);
        stats.total_enrollments = stats
            .total_enrollments
            .checked_add(1)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow));
        stats.active_enrollments = stats
            .active_enrollments
            .checked_add(1)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow));
        stats.total_earned = stats
            .total_earned
            .checked_add(course.price)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow));
        course.last_updated_ledger = env.ledger().sequence();
        Self::persist_course(&env, &course_id, &course);
        Self::write_course_stats(&env, &course_id, &stats);

        // Increment platform-wide active enrollment counter
        let total_active: u32 = Self::read_total_active_enrollments(env);
        Self::write_total_active_enrollments(
            env,
            total_active
                .checked_add(1)
                .expect("platform enrollment counter overflow"),
        );
        Self::inc_total_active(&env);
        // Increment the platform-wide paid-enrollment counter. Free courses
        // are intentionally excluded so they cannot exhaust paid capacity.
        if course.price > 0 {
            let total_active: u32 = env
                .storage()
                .instance()
                .get(&DataKey::TotalActiveEnrollments)
                .unwrap_or(0);
            env.storage().instance().set(
                &DataKey::TotalActiveEnrollments,
                &(total_active
                    .checked_add(1)
                    .unwrap_or_else(|| panic!("platform enrollment counter overflow"))),
            );
        }

        Self::update_instructor_stats(env, &course.instructor, |s| {
            s.total_students = s
                .total_students
                .checked_add(1)
                .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow));
        });
        if course.price > 0 {
            Self::update_instructor_stats(env, &course.instructor, |s| {
                s.total_students = s
                    .total_students
                    .checked_add(1)
                    .unwrap_or_else(|| panic!("instructor stats overflow"));
            });
        }

        Self::increment_student_enrollment_count(env, student);

        // Emit enrollment receipt event with complete payment breakdown
        env.events().publish(
            (Symbol::new(env, "student_enrolled"), course_id.clone()),
            (
                student.clone(),
                course_id.clone(),
                course.price,
                platform_amount,
                instructor_amount,
                env.ledger().sequence(),
                treasury.clone(),
                Self::payout_breakdown(env, &course, instructor_amount),
            ),
        );
    }

    /// #704 — batch enroll now uses `enroll_internal` so payment/recording,
    /// risk flags, circuit breaker, and student enrollment counts stay aligned.
    ///
    /// Unlike `enroll()`, this is allowed even though an `Enrollment` record
    /// already exists for this (student, course_id) pair when that record is
    /// completed or has expired. The prior record — including evidence hash
    /// and certificate linkage for completed attempts — is archived to
    /// `EnrollmentHistory` before a fresh `Enrollment` is created, so nothing
    /// about the original attempt or any certificate already issued for it
    /// is overwritten. The student is charged again, exactly as for a
    /// first-time enrollment.
    ///
    /// An incomplete, unexpired enrollment must still be completed before
    /// re-enrolling.
    ///
    /// # Arguments
    /// - `student`   — student's Stellar address (must sign)
    /// - `course_id` — the course to re-enroll in
    pub fn re_enroll(env: Env, student: Address, course_id: String, max_price: i128) {
        student.require_auth();

        if env
            .storage()
            .instance()
            .get(&DataKey::PlatformPaused)
            .unwrap_or(false)
        {
            soroban_sdk::panic_with_error!(&env, Error::PlatformPaused);
        }

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CourseNotFound));

        if Self::is_student_blocked_internal(&env, &student) {
            soroban_sdk::panic_with_error!(&env, Error::StudentBlocked);
        }

        if Self::is_any_course_instructor_frozen(&env, &course) {
            soroban_sdk::panic_with_error!(&env, Error::InstructorFrozen);
        }

        if student == course.instructor {
            soroban_sdk::panic_with_error!(&env, Error::EntryRestricted);
        }

        if course.status != CourseStatus::Active {
            soroban_sdk::panic_with_error!(&env, Error::CourseNotAvailable);
        }

        if let Some(start) = course.enrollment_start_ledger {
            if env.ledger().sequence() < start {
                soroban_sdk::panic_with_error!(&env, Error::CourseNotAvailable);
            }
        }

        if !env
            .storage()
            .instance()
            .has(&DataKey::ApprovedToken(course.token.clone()))
        {
            soroban_sdk::panic_with_error!(&env, Error::CourseNotAvailable);
        }
        // A re-enrollment is a paid enrollment, so it has to clear exactly the
        // same guards as a first one: paused platform, blocked student, frozen
        // instructor, admin/contract/self-enrollment, course status and
        // schedule, approved token and token precision, prerequisites, course
        // capacity and the platform-wide cap. The only guard skipped is
        // "already enrolled", because a re-enrollment requires the prior record.
        Self::validate_enrollment_for(&env, &student, &course_id, &course, false);

        let enrollment_key = DataKey::Enrollment(student.clone(), course_id.clone());
        let previous_enrollment: Enrollment = env
            .storage()
            .persistent()
            .get(&enrollment_key)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::EnrollmentStateError));

        let previous_enrollment_expired =
            Self::is_enrollment_expired(&env, &course, &previous_enrollment);
        if !previous_enrollment.completed && !previous_enrollment_expired {
            soroban_sdk::panic_with_error!(&env, Error::EnrollmentStateError);
        }

        let token_client = token::Client::new(&env, &course.token);

        // Use the centralized fee deduction function that supports:
        // 1. Per-token fee configuration (map token → FeeConfig)
        // 2. Risk-based surcharges for large payments, new customers, and BTC/ETH
        // 3. Publishes RiskFeeApplied event when surcharge applies
        let (is_new_customer, is_btc_eth) = Self::resolve_risk_flags(&env, &student, &course.token);
        let course_fee_override_bps = Self::course_fee_override_bps(&env, &course.id);
        let (instructor_amount, platform_amount) = Self::deduct_fee(
            &env,
            &course.token,
            course.price,
            course_fee_override_bps,
            is_new_customer,
            is_btc_eth,
        );

        let treasury = Self::effective_treasury(&env);
        let mut treasury: Address = env
            .storage()
            .instance()
            .get(&DataKey::Treasury)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::InvalidTreasury));

        if let Some(pending) = env
            .storage()
            .instance()
            .get::<DataKey, TreasuryUpdate>(&DataKey::PendingTreasury)
        {
            if env.ledger().sequence() >= pending.effective_ledger {
                treasury = pending.address.clone();
                env.storage().instance().set(&DataKey::Treasury, &treasury);
                env.storage().instance().remove(&DataKey::PendingTreasury);
            }
        }
        let treasury: Address = Self::read_treasury(&env);

        if course.price > 0 {
            let balance_before = token_client.balance(&env.current_contract_address());
            if token_client
                .try_transfer(&student, &env.current_contract_address(), &course.price)
                .is_err()
            {
                Self::emit_enrollment_failed(&env, &student, &course_id, "INSUFFICIENT_BALANCE");
                panic!("insufficient balance");
            }

            let balance_after = token_client.balance(&env.current_contract_address());
            let actual_received = balance_after
                .checked_sub(balance_before)
                .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow));

            if actual_received != course.price {
                Self::emit_enrollment_failed(
                    &env,
                    &student,
                    &course_id,
                    "TRANSFER_AMOUNT_MISMATCH",
                );
                panic!("token transfer amount mismatch");
                return;
            }

            if platform_amount > 0 {
                token_client.transfer(&env.current_contract_address(), &treasury, &platform_amount);
                Self::increment_platform_fees_collected(&env, &course.token, platform_amount);
            }

            if instructor_amount > 0 {
                Self::credit_instructor_shares(&env, &course, instructor_amount);
            }
        }

        let previous_was_completed = previous_enrollment.completed;

        // Archive the prior enrollment — completed or expired — before it is
        // overwritten. Completed attempts keep their evidence hash and
        // certificate_id linkage; expired attempts remain incomplete.
        let history_key = DataKey::EnrollmentHistory(student.clone(), course_id.clone());
        let mut history: Vec<Enrollment> = env
            .storage()
            .persistent()
            .get(&history_key)
            .unwrap_or_else(|| Vec::new(&env));
        history.push_back(previous_enrollment);
        Self::persist(&env, &history_key, &history);

        let new_enrollment_ref = Self::generate_enrollment_ref(&env);
        let new_enrollment = Enrollment {
            student: student.clone(),
            course_id: course_id.clone(),
            amount_paid: course.price,
            enrolled_at_ledger: env.ledger().sequence(),
            completed: false,
            certificate_issued: false,
            certificate_id: None,
            evidence_hash: None,
            is_refunded: false,
            course_version: course.version,
            content_hash: course.content_hash.clone(),
            platform_amount,
            instructor_amount,
            enrollment_ref: new_enrollment_ref,
            completed_at_ledger: None,
            enrollment_expiry_ledgers: course.enrollment_expiry_ledgers,
        };

        Self::persist(&env, &enrollment_key, &new_enrollment);

        let mut stats = Self::read_course_stats(&env, &course_id, &course);
        stats.total_enrollments = stats
            .total_enrollments
            .checked_add(1)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow));

        // A completed attempt was already removed from the active counters
        // by mark_completed(). An expired attempt is still counted as active,
        // so release that slot before counting the new enrollment.
        if !previous_was_completed && stats.active_enrollments > 0 {
            stats.active_enrollments -= 1;
        }
        stats.active_enrollments = stats
            .active_enrollments
            .checked_add(1)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow));
        stats.total_earned = stats
            .total_earned
            .checked_add(course.price)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow));
        course.last_updated_ledger = env.ledger().sequence();
        Self::persist_course(&env, &course_id, &course);
        Self::write_course_stats(&env, &course_id, &stats);

        // expired attempt is net-zero; re-enrolling after completion adds one.
        Self::inc_total_active(&env);

        Self::update_instructor_stats(&env, &course.instructor, |s| {
            s.total_students = s
                .total_students
                .checked_add(1)
                .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow));
        });

        Self::increment_student_enrollment_count(&env, &student);

        // #358 - re-enrollment moves the same funds as enroll(), so it emits
        // the same single canonical payment event (see #366).
        env.events().publish(
            (Symbol::new(&env, "student_enrolled"), course_id.clone()),
            (
                student.clone(),
                course_id.clone(),
                course.price,
                platform_amount,
                instructor_amount,
                env.ledger().sequence(),
                treasury.clone(),
                Self::payout_breakdown(&env, &course, instructor_amount),
            ),
        );

        env.events().publish(
            (Symbol::new(&env, "student_re_enrolled"), course_id.clone()),
            (
                student,
                course_id,
                course.price,
                platform_amount,
                instructor_amount,
                env.ledger().sequence(),
            ),
        );
    }

    /// Instructor withdraws a specific positive `amount` of accumulated earnings
    /// for a given token. Panics with `Error::InvalidWithdrawal` when
    /// `amount <= 0`. Use `withdraw_all_earnings()` to withdraw the full
    /// available balance.
    ///
    /// `destination` is where the funds are sent. `None` sends them to
    /// `instructor` (previous behaviour). Only the instructor must sign;
    /// the destination does not need to authorize anything. (#349)
    ///
    /// Frozen instructors cannot withdraw until an admin unfreezes them.
    /// Withdrawals reduce `get_instructor_earnings()` but do not change
    /// `get_instructor_lifetime_earnings()`.
    ///
    /// Emits `earnings_withdrawn`:
    /// - Topics: `("earnings_withdrawn", instructor, token)`
    /// - Data: `(token, withdraw_amount, recipient, new_balance, ledger_sequence)`
    pub fn withdraw_earnings(
        env: Env,
        instructor: Address,
        token: Address,
        amount: i128,
        destination: Option<Address>,
    ) {
        instructor.require_auth();

        if Self::is_instructor_frozen_internal(&env, &instructor) {
            panic!("instructor is frozen");
        }

        if amount <= 0 {
            soroban_sdk::panic_with_error!(&env, Error::InvalidWithdrawal);
        }

        Self::withdraw_earnings_internal(&env, &instructor, &token, amount, destination);
    }

    /// Instructor withdraws all accumulated earnings for a given token.
    ///
    /// `destination` is where the funds are sent. `None` sends them to
    /// `instructor`. Only the instructor must sign; the destination does not
    /// need to authorize anything. (#346, #349)
    ///
    /// Frozen instructors cannot withdraw until an admin unfreezes them.
    /// Returns without transferring when the available balance is zero.
    /// Withdrawals reduce `get_instructor_earnings()` but do not change
    /// `get_instructor_lifetime_earnings()`.
    pub fn withdraw_all_earnings(
        env: Env,
        instructor: Address,
        token: Address,
        destination: Option<Address>,
    ) {
        instructor.require_auth();

        if Self::is_instructor_frozen_internal(&env, &instructor) {
            panic!("instructor is frozen");
        }

        let earnings_key = DataKey::InstructorEarnings(instructor.clone(), token.clone());
        let balance: i128 = env.storage().persistent().get(&earnings_key).unwrap_or(0);

        Self::withdraw_earnings_internal(&env, &instructor, &token, balance, destination);
    }

    fn withdraw_earnings_internal(
        env: &Env,
        instructor: &Address,
        token: &Address,
        withdraw_amount: i128,
        destination: Option<Address>,
    ) {
        let recipient = destination.unwrap_or_else(|| instructor.clone());

        if recipient == env.current_contract_address() {
            soroban_sdk::panic_with_error!(env, Error::InvalidWithdrawal);
        }

        if withdraw_amount == 0 {
            return;
        }

        let earnings_key = DataKey::InstructorEarnings(instructor.clone(), token.clone());
        let balance: i128 = env.storage().persistent().get(&earnings_key).unwrap_or(0);

        if withdraw_amount > balance {
            soroban_sdk::panic_with_error!(env, Error::InvalidWithdrawal);
        }

        let new_balance = balance
            .checked_sub(withdraw_amount)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(env, Error::ArithmeticOverflow));

        if new_balance == 0 {
            env.storage().persistent().remove(&earnings_key);
            Self::remove_instructor_earning_token(&env, &instructor, &token);
        } else {
            Self::persist(env, &earnings_key, &new_balance);
        }

        // Decrement total outstanding instructor earnings for this token
        let outstanding_key = DataKey2::TotalInstructorEarnings(token.clone());
        let current_outstanding: i128 = env.storage().instance().get(&outstanding_key).unwrap_or(0);
        let new_outstanding = current_outstanding.saturating_sub(withdraw_amount);
        if new_outstanding == 0 {
            env.storage().instance().remove(&outstanding_key);
        } else {
            env.storage()
                .instance()
                .set(&outstanding_key, &new_outstanding);
        }

        let token_client = token::Client::new(env, token);
        token_client.transfer(
            &env.current_contract_address(),
            &recipient,
            &withdraw_amount,
        );

        env.events().publish(
            (
                Symbol::new(env, "earnings_withdrawn"),
                instructor.clone(),
                token.clone(),
            ),
            (
                token.clone(),
                withdraw_amount,
                recipient,
                new_balance,
                env.ledger().sequence(),
            ),
        );
    }

    /// Get the currently withdrawable earnings for an instructor and token.
    /// This balance is reduced by `withdraw_earnings()` and by refunds.
    pub fn get_instructor_earnings(env: Env, instructor: Address, token: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::InstructorEarnings(instructor, token))
            .unwrap_or(0)
    }

    /// List all token addresses in which `instructor` currently holds a
    /// non-zero withdrawable earnings balance.
    pub fn get_instructor_earning_tokens(env: Env, instructor: Address) -> Vec<Address> {
        env.storage()
            .persistent()
            .get(&DataKey2::InstructorEarningTokens(instructor))
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Get lifetime net earnings for an instructor in a specific token.
    ///
    /// This counter is credited on enrollment and debited on refunds, and is
    /// intentionally unaffected by withdrawals. It is token-scoped so values
    /// denominated in different assets are never added together.
    pub fn get_instructor_lifetime_earnings(env: Env, instructor: Address, token: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::InstructorLifetimeEarnings(instructor, token))
            .unwrap_or(0)
    }

    /// Internal helper: returns instructor earnings, open arbitration fees
    /// and refund-reserve balances owed for a token.
    pub(crate) fn get_total_liabilities_internal(env: &Env, token: &Address) -> i128 {
        let earnings: i128 = env
            .storage()
            .instance()
            .get(&DataKey2::TotalInstructorEarnings(token.clone()))
            .unwrap_or(0);
        let arb_fees: i128 = env
            .storage()
            .persistent()
            .get(&DataKey2::ArbitrationFeesHeld(token.clone()))
            .unwrap_or(0);
        let refund_reserve: i128 = env
            .storage()
            .persistent()
            .get(&DataKey2::RefundReserve(token.clone()))
            .unwrap_or(0);
        earnings
            .checked_add(arb_fees)
            .and_then(|total| total.checked_add(refund_reserve))
            .unwrap_or_else(|| panic!("liabilities overflow"))
    }

    /// Get total tracked liabilities for instructor earnings, open arbitration
    /// fees and the refund reserve for a token.
    pub fn get_total_liabilities(env: Env, token: Address) -> i128 {
        Self::get_total_liabilities_internal(&env, &token)
    }

    /// Alias for get_total_liabilities: returns total owed balance for a token
    pub fn get_owed_balance(env: Env, token: Address) -> i128 {
        Self::get_total_liabilities_internal(&env, &token)
    }

    /// Get total outstanding un-withdrawn instructor earnings for a token
    pub fn get_outstanding_earnings(env: Env, token: Address) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey2::TotalInstructorEarnings(token))
            .unwrap_or(0)
    }

    /// Get total arbitration fees held for a token
    pub fn get_total_arbitration_fees(env: Env, token: Address) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey2::TotalArbitrationFees(token))
            .unwrap_or(0)
    }

    /// Get net cumulative platform fees collected for a token.
    /// Incremented when platform fees are collected on enrollment and
    /// decremented when platform fees are refunded.
    pub fn get_platform_fees_collected(env: Env, token: Address) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey2::PlatformFeesCollected(token))
            .unwrap_or(0)
    }

    /// Get the available surplus balance that can be swept via withdraw_tokens()
    pub fn get_surplus_balance(env: Env, token: Address) -> i128 {
        let token_client = token::Client::new(&env, &token);
        let balance = token_client.balance(&env.current_contract_address());
        let liabilities = Self::get_total_liabilities_internal(&env, &token);
        balance.saturating_sub(liabilities)
    }

    // ----------------------------------------------------------
    // COURSE COMPLETION & CERTIFICATES
    // ----------------------------------------------------------

    /// Admin marks a student's enrollment as completed.
    /// This is called by the admin after the backend verifies the student
    /// has finished all lessons and passed all assignments.
    ///
    /// # Arguments
    /// - `admin`         — must match stored admin
    /// - `student`       — the student's address
    /// - `course_id`     — the course completed
    /// - `evidence_hash` — fixed 32-byte proof of completion; when `None`, the
    ///   student must co-sign instead
    pub fn mark_completed(
        env: Env,
        admin: Address,
        student: Address,
        course_id: String,
        evidence_hash: Option<BytesN<32>>,
    ) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "mark_completed");
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        // Supplying a 32-byte evidence hash waives the student co-signature.
        // A fixed-size hash is always a valid value, even if all bytes are 0.
        match &evidence_hash {
            Some(_) => {}
            None => student.require_auth(),
        }

        // Check if student is blocked
        if Self::is_student_blocked_internal(&env, &student) {
            soroban_sdk::panic_with_error!(&env, Error::StudentBlocked);
        }

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CourseNotFound));

        // Validate course is in a valid state for completion (Active or Paused, not Archived)
        if course.status == CourseStatus::Archived {
            soroban_sdk::panic_with_error!(&env, Error::ArchivedCourseImmutable);
        }

        let mut enrollment = Self::get_enrollment_internal(&env, &student, &course_id);

        if enrollment.is_refunded {
            soroban_sdk::panic_with_error!(&env, Error::RefundError);
        }

        // A pending refund request must be resolved or cancelled first, so a
        // student cannot complete and then still be refunded for the course.
        let refund_key = DataKey::RefundRequest(student.clone(), course_id.clone());
        let pending: Option<RefundRequest> = env.storage().persistent().get(&refund_key);
        if let Some(request) = pending {
            if request.status == RefundStatus::Pending {
                panic!("cannot mark completed while refund request is pending");
            }
        }

        let elapsed = env
            .ledger()
            .sequence()
            .checked_sub(enrollment.enrolled_at_ledger)
            .unwrap_or(0);
        if elapsed < course.min_completion_ledgers {
            soroban_sdk::panic_with_error!(&env, Error::EnrollmentStateError);
        }

        if enrollment.course_id != course_id {
            soroban_sdk::panic_with_error!(&env, Error::EnrollmentStateError);
        }

        if enrollment.completed {
            soroban_sdk::panic_with_error!(&env, Error::EnrollmentStateError);
        }

        // Check enrollment expiry — expired enrollments cannot be completed
        if Self::is_enrollment_expired(&env, &course, &enrollment) {
            panic!("enrollment has expired");
        }

        enrollment.completed = true;
        enrollment.evidence_hash = evidence_hash.clone();
        enrollment.completed_at_ledger = Some(env.ledger().sequence()); // Issue 166

        Self::persist(
            &env,
            &DataKey::Enrollment(student.clone(), course_id.clone()),
            &enrollment,
        );

        // Update active enrollments count on course
        let mut stats = Self::read_course_stats(&env, &course_id, &course);
        if stats.active_enrollments > 0 {
            stats.active_enrollments -= 1;
            Self::write_course_stats(&env, &course_id, &stats);
        }
        course.last_updated_ledger = env.ledger().sequence();
        Self::persist_course(&env, &course_id, &course);

        // Decrement platform-wide active enrollment counter
        let total_active: u32 = Self::read_total_active_enrollments(&env);
        if total_active > 0 {
            Self::write_total_active_enrollments(&env, total_active - 1);
        }

        Self::update_instructor_stats(&env, &course.instructor, |s| {
            s.total_completions = s
                .total_completions
                .checked_add(1)
                .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow));
        });

        let completion_ledger = env.ledger().sequence();
        env.events().publish(
            (Symbol::new(&env, "course_completed"), course_id.clone()),
            (
                course_id.clone(),
                student,
                admin,
                evidence_hash,
                completion_ledger,
            ),
        );
    }

    /// Permissionlessly clean up an expired, never-completed enrollment.
    ///
    /// The expired enrollment is archived to `EnrollmentHistory`, then removed
    /// from active storage. `course.active_enrollments` and the platform-wide
    /// `TotalActiveEnrollments` counter are each decremented exactly once; a
    /// second call fails because the active enrollment no longer exists.
    ///
    /// Refunds are explicitly out of scope: this function only releases the
    /// active-enrollment counters.
    pub fn expire_enrollment(env: Env, student: Address, course_id: String) {
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| panic!("course not found"));
        if course.status == CourseStatus::Archived {
            panic!("course is archived");
        }
        let enrollment_key = DataKey::Enrollment(student.clone(), course_id.clone());
        let enrollment = env
            .storage()
            .persistent()
            .get::<DataKey, Enrollment>(&enrollment_key)
            .unwrap_or_else(|| panic!("enrollment not found"));

        if enrollment.completed {
            panic!("completed enrollment cannot expire");
        }
        if enrollment.is_refunded {
            panic!("refunded enrollment cannot expire");
        }

        let expiry_ledgers = enrollment
            .enrollment_expiry_ledgers
            .unwrap_or_else(|| panic!("course has no enrollment expiry"));
        let expiry_at = enrollment
            .enrolled_at_ledger
            .checked_add(expiry_ledgers)
            .unwrap_or(u32::MAX);
        if env.ledger().sequence() < expiry_at {
            panic!("enrollment has not expired");
        }

        // Preserve an audit trail before removing the active record.
        let history_key = DataKey::EnrollmentHistory(student.clone(), course_id.clone());
        let mut history: Vec<Enrollment> = env
            .storage()
            .persistent()
            .get(&history_key)
            .unwrap_or_else(|| Vec::new(&env));
        history.push_back(enrollment.clone());
        env.storage().persistent().set(&history_key, &history);
        env.storage().persistent().extend_ttl(
            &history_key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        env.storage().persistent().remove(&enrollment_key);

        let mut stats = Self::read_course_stats(&env, &course_id, &course);
        if stats.active_enrollments > 0 {
            stats.active_enrollments -= 1;
            Self::write_course_stats(&env, &course_id, &stats);
        }
        course.last_updated_ledger = env.ledger().sequence();
        Self::persist_course(&env, &course_id, &course);

        if enrollment.amount_paid > 0 {
            let total_active: u32 = env
                .storage()
                .instance()
                .get(&DataKey::TotalActiveEnrollments)
                .unwrap_or(0);
            if total_active > 0 {
                env.storage()
                    .instance()
                    .set(&DataKey::TotalActiveEnrollments, &(total_active - 1));
            }
        }

        let expired_at_ledger = env.ledger().sequence();
        env.events().publish(
            (Symbol::new(&env, "enrollment_expired"), course_id.clone()),
            (student, course_id, expired_at_ledger),
        );
    }

    /// Issue an on-chain certificate to a student who has completed a course.
    /// Certificates are permanent, verifiable proofs of skill attainment.
    ///
    /// Admin calls this after `mark_completed`. The certificate ID must be
    /// unique (e.g. generated by the backend as UUID or hash).
    ///
    /// The enrollment is looked up by the explicit `(student, course_id)`
    /// pair — the same key it is stored under — so the certificate's
    /// course_id is always the course the student actually enrolled in and
    /// completed. `enrollment_reference` is a free-form backend identifier
    /// (e.g. a UUID) stored on the certificate for off-chain reconciliation;
    /// it is never parsed.
    ///
    /// # Arguments
    /// - `admin`                — must match stored admin
    /// - `student`              — the student receiving the certificate
    /// - `course_id`            — the course the student completed
    /// - `certificate_id`       — unique certificate identifier
    /// - `course_title`         — short title stored on-chain for verifiability
    /// - `enrollment_reference` — free-form backend enrollment ID (e.g. UUID)
    /// - `expires_at_ledger`       — optional expiry ledger
    /// - `instructor_attestation`  — optional 64-byte blob supplied by the
    ///   instructor as an off-chain attestation.  **Not verified on-chain**;
    ///   see `Certificate.instructor_attestation` for the full limitation note.
    pub fn issue_certificate(
        env: Env,
        admin: Address,
        student: Address,
        course_id: String,
        certificate_id: String,
        course_title: String,
        enrollment_reference: String,
        expires_at_ledger: Option<u32>,
        instructor_attestation: Option<BytesN<64>>,
    ) -> String {
        admin.require_auth();
        Self::require_admin(&env, &admin, "issue_certificate");
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        if certificate_id.len() == 0 {
            soroban_sdk::panic_with_error!(&env, Error::InvalidCertificateInput);
        }
        if certificate_id.len() > Self::MAX_COURSE_ID_LEN {
            soroban_sdk::panic_with_error!(&env, Error::InvalidCertificateInput);
        }
        if course_title.len() == 0 {
            soroban_sdk::panic_with_error!(&env, Error::InvalidCertificateInput);
        }
        if course_title.len() > Self::MAX_COURSE_TITLE_LEN {
            soroban_sdk::panic_with_error!(&env, Error::InvalidCertificateInput);
        }
        if enrollment_reference.len() == 0 {
            soroban_sdk::panic_with_error!(&env, Error::InvalidCertificateInput);
        }
        if enrollment_reference.len() > Self::MAX_COURSE_ID_LEN {
            soroban_sdk::panic_with_error!(&env, Error::InvalidCertificateInput);
        }
        if let Some(expiry) = expires_at_ledger {
            if expiry <= env.ledger().sequence() {
                soroban_sdk::panic_with_error!(&env, Error::InvalidCertificateInput);
            }
        }

        // Check if student is blocked
        if Self::is_student_blocked_internal(&env, &student) {
            soroban_sdk::panic_with_error!(&env, Error::StudentBlocked);
        }

        // Student must have completed the course — enrollment is looked up
        // from the authoritative enrollment record for (student, course_id).
        let mut enrollment = Self::get_enrollment_internal(&env, &student, &course_id);
        if enrollment.course_id != course_id {
            soroban_sdk::panic_with_error!(&env, Error::EnrollmentStateError);
        }
        if enrollment.is_refunded {
            soroban_sdk::panic_with_error!(&env, Error::RefundError);
        }
        if !enrollment.completed {
            soroban_sdk::panic_with_error!(&env, Error::EnrollmentStateError);
        }

        // Track the previous certificate ID when replacing a revoked one, so we
        // can set `superseded_by` on the old cert after the new one is stored.
        let superseded_cert_id: Option<String> = if enrollment.certificate_issued {
            // Allow reissue only when the existing certificate is revoked.
            // Any other state means a valid certificate already exists.
            let prev_cert_id = enrollment
                .certificate_id
                .clone()
                .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CertificateError));
            let prev_cert: Certificate = env
                .storage()
                .persistent()
                .get(&DataKey::Certificate(prev_cert_id.clone()))
                .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CertificateError));
            if !prev_cert.revoked {
                soroban_sdk::panic_with_error!(&env, Error::CertificateAlreadyIssued);
            }
            Some(prev_cert_id)
        } else {
            None
        };

        // Certificate ID must be unique
        if env
            .storage()
            .persistent()
            .has(&DataKey::Certificate(certificate_id.clone()))
        {
            soroban_sdk::panic_with_error!(&env, Error::CertificateAlreadyIssued);
        }

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CourseNotFound));

        // Archived courses are immutable — reject before any write so the
        // archived record is never promoted back to active storage.
        if course.status == CourseStatus::Archived {
            soroban_sdk::panic_with_error!(&env, Error::ArchivedCourseImmutable);
        }

        // Check per-course certificate limit against *live* certificates
        // (issued minus revoked).  Revocations free a slot so it can be reused.
        if let Some(max_certs) = course.max_certificates {
            let live_certs = course
                .certificates_issued
                .saturating_sub(course.certificates_revoked);
            if live_certs >= max_certs {
                soroban_sdk::panic_with_error!(&env, Error::MaxCertificatesReached);
            }
        }

        let issued_at_ledger = env.ledger().sequence();
        let certificate = Certificate {
            id: certificate_id.clone(),
            student: enrollment.student.clone(),
            course_id: course_id.clone(),
            course_title,
            course_version: enrollment.course_version,
            content_hash: enrollment.content_hash.clone(),
            enrollment_reference: enrollment_reference.clone(),
            instructor: course.instructor.clone(),
            issued_by: env.current_contract_address(),
            issued_at_ledger,
            revoked: false,
            revoked_by: None,
            revocation_ledger: None,
            revocation_reason: None,
            expires_at_ledger,
            instructor_attestation,
            revocation_deadline: None,
            superseded_by: None,
        };

        Self::persist(
            &env,
            &DataKey::Certificate(certificate_id.clone()),
            &certificate,
        );

        // If this is a corrected reissue, back-link the old revoked certificate
        // to the new one so verifiers can follow the chain.
        if let Some(ref prev_id) = superseded_cert_id {
            let prev_key = DataKey::Certificate(prev_id.clone());
            if let Some(mut prev_cert) = env
                .storage()
                .persistent()
                .get::<DataKey, Certificate>(&prev_key)
            {
                prev_cert.superseded_by = Some(certificate_id.clone());
                Self::persist(&env, &prev_key, &prev_cert);
            }
        }

        // Mark enrollment as certificate issued
        enrollment.certificate_issued = true;
        enrollment.certificate_id = Some(certificate_id.clone());
        Self::persist(
            &env,
            &DataKey::Enrollment(student.clone(), course_id.clone()),
            &enrollment,
        );

        // Increment certificate count for course
        course.certificates_issued = course
            .certificates_issued
            .checked_add(1)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow));
        course.last_updated_ledger = env.ledger().sequence();
        Self::persist_course(&env, &course_id, &course);

        // Index the certificate under its course so course-wide bulk
        // operations (bulk_revoke_course_certificates) can enumerate every
        // certificate ever issued for this course without off-chain data.
        let course_certs_key = DataKey::CourseCertificates(course_id.clone());
        let mut course_certs: Vec<String> = env
            .storage()
            .persistent()
            .get(&course_certs_key)
            .unwrap_or_else(|| Vec::new(&env));
        course_certs.push_back(certificate_id.clone());
        Self::persist(&env, &course_certs_key, &course_certs);

        // Index the certificate under its student so a wallet or profile page
        // can enumerate all credentials (including revoked ones) on-chain (#414).
        let student_certs_key = DataKey2::StudentCertificates(student.clone());
        let mut student_certs: Vec<String> = env
            .storage()
            .persistent()
            .get(&student_certs_key)
            .unwrap_or_else(|| Vec::new(&env));
        student_certs.push_back(certificate_id.clone());
        Self::persist(&env, &student_certs_key, &student_certs);

        Self::update_instructor_stats(&env, &certificate.instructor, |s| {
            s.total_certificates = s
                .total_certificates
                .checked_add(1)
                .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow));
        });

        // The certificate ID is indexed in the topics for efficient filtering;
        // the payload carries the issuance details and audit actor.
        env.events().publish(
            (
                Symbol::new(&env, "certificate_issued"),
                certificate_id.clone(),
            ),
            (
                certificate_id.clone(),
                student,
                course_id,
                admin,
                issued_at_ledger,
                certificate.instructor.clone(),
                expires_at_ledger,
            ),
        );

        certificate_id
    }

    /// Extend the expiry of an existing, non-revoked certificate (#419).
    ///
    /// Only an admin may call this. The new expiry must be in the future and
    /// must not shorten the current validity window. Revoked certificates
    /// (including ones with a pending, challengeable revocation) cannot be
    /// renewed; they must be reissued instead.
    ///
    /// Emits `certificate_renewed` with
    /// `(certificate_id, admin, new_expiry, ledger_sequence)`.
    pub fn renew_certificate(
        env: Env,
        admin: Address,
        certificate_id: String,
        new_expiry: u32,
    ) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "renew_certificate");

        let current_ledger = env.ledger().sequence();
        if new_expiry <= current_ledger {
            soroban_sdk::panic_with_error!(&env, Error::InvalidCertificateInput);
        }

        let key = DataKey::Certificate(certificate_id.clone());
        let mut certificate: Certificate = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CertificateError));
        if certificate.revoked {
            soroban_sdk::panic_with_error!(&env, Error::CertificateError);
        }
        if let Some(existing) = certificate.expires_at_ledger {
            if new_expiry <= existing {
                soroban_sdk::panic_with_error!(&env, Error::InvalidCertificateInput);
            }
        }

        certificate.expires_at_ledger = Some(new_expiry);
        Self::persist(&env, &key, &certificate);

        env.events().publish(
            (
                Symbol::new(&env, "certificate_renewed"),
                certificate_id.clone(),
            ),
            (certificate_id, admin, new_expiry, current_ledger),
        );
    }

    /// Admin revokes a certificate (e.g. issued in error or academic dishonesty).
    /// Revocation is not immediate — it enters a pending state during which the
    /// certificate holder may dispute the revocation via `challenge_revocation()`.
    /// After the configured challenge period (measured in ledger sequences) expires
    /// without a successful challenge, the revocation becomes permanent.
    ///
    /// Revoked certificates remain on-chain for audit purposes but are flagged.
    /// The revoking admin's address, the ledger sequence, and a reason code are
    /// all persisted so the revocation is fully auditable after the fact.
    ///
    /// Revocation does **not** clear `Enrollment.certificate_issued` or
    /// `Enrollment.certificate_id` — that enrollment record permanently
    /// carries the fact that a certificate was issued for it.
    ///
    /// When the reason is `ISSUED_IN_ERROR` (e.g. wrong title, wrong expiry),
    /// the admin may call `issue_certificate` again for the same enrollment to
    /// issue a corrected replacement.  The replacement is a brand-new
    /// certificate with its own ID; the old revoked certificate remains
    /// on-chain and gains a `superseded_by` link to the new one.  Revocations
    /// made for any other reason (e.g. `ACADEMIC_DISHONESTY`) are final and
    /// block reissuance against the same enrollment.
    ///
    /// # Arguments
    /// - `admin`          — must match stored admin
    /// - `certificate_id` — the certificate to revoke
    /// - `reason`         — short reason code (e.g. "ACADEMIC_DISHONESTY", "ISSUED_IN_ERROR")
    ///   must be non-empty and at most `MAX_REVOCATION_REASON_LEN` (64) bytes
    ///
    /// # Panics
    /// - If `reason` is empty or longer than 64 bytes
    pub fn revoke_certificate(env: Env, admin: Address, certificate_id: String, reason: String) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "revoke_certificate");
        if reason.len() == 0 {
            panic!("revocation reason cannot be empty");
        }
        if reason.len() > Self::MAX_REVOCATION_REASON_LEN {
            panic!("revocation reason exceeds maximum length");
        }
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        // The certificate must exist and must not already be revoked (either
        // pending or permanent). This check is kept here — rather than only in
        // the shared helper — so a duplicate single revocation still fails
        // loudly instead of silently succeeding.
        let cert = env
            .storage()
            .persistent()
            .get::<DataKey, Certificate>(&DataKey::Certificate(certificate_id.clone()))
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CertificateError));
        if cert.revoked {
            soroban_sdk::panic_with_error!(&env, Error::CertificateError);
        }

        Self::apply_revocation(&env, &admin, &certificate_id, &reason);
    }

    /// Admin revokes a page of certificates issued for a single course.
    ///
    /// Used when a course as a whole is found to be fraudulent or its content
    /// invalid: instead of calling `revoke_certificate()` once per graduate —
    /// operationally infeasible for courses with hundreds of students — the
    /// admin revokes the entire cohort at once.
    ///
    /// Each certificate receives exactly the same pending-revocation semantics
    /// as `revoke_certificate()`: the holder keeps the standard challenge
    /// period and may call `challenge_revocation()` to dispute it. Certificates
    /// that are already revoked are skipped, so the call is safe to retry.
    /// `limit` is clamped to `MAX_PAGE_SIZE`; continue from the returned offset
    /// until it reaches the indexed certificate count.
    ///
    /// # Arguments
    /// - `admin`     — must match stored admin
    /// - `course_id` — the course whose certificates should be revoked
    /// - `offset`    — zero-based index of the first certificate to process
    /// - `limit`     — requested page size; must be positive and is clamped
    ///
    /// # Returns
    /// A tuple of the number newly marked for revocation and the next offset.
    pub fn bulk_revoke_course_certificates(
        env: Env,
        admin: Address,
        course_id: String,
        offset: u32,
        limit: u32,
    ) -> (u32, u32) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "bulk_revoke_course_certificates");
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        if Self::get_course_internal(&env, &course_id).is_none() {
            soroban_sdk::panic_with_error!(&env, Error::CourseNotFound);
        }

        let certificate_ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::CourseCertificates(course_id.clone()))
            .unwrap_or_else(|| Vec::new(&env));

        if certificate_ids.is_empty() {
            soroban_sdk::panic_with_error!(&env, Error::CertificateError);
        }
        if limit == 0 {
            soroban_sdk::panic_with_error!(&env, Error::CertificateError);
        }

        let total = certificate_ids.len();
        let end = offset
            .saturating_add(limit.min(Self::MAX_PAGE_SIZE))
            .min(total);

        // One fixed reason code keeps the bulk path auditable and lets
        // off-chain indexers tell bulk revocations apart from targeted ones.
        let reason = String::from_str(&env, "BULK_COURSE_REVOCATION");
        let mut revoked_count: u32 = 0;

        for i in offset.min(total)..end {
            let certificate_id = certificate_ids.get(i).unwrap();
            if Self::apply_revocation(&env, &admin, &certificate_id, &reason) {
                revoked_count = revoked_count.checked_add(1).unwrap_or_else(|| {
                    soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow)
                });
            }
        }

        env.events().publish(
            (
                Symbol::new(&env, "course_certificates_revoked"),
                course_id.clone(),
            ),
            (admin, course_id, revoked_count, env.ledger().sequence()),
        );

        (revoked_count, end)
    }

    /// Apply a pending revocation to a single certificate.
    ///
    /// Returns `true` when the certificate was newly flagged as revoked and
    /// `false` when it was already revoked (pending or permanent). Both
    /// `revoke_certificate` and `bulk_revoke_course_certificates` route
    /// through here so a certificate ends up in an identical state — same
    /// metadata, same event, same challenge-period rules — regardless of which
    /// path revoked it.
    fn apply_revocation(
        env: &Env,
        admin: &Address,
        certificate_id: &String,
        reason: &String,
    ) -> bool {
        let certificate_key = DataKey::Certificate(certificate_id.clone());
        let mut cert: Certificate = env
            .storage()
            .persistent()
            .get(&certificate_key)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CertificateError));

        if cert.revoked {
            return false;
        }

        let challenge_period: u32 = env
            .storage()
            .instance()
            .get(&DataKey::RevocationChallengePeriod)
            .unwrap_or(Self::DEFAULT_REVOCATION_CHALLENGE_PERIOD);

        let current_ledger = env.ledger().sequence();
        let deadline = current_ledger
            .checked_add(challenge_period)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow));

        cert.revoked = true;

        // #407 - a revoked certificate no longer counts toward the
        // instructor's track record.
        Self::update_instructor_stats(env, &cert.instructor, |s| {
            s.total_certificates = s.total_certificates.saturating_sub(1);
        });
        cert.revoked_by = Some(admin.clone());
        cert.revocation_ledger = Some(current_ledger);
        cert.revocation_reason = Some(reason.clone());
        cert.revocation_deadline = Some(deadline);

        Self::persist(&env, &certificate_key, &cert);

        // Update the course counters: increment certificates_revoked and
        // decrement certificates_issued so the live (net-active) count stays
        // accurate and a freed slot can be reissued to another student.
        // We load the course here rather than up-front so we don't pay the
        // storage cost when the cert is already revoked (early-return above).
        let course_key = DataKey::Course(cert.course_id.clone());
        if let Some(mut course) = env
            .storage()
            .persistent()
            .get::<DataKey, Course>(&course_key)
        {
            course.certificates_revoked = course
                .certificates_revoked
                .checked_add(1)
                .unwrap_or_else(|| soroban_sdk::panic_with_error!(env, Error::ArithmeticOverflow));
            course.certificates_issued = course
                .certificates_issued
                .checked_sub(1)
                .unwrap_or_else(|| soroban_sdk::panic_with_error!(env, Error::ArithmeticOverflow));
            course.last_updated_ledger = current_ledger;
            Self::persist(env, &course_key, &course);
        }

        env.events().publish(
            (
                Symbol::new(env, "certificate_revoked"),
                certificate_id.clone(),
            ),
            (
                admin.clone(),
                certificate_id.clone(),
                cert.student.clone(),
                cert.course_id.clone(),
                reason.clone(),
                current_ledger,
                deadline,
            ),
        );

        true
    }

    /// Student challenges a pending certificate revocation.
    ///
    /// During the configured challenge period (measured in ledger sequences),
    /// the certificate holder may dispute a pending revocation. If the challenge
    /// is submitted before the revocation deadline, the pending revocation is
    /// cancelled and the certificate is restored to a valid state.
    ///
    /// # Arguments
    /// - `student`        — must be the certificate holder
    /// - `certificate_id` — the certificate to challenge
    pub fn challenge_revocation(env: Env, student: Address, certificate_id: String) {
        student.require_auth();

        let mut cert = env
            .storage()
            .persistent()
            .get::<DataKey, Certificate>(&DataKey::Certificate(certificate_id.clone()))
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CertificateError));

        // Only the certificate holder may challenge
        if cert.student != student {
            soroban_sdk::panic_with_error!(&env, Error::CertificateError);
        }

        // A revocation must be pending (revoked == true with a deadline)
        if !cert.revoked {
            soroban_sdk::panic_with_error!(&env, Error::CertificateError);
        }

        let deadline = cert
            .revocation_deadline
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CertificateError));

        let current_ledger = env.ledger().sequence();

        // Challenge must be submitted before the deadline
        if current_ledger >= deadline {
            soroban_sdk::panic_with_error!(&env, Error::CertificateError);
        }

        // Cancel the pending revocation
        cert.revoked = false;
        cert.revoked_by = None;
        cert.revocation_ledger = None;
        cert.revocation_reason = None;
        cert.revocation_deadline = None;

        // #407 - revocation cancelled, so the certificate counts again.
        Self::update_instructor_stats(&env, &cert.instructor, |s| {
            s.total_certificates = s
                .total_certificates
                .checked_add(1)
                .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow));
        });

        Self::persist(&env, &DataKey::Certificate(certificate_id.clone()), &cert);

        env.events().publish(
            (
                Symbol::new(&env, "revocation_challenged"),
                certificate_id.clone(),
            ),
            (student, certificate_id, current_ledger),
        );
    }

    // ----------------------------------------------------------
    // ADMIN MANAGEMENT
    // ----------------------------------------------------------

    pub fn pause_platform(env: Env, admin: Address) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "pause_platform");
        if env
            .storage()
            .instance()
            .get(&DataKey::PlatformPaused)
            .unwrap_or(false)
        {
            soroban_sdk::panic_with_error!(&env, Error::PlatformPaused);
        }
        env.storage()
            .instance()
            .set(&DataKey::PlatformPaused, &true);
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        env.events().publish(
            (Symbol::new(&env, "platform_paused"), admin.clone()),
            (admin.clone(), env.ledger().sequence()),
        );
    }

    pub fn unpause_platform(env: Env, admin: Address) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "unpause_platform");
        if !env
            .storage()
            .instance()
            .get(&DataKey::PlatformPaused)
            .unwrap_or(false)
        {
            soroban_sdk::panic_with_error!(&env, Error::PlatformPaused);
        }
        env.storage()
            .instance()
            .set(&DataKey::PlatformPaused, &false);
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        env.events().publish(
            (Symbol::new(&env, "platform_unpaused"), admin.clone()),
            (admin.clone(), env.ledger().sequence()),
        );
    }

    /// Emergency sweep of contract-held tokens to a destination address.
    ///
    /// # Arguments
    /// - `admin`       — admin address (must sign and be the registered admin)
    /// - `token`       — token contract address to withdraw
    /// - `amount`      — amount to withdraw in stroops; must be > 0
    /// - `destination` — recipient address
    ///
    /// Rejects `amount <= 0` with a clear message before attempting the
    /// token transfer, consistent with `withdraw_earnings()`. (#236)
    pub fn withdraw_tokens(
        env: Env,
        admin1: Address,
        admin2: Address,
        token: Address,
        amount: i128,
        destination: Address,
    ) {
        // #236 — Reject zero or negative amounts before any token interaction.
        // A zero-amount call would either be silently accepted by the token
        // contract (emitting a misleading tokens_withdrawn event) or panic
        // with an opaque error from the token contract rather than a clear
        // message from this contract. Negative amounts are never valid.
        if amount <= 0 {
            soroban_sdk::panic_with_error!(&env, Error::InvalidWithdrawal);
        }

        if admin1 == admin2 {
            soroban_sdk::panic_with_error!(&env, Error::Unauthorized);
        }

        admin1.require_auth();
        admin2.require_auth();
        Self::require_multi_admin(&env, &admin1, &admin2);
        let token_client = token::Client::new(&env, &token);
        let contract_balance = token_client.balance(&env.current_contract_address());
        let total_liabilities = Self::get_total_liabilities_internal(&env, &token);

        let available_surplus = contract_balance.saturating_sub(total_liabilities);
        if amount > available_surplus {
            panic!("withdraw_tokens: cannot withdraw below tracked liabilities");
        }

        token_client.transfer(&env.current_contract_address(), &destination, &amount);

        env.events().publish(
            (Symbol::new(&env, "tokens_withdrawn"), admin1.clone()),
            (
                admin1,
                admin2,
                token,
                amount,
                destination,
                env.ledger().sequence(),
            ),
        );
    }

    /// Propose a new admin address (step 1 of two-step transfer).
    /// The new admin must call accept_admin() to complete the handover.
    /// Propose a new admin address (step 1 of two-step transfer).
    /// The new admin must call accept_admin() to complete the handover.
    ///
    /// A minimum cooldown of `MIN_ADMIN_TRANSFER_COOLDOWN` ledger sequences
    /// must elapse between consecutive `transfer_admin()` calls. This prevents
    /// an attacker who briefly gains admin access from rapidly rotating keys
    /// to create confusion and make recovery harder. (#176)
    pub fn transfer_admin(
        env: Env,
        admin1: Address,
        admin2: Address,
        new_admin: Address,
        new_secondary_admin: Address,
    ) {
        admin1.require_auth();
        admin2.require_auth();
        Self::require_multi_admin(&env, &admin1, &admin2);

        let current_admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        let current_sec_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::SecondaryAdmin)
            .unwrap();

        if new_admin == current_admin && new_secondary_admin == current_sec_admin {
            soroban_sdk::panic_with_error!(&env, Error::InvalidAdminConfig);
        }

        if new_admin == new_secondary_admin {
            soroban_sdk::panic_with_error!(&env, Error::InvalidAdminConfig);
        }

        let contract_addr = env.current_contract_address();
        if new_admin == contract_addr || new_secondary_admin == contract_addr {
            soroban_sdk::panic_with_error!(&env, Error::InvalidAdminConfig);
        }

        let treasury = Self::read_treasury(&env);
        if new_admin == treasury || new_secondary_admin == treasury {
            soroban_sdk::panic_with_error!(&env, Error::InvalidAdminConfig);
        }

        if let Some(pending_treasury) = env
            .storage()
            .instance()
            .get::<DataKey, TreasuryUpdate>(&DataKey::PendingTreasury)
        {
            if new_admin == pending_treasury.address
                || new_secondary_admin == pending_treasury.address
            {
                soroban_sdk::panic_with_error!(&env, Error::InvalidAdminConfig);
            }
        }

        // #176 — Enforce minimum cooldown between consecutive transfer_admin() calls.
        // After a key compromise, rapid re-rotation confuses incident response
        // and makes recovery harder. The cooldown gives defenders a predictable
        // window to react before another rotation can be proposed.
        let current_ledger = env.ledger().sequence();
        if let Some(last_transfer_ledger) = env
            .storage()
            .instance()
            .get::<DataKey, u32>(&DataKey::LastAdminTransferLedger)
        {
            let elapsed = current_ledger.saturating_sub(last_transfer_ledger);
            if elapsed < Self::MIN_ADMIN_TRANSFER_COOLDOWN {
                soroban_sdk::panic_with_error!(&env, Error::InvalidAdminConfig);
            }
        }

        // Record this transfer's ledger sequence for the next cooldown check.
        env.storage()
            .instance()
            .set(&DataKey::LastAdminTransferLedger, &current_ledger);

        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);
        env.storage()
            .instance()
            .set(&DataKey::PendingAdmin, &new_admin);
        env.storage()
            .instance()
            .set(&DataKey::PendingSecondaryAdmin, &new_secondary_admin);
        // #427 — the proposal must be accepted within a bounded window.
        env.storage().instance().set(
            &DataKey2::PendingAdminExpiresAt,
            &(current_ledger
                .checked_add(Self::ADMIN_PROPOSAL_VALIDITY_LEDGERS)
                .unwrap_or(u32::MAX)),
        );

        env.events().publish(
            (Symbol::new(&env, "admin_proposed"), new_admin.clone()),
            (
                new_admin,
                new_secondary_admin,
                admin1,
                admin2,
                env.ledger().sequence(),
            ),
        );
    }

    /// Accept a pending admin transfer (step 2 of two-step transfer).
    /// Only the addresses nominated by transfer_admin() can call this.
    pub fn accept_admin(env: Env, new_admin: Address, new_secondary_admin: Address) {
        new_admin.require_auth();
        new_secondary_admin.require_auth();

        let pending: Address = env
            .storage()
            .instance()
            .get(&DataKey::PendingAdmin)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::InvalidAdminConfig));

        let pending_sec: Address = env
            .storage()
            .instance()
            .get(&DataKey::PendingSecondaryAdmin)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::InvalidAdminConfig));

        if pending != new_admin || pending_sec != new_secondary_admin {
            soroban_sdk::panic_with_error!(&env, Error::InvalidAdminConfig);
        }

        // #427 — a stale proposal (e.g. to keys compromised later) must not be
        // activatable years after it was made. Expire it and clear the pending
        // state so the current admins can propose a fresh pair.
        if let Some(expires_at) = env
            .storage()
            .instance()
            .get::<DataKey2, u32>(&DataKey2::PendingAdminExpiresAt)
        {
            if env.ledger().sequence() >= expires_at {
                env.storage().instance().remove(&DataKey::PendingAdmin);
                env.storage()
                    .instance()
                    .remove(&DataKey::PendingSecondaryAdmin);
                env.storage()
                    .instance()
                    .remove(&DataKey2::PendingAdminExpiresAt);
                soroban_sdk::panic_with_error!(&env, Error::AdminExpired);
            }
        }

        let previous_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::InvalidAdminConfig));

        // Capture the outgoing secondary admin before it is overwritten so the
        // emitted event fully describes the key rotation (#429).
        let previous_secondary_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::SecondaryAdmin)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::InvalidAdminConfig));

        env.storage().instance().set(&DataKey::Admin, &new_admin);
        env.storage()
            .instance()
            .set(&DataKey::SecondaryAdmin, &new_secondary_admin);
        env.storage().instance().remove(&DataKey::PendingAdmin);
        env.storage()
            .instance()
            .remove(&DataKey::PendingSecondaryAdmin);
        env.storage()
            .instance()
            .remove(&DataKey2::PendingAdminExpiresAt);

        // Clear the admin expiry when new admins take over
        // This ensures the new admin pair starts fresh without inheriting
        // the previous admin's expiry time
        env.storage().instance().remove(&DataKey::AdminExpiresAt);

        let ledger_sequence = env.ledger().sequence();

        env.events().publish(
            (Symbol::new(&env, "admin_transferred"), new_admin.clone()),
            (
                previous_admin,
                new_admin.clone(),
                previous_secondary_admin,
                new_secondary_admin,
                ledger_sequence,
            ),
        );
    }

    /// Update the platform treasury address.
    /// Emits `treasury_updated` immediately so the pending change is
    /// auditable in real time, even though it only takes effect
    /// `TREASURY_UPDATE_DELAY_LEDGERS` ledgers later (see `TreasuryUpdate`).
    ///
    /// Panics if an earlier update is still pending: a pending update is never
    /// silently replaced, so monitors can rely on it. Call
    /// `cancel_treasury_update` first. An update whose delay has elapsed is
    /// applied (`treasury_activated`) before the new proposal is checked.
    pub fn update_treasury(env: Env, admin1: Address, admin2: Address, new_treasury: Address) {
        admin1.require_auth();
        admin2.require_auth();
        Self::require_multi_admin(&env, &admin1, &admin2);

        if new_treasury == env.current_contract_address() {
            soroban_sdk::panic_with_error!(&env, Error::InvalidTreasury);
        }

        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        let secondary_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::SecondaryAdmin)
            .unwrap();

        if new_treasury == admin {
            soroban_sdk::panic_with_error!(&env, Error::InvalidTreasury);
        }

        if new_treasury == secondary_admin {
            soroban_sdk::panic_with_error!(&env, Error::InvalidTreasury);
        }

        // Apply a matured update first so it is never overwritten, then refuse
        // to replace one that is still pending (cancel it first).
        Self::read_treasury(&env);
        if env.storage().instance().has(&DataKey::PendingTreasury) {
            panic!("pending treasury update exists; cancel it first");
        }

        let current_treasury: Address = env
            .storage()
            .instance()
            .get(&DataKey::Treasury)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::InvalidTreasury));

        if new_treasury == current_treasury {
            soroban_sdk::panic_with_error!(&env, Error::InvalidTreasury);
        }

        // Perform a dry-run token transfer to verify the new treasury can receive tokens.
        // This prevents setting an incompatible address that would silently fail platform fee transfers.
        // We use the approved token (assuming USDC) for the validation - if the treasury can
        // receive one token, it's likely compatible with others. We pick a minimal non-zero
        // amount (1 stroop) to trigger actual reception logic without significant value transfer.

        // Note: Skipping dry-run token transfer validation for now
        // This would require iterating through all approved tokens

        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        let old_treasury: Address = env
            .storage()
            .instance()
            .get(&DataKey::Treasury)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::InvalidTreasury));

        let ledger_sequence = env.ledger().sequence();
        let effective_ledger = ledger_sequence
            .checked_add(Self::TREASURY_UPDATE_DELAY_LEDGERS)
            .unwrap_or_else(|| panic!("overflow computing effective treasury update ledger"));
        let update = TreasuryUpdate {
            address: new_treasury.clone(),
            effective_ledger,
        };
        env.storage()
            .instance()
            .set(&DataKey::PendingTreasury, &update);

        env.events().publish(
            (Symbol::new(&env, "treasury_updated"), new_treasury.clone()),
            (
                old_treasury,
                new_treasury,
                admin1,
                admin2,
                ledger_sequence,
                effective_ledger,
            ),
        );
    }

    /// Get the effective platform treasury, i.e. where fees go right now.
    ///
    /// A scheduled `update_treasury` takes effect once the ledger reaches its
    /// `effective_ledger`, so this returns the pending address from that
    /// ledger onward. Read-only: unlike `enroll`, it never writes the
    /// promotion back to storage.
    pub fn get_treasury(env: Env) -> Address {
        if let Some(pending) = env
            .storage()
            .instance()
            .get::<DataKey, TreasuryUpdate>(&DataKey::PendingTreasury)
        {
            if env.ledger().sequence() >= pending.effective_ledger {
                return pending.address;
            }
        }
        env.storage()
            .instance()
            .get(&DataKey::Treasury)
            .unwrap_or_else(|| panic!("treasury not set"))
    }

    /// Get the scheduled treasury update that has not yet taken effect.
    ///
    /// Returns `None` when no update is scheduled, or once its
    /// `effective_ledger` has been reached (it is then the effective
    /// treasury; see `get_treasury`).
    pub fn get_pending_treasury(env: Env) -> Option<TreasuryUpdate> {
        env.storage()
            .instance()
            .get::<DataKey, TreasuryUpdate>(&DataKey::PendingTreasury)
            .filter(|p| env.ledger().sequence() < p.effective_ledger)
    }
    /// Apply a matured pending treasury update and emit `treasury_activated`.
    ///
    /// This function is **permissionless** — any caller may invoke it once
    /// the `effective_ledger` of the pending update has been reached.  It
    /// exists so that:
    ///
    /// 1. Off-chain systems can trigger the on-chain treasury switch at an
    ///    exact, predictable ledger without having to wait for the next
    ///    enrollment to happen naturally.
    /// 2. Every enrollment path (enroll, batch_enroll, re_enroll) and the
    ///    archive refund path each call `read_treasury()`, which also applies
    ///    the update lazily; this public entry point provides a dedicated,
    ///    observable transaction for the switch that emits an event indexers
    ///    can rely on.
    ///
    /// If no pending update exists, or its `effective_ledger` has not yet been
    /// reached, the call is a no-op (returns the already-active treasury
    /// address without emitting an event).
    ///
    /// # Returns
    /// The current effective treasury address after the call.
    pub fn finalize_treasury_update(env: Env) -> Address {
        Self::read_treasury(&env)
    }

    /// Update the default platform fee percentage.
    ///
    /// Rejects a `new_fee_pct` that is identical to the currently stored
    /// value. A no-op update wastes ledger fees and emits a misleading
    /// fee-change event that could confuse off-chain monitors. (#175)
    pub fn update_default_fee(env: Env, admin: Address, new_fee_pct: u32) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "update_default_fee");
        if new_fee_pct > 100 {
            soroban_sdk::panic_with_error!(&env, Error::InvalidFeeConfig);
        }

        // #175 — Reject a same-value update before writing or emitting an event.
        let current_fee: u32 = env
            .storage()
            .instance()
            .get(&DataKey::DefaultFee)
            .unwrap_or(0);
        if new_fee_pct == current_fee {
            soroban_sdk::panic_with_error!(&env, Error::InvalidFeeConfig);
        }

        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);
        env.storage()
            .instance()
            .set(&DataKey::DefaultFee, &new_fee_pct);

        let ledger_sequence = env.ledger().sequence();
        env.events().publish(
            (Symbol::new(&env, "FeeUpdated"), admin.clone()),
            (current_fee, new_fee_pct, admin.clone(), ledger_sequence),
        );
        env.events().publish(
            (Symbol::new(&env, "default_fee_updated"), admin.clone()),
            (current_fee, new_fee_pct, admin, ledger_sequence),
        );
    }

    /// Admin adds a token contract address to the enrollment whitelist.
    ///
    /// The token must report exactly 7 decimals. Precision is checked here,
    /// once, and cached under `DataKey2::ApprovedTokenDecimals` so neither
    /// course registration nor enrollment needs a cross-contract call (#705).
    pub fn add_approved_token(env: Env, admin: Address, token: Address) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "add_approved_token");

        // Guard: skip the write and list update if the token is already whitelisted.
        if env
            .storage()
            .instance()
            .has(&DataKey::ApprovedToken(token.clone()))
        {
            return;
        }

        // #705 — Reject unsupported precision with a dedicated error before
        // the token enters the whitelist. CourseNotAvailable is reserved for
        // courses that cannot be enrolled in.
        let token_decimals = token::Client::new(&env, &token).decimals();
        if token_decimals != Self::EXPECTED_TOKEN_DECIMALS {
            soroban_sdk::panic_with_error!(&env, Error::InvalidTokenDecimals);
        }
        env.storage().instance().set(
            &DataKey2::ApprovedTokenDecimals(token.clone()),
            &token_decimals,
        );

        env.storage()
            .instance()
            .set(&DataKey::ApprovedToken(token.clone()), &true);

        // Maintain the ordered list for get_approved_tokens().
        let mut list: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey2::ApprovedTokenList)
            .unwrap_or_else(|| Vec::new(&env));
        list.push_back(token.clone());
        env.storage()
            .instance()
            .set(&DataKey2::ApprovedTokenList, &list);

        env.events().publish(
            (Symbol::new(&env, "token_whitelisted"), admin.clone()),
            (admin, token, env.ledger().sequence()),
        );
    }

    /// Admin classifies an approved token as BTC/ETH-denominated (or not).
    /// Payments in a BTC/ETH token incur `RiskFeeConfig.btc_eth_surcharge_bps`
    /// at enroll()/re_enroll() when risk pricing is enabled.
    pub fn set_token_btc_eth(env: Env, admin: Address, token: Address, is_btc_eth: bool) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "set_token_btc_eth");
        if !env
            .storage()
            .instance()
            .has(&DataKey::ApprovedToken(token.clone()))
        {
            soroban_sdk::panic_with_error!(&env, Error::CourseNotAvailable);
        }

        if is_btc_eth {
            env.storage()
                .instance()
                .set(&DataKey2::BtcEthToken(token.clone()), &true);
        } else {
            env.storage()
                .instance()
                .remove(&DataKey2::BtcEthToken(token.clone()));
        }

        env.events().publish(
            (Symbol::new(&env, "token_btc_eth_updated"), admin.clone()),
            (token, is_btc_eth),
        );
    }

    /// Returns whether a token is classified as BTC/ETH-denominated.
    pub fn is_btc_eth_token(env: Env, token: Address) -> bool {
        Self::is_btc_eth_token_internal(&env, &token)
    }

    /// Returns `true` if the given token address is on the enrollment whitelist.
    ///
    /// No authentication required — instructors call this before registering a
    /// course to confirm their chosen token is accepted by the platform.
    pub fn is_token_approved(env: Env, token: Address) -> bool {
        env.storage().instance().has(&DataKey::ApprovedToken(token))
    }

    /// Returns the full list of whitelisted token addresses.
    ///
    /// Admin-only. Returns every address that has been added via
    /// `add_approved_token()` and not yet removed via `remove_approved_token()`.
    /// The ordering is insertion order (append-only).
    pub fn get_approved_tokens(env: Env, admin: Address) -> Vec<Address> {
        admin.require_auth();
        Self::require_admin(&env, &admin, "get_approved_tokens");
        env.storage()
            .instance()
            .get(&DataKey2::ApprovedTokenList)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Returns the number of enrollments a student has ever created on the
    /// platform. Zero means their next enrollment is as a new customer.
    pub fn get_student_enrollment_count(env: Env, student: Address) -> u32 {
        Self::get_student_enrollment_count_internal(&env, &student)
    }

    /// Admin removes a token contract address from the enrollment whitelist.
    pub fn remove_approved_token(env: Env, admin: Address, token: Address) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "remove_approved_token");
        env.storage()
            .instance()
            .remove(&DataKey::ApprovedToken(token.clone()));
        env.storage()
            .instance()
            .remove(&DataKey2::BtcEthToken(token.clone()));
        // #705 — Drop the cached precision alongside the approval flag.
        env.storage()
            .instance()
            .remove(&DataKey2::ApprovedTokenDecimals(token.clone()));

        // Remove the token from the ordered list maintained for get_approved_tokens().
        let list: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey2::ApprovedTokenList)
            .unwrap_or_else(|| Vec::new(&env));
        let mut updated = Vec::new(&env);
        for i in 0..list.len() {
            let entry = list.get(i).unwrap();
            if entry != token {
                updated.push_back(entry);
            }
        }
        env.storage()
            .instance()
            .set(&DataKey2::ApprovedTokenList, &updated);

        env.events().publish(
            (
                Symbol::new(&env, "token_removed_from_whitelist"),
                admin.clone(),
            ),
            (admin, token, env.ledger().sequence()),
        );
    }

    /// Admin updates the maximum number of courses an instructor can register.
    pub fn update_max_courses_limit(env: Env, admin: Address, new_max: u32) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "update_max_courses_limit");
        if new_max == 0 {
            soroban_sdk::panic_with_error!(&env, Error::InvalidMaxCoursesLimit);
        }
        let old_max: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MaxCoursesPerInstructor)
            .unwrap_or(50);
        if new_max == old_max {
            soroban_sdk::panic_with_error!(&env, Error::InvalidCourseInput);
        }
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);
        env.storage()
            .instance()
            .set(&DataKey::MaxCoursesPerInstructor, &new_max);

        env.events().publish(
            (
                Symbol::new(&env, "max_courses_limit_updated"),
                admin.clone(),
            ),
            (admin, old_max, new_max),
        );
    }

    /// Admin sets the per-instructor cap on courses awaiting approval (#377).
    /// Independent of `MaxCoursesPerInstructor`; when never set, the pending
    /// cap falls back to the total cap (the previous behaviour).
    pub fn update_max_pending_limit(env: Env, admin: Address, new_max: u32) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "update_max_pending_limit");
        if new_max == 0 {
            soroban_sdk::panic_with_error!(&env, Error::InvalidMaxCoursesLimit);
        }
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        let old_max: u32 = Self::get_max_pending_limit(env.clone());
        env.storage()
            .instance()
            .set(&DataKey2::MaxPendingCoursesPerInstructor, &new_max);

        env.events().publish(
            (
                Symbol::new(&env, "max_pending_limit_updated"),
                admin.clone(),
            ),
            (admin, old_max, new_max, env.ledger().sequence()),
        );
    }

    /// Current per-instructor cap on courses awaiting approval (#377).
    pub fn get_max_pending_limit(env: Env) -> u32 {
        let max_courses: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MaxCoursesPerInstructor)
            .unwrap_or(50);
        Self::effective_max_pending_courses(&env, max_courses)
    }

    fn effective_max_pending_courses(env: &Env, max_courses: u32) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey2::MaxPendingCoursesPerInstructor)
            .unwrap_or(max_courses)
    }
    /// Admin freezes/blocks a specific instructor address.
    /// Automatically pauses all of the instructor's currently Active courses
    /// to prevent students from enrolling in or paying for frozen instructor courses.
    pub fn freeze_instructor(env: Env, admin: Address, instructor: Address, reason: String) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "freeze_instructor");
        Self::validate_moderation_reason(&reason);
        if Self::is_instructor_frozen_internal(&env, &instructor) {
            soroban_sdk::panic_with_error!(&env, Error::InstructorState);
        }
        env.storage()
            .instance()
            .set(&DataKey::InstructorBlocked(instructor.clone()), &true);
        env.storage().persistent().set(
            &DataKey2::InstructorBlockReason(instructor.clone()),
            &reason,
        );
        env.storage().persistent().extend_ttl(
            &DataKey2::InstructorBlockReason(instructor.clone()),
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        // Pause and mark only currently Active courses so unfreezing can
        // distinguish them from courses that were already paused.
        let course_list_key = DataKey::InstructorCourseList(instructor.clone());
        Self::extend_persistent_ttl_if_exists(&env, &course_list_key);
        if let Some(course_ids) = env.storage().persistent().get::<DataKey, Vec<String>>(
            &course_list_key,
        ) {
            for i in 0..course_ids.len() {
                let course_id = course_ids.get(i).unwrap();
                if let Some(mut course) = env
                    .storage()
                    .persistent()
                    .get::<DataKey, Course>(&DataKey::Course(course_id.clone()))
                {
                    if course.instructor == instructor && course.status == CourseStatus::Active {
                        course.status = CourseStatus::Paused;
                        course.paused_by = PausedBy::InstructorFreeze;
                        course.last_updated_ledger = env.ledger().sequence();
                        Self::persist_course(&env, &course_id, &course);

                        env.events().publish(
                            (Symbol::new(&env, "course_paused"), course_id.clone()),
                            (
                                course_id.clone(),
                                instructor.clone(),
                                env.ledger().sequence(),
                            ),
                        );
                    }
                }
            }
        }

        env.events().publish(
            (Symbol::new(&env, "instructor_frozen"), instructor.clone()),
            (instructor, admin, reason),
        );
    }

    /// Admin unfreezes/unblocks a specific instructor address.
    pub fn unfreeze_instructor(env: Env, admin: Address, instructor: Address) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "unfreeze_instructor");
        if !Self::is_instructor_frozen_internal(&env, &instructor) {
            soroban_sdk::panic_with_error!(&env, Error::InstructorState);
        }

        let course_list_key = DataKey::InstructorCourseList(instructor.clone());
        Self::extend_persistent_ttl_if_exists(&env, &course_list_key);
        if let Some(course_ids) = env.storage().persistent().get::<DataKey, Vec<String>>(
            &course_list_key,
        ) {
            for i in 0..course_ids.len() {
                let course_id = course_ids.get(i).unwrap();
                if let Some(mut course) = env
                    .storage()
                    .persistent()
                    .get::<DataKey, Course>(&DataKey::Course(course_id.clone()))
                {
                    if course.instructor == instructor
                        && course.status == CourseStatus::Paused
                        && course.paused_by == PausedBy::InstructorFreeze
                    {
                        course.status = CourseStatus::Active;
                        course.paused_by = PausedBy::None;
                        course.last_updated_ledger = env.ledger().sequence();
                        Self::persist_course(&env, &course_id, &course);

                        env.events().publish(
                            (Symbol::new(&env, "course_unpaused"), course_id.clone()),
                            (
                                course_id,
                                CourseStatus::Active,
                                admin.clone(),
                                env.ledger().sequence(),
                            ),
                        );
                    }
                }
            }
        }

        env.storage()
            .instance()
            .remove(&DataKey::InstructorBlocked(instructor.clone()));
        env.storage()
            .persistent()
            .remove(&DataKey2::InstructorBlockReason(instructor.clone()));
        env.events().publish(
            (Symbol::new(&env, "instructor_unfrozen"), instructor.clone()),
            (instructor, admin),
        );
    }

    /// Admin adds an approver address that can approve/reject courses.
    /// Approvers have the authority to approve and reject courses without
    /// access to treasury, fee, or admin transfer functions.
    pub fn add_approver(env: Env, admin: Address, approver: Address) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "add_approver");

        if approver == admin {
            soroban_sdk::panic_with_error!(&env, Error::ApproverState);
        }

        // Verify the address is not already an approver
        if Self::is_approver(&env, &approver) {
            soroban_sdk::panic_with_error!(&env, Error::ApproverState);
        }

        env.storage()
            .instance()
            .set(&DataKey::Approver(approver.clone()), &true);
        env.events().publish(
            (Symbol::new(&env, "approver_added"), approver.clone()),
            (approver, admin),
        );
    }

    /// Admin removes an approver address, revoking their approval authority.
    pub fn remove_approver(env: Env, admin: Address, approver: Address) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "remove_approver");

        // Verify the address is actually an approver
        if !Self::is_approver(&env, &approver) {
            soroban_sdk::panic_with_error!(&env, Error::ApproverState);
        }

        env.storage()
            .instance()
            .remove(&DataKey::Approver(approver.clone()));
        env.events().publish(
            (Symbol::new(&env, "approver_removed"), approver.clone()),
            (approver, admin),
        );
    }

    /// Check if an address is an approved approver
    pub fn is_approver_address(env: Env, address: Address) -> bool {
        Self::is_approver(&env, &address)
    }

    /// Check if an instructor is frozen/blocked
    pub fn is_instructor_frozen(env: Env, instructor: Address) -> bool {
        Self::is_instructor_frozen_internal(&env, &instructor)
    }

    /// Get the admin-supplied reason for freezing an instructor, if frozen.
    pub fn get_instructor_freeze_reason(env: Env, instructor: Address) -> Option<String> {
        env.storage()
            .persistent()
            .get(&DataKey2::InstructorBlockReason(instructor))
    }

    /// Admin blocks/bans a specific student address from the platform.
    /// Blocked students cannot enroll in any course.
    pub fn block_student(env: Env, admin: Address, student: Address, reason: String) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "block_student");
        Self::validate_moderation_reason(&reason);
        if Self::is_student_blocked_internal(&env, &student) {
            soroban_sdk::panic_with_error!(&env, Error::StudentBlocked);
        }
        env.storage()
            .instance()
            .set(&DataKey::StudentBlocked(student.clone()), &true);
        env.storage()
            .persistent()
            .set(&DataKey2::StudentBlockReason(student.clone()), &reason);
        env.storage().persistent().extend_ttl(
            &DataKey2::StudentBlockReason(student.clone()),
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );
        env.events().publish(
            (Symbol::new(&env, "student_blocked"), student.clone()),
            (student, admin, reason),
        );
    }

    /// Admin unblocks a previously blocked student address.
    pub fn unblock_student(env: Env, admin: Address, student: Address) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "unblock_student");
        if !Self::is_student_blocked_internal(&env, &student) {
            soroban_sdk::panic_with_error!(&env, Error::StudentBlocked);
        }
        env.storage()
            .instance()
            .remove(&DataKey::StudentBlocked(student.clone()));
        env.storage()
            .persistent()
            .remove(&DataKey2::StudentBlockReason(student.clone()));
        env.events().publish(
            (Symbol::new(&env, "student_unblocked"), student.clone()),
            (student, admin, env.ledger().sequence()),
        );
    }

    /// Check if a student is blocked/banned from the platform
    pub fn is_student_blocked(env: Env, student: Address) -> bool {
        Self::is_student_blocked_internal(&env, &student)
    }

    /// Get the admin-supplied reason for blocking a student, if blocked.
    pub fn get_student_block_reason(env: Env, student: Address) -> Option<String> {
        env.storage()
            .persistent()
            .get(&DataKey2::StudentBlockReason(student))
    }

    /// Get the current per-instructor course registration limit.
    pub fn get_max_courses_limit(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::MaxCoursesPerInstructor)
            .unwrap_or(50)
    }

    /// Set the admin role expiry ledger sequence.
    /// When the current ledger exceeds this value, admin operations are blocked.
    /// Pass `None` to remove the expiry (admin role remains valid indefinitely).
    pub fn set_admin_expiry(env: Env, admin1: Address, admin2: Address, expires_at: Option<u32>) {
        admin1.require_auth();
        admin2.require_auth();
        Self::require_multi_admin(&env, &admin1, &admin2);

        let old_expiry: Option<u32> = env.storage().instance().get(&DataKey::AdminExpiresAt);

        if let Some(expiry) = expires_at {
            if expiry <= env.ledger().sequence() {
                soroban_sdk::panic_with_error!(&env, Error::InvalidAdminConfig);
            }
            env.storage()
                .instance()
                .set(&DataKey::AdminExpiresAt, &expiry);
        } else {
            env.storage().instance().remove(&DataKey::AdminExpiresAt);
        }

        env.events().publish(
            (Symbol::new(&env, "admin_expiry_set"), admin1.clone()),
            (admin1, admin2, old_expiry, expires_at),
        );
    }

    /// Get the admin role expiry ledger sequence, if set.
    pub fn get_admin_expiry(env: Env) -> Option<u32> {
        env.storage().instance().get(&DataKey::AdminExpiresAt)
    }

    /// Set the platform-wide maximum total active paid-enrollment cap.
    /// Free-course enrollments are excluded from the counter and the cap.
    /// Pass `None` to remove the cap (unlimited paid enrollments).
    /// Current primary admin address.
    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic!("admin not set"))
    }

    /// Current secondary admin address.
    pub fn get_secondary_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::SecondaryAdmin)
            .unwrap_or_else(|| panic!("secondary admin not set"))
    }

    /// Pending primary admin from `transfer_admin`, if any.
    pub fn get_pending_admin(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::PendingAdmin)
    }

    /// Pending secondary admin from `transfer_admin`, if any.
    pub fn get_pending_secondary_admin(env: Env) -> Option<Address> {
        env.storage()
            .instance()
            .get(&DataKey::PendingSecondaryAdmin)
    }

    /// Set the platform-wide maximum total active enrollment cap.
    /// Pass `None` to remove the cap (unlimited enrollments).
    pub fn set_platform_enrollment_cap(env: Env, admin: Address, cap: Option<u32>) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "set_platform_enrollment_cap");

        let old_cap: Option<u32> = env.storage().instance().get(&DataKey::PlatformEnrollmentCap);
        let current_total = Self::read_total_active_enrollments(&env);

        if let Some(cap_value) = cap {
            // Reject a cap that would immediately prevent all new enrollments
            // by being below the number already active (#691).
            if cap_value < current_total {
                soroban_sdk::panic_with_error!(&env, Error::InvalidCapBelowActiveEnrollments);
            }
            env.storage()
                .instance()
                .set(&DataKey::PlatformEnrollmentCap, &cap_value);
        } else {
            env.storage()
                .instance()
                .remove(&DataKey::PlatformEnrollmentCap);
        }

        env.events().publish(
            (
                Symbol::new(&env, "platform_enrollment_cap_set"),
                admin.clone(),
            ),
            (admin, old_cap, cap, current_total),
        );
    }

    /// Get the platform-wide maximum total active paid-enrollment cap, if set.
    pub fn get_platform_enrollment_cap(env: Env) -> Option<u32> {
        env.storage()
            .instance()
            .get(&DataKey::PlatformEnrollmentCap)
    }

    // Existing deployments may still have the legacy instance value. Read it
    // until the first mutation writes the canonical persistent entry. Leaving
    // the old value in place avoids an instance write in enrollment paths.
    fn read_total_active_enrollments(env: &Env) -> u32 {
        let key = DataKey::TotalActiveEnrollments;
        if let Some(count) = env.storage().persistent().get(&key) {
            env.storage().persistent().extend_ttl(
                &key,
                Self::PERSISTENT_TTL_THRESHOLD,
                Self::PERSISTENT_TTL_EXTEND_TO,
            );
            return count;
        }
        env.storage().instance().get(&key).unwrap_or(0)
    }

    fn write_total_active_enrollments(env: &Env, count: u32) {
        let key = DataKey::TotalActiveEnrollments;
        env.storage().persistent().set(&key, &count);
        env.storage().persistent().extend_ttl(
            &key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );
    }

    /// Get the current total active enrollments platform-wide.
    pub fn get_total_active_enrollments(env: Env) -> u32 {
        Self::read_total_active_enrollments(&env)
    }

    /// Update the minimum review delay (in ledger sequences).
    /// Maximum: 120,960 ledgers (approximately seven days at five seconds per ledger).
    /// Longer delays could indefinitely prevent course approvals.
    pub fn update_min_review_delay(env: Env, admin: Address, delay: u32) {
        const MAX_REVIEW_DELAY_LEDGERS: u32 = 120_960;

        admin.require_auth();
        Self::require_admin(&env, &admin, "update_min_review_delay");
        assert!(
            delay <= MAX_REVIEW_DELAY_LEDGERS,
            "review delay exceeds maximum"
        );
        let old_delay: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MinReviewDelay)
            .unwrap_or(0);
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);
        env.storage()
            .instance()
            .set(&DataKey::MinReviewDelay, &delay);

        env.events().publish(
            (Symbol::new(&env, "min_review_delay_updated"), admin.clone()),
            (admin, old_delay, delay),
        );
    }

    /// Get the minimum review delay (in ledger sequences)
    pub fn get_min_review_delay(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::MinReviewDelay)
            .unwrap_or(0)
    }

    /// Update the default minimum enrollment duration (in ledger sequences)
    /// that newly-registered courses will require between enroll() and
    /// mark_completed(). Does not retroactively change already-registered
    /// courses.
    pub fn update_min_completion_ledgers(env: Env, admin: Address, ledgers: u32) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "update_min_completion_ledgers");
        let old_ledgers: u32 = env
            .storage()
            .instance()
            .get(&DataKey2::DefaultMinCompletionLedgers)
            .unwrap_or(0);
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        if ledgers > Self::MAX_MIN_COMPLETION_LEDGERS {
            panic!(
                "min completion ledgers {} exceeds maximum allowed {}",
                ledgers,
                Self::MAX_MIN_COMPLETION_LEDGERS
            );
        }

        env.storage()
            .instance()
            .set(&DataKey2::DefaultMinCompletionLedgers, &ledgers);

        env.events().publish(
            (
                Symbol::new(&env, "min_completion_ledgers_updated"),
                admin.clone(),
            ),
            (admin, old_ledgers, ledgers),
        );
    }

    /// Get the default minimum enrollment duration (in ledger sequences)
    pub fn get_min_completion_ledgers(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey2::DefaultMinCompletionLedgers)
            .unwrap_or(0)
    }

    /// Update the refund window (in ledger sequences)
    pub fn update_refund_window(env: Env, admin: Address, window: u32) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "update_refund_window");
        if window > Self::MAX_REFUND_WINDOW_LEDGERS {
            soroban_sdk::panic_with_error!(&env, Error::InvalidRefundWindow);
        }
        let old_window: u32 = env
            .storage()
            .instance()
            .get(&DataKey::RefundWindow)
            .unwrap_or(Self::DEFAULT_REFUND_WINDOW_LEDGERS);
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);
        env.storage()
            .instance()
            .set(&DataKey::RefundWindow, &window);

        env.events().publish(
            (Symbol::new(&env, "refund_window_updated"), admin.clone()),
            (admin, old_window, window),
        );
    }

    /// Get the refund window (in ledger sequences)
    pub fn get_refund_window(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::RefundWindow)
            .unwrap_or(Self::DEFAULT_REFUND_WINDOW_LEDGERS)
    }

    /// Configure the refund processing deadline (in ledger sequences).
    /// After this many ledgers elapse from a refund request, the student
    /// may self-approve it via `claim_refund` without admin action. Bounded
    /// to `[MIN_REFUND_PROCESSING_DEADLINE, MAX_REFUND_PROCESSING_DEADLINE]`.
    pub fn set_refund_processing_deadline(env: Env, admin: Address, ledgers: u32) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "set_refund_processing_deadline");
        if ledgers < Self::MIN_REFUND_PROCESSING_DEADLINE
            || ledgers > Self::MAX_REFUND_PROCESSING_DEADLINE
        {
            panic!("refund processing deadline out of bounds");
        }
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);
        env.storage()
            .instance()
            .set(&DataKey::RefundProcessingDeadline, &ledgers);

        env.events().publish(
            (
                Symbol::new(&env, "refund_processing_deadline_set"),
                admin.clone(),
            ),
            (admin, ledgers),
        );
    }

    /// Get the configured refund processing deadline (in ledger sequences),
    /// or the default if the admin has not set one.
    pub fn get_refund_processing_deadline(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::RefundProcessingDeadline)
            .unwrap_or(Self::DEFAULT_REFUND_PROCESSING_DEADLINE)
    }

    /// Update the revocation challenge period (in ledger sequences).
    /// This is the window during which a certificate holder may dispute
    /// a pending revocation after `revoke_certificate` is called.
    pub fn set_revocation_challenge_period(env: Env, admin: Address, period: u32) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "set_revocation_challenge_period");
        if period == 0 || period > 1_209_600 {
            soroban_sdk::panic_with_error!(&env, Error::InvalidFeeConfig);
        }
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);
        env.storage()
            .instance()
            .set(&DataKey::RevocationChallengePeriod, &period);

        env.events().publish(
            (
                Symbol::new(&env, "revocation_period_updated"),
                admin.clone(),
            ),
            (admin, period),
        );
    }

    /// Get the revocation challenge period (in ledger sequences)
    pub fn get_revocation_challenge_period(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::RevocationChallengePeriod)
            .unwrap_or(Self::DEFAULT_REVOCATION_CHALLENGE_PERIOD)
    }

    /// Request a refund for an enrollment within the refund window.
    ///
    /// ## Resubmission policy
    ///
    /// A student may submit a new refund request for a course whose existing
    /// record has status `Rejected`, provided the current ledger sequence is
    /// still inside the refund window (i.e. `current_sequence -
    /// enrolled_at_ledger <= refund_window`).  When allowed, the `Rejected`
    /// record is replaced: status is reset to `Pending`, `requested_at_ledger`
    /// is updated to the current ledger, and the same `refund_requested` event
    /// is emitted as for a fresh request.
    ///
    /// Requests whose existing record has status `Pending` or `Approved` still
    /// block a new request with `"refund request already exists"` — this
    /// prevents double refunds regardless of the window.
    ///
    /// Outside the window, a resubmission after rejection fails with
    /// `"refund window has closed for resubmission"`.
    pub fn request_refund(env: Env, student: Address, course_id: String) {
        student.require_auth();
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        // Check if student is blocked
        if Self::is_student_blocked_internal(&env, &student) {
            soroban_sdk::panic_with_error!(&env, Error::StudentBlocked);
        }

        let enrollment = Self::get_enrollment_internal(&env, &student, &course_id);

        if enrollment.completed {
            soroban_sdk::panic_with_error!(&env, Error::EnrollmentStateError);
        }

        let refund_window = env
            .storage()
            .instance()
            .get::<DataKey, u32>(&DataKey::RefundWindow)
            .unwrap_or(1000);

        let elapsed = env
            .ledger()
            .sequence()
            .checked_sub(enrollment.enrolled_at_ledger)
            .unwrap_or(0);

        let key = DataKey::RefundRequest(student.clone(), course_id.clone());

        // Check for an existing record before the window check so we can
        // give a clearer error for the out-of-window resubmission case.
        if let Some(existing) = env
            .storage()
            .persistent()
            .get::<DataKey, RefundRequest>(&key)
        {
            match existing.status {
                // Pending or Approved requests block any new request to
                // prevent double refunds.
                RefundStatus::Pending | RefundStatus::Approved => {
                    panic!("refund request already exists");
                }
                // A Rejected request can be replaced if still inside the
                // window; outside the window it fails with a clear message.
                RefundStatus::Rejected => {
                    if elapsed > refund_window {
                        panic!("refund window has closed for resubmission");
                    }
                    // Fall through to overwrite the rejected record below.
                }
                RefundStatus::Cancelled => {
                    if elapsed > refund_window {
                        panic!("refund window has expired");
                    }
                }
            }
        } else {
            // No existing record — apply the normal window guard.
            if elapsed > refund_window {
                panic!("refund window has expired");
            }
        }

        let now = env.ledger().sequence();
        let deadline = now
            .checked_add(
                env.storage()
                    .instance()
                    .get::<DataKey, u32>(&DataKey::RefundProcessingDeadline)
                    .unwrap_or(Self::DEFAULT_REFUND_PROCESSING_DEADLINE),
            )
            .expect("refund deadline overflow");

        let request = RefundRequest {
            student: student.clone(),
            course_id: course_id.clone(),
            requested_at_ledger: now,
            deadline_ledger: deadline,
            status: RefundStatus::Pending,
            refund_amount: None,
        };

        Self::persist(&env, &key, &request);

        env.events().publish(
            (Symbol::new(&env, "refund_requested"), course_id.clone()),
            (
                student,
                course_id,
                enrollment.amount_paid,
                env.ledger().sequence(),
            ),
        );
    }

    /// Let a student withdraw their own pending refund request. Only the
    /// student who filed the request can call this, and only while the
    /// request is still `Pending`.
    pub fn cancel_refund_request(env: Env, student: Address, course_id: String) {
        student.require_auth();
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        let key = DataKey::RefundRequest(student.clone(), course_id.clone());
        let mut request = env
            .storage()
            .persistent()
            .get::<DataKey, RefundRequest>(&key)
            .unwrap_or_else(|| panic!("refund request not found"));

        if request.status != RefundStatus::Pending {
            panic!("refund request is not pending");
        }

        request.status = RefundStatus::Cancelled;
        env.storage().persistent().set(&key, &request);

        env.events().publish(
            (
                Symbol::new(&env, "refund_request_cancelled"),
                course_id.clone(),
            ),
            (student, course_id),
        );
    }

    /// Admin processes a pending refund request (approve or reject).
    ///
    /// Only a request with status `Pending` can be processed.  Calling this
    /// on an `Approved` or `Rejected` record panics with
    /// `"refund request is not pending"`.
    ///
    /// When `approved = false` the record is updated to `Rejected` and **no
    /// funds are moved**.  The student may resubmit a new request (resetting
    /// it to `Pending`) as long as they are still inside the refund window —
    /// see `request_refund` for the full resubmission policy.
    pub fn process_refund(
        env: Env,
        admin: Address,
        student: Address,
        course_id: String,
        approved: bool,
        refund_amount: Option<i128>,
    ) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "process_refund");
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        let key = DataKey::RefundRequest(student.clone(), course_id.clone());
        let mut request = env
            .storage()
            .persistent()
            .get::<DataKey, RefundRequest>(&key)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::RefundError));

        if request.status != RefundStatus::Pending {
            soroban_sdk::panic_with_error!(&env, Error::RefundError);
        }

        let (event_refund_amount, event_platform_amount, event_instructor_amount) = if approved {
            let breakdown =
                Self::apply_refund_approval(&env, &key, &mut request, refund_amount);
            Self::emit_refund_issued(
                &env,
                &course_id,
                &student,
                breakdown.refund_amount,
                breakdown.platform_refund,
                breakdown.instructor_refund,
            );
            (
                breakdown.refund_amount,
                breakdown.platform_refund,
                breakdown.instructor_refund,
            )
        } else {
            request.status = RefundStatus::Rejected;
            request.refund_amount = None;
            Self::persist(&env, &key, &request);
            (0i128, 0i128, 0i128)
        };

        env.events().publish(
            (Symbol::new(&env, "refund_processed"), course_id.clone()),
            (
                student,
                course_id,
                approved,
                admin,
                event_refund_amount,
                event_platform_amount,
                event_instructor_amount,
                env.ledger().sequence(),
            ),
        );
    }

    /// Let a student self-approve a pending refund once the processing
    /// deadline recorded on the request has elapsed, removing the need for
    /// admin action. The deadline is snapshotted at request time, so this
    /// honours whatever `refund_processing_deadline` was configured then.
    pub fn claim_refund(env: Env, student: Address, course_id: String) {
        student.require_auth();
        if Self::is_student_blocked_internal(&env, &student) {
            soroban_sdk::panic_with_error!(&env, Error::StudentBlocked);
        }
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        let key = DataKey::RefundRequest(student.clone(), course_id.clone());
        let mut request = env
            .storage()
            .persistent()
            .get::<DataKey, RefundRequest>(&key)
            .unwrap_or_else(|| panic!("refund request not found"));

        if request.status != RefundStatus::Pending {
            panic!("refund request is not pending");
        }

        if env.ledger().sequence() < request.deadline_ledger {
            panic!("refund processing deadline has not elapsed");
        }

        Self::apply_refund_approval(&env, &key, &mut request, None);

        env.events().publish(
            (Symbol::new(&env, "refund_claimed"), course_id.clone()),
            (student, course_id),
        );
    }

    /// Refund the requested amount back to the student (the full payment
    /// when `refund_amount` is `None`), archive the enrollment to history,
    /// decrement the active-enrollment counters, and mark the request
    /// `Approved`. Shared by the admin `process_refund` path and the
    /// student `claim_refund` self-approval path so both pay and account for
    /// the refund exactly once from the pre-funded refund reserve.
    fn apply_refund_approval(
        env: &Env,
        key: &DataKey,
        request: &mut RefundRequest,
        refund_amount: Option<i128>,
    ) -> RefundBreakdown {
        let mut course = Self::get_course_internal(env, &request.course_id)
            .unwrap_or_else(|| panic!("course not found"));
        let enrollment_key =
            DataKey::Enrollment(request.student.clone(), request.course_id.clone());
        let mut enrollment = env
            .storage()
            .persistent()
            .get::<DataKey, Enrollment>(&enrollment_key)
            .unwrap_or_else(|| panic!("enrollment not found"));

        // Re-check: if the enrollment was marked completed after the refund
        // request was filed (e.g. admin called mark_completed while the request
        // was pending), reject the refund. The student received the course value
        // so a refund would be unwarranted. (#316)
        if enrollment.completed {
            soroban_sdk::panic_with_error!(env, Error::EnrollmentStateError);
        }

        // Use the split actually applied at enroll()/re_enroll() time
        // (persisted on the enrollment record) rather than recomputing
        // it from course.platform_fee_percent, which is never consulted
        // at enrollment and may have since diverged from the FeeConfig
        // (and any risk surcharge) that deduct_fee() actually applied.
        let platform_amount = enrollment.platform_amount;
        // `None` refunds the full payment; `Some` refunds that many stroops
        // and splits them between the platform and instructor proportionally.
        let actual_refund_amount = match refund_amount {
            Some(amount) => {
                if amount < 0 || amount > enrollment.amount_paid {
                    panic!("refund amount must be <= amount_paid");
                }
                amount
            }
            None => enrollment.amount_paid,
        };
        let platform_refund = if enrollment.amount_paid == 0 {
            0
        } else {
            actual_refund_amount
                .checked_mul(platform_amount)
                .map(|v| v / enrollment.amount_paid)
                .unwrap_or_else(|| panic!("overflow computing platform refund"))
        };
        let instructor_refund = actual_refund_amount
            .checked_sub(platform_refund)
            .unwrap_or_else(|| panic!("refund split underflow"));

        let token_client = token::Client::new(env, &course.token);
        if platform_refund > 0 {
            Self::pay_platform_refund(env, &course.token, &request.student, platform_refund);
        }

        // Refund instructor share from contract-held earnings, again using the
        // proportional partial split when a refund amount is requested.
        if instructor_refund > 0 {
            Self::debit_instructor_shares(env, &course, instructor_refund);
            token_client.transfer(
                &env.current_contract_address(),
                &request.student,
                &instructor_refund,
            );
        }

        // Archive the refunded enrollment to EnrollmentHistory before
        // removing the active record, so the refund is auditable on-chain.
        enrollment.is_refunded = true;
        let history_key =
            DataKey::EnrollmentHistory(request.student.clone(), request.course_id.clone());
        let mut history: Vec<Enrollment> = env
            .storage()
            .persistent()
            .get(&history_key)
            .unwrap_or_else(|| Vec::new(env));
        history.push_back(enrollment.clone());
        Self::persist(env, &history_key, &history);
        env.storage().persistent().remove(&enrollment_key);

        let mut stats = Self::read_course_stats(env, &request.course_id, &course);
        if stats.active_enrollments > 0 {
            stats.active_enrollments -= 1;
        }
        if stats.total_enrollments > 0 {
            stats.total_enrollments -= 1;
        }
        Self::write_course_stats(env, &request.course_id, &stats);
        course.active_enrollments = stats.active_enrollments;
        course.total_enrollments = stats.total_enrollments;
        course.refund_count = course
            .refund_count
            .checked_add(1)
            .unwrap_or_else(|| panic!("course refund_count overflow"));
        course.total_refunded = course
            .total_refunded
            .checked_add(actual_refund_amount)
            .unwrap_or_else(|| panic!("course total_refunded overflow"));
        course.last_updated_ledger = env.ledger().sequence();
        Self::persist_course(env, &request.course_id, &course);

        let total_active = Self::read_total_active_enrollments(env);
        if total_active > 0 {
            Self::write_total_active_enrollments(env, total_active - 1);
        }
        Self::dec_total_active(env);
        if enrollment.amount_paid > 0 {
            let paid_active: u32 = env
                .storage()
                .instance()
                .get(&DataKey::TotalActiveEnrollments)
                .unwrap_or(0);
            if paid_active > 0 {
                env.storage()
                    .instance()
                    .set(&DataKey::TotalActiveEnrollments, &(paid_active - 1));
            }
        }

        Self::update_instructor_stats(env, &course.instructor, |s| {
            s.total_students = s.total_students.saturating_sub(1);
        });

        request.status = RefundStatus::Approved;
        request.refund_amount = Some(actual_refund_amount);
        Self::persist(env, key, &*request);

        RefundBreakdown {
            refund_amount: actual_refund_amount,
            platform_refund,
            instructor_refund,
        }
    }

    /// Top up the refund reserve for an approved `token`.
    ///
    /// Rejects unapproved tokens before any token contract call.
    ///
    /// Platform fees are forwarded to the treasury at enrollment, so refunding
    /// the platform-fee share needs funds the contract itself holds. The
    /// `funder` (normally the treasury) signs this transfer once, up front;
    /// `process_refund()` and `archive_course()` then pay from the reserve
    /// and need no treasury authorisation.
    pub fn fund_refund_reserve(env: Env, funder: Address, token: Address, amount: i128) {
        funder.require_auth();
        if amount <= 0 {
            panic!("refund reserve amount must be greater than zero");
        }

        if !Self::is_token_approved(env.clone(), token.clone()) {
            soroban_sdk::panic_with_error!(&env, HamplardError::TokenNotApproved);
        }

        let token_client = token::Client::new(&env, &token);
        token_client.transfer(&funder, &env.current_contract_address(), &amount);

        let key = DataKey2::RefundReserve(token.clone());
        let balance: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        let new_balance = balance
            .checked_add(amount)
            .unwrap_or_else(|| panic!("refund reserve overflow"));
        Self::persist(&env, &key, &new_balance);

        env.events().publish(
            (Symbol::new(&env, "refund_reserve_funded"), token),
            (funder, amount, new_balance),
        );
    }

    /// Get the contract-held refund reserve available for `token`.
    pub fn get_refund_reserve(env: Env, token: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey2::RefundReserve(token))
            .unwrap_or(0)
    }

    /// Return the total platform fees accumulated for `token` since the last
    /// sweep (or since contract deployment).
    pub fn get_platform_fees_accumulated(env: Env, token: Address) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey2::PlatformFeesAccumulated(token))
            .unwrap_or(0)
    }

    /// Transfer all accumulated platform fees for `token` to the current
    /// treasury address in a single call.
    ///
    /// This is permissionless: anyone may call it, and the funds always go to
    /// the treasury (never to `caller`).  Callers pay the gas for the
    /// cross-contract transfer, which benefits the platform by reducing per-
    /// enrollment overhead.
    ///
    /// Panics if there are no fees to sweep (to avoid a no-op transfer).
    pub fn sweep_platform_fees(env: Env, caller: Address, token: Address) {
        caller.require_auth();
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        let acc_key = DataKey2::PlatformFeesAccumulated(token.clone());
        let amount: i128 = env.storage().instance().get(&acc_key).unwrap_or(0);
        if amount == 0 {
            panic!("no platform fees to sweep");
        }

        // Clear the accumulator before transferring to protect against
        // re-entrancy (even though Soroban's single-threaded model makes it
        // practically impossible today).
        env.storage().instance().set(&acc_key, &0i128);

        let treasury = Self::read_treasury(&env);
        token::Client::new(&env, &token).transfer(
            &env.current_contract_address(),
            &treasury,
            &amount,
        );

        env.events().publish(
            (Symbol::new(&env, "platform_fees_swept"), token.clone()),
            (treasury, token, amount, env.ledger().sequence()),
        );
    }

    /// Pay the platform-fee share of a refund to `student` out of the
    /// contract-held refund reserve for `token`.
    fn pay_platform_refund(env: &Env, token: &Address, student: &Address, amount: i128) {
        let key = DataKey2::RefundReserve(token.clone());
        let balance: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        if balance < amount {
            panic!("insufficient refund reserve");
        }
        Self::persist(env, &key, &(balance - amount));
        token::Client::new(env, token).transfer(&env.current_contract_address(), student, &amount);
        Self::decrement_platform_fees_collected(env, token, amount);
    }

    /// Increment net cumulative platform fees collected for `token`.
    fn increment_platform_fees_collected(env: &Env, token: &Address, amount: i128) {
        let key = DataKey2::PlatformFeesCollected(token.clone());
        let current: i128 = env.storage().instance().get(&key).unwrap_or(0);
        let updated = current
            .checked_add(amount)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(env, Error::ArithmeticOverflow));
        env.storage().instance().set(&key, &updated);
    }

    /// Decrement net cumulative platform fees collected for `token`.
    fn decrement_platform_fees_collected(env: &Env, token: &Address, amount: i128) {
        let key = DataKey2::PlatformFeesCollected(token.clone());
        let current: i128 = env.storage().instance().get(&key).unwrap_or(0);
        let updated = current.saturating_sub(amount);
        if updated == 0 {
            env.storage().instance().remove(&key);
        } else {
            env.storage().instance().set(&key, &updated);
        }
    }

    /// Get a refund request record by student and course ID
    pub fn get_refund_request(
        env: Env,
        student: Address,
        course_id: String,
    ) -> Option<RefundRequest> {
        env.storage()
            .persistent()
            .get(&DataKey::RefundRequest(student, course_id))
    }

    /// Get the number of courses an instructor has registered.
    pub fn get_instructor_course_count(env: Env, instructor: Address) -> u32 {
        Self::read_instructor_course_count(&env, &instructor)
    }

    /// Get the ordered list of course IDs an instructor has registered.
    /// The list is append-only: pausing, unpausing, or archiving a course
    /// changes only its `Course.status` and never removes it from this
    /// list, so it always reflects every course the instructor has ever
    /// registered.
    pub fn get_courses_by_instructor(env: Env, instructor: Address) -> Vec<String> {
        let key = DataKey::InstructorCourseList(instructor);
        Self::extend_persistent_ttl_if_exists(&env, &key);
        env.storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Get an instructor's on-chain reputation stats — total students
    /// enrolled, total completions, and total certificates issued across
    /// all of their courses. Gives students an on-chain signal of an
    /// instructor's track record. A completion rate can be derived off
    /// this as `total_completions / total_students`.
    ///
    /// Returns zeroed stats if the instructor has no recorded activity yet.
    pub fn get_instructor_stats(env: Env, instructor: Address) -> InstructorStats {
        env.storage()
            .persistent()
            .get(&DataKey::InstructorStats(instructor))
            .unwrap_or(InstructorStats {
                total_students: 0,
                total_completions: 0,
                total_certificates: 0,
            })
    }

    /// Get the global list of all instructors who have registered courses.
    /// Admin-only to prevent potential privacy concerns with exposing the full
    /// instructor list.
    ///
    /// Returns an empty list if no instructors have registered yet.
    pub fn get_all_instructors(env: Env, admin: Address) -> Vec<Address> {
        admin.require_auth();
        Self::require_admin(&env, &admin, "get_all_instructors");

        env.storage()
            .persistent()
            .get(&DataKey::InstructorRegistry)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Get the total number of registered instructors.
    /// Admin-only query for platform analytics.
    pub fn get_instructor_count(env: Env, admin: Address) -> u32 {
        admin.require_auth();
        Self::require_admin(&env, &admin, "get_instructor_count");

        let registry: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::InstructorRegistry)
            .unwrap_or_else(|| Vec::new(&env));

        registry.len()
    }

    // ----------------------------------------------------------
    // CONTRACT UPGRADE
    // ----------------------------------------------------------
    //
    // Upgrades are two-step and time-locked:
    //   1. `propose_upgrade`  — both admins sign, recording the new Wasm
    //      hash and an `effective_ledger` at least `UpgradeTimelock`
    //      ledgers in the future.
    //   2. `upgrade_contract` — both admins sign again, after
    //      `effective_ledger` has passed, to actually swap the code.
    // This gives observers a mandatory window to review a pending upgrade
    // (and, if compromised, react) before it can take effect. A pending
    // upgrade can be withdrawn at any time via `cancel_upgrade`.
    //
    // Enrollment, certificate, and all other contract storage is untouched
    // by an upgrade — only the executable Wasm code is replaced, so
    // existing data survives across versions.

    /// Propose a contract code upgrade. Requires both admin signatures.
    ///
    /// `new_wasm_hash` must reference Wasm already uploaded on-chain via
    /// `env.deployer().upload_contract_wasm()`. The upgrade cannot be
    /// executed via `upgrade_contract` until `UpgradeTimelock` ledgers have
    /// elapsed, giving the community a governance review window.
    ///
    /// Proposing again while an upgrade is already pending replaces it
    /// (and resets the time-lock) rather than stacking proposals.
    pub fn propose_upgrade(env: Env, admin1: Address, admin2: Address, new_wasm_hash: BytesN<32>) {
        admin1.require_auth();
        admin2.require_auth();
        Self::require_multi_admin(&env, &admin1, &admin2);
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        let timelock = Self::get_upgrade_timelock(env.clone());
        let proposed_at_ledger = env.ledger().sequence();
        let effective_ledger = proposed_at_ledger
            .checked_add(timelock)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow));

        let pending = PendingUpgrade {
            new_wasm_hash: new_wasm_hash.clone(),
            proposed_at_ledger,
            effective_ledger,
        };
        env.storage()
            .instance()
            .set(&DataKey::PendingUpgrade, &pending);

        env.events().publish(
            (Symbol::new(&env, "upgrade_proposed"), new_wasm_hash.clone()),
            (
                new_wasm_hash,
                admin1,
                admin2,
                proposed_at_ledger,
                effective_ledger,
            ),
        );
    }

    /// Cancel a pending upgrade proposal. Requires both admin signatures.
    pub fn cancel_upgrade(env: Env, admin1: Address, admin2: Address) {
        admin1.require_auth();
        admin2.require_auth();
        Self::require_multi_admin(&env, &admin1, &admin2);

        let pending: PendingUpgrade = env
            .storage()
            .instance()
            .get(&DataKey::PendingUpgrade)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::UpgradeError));

        env.storage().instance().remove(&DataKey::PendingUpgrade);

        env.events().publish(
            (
                Symbol::new(&env, "upgrade_cancelled"),
                pending.new_wasm_hash.clone(),
            ),
            (
                pending.new_wasm_hash,
                admin1,
                admin2,
                env.ledger().sequence(),
            ),
        );
    }

    /// Execute a pending upgrade once its time-lock has elapsed. Requires
    /// both admin signatures. Replaces this contract's executable Wasm —
    /// all persistent and instance storage (enrollments, certificates,
    /// courses, admin config, etc.) is preserved across the upgrade.
    pub fn upgrade_contract(env: Env, admin1: Address, admin2: Address) {
        admin1.require_auth();
        admin2.require_auth();
        Self::require_multi_admin(&env, &admin1, &admin2);

        let pending: PendingUpgrade = env
            .storage()
            .instance()
            .get(&DataKey::PendingUpgrade)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::UpgradeError));

        if env.ledger().sequence() < pending.effective_ledger {
            soroban_sdk::panic_with_error!(&env, Error::UpgradeError);
        }

        env.storage().instance().remove(&DataKey::PendingUpgrade);

        env.events().publish(
            (
                Symbol::new(&env, "upgrade_executed"),
                pending.new_wasm_hash.clone(),
            ),
            (
                pending.new_wasm_hash.clone(),
                admin1,
                admin2,
                env.ledger().sequence(),
            ),
        );

        env.deployer()
            .update_current_contract_wasm(pending.new_wasm_hash);
    }

    /// Configure the upgrade governance time-lock, in ledger sequences.
    /// Requires both admin signatures. Does not affect an already-pending
    /// upgrade's `effective_ledger` — propose a new upgrade to apply a
    /// changed time-lock.
    pub fn set_upgrade_timelock(env: Env, admin1: Address, admin2: Address, ledgers: u32) {
        admin1.require_auth();
        admin2.require_auth();
        Self::require_multi_admin(&env, &admin1, &admin2);

        env.storage()
            .instance()
            .set(&DataKey::UpgradeTimelock, &ledgers);

        env.events().publish(
            (
                Symbol::new(&env, "upgrade_timelock_updated"),
                admin1.clone(),
            ),
            (admin1, admin2, ledgers),
        );
    }

    /// Get the currently configured upgrade time-lock, in ledger sequences.
    pub fn get_upgrade_timelock(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::UpgradeTimelock)
            .unwrap_or(Self::DEFAULT_UPGRADE_TIMELOCK_LEDGERS)
    }

    /// Get the currently pending upgrade proposal, if any.
    pub fn get_pending_upgrade(env: Env) -> Option<PendingUpgrade> {
        env.storage().instance().get(&DataKey::PendingUpgrade)
    }

    // ----------------------------------------------------------
    // READ-ONLY QUERIES
    // ----------------------------------------------------------

    /// Get a course record by ID.
    /// Extends the course's persistent storage TTL on every read so that
    /// actively queried courses never expire silently due to read-only traffic.
    pub fn get_course(env: Env, course_id: String) -> Option<Course> {
        let key = DataKey::Course(course_id.clone());
        // Extend TTL whenever the entry exists, regardless of whether we return
        // Some or None — the has() check is cheap and the extend is a no-op when
        // the entry is absent.
        if env.storage().persistent().has(&key) {
            env.storage().persistent().extend_ttl(
                &key,
                Self::PERSISTENT_TTL_THRESHOLD,
                Self::PERSISTENT_TTL_EXTEND_TO,
            );
            let stats_key = DataKey2::CourseStats(course_id.clone());
            if env.storage().persistent().has(&stats_key) {
                env.storage().persistent().extend_ttl(
                    &stats_key,
                    Self::PERSISTENT_TTL_THRESHOLD,
                    Self::PERSISTENT_TTL_EXTEND_TO,
                );
            }
        }
        Self::get_course_internal(&env, &course_id)
    }

    /// Get the `CourseTerms` snapshot for a specific `(course_id, version)` (#371).
    pub fn get_course_terms(env: Env, course_id: String, version: u32) -> Option<CourseTerms> {
        let key = DataKey2::CourseTerms(course_id, version);
        if env.storage().persistent().has(&key) {
            env.storage().persistent().extend_ttl(
                &key,
                Self::PERSISTENT_TTL_THRESHOLD,
                Self::PERSISTENT_TTL_EXTEND_TO,
            );
        }
        env.storage().persistent().get(&key)
    }

    /// Get an enrollment record for a student + course pair
    ///
    /// Returns `Some(Enrollment)` if the record exists and has not expired.
    /// Returns `None` if:
    /// - The student has never enrolled in this course
    /// - The enrollment record has exceeded its TTL and been garbage collected
    ///
    /// To check only existence without retrieving data, use `is_enrolled()`.
    pub fn get_enrollment(
        env: Env,
        caller: Address,
        student: Address,
        course_id: String,
    ) -> Option<Enrollment> {
        caller.require_auth();
        let is_admin = Self::is_admin(&env, &caller);

        // #374 — Only load the course when we can't already grant access.
        // Students and admins are always authorised; the course entry is only
        // needed to check whether the caller is the course instructor.
        // If the course entry has been archived by TTL, students and admins
        // can still read their enrollment; a third party gets Unauthorized
        // rather than a misleading CourseNotFound.
        if caller != student && !is_admin {
            let course = Self::get_course_internal(&env, &course_id)
                .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CourseNotFound));
            if caller != course.instructor {
                soroban_sdk::panic_with_error!(&env, Error::Unauthorized);
            }
        }

        env.storage()
            .persistent()
            .get(&DataKey::Enrollment(student, course_id))
    }

    /// Get the archived enrollment history (past completed or expired
    /// attempts) for a student/course pair, populated by `re_enroll()`.
    /// Access follows the same rules as `get_enrollment`.
    pub fn get_enrollment_history(
        env: Env,
        caller: Address,
        student: Address,
        course_id: String,
    ) -> Vec<Enrollment> {
        caller.require_auth();
        let is_admin = Self::is_admin(&env, &caller);

        // #374 — Same lazy course-load logic as get_enrollment: students and
        // admins are always authorised without needing the course entry.
        if caller != student && !is_admin {
            let course = Self::get_course_internal(&env, &course_id)
                .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CourseNotFound));
            if caller != course.instructor {
                soroban_sdk::panic_with_error!(&env, Error::Unauthorized);
            }
        }

        env.storage()
            .persistent()
            .get(&DataKey::EnrollmentHistory(student, course_id))
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Get a certificate by ID.
    ///
    /// Only the certificate's student, the course instructor, or the admin
    /// may read it; any other caller is rejected (same rules as
    /// `get_enrollment`).
    pub fn get_certificate(env: Env, caller: Address, certificate_id: String) -> Certificate {
        caller.require_auth();
        let cert = env
            .storage()
            .persistent()
            .get::<DataKey, Certificate>(&DataKey::Certificate(certificate_id))
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CertificateError));

        let is_admin = Self::is_admin(&env, &caller);
        let is_instructor = caller == cert.instructor;
        let is_student = caller == cert.student;

        if !is_student && !is_admin && !is_instructor {
            soroban_sdk::panic_with_error!(&env, Error::Unauthorized);
        }
        cert
    }

    /// List every certificate ID issued for a course, in issuance order.
    ///
    /// Populated by `issue_certificate` as an append-only index and consumed by
    /// `bulk_revoke_course_certificates()`. Returns an empty list for a course
    /// with no issued certificates.
    pub fn get_course_certificates(env: Env, course_id: String) -> Vec<String> {
        env.storage()
            .persistent()
            .get(&DataKey::CourseCertificates(course_id))
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Paginated view of the certificate IDs issued for a course (#415).
    /// `limit` is clamped to 50 per page; an out-of-range `offset` returns an
    /// empty page. Pages follow issuance order.
    pub fn get_course_certificates_page(
        env: Env,
        course_id: String,
        offset: u32,
        limit: u32,
    ) -> Vec<String> {
        const MAX_PAGE: u32 = 50;
        let all: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::CourseCertificates(course_id))
            .unwrap_or_else(|| Vec::new(&env));
        let total = all.len();
        let mut page: Vec<String> = Vec::new(&env);
        if limit == 0 || offset >= total {
            return page;
        }
        let end = total.min(offset.saturating_add(limit.min(MAX_PAGE)));
        let mut i = offset;
        while i < end {
            page.push_back(all.get(i).unwrap());
            i += 1;
        }
        page
    }

    /// Number of certificates ever issued (indexed) for a course (#415).
    pub fn get_course_certificate_count(env: Env, course_id: String) -> u32 {
        let all: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::CourseCertificates(course_id))
            .unwrap_or_else(|| Vec::new(&env));
        all.len()
    }

    /// Paginated view of all certificate IDs issued to `student` in issuance order (#414).
    ///
    /// Includes revoked certificates (which remain flagged on-chain on their
    /// `Certificate` record). `limit` is clamped to `MAX_PAGE_SIZE` (50);
    /// `limit == 0` or an out-of-range `offset` returns an empty `Vec`.
    pub fn get_certificates_by_student(
        env: Env,
        student: Address,
        offset: u32,
        limit: u32,
    ) -> Vec<String> {
        let all: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey2::StudentCertificates(student))
            .unwrap_or_else(|| Vec::new(&env));
        let total = all.len();
        let mut page: Vec<String> = Vec::new(&env);
        if limit == 0 || offset >= total {
            return page;
        }
        let end = total.min(offset.saturating_add(limit.min(Self::MAX_PAGE_SIZE)));
        let mut i = offset;
        while i < end {
            page.push_back(all.get(i).unwrap());
            i += 1;
        }
        page
    }
    /// Check whether a student has an active enrollment record for a course.
    ///
    /// This is a storage-presence check: it returns `true` for completed and
    /// expired enrollments as long as their record still exists, and `false`
    /// after a refund removes the record. Use `get_enrollment_status()` when
    /// the lifecycle state matters.
    pub fn is_enrolled(env: Env, student: Address, course_id: String) -> bool {
        env.storage()
            .persistent()
            .has(&DataKey::Enrollment(student, course_id))
    }

    /// Return the lifecycle status of a student's enrollment in a course.
    ///
    /// Active and completed records take precedence over history. If no active
    /// record exists, archived refunded records are checked so a refunded
    /// enrollment is reported as `Refunded` rather than `None`.
    pub fn get_enrollment_status(
        env: Env,
        student: Address,
        course_id: String,
    ) -> EnrollmentStatus {
        if let Some(enrollment) = env
            .storage()
            .persistent()
            .get::<DataKey, Enrollment>(&DataKey::Enrollment(student.clone(), course_id.clone()))
        {
            if enrollment.is_refunded {
                return EnrollmentStatus::Refunded;
            }
            if enrollment.completed {
                return EnrollmentStatus::Completed;
            }
            if let Some(course) = Self::get_course_internal(&env, &course_id) {
                if Self::is_enrollment_expired_internal(&env, &course, &enrollment) {
                    return EnrollmentStatus::Expired;
                }
            }
            return EnrollmentStatus::Active;
        }

        if let Some(history) = env
            .storage()
            .persistent()
            .get::<DataKey, Vec<Enrollment>>(&DataKey::EnrollmentHistory(student, course_id))
        {
            let mut i = history.len();
            while i > 0 {
                i -= 1;
                if let Some(entry) = history.get(i) {
                    if entry.is_refunded {
                        return EnrollmentStatus::Refunded;
                    }
                }
            }
        }

        EnrollmentStatus::None
    }

    /// Check whether a student has completed a course.
    ///
    /// Returns:
    /// - `None`        — student has no enrollment record for this course
    /// - `Some(false)` — enrolled but not yet completed
    /// - `Some(true)`  — enrollment exists and is marked completed
    pub fn has_completed(env: Env, student: Address, course_id: String) -> Option<bool> {
        if let Some(enrollment) = env
            .storage()
            .persistent()
            .get::<DataKey, Enrollment>(&DataKey::Enrollment(student, course_id))
        {
            if enrollment.completed {
                return Some(true);
            }
            // Not yet completed — expired and active-but-incomplete both
            // report `Some(false)`.
            Some(false)
        } else {
            None
        }
    }

    /// Return the verification status of a certificate without exposing private
    /// certificate data.
    ///
    /// Returns:
    /// - `CertificateStatus::NotFound` if no certificate exists for `certificate_id`
    /// - `CertificateStatus::Revoked` if the certificate has been permanently revoked
    ///   (either directly or after its `revocation_deadline` challenge period elapsed)
    /// - `CertificateStatus::Expired` if the certificate has reached or passed its
    ///   `expires_at_ledger` sequence
    /// - `CertificateStatus::Valid` otherwise (including while a revocation is still
    ///   pending within its challenge period, provided the certificate has not expired)
    pub fn get_certificate_status(env: Env, certificate_id: String) -> CertificateStatus {
        let Some(cert) = env
            .storage()
            .persistent()
            .get::<DataKey, Certificate>(&DataKey::Certificate(certificate_id))
        else {
            return CertificateStatus::NotFound;
        };

        if cert.revoked {
            match cert.revocation_deadline {
                // Revocation is permanent once the challenge period has elapsed,
                // or immediately if no deadline was recorded (legacy/direct revocation).
                Some(deadline) if env.ledger().sequence() < deadline => {}
                _ => return CertificateStatus::Revoked,
            }
        }
        if let Some(expiry) = cert.expires_at_ledger {
            if env.ledger().sequence() >= expiry {
                return CertificateStatus::Expired;
            }
        }
        CertificateStatus::Valid
    }

    /// Verify a certificate — returns true if it exists, has not been
    /// permanently revoked, and has not expired.
    ///
    /// A certificate with a pending revocation (revoked == true but
    /// revocation_deadline in the future) is still considered valid until
    /// the challenge period expires.
    pub fn verify_certificate(env: Env, certificate_id: String) -> bool {
        Self::get_certificate_status(env, certificate_id) == CertificateStatus::Valid
    }

    /// Verify both the validity and the ownership of a certificate without
    /// returning any private certificate data (#420).
    ///
    /// A third party can use this to confirm that a presented certificate is
    /// valid *and* belongs to the student presenting it, without the auth that
    /// `get_certificate()` requires. Returns `false` for unknown, revoked,
    /// expired or mismatched certificates.
    pub fn verify_certificate_for(
        env: Env,
        certificate_id: String,
        student: Address,
    ) -> bool {
        let key = DataKey::Certificate(certificate_id.clone());
        let Some(cert) = env
            .storage()
            .persistent()
            .get::<DataKey, Certificate>(&key)
        else {
            return false;
        };
        cert.student == student
            && Self::get_certificate_status(env, certificate_id) == CertificateStatus::Valid
    }

    /// Permissionlessly extend the persistent storage TTL of an existing
    /// certificate entry.
    ///
    /// Anyone (student, issuer, verifier, or keeper) can call this function
    /// to keep a certificate live in persistent state and prevent archival
    /// after the initial ~6.3M-ledger (~1 year) TTL window.
    ///
    /// Panics with `Error::CertificateError` if the certificate does not exist
    /// in live persistent storage.
    pub fn extend_certificate_ttl(env: Env, certificate_id: String) {
        let key = DataKey::Certificate(certificate_id);
        if !env.storage().persistent().has(&key) {
            soroban_sdk::panic_with_error!(&env, Error::CertificateError);
        }
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);
        env.storage().persistent().extend_ttl(
            &key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );
    }

    /// Return a page of registered course IDs from the on-chain catalog.
    ///
    /// `limit` is silently clamped to `MAX_PAGE_SIZE` so every call has a
    /// predictable, safe upper bound on work performed. Clients should page
    /// through larger catalogs with successive `offset` values.
    ///
    /// # Arguments
    /// - `offset` — zero-based index of the first course to return
    /// - `limit`  — requested page size; values above `MAX_PAGE_SIZE` are clamped
    ///
    /// Returns an empty list when `offset` is beyond the end of the catalog.
    pub fn list_courses(env: Env, offset: u32, limit: u32) -> Vec<String> {
        let key = DataKey::CourseList;
        Self::extend_persistent_ttl_if_exists(&env, &key);
        let catalog: Vec<String> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(&env));

        let total = catalog.len();
        let start = offset.min(total);
        let limit = limit.min(Self::MAX_PAGE_SIZE);
        // Saturating add: an oversized caller `limit` (e.g. u32::MAX) must
        // clamp to the catalog end rather than trap on arithmetic overflow.
        let end = start.saturating_add(limit).min(total);

        let mut page = Vec::new(&env);
        for i in start..end {
            page.push_back(catalog.get(i).unwrap());
        }
        page
    }

    /// Return a page of course IDs whose current status equals `status`.
    ///
    /// Lets clients page through only enrollable (`Active`) courses without
    /// fetching every course record. `offset` is the zero-based index into the
    /// *filtered* result set (not the full catalog), and `limit` is clamped to
    /// `MAX_PAGE_SIZE`, so the returned page is always bounded. Scanning stops
    /// as soon as the page is full.
    ///
    /// Returns an empty list when `offset` is beyond the last matching course.
    pub fn list_courses_by_status(
        env: Env,
        status: CourseStatus,
        offset: u32,
        limit: u32,
    ) -> Vec<String> {
        let key = DataKey::CourseList;
        Self::extend_persistent_ttl_if_exists(&env, &key);
        let catalog: Vec<String> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(&env));

        let limit = limit.min(Self::MAX_PAGE_SIZE);
        let mut page = Vec::new(&env);
        if limit == 0 {
            return page;
        }

        let mut matched: u32 = 0;
        for course_id in catalog.iter() {
            let course: Option<Course> = env
                .storage()
                .persistent()
                .get(&DataKey::Course(course_id.clone()));
            if let Some(course) = course {
                if course.status == status {
                    if matched >= offset {
                        page.push_back(course_id);
                        if page.len() >= limit {
                            break;
                        }
                    }
                    matched += 1;
                }
            }
        }
        page
    }

    /// Get the current platform fee percentage (Admin only)
    pub fn get_platform_fee(env: Env, admin: Address) -> u32 {
        admin.require_auth();
        Self::require_admin(&env, &admin, "get_platform_fee");
        env.storage()
            .instance()
            .get::<DataKey, u32>(&DataKey::DefaultFee)
            .unwrap_or(20)
    }

    // ----------------------------------------------------------
    // INTERNAL HELPERS
    // ----------------------------------------------------------

    /// Resolve the effective treasury address, promoting a matured
    /// `PendingTreasury` update if its `effective_ledger` has been reached.
    ///
    /// When a pending update matures it is committed to `DataKey::Treasury`
    /// and `DataKey::PendingTreasury` is cleared so subsequent calls in the
    /// same transaction (and all future calls) see the new address without
    /// re-reading the pending record.
    fn effective_treasury(env: &Env) -> Address {
        let mut treasury: Address = env
            .storage()
            .instance()
            .get(&DataKey::Treasury)
            .unwrap_or_else(|| panic!("treasury not set"));

        if let Some(pending) = env
            .storage()
            .instance()
            .get::<DataKey, TreasuryUpdate>(&DataKey::PendingTreasury)
        {
            if env.ledger().sequence() >= pending.effective_ledger {
                treasury = pending.address.clone();
                env.storage().instance().set(&DataKey::Treasury, &treasury);
                env.storage().instance().remove(&DataKey::PendingTreasury);
            }
        }

        treasury
    }

    /// Emit the shared per-student refund event used by both
    /// `process_refund()` and `archive_course()`.
    fn emit_refund_issued(
        env: &Env,
        course_id: &String,
        student: &Address,
        amount: i128,
        platform_amount: i128,
        instructor_amount: i128,
    ) {
        env.events().publish(
            (Symbol::new(env, "refund_issued"), course_id.clone()),
            (
                student.clone(),
                course_id.clone(),
                amount,
                platform_amount,
                instructor_amount,
                env.ledger().sequence(),
            ),
        );
    }
    /// Persist a value and extend its TTL in one step.
    ///
    /// Every persistent write must go through this helper so a key can never
    /// be written without also renewing its TTL.
    fn persist<K, V>(env: &Env, key: &K, value: &V)
    where
        K: IntoVal<Env, Val>,
        V: IntoVal<Env, Val>,
    {
        env.storage().persistent().set(key, value);
        env.storage().persistent().extend_ttl(
            key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );
    }

    /// Extend the persistent storage TTL of `key` if it exists.
    fn extend_persistent_ttl_if_exists<K>(env: &Env, key: &K)
    where
        K: IntoVal<Env, Val>,
    {
        if env.storage().persistent().has(key) {
            env.storage().persistent().extend_ttl(
                key,
                Self::PERSISTENT_TTL_THRESHOLD,
                Self::PERSISTENT_TTL_EXTEND_TO,
            );
        }
    }

    /// Persist a `CourseTerms` snapshot for `course.version` (#371).
    fn save_course_terms(env: &Env, course: &Course) {
        let terms = CourseTerms {
            price: course.price,
            platform_fee_percent: course.platform_fee_percent,
            content_hash: course.content_hash.clone(),
            max_capacity: course.max_capacity,
            capacity_mode: course.capacity_mode.clone(),
        };
        Self::persist(
            env,
            &DataKey2::CourseTerms(course.id.clone(), course.version),
            &terms,
        );
    }

    fn persist_course(env: &Env, course_id: &String, course: &Course) {
        Self::persist(env, &DataKey::Course(course_id.clone()), course);
    }

    fn read_course_stats(env: &Env, course_id: &String, course: &Course) -> CourseStats {
        env.storage()
            .persistent()
            .get::<DataKey2, CourseStats>(&DataKey2::CourseStats(course_id.clone()))
            .unwrap_or(CourseStats {
                total_enrollments: course.total_enrollments,
                active_enrollments: course.active_enrollments,
                total_earned: course.total_earned,
            })
    }

    fn write_course_stats(env: &Env, course_id: &String, stats: &CourseStats) {
        Self::persist(env, &DataKey2::CourseStats(course_id.clone()), stats);
    }

    fn merge_course_stats(env: &Env, course_id: &String, mut course: Course) -> Course {
        if let Some(stats) = env
            .storage()
            .persistent()
            .get::<DataKey2, CourseStats>(&DataKey2::CourseStats(course_id.clone()))
        {
            course.total_enrollments = stats.total_enrollments;
            course.active_enrollments = stats.active_enrollments;
            course.total_earned = stats.total_earned;
        }
        course
    }

    fn get_course_internal(env: &Env, course_id: &String) -> Option<Course> {
        if let Some(course) = env
            .storage()
            .persistent()
            .get::<DataKey, Course>(&DataKey::Course(course_id.clone()))
        {
            return Some(Self::merge_course_stats(env, course_id, course));
        }

        // If not found in active courses, check archived courses
        env.storage()
            .persistent()
            .get::<DataKey, Course>(&DataKey::ArchivedCourse(course_id.clone()))
            .map(|course| Self::merge_course_stats(env, course_id, course))
    }

    fn get_enrollment_internal(env: &Env, student: &Address, course_id: &String) -> Enrollment {
        env.storage()
            .persistent()
            .get(&DataKey::Enrollment(student.clone(), course_id.clone()))
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::EnrollmentNotFound))
    }

    fn is_enrollment_expired(env: &Env, _course: &Course, enrollment: &Enrollment) -> bool {
        match enrollment.enrollment_expiry_ledgers {
            Some(expiry_ledgers) => {
                let expiry_at = enrollment
                    .enrolled_at_ledger
                    .checked_add(expiry_ledgers)
                    .unwrap_or(u32::MAX);
                env.ledger().sequence() >= expiry_at
            }
            None => false,
        }
    }

    fn is_enrollment_expired_internal(
        env: &Env,
        _course: &Course,
        enrollment: &Enrollment,
    ) -> bool {
        if let Some(expiry_ledgers) = enrollment.enrollment_expiry_ledgers {
            let expiry_at = enrollment
                .enrolled_at_ledger
                .checked_add(expiry_ledgers)
                .unwrap_or(u32::MAX);
            return env.ledger().sequence() >= expiry_at;
        }
        false
    }

    fn validate_moderation_reason(reason: &String) {
        if reason.is_empty() {
            panic!("moderation reason cannot be empty");
        }
        if reason.len() > Self::MAX_MODERATION_REASON_LEN {
            panic!("moderation reason exceeds maximum length");
        }
    }

    fn is_instructor_frozen_internal(env: &Env, instructor: &Address) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::InstructorBlocked(instructor.clone()))
            .unwrap_or(false)
    }

    /// Returns `true` if the lead instructor or any configured co-instructor
    /// on `course` is currently frozen. Enrollment and re-enrollment reject
    /// courses when any revenue recipient on the instructor side is frozen so
    /// frozen co-instructors cannot accrue new earnings. (#343)
    fn is_any_course_instructor_frozen(env: &Env, course: &Course) -> bool {
        if Self::is_instructor_frozen_internal(env, &course.instructor) {
            return true;
        }
        for co_instructor in course.co_instructors.iter() {
            if Self::is_instructor_frozen_internal(env, &co_instructor.address) {
                return true;
            }
        }
        false
    }

    fn is_student_blocked_internal(env: &Env, student: &Address) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::StudentBlocked(student.clone()))
            .unwrap_or(false)
    }

    fn course_fee_override_bps(env: &Env, course_id: &String) -> Option<u32> {
        // `platform_fee_percent == 0` means "follow the live token rate";
        // any other value is an explicit override captured at registration.
        Self::get_course_internal(env, course_id).and_then(|course| {
            if course.platform_fee_percent == 0 {
                None
            } else {
                Some(course.platform_fee_percent.saturating_mul(100))
            }
        })
    }

    fn get_student_enrollment_count_internal(env: &Env, student: &Address) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey2::StudentEnrollmentCount(student.clone()))
            .unwrap_or(0)
    }

    /// Increment the platform-wide active-enrollment counter.
    fn inc_total_active(env: &Env) {
        let total_active: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TotalActiveEnrollments)
            .unwrap_or(0);
        env.storage().instance().set(
            &DataKey::TotalActiveEnrollments,
            &(total_active.checked_add(1).unwrap_or_else(|| {
                soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow)
            })),
        );
    }

    /// Decrement the platform-wide active-enrollment counter, saturating at zero.
    fn dec_total_active(env: &Env) {
        let total_active: u32 = env
            .storage()
            .instance()
            .get(&DataKey::TotalActiveEnrollments)
            .unwrap_or(0);
        if total_active > 0 {
            env.storage()
                .instance()
                .set(&DataKey::TotalActiveEnrollments, &(total_active - 1));
        }
    }

    fn increment_student_enrollment_count(env: &Env, student: &Address) {
        let key = DataKey2::StudentEnrollmentCount(student.clone());
        let count = Self::get_student_enrollment_count_internal(env, student)
            .checked_add(1)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow));
        Self::persist(env, &key, &count);
    }

    fn is_btc_eth_token_internal(env: &Env, token: &Address) -> bool {
        env.storage()
            .instance()
            .get(&DataKey2::BtcEthToken(token.clone()))
            .unwrap_or(false)
    }

    /// Return the current effective treasury address.
    ///
    /// If a `PendingTreasury` update exists and its `effective_ledger` has
    /// been reached, the update is applied atomically (promoting the pending
    /// address to `DataKey::Treasury` and clearing `DataKey::PendingTreasury`)
    /// and the new address is returned.  Otherwise the already-stored
    /// `DataKey::Treasury` address is returned unchanged.
    ///
    /// Every code path that needs the treasury address must call this helper
    /// instead of reading `DataKey::Treasury` directly, so the promotion is
    /// never missed and there is exactly one place that owns the transition
    /// logic.
    fn read_treasury(env: &Env) -> Address {
        let current: Address = env
            .storage()
            .instance()
            .get(&DataKey::Treasury)
            .unwrap_or_else(|| panic!("treasury not set"));

        if let Some(pending) = env
            .storage()
            .instance()
            .get::<DataKey, TreasuryUpdate>(&DataKey::PendingTreasury)
        {
            if env.ledger().sequence() >= pending.effective_ledger {
                let new_treasury = pending.address.clone();
                env.storage()
                    .instance()
                    .set(&DataKey::Treasury, &new_treasury);
                env.storage().instance().remove(&DataKey::PendingTreasury);

                env.events().publish(
                    (Symbol::new(env, "treasury_activated"), new_treasury.clone()),
                    (new_treasury.clone(), env.ledger().sequence()),
                );

                return new_treasury;
            }
        }

        current
    }

    fn is_admin(env: &Env, caller: &Address) -> bool {
        let admin: Option<Address> = env.storage().instance().get(&DataKey::Admin);
        admin.map(|a| a == *caller).unwrap_or(false)
    }

    fn is_moderator(env: &Env, caller: &Address) -> bool {
        let moderator: Option<Address> = env.storage().instance().get(&DataKey::Moderator);
        moderator.map(|m| m == *caller).unwrap_or(false)
    }

    fn require_moderator_or_admin(env: &Env, caller: &Address, _operation: &str) {
        let is_admin = Self::is_admin(env, caller);
        let is_moderator = Self::is_moderator(env, caller);
        if !is_admin && !is_moderator {
            soroban_sdk::panic_with_error!(env, Error::Unauthorized);
        }

        if is_moderator {
            return;
        }

        if let Some(expires_at) = env
            .storage()
            .instance()
            .get::<DataKey, u32>(&DataKey::AdminExpiresAt)
        {
            if env.ledger().sequence() >= expires_at {
                soroban_sdk::panic_with_error!(env, Error::AdminExpired);
            }
        }
    }

    /// Check if an address is an approved approver
    fn is_approver(env: &Env, caller: &Address) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Approver(caller.clone()))
            .unwrap_or(false)
    }

    /// Check if caller is either admin or approver
    fn is_admin_or_approver(env: &Env, caller: &Address) -> bool {
        Self::is_admin(env, caller) || Self::is_approver(env, caller)
    }

    fn require_admin(env: &Env, caller: &Address, _operation: &str) {
        if !Self::is_admin(env, caller) {
            soroban_sdk::panic_with_error!(&env, Error::Unauthorized);
        }

        // Check if admin role has expired
        if let Some(expires_at) = env
            .storage()
            .instance()
            .get::<DataKey, u32>(&DataKey::AdminExpiresAt)
        {
            if env.ledger().sequence() >= expires_at {
                soroban_sdk::panic_with_error!(&env, Error::AdminExpired);
            }
        }
    }

    /// Require that caller is either admin or an approved approver
    fn require_admin_or_approver(env: &Env, caller: &Address, _operation: &str) {
        if !Self::is_admin_or_approver(env, caller) {
            soroban_sdk::panic_with_error!(&env, Error::Unauthorized);
        }

        // Check if admin role has expired (approvers don't expire)
        if Self::is_admin(env, caller) {
            if let Some(expires_at) = env
                .storage()
                .instance()
                .get::<DataKey, u32>(&DataKey::AdminExpiresAt)
            {
                if env.ledger().sequence() >= expires_at {
                    soroban_sdk::panic_with_error!(&env, Error::AdminExpired);
                }
            }
        }
    }

    /// Require that caller is the course instructor or the platform admin.
    /// Panics with "unauthorized" otherwise. Also enforces the admin-expiry
    /// rule when the caller is an admin.
    fn require_admin_or_instructor(env: &Env, caller: &Address, course: &Course) {
        let is_admin = Self::is_admin(env, caller);
        let is_instructor = caller == &course.instructor;

        if !is_admin && !is_instructor {
            soroban_sdk::panic_with_error!(env, Error::Unauthorized);
        }

        // Check if admin role has expired
        if is_admin {
            if let Some(expires_at) = env
                .storage()
                .instance()
                .get::<DataKey, u32>(&DataKey::AdminExpiresAt)
            {
                if env.ledger().sequence() >= expires_at {
                    soroban_sdk::panic_with_error!(env, Error::AdminExpired);
                }
            }
        }
    }

    fn require_multi_admin(env: &Env, caller1: &Address, caller2: &Address) {
        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        let secondary_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::SecondaryAdmin)
            .unwrap();

        if caller1 == caller2 {
            soroban_sdk::panic_with_error!(&env, Error::Unauthorized);
        }

        if (*caller1 == admin && *caller2 == secondary_admin)
            || (*caller1 == secondary_admin && *caller2 == admin)
        {
            // ok
        } else {
            soroban_sdk::panic_with_error!(&env, Error::Unauthorized);
        }

        // Check if admin role has expired
        if let Some(expires_at) = env
            .storage()
            .instance()
            .get::<DataKey, u32>(&DataKey::AdminExpiresAt)
        {
            if env.ledger().sequence() >= expires_at {
                soroban_sdk::panic_with_error!(&env, Error::AdminExpired);
            }
        }
    }

    /// Convert a u32 counter to a Soroban String (decimal representation).
    /// Used for generating unique enrollment references.
    fn counter_to_string(env: &Env, n: u32) -> String {
        if n == 0 {
            return String::from_str(env, "0");
        }
        let mut digits = [0u8; 10]; // u32 has at most 10 decimal digits
        let mut len = 0;
        let mut val = n;
        while val > 0 {
            digits[len] = b'0' + (val % 10) as u8;
            val /= 10;
            len += 1;
        }
        // Reverse in place
        let mut i = 0;
        while i < len / 2 {
            let tmp = digits[i];
            digits[i] = digits[len - 1 - i];
            digits[len - 1 - i] = tmp;
            i += 1;
        }
        String::from_bytes(env, &digits[..len])
    }

    /// Generate a unique enrollment reference.
    fn generate_enrollment_ref(env: &Env) -> String {
        let key = DataKey::EnrollmentRefCounter;
        let counter: u32 = env
            .storage()
            .persistent()
            .get(&key)
            .or_else(|| env.storage().instance().get(&key))
            .unwrap_or(1);
        env.storage().persistent().set(
            &key,
            &counter
                .checked_add(1)
                .expect("enrollment reference counter overflow"),
        );
        env.storage().persistent().extend_ttl(
            &key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        Self::counter_to_string(env, counter)
    }

    /// Split `total_amount` across the course's co-instructors according to
    /// their configured basis-point shares, then credit the remainder to the
    /// lead instructor. Returns the amount credited to the lead instructor.
    fn credit_instructor_shares(env: &Env, course: &Course, total_amount: i128) -> i128 {
        if total_amount <= 0 {
            return 0;
        }

        let mut distributed: i128 = 0;
        for co_instructor in course.co_instructors.iter() {
            if co_instructor.share_bps == 0 {
                continue;
            }

            let share_amount = total_amount
                .checked_mul(co_instructor.share_bps as i128)
                .unwrap_or_else(|| panic!("overflow computing co-instructor share"))
                / Self::MAX_SHARE_BPS as i128;

            if share_amount > 0 {
                Self::credit_instructor_earnings(
                    env,
                    &co_instructor.address,
                    &course.token,
                    share_amount,
                );
                distributed = distributed
                    .checked_add(share_amount)
                    .unwrap_or_else(|| panic!("co-instructor share total overflow"));
            }
        }

        let lead_share = total_amount
            .checked_sub(distributed)
            .unwrap_or_else(|| panic!("co-instructor split underflow"));

        if lead_share > 0 {
            Self::credit_instructor_earnings(env, &course.instructor, &course.token, lead_share);
        }

        lead_share
    }

    /// Read-only mirror of the split performed by `credit_instructor_shares`.
    /// Returns every instructor payout for `total_amount` (lead instructor
    /// first, then co-instructors) so `student_enrolled` can carry the full
    /// breakdown as the single canonical enrollment event (#366).
    fn payout_breakdown(env: &Env, course: &Course, total_amount: i128) -> Vec<(Address, i128)> {
        let mut payouts: Vec<(Address, i128)> = Vec::new(env);
        if total_amount <= 0 {
            return payouts;
        }

        let mut distributed: i128 = 0;
        for co_instructor in course.co_instructors.iter() {
            if co_instructor.share_bps == 0 {
                continue;
            }
            let share_amount = total_amount
                .checked_mul(co_instructor.share_bps as i128)
                .unwrap_or_else(|| panic!("overflow computing co-instructor share"))
                / Self::MAX_SHARE_BPS as i128;
            if share_amount > 0 {
                payouts.push_back((co_instructor.address.clone(), share_amount));
                distributed = distributed
                    .checked_add(share_amount)
                    .unwrap_or_else(|| panic!("co-instructor share total overflow"));
            }
        }

        let lead_share = total_amount
            .checked_sub(distributed)
            .unwrap_or_else(|| panic!("co-instructor split underflow"));
        if lead_share > 0 {
            payouts.push_front((course.instructor.clone(), lead_share));
        }
        payouts
    }
    /// Debit `total_amount` from the course's instructor split, taking each
    /// co-instructor's pro-rata share first and the remainder from the lead
    /// instructor. Used by refund paths so every recipient that was credited
    /// at enrollment is debited by the same amount.
    fn debit_instructor_shares(env: &Env, course: &Course, total_amount: i128) {
        if total_amount <= 0 {
            return;
        }

        let mut debited: i128 = 0;
        for co_instructor in course.co_instructors.iter() {
            if co_instructor.share_bps == 0 {
                continue;
            }

            let share_amount = total_amount
                .checked_mul(co_instructor.share_bps as i128)
                .unwrap_or_else(|| panic!("overflow computing co-instructor refund share"))
                / Self::MAX_SHARE_BPS as i128;

            if share_amount > 0 {
                Self::debit_instructor_earnings(
                    env,
                    &co_instructor.address,
                    &course.token,
                    share_amount,
                );
                debited = debited
                    .checked_add(share_amount)
                    .unwrap_or_else(|| panic!("co-instructor refund total overflow"));
            }
        }

        let lead_share = total_amount
            .checked_sub(debited)
            .unwrap_or_else(|| panic!("co-instructor refund underflow"));

        if lead_share > 0 {
            Self::debit_instructor_earnings(env, &course.instructor, &course.token, lead_share);
        }
    }

    fn credit_instructor_earnings(env: &Env, instructor: &Address, token: &Address, amount: i128) {
        let key = DataKey::InstructorEarnings(instructor.clone(), token.clone());
        let current: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        let new_balance = current
            .checked_add(amount)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow));
        Self::persist(env, &key, &new_balance);
        if new_balance > 0 {
            Self::add_instructor_earning_token(env, instructor, token);
        }
        // Update the token-scoped lifetime net earnings accumulator. The
        // counter is per-token so amounts denominated in different assets are
        // never added together (#645).
        let lifetime_key = DataKey::InstructorLifetimeEarnings(instructor.clone(), token.clone());
        let current_lifetime: i128 = env.storage().persistent().get(&lifetime_key).unwrap_or(0);
        let new_lifetime = current_lifetime
            .checked_add(amount)
            .unwrap_or_else(|| panic!("overflow computing instructor lifetime earnings"));
        env.storage().persistent().set(&lifetime_key, &new_lifetime);
        env.storage().persistent().extend_ttl(
            &lifetime_key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        // Track total outstanding instructor earnings for this token across all instructors
        let outstanding_key = DataKey2::TotalInstructorEarnings(token.clone());
        let current_outstanding: i128 = env.storage().instance().get(&outstanding_key).unwrap_or(0);
        let new_outstanding = current_outstanding
            .checked_add(amount)
            .unwrap_or_else(|| panic!("overflow computing total outstanding instructor earnings"));
        env.storage()
            .instance()
            .set(&outstanding_key, &new_outstanding);
    }

    /// Read an instructor's total course count.
    ///
    /// Reads the persistent counter, falling back to the legacy instance
    /// entry for instructors whose counters were written before the
    /// storage-layout migration. See `migrate_instructor_counters`.
    fn read_instructor_course_count(env: &Env, instructor: &Address) -> u32 {
        let key = DataKey2::InstructorCourseCountPersistent(instructor.clone());
        if let Some(count) = env.storage().persistent().get::<DataKey2, u32>(&key) {
            return count;
        }
        env.storage()
            .instance()
            .get::<DataKey2, u32>(&DataKey2::InstructorCourseCountLegacy(instructor.clone()))
            .unwrap_or(0)
    }

    /// Read an instructor's pending course count.
    ///
    /// Reads the persistent counter, falling back to the legacy instance
    /// entry for instructors whose counters were written before the
    /// storage-layout migration. See `migrate_instructor_counters`.
    fn read_instructor_pending_course_count(env: &Env, instructor: &Address) -> u32 {
        let key = DataKey2::InstructorPendingCountPersistent(instructor.clone());
        if let Some(count) = env.storage().persistent().get::<DataKey2, u32>(&key) {
            return count;
        }
        env.storage()
            .instance()
            .get::<DataKey2, u32>(&DataKey2::InstructorPendingCountLegacy(instructor.clone()))
            .unwrap_or(0)
    }

    /// Migration helper: move a batch of instructors' course counters from
    /// instance storage into persistent storage.
    ///
    /// Must be called by the admin after upgrading to the version that
    /// introduced `InstructorCourseCountPersistent` /
    /// `InstructorPendingCountPersistent`. For each instructor in
    /// `instructors`, the legacy instance entries are copied to persistent
    /// storage (if not already present) and then removed from instance
    /// storage, so the instance entry stops growing with the instructor
    /// count. Callers should pass every address from `get_instructor_registry`
    /// (in batches) until the migration is complete.
    ///
    /// Idempotent: re-running over an already-migrated instructor is a no-op.
    pub fn migrate_instructor_counters(env: Env, admin: Address, instructors: Vec<Address>) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "migrate_instructor_counters");

        for instructor in instructors.iter() {
            let course_key = DataKey2::InstructorCourseCountPersistent(instructor.clone());
            if !env.storage().persistent().has(&course_key) {
                let legacy_key = DataKey2::InstructorCourseCountLegacy(instructor.clone());
                if let Some(count) = env.storage().instance().get::<DataKey2, u32>(&legacy_key) {
                    env.storage().persistent().set(&course_key, &count);
                    env.storage().persistent().extend_ttl(
                        &course_key,
                        Self::PERSISTENT_TTL_THRESHOLD,
                        Self::PERSISTENT_TTL_EXTEND_TO,
                    );
                }
            }
            env.storage()
                .instance()
                .remove(&DataKey2::InstructorCourseCountLegacy(instructor.clone()));

            let pending_key = DataKey2::InstructorPendingCountPersistent(instructor.clone());
            if !env.storage().persistent().has(&pending_key) {
                let legacy_key = DataKey2::InstructorPendingCountLegacy(instructor.clone());
                if let Some(count) = env.storage().instance().get::<DataKey2, u32>(&legacy_key) {
                    env.storage().persistent().set(&pending_key, &count);
                    env.storage().persistent().extend_ttl(
                        &pending_key,
                        Self::PERSISTENT_TTL_THRESHOLD,
                        Self::PERSISTENT_TTL_EXTEND_TO,
                    );
                }
            }
            env.storage()
                .instance()
                .remove(&DataKey2::InstructorPendingCountLegacy(instructor.clone()));
        }
    }

    fn debit_instructor_earnings(env: &Env, instructor: &Address, token: &Address, amount: i128) {
        let key = DataKey::InstructorEarnings(instructor.clone(), token.clone());
        let current: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        if amount > current {
            soroban_sdk::panic_with_error!(&env, Error::InvalidWithdrawal);
        }
        let new_balance = current - amount;
        if new_balance == 0 {
            env.storage().persistent().remove(&key);
            Self::remove_instructor_earning_token(env, instructor, token);
        } else {
            Self::persist(&env, &key, &new_balance);
        }
        // Refunds reduce the token-scoped lifetime net earnings accumulator.
        // Withdrawals never touch this counter (#645).
        let lifetime_key = DataKey::InstructorLifetimeEarnings(instructor.clone(), token.clone());
        let current_lifetime: i128 = env.storage().persistent().get(&lifetime_key).unwrap_or(0);
        let new_lifetime = current_lifetime.saturating_sub(amount);
        if new_lifetime == 0 {
            env.storage().persistent().remove(&lifetime_key);
        } else {
            env.storage().persistent().set(&lifetime_key, &new_lifetime);
        }

        // Decrement the token-scoped lifetime net earnings accumulator. Use a
        // saturating subtraction so refunds for enrollments created before this
        // key existed cannot be blocked by an unmigrated (zero) counter.
        let lifetime_key = DataKey::InstructorLifetimeEarnings(instructor.clone(), token.clone());
        let current_lifetime: i128 = env.storage().persistent().get(&lifetime_key).unwrap_or(0);
        let new_lifetime = current_lifetime.saturating_sub(amount);
        if new_lifetime == 0 {
            env.storage().persistent().remove(&lifetime_key);
        } else {
            Self::persist(&env, &lifetime_key, &new_lifetime);
        }

        // Decrement total outstanding instructor earnings for this token
        let outstanding_key = DataKey2::TotalInstructorEarnings(token.clone());
        let current_outstanding: i128 = env.storage().instance().get(&outstanding_key).unwrap_or(0);
        let new_outstanding = current_outstanding.saturating_sub(amount);
        if new_outstanding == 0 {
            env.storage().instance().remove(&outstanding_key);
        } else {
            env.storage()
                .instance()
                .set(&outstanding_key, &new_outstanding);
        }
    }

    /// Ensure `token` is present in `instructor`'s non-zero earning tokens set.
    fn add_instructor_earning_token(env: &Env, instructor: &Address, token: &Address) {
        let key = DataKey2::InstructorEarningTokens(instructor.clone());
        let mut tokens: Vec<Address> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(env));
        if !tokens.contains(token) {
            tokens.push_back(token.clone());
            Self::persist(env, &key, &tokens);
        }
    }

    /// Remove `token` from `instructor`'s non-zero earning tokens set.
    fn remove_instructor_earning_token(env: &Env, instructor: &Address, token: &Address) {
        let key = DataKey2::InstructorEarningTokens(instructor.clone());
        if let Some(tokens) = env
            .storage()
            .persistent()
            .get::<DataKey2, Vec<Address>>(&key)
        {
            let mut updated = Vec::new(env);
            for t in tokens.iter() {
                if t != *token {
                    updated.push_back(t);
                }
            }
            if updated.is_empty() {
                env.storage().persistent().remove(&key);
            } else {
                Self::persist(env, &key, &updated);
            }
        }
    }

    fn refund_enrollment(
        env: &Env,
        course: &mut Course,
        student: &Address,
        refund_amount: Option<i128>,
    ) -> RefundBreakdown {
        let enrollment_key = DataKey::Enrollment(student.clone(), course.id.clone());
        let mut enrollment: Enrollment = env
            .storage()
            .persistent()
            .get(&enrollment_key)
            .unwrap_or_else(|| panic!("enrollment not found"));

        // Use the split actually applied at enrollment time. For partial
        // refunds, preserve that ratio and assign rounding remainder to instructor.
        let actual_refund_amount = match refund_amount {
            Some(amount) => {
                if amount < 0 || amount > enrollment.amount_paid {
                    panic!("refund amount must be <= amount_paid");
                }
                amount
            }
            None => enrollment.amount_paid,
        };
        let platform_refund = if enrollment.amount_paid == 0 {
            0
        } else {
            actual_refund_amount
                .checked_mul(enrollment.platform_amount)
                .map(|value| value / enrollment.amount_paid)
                .unwrap_or_else(|| panic!("overflow computing platform refund"))
        };
        let instructor_refund = actual_refund_amount
            .checked_sub(platform_refund)
            .unwrap_or_else(|| panic!("refund split underflow"));

        let token_client = token::Client::new(env, &course.token);
        if platform_refund > 0 {
            Self::pay_platform_refund(env, &course.token, student, platform_refund);
        }
        if instructor_refund > 0 {
            Self::debit_instructor_shares(env, course, instructor_refund);
            token_client.transfer(&env.current_contract_address(), student, &instructor_refund);
        }

        enrollment.is_refunded = true;
        let history_key = DataKey::EnrollmentHistory(student.clone(), course.id.clone());
        let mut history: Vec<Enrollment> = env
            .storage()
            .persistent()
            .get(&history_key)
            .unwrap_or_else(|| Vec::new(env));
        history.push_back(enrollment.clone());
        env.storage().persistent().set(&history_key, &history);
        env.storage().persistent().extend_ttl(
            &history_key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );
        env.storage().persistent().remove(&enrollment_key);

        let mut stats = Self::read_course_stats(env, &course.id, course);
        if stats.active_enrollments > 0 {
            stats.active_enrollments -= 1;
            Self::write_course_stats(env, &course.id, &stats);
        }

        let total_active: u32 = Self::read_total_active_enrollments(env);
        if total_active > 0 {
            Self::write_total_active_enrollments(env, total_active - 1);
        }

        RefundBreakdown {
            refund_amount: actual_refund_amount,
            platform_refund,
            instructor_refund,
        }
    }

    /// Add to the per-token arbitration fee reserve.
    fn credit_arbitration_fees_held(env: &Env, token: &Address, amount: i128) {
        let key = DataKey2::ArbitrationFeesHeld(token.clone());
        let current: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        let new_balance = current
            .checked_add(amount)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ArithmeticOverflow));
        env.storage().persistent().set(&key, &new_balance);
        env.storage().persistent().extend_ttl(
            &key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );
    }

    /// Release from the per-token arbitration fee reserve.
    fn debit_arbitration_fees_held(env: &Env, token: &Address, amount: i128) {
        let key = DataKey2::ArbitrationFeesHeld(token.clone());
        let current: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        if amount > current {
            soroban_sdk::panic_with_error!(&env, Error::InconsistentState);
        }
        let new_balance = current - amount;
        if new_balance == 0 {
            env.storage().persistent().remove(&key);
        } else {
            env.storage().persistent().set(&key, &new_balance);
        }
    }

    /// Load an instructor's reputation stats, apply `f`, and persist the result.
    fn update_instructor_stats<F: FnOnce(&mut InstructorStats)>(
        env: &Env,
        instructor: &Address,
        f: F,
    ) {
        let key = DataKey::InstructorStats(instructor.clone());
        let mut stats: InstructorStats =
            env.storage()
                .persistent()
                .get(&key)
                .unwrap_or(InstructorStats {
                    total_students: 0,
                    total_completions: 0,
                    total_certificates: 0,
                });
        f(&mut stats);
        Self::persist(&env, &key, &stats);
    }

    // ============================================================
    // FEE & RISK MANAGEMENT
    // ============================================================

    /// Admin sets or updates the per-token fee configuration.
    /// `fee_bps` (0-10000): e.g. 2000 = 20%, 500 = 5%, 0 = free.
    pub fn set_fee_config(env: Env, admin: Address, token: Address, fee_bps: u32) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "set_fee_config");

        if fee_bps > 10000 {
            soroban_sdk::panic_with_error!(&env, Error::InvalidFeeConfig);
        }

        let old_fee_bps: Option<u32> = env
            .storage()
            .instance()
            .get::<DataKey, FeeConfig>(&DataKey::FeeConfig(token.clone()))
            .map(|c| c.fee_bps);
        // Reject tokens that are not on the approved list — a per-token fee
        // override for an unapproved token would be dead configuration that
        // is easy to mistake for a live one.
        if !env
            .storage()
            .persistent()
            .has(&DataKey::ApprovedToken(token.clone()))
        {
            panic!("token is not approved");
        }

        let config = FeeConfig { fee_bps };
        env.storage()
            .instance()
            .set(&DataKey::FeeConfig(token.clone()), &config);

        env.events().publish(
            (Symbol::new(&env, "fee_config_updated"), admin.clone()),
            (token, old_fee_bps, fee_bps),
        );
    }

    /// Admin removes the per-token fee configuration override so the token
    /// goes back to following the platform default fee (`DefaultFee`).
    /// Removing a non-existent override is a no-op.
    pub fn remove_fee_config(env: Env, admin: Address, token: Address) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "remove_fee_config");

        env.storage()
            .instance()
            .remove(&DataKey::FeeConfig(token.clone()));

        env.events().publish(
            (Symbol::new(&env, "fee_config_removed"), admin.clone()),
            token,
        );
    }

    /// Get the per-token fee configuration. Returns the platform default
    /// (converted to bps) if no per-token override has been set.
    pub fn get_fee_config(env: Env, token: Address) -> FeeConfig {
        // Prefer a per-token override; fall back to the platform default
        // fee percentage (stored as `DefaultFee`) converted to basis points.
        if let Some(cfg) = env
            .storage()
            .instance()
            .get::<DataKey, FeeConfig>(&DataKey::FeeConfig(token))
        {
            cfg
        } else {
            let default_pct: u32 = env
                .storage()
                .instance()
                .get(&DataKey::DefaultFee)
                .unwrap_or(20u32);
            let fee_bps = match default_pct.checked_mul(100) {
                Some(value) => value,
                None => soroban_sdk::panic_with_error!(&env, Error::InvalidFeeConfig),
            };
            FeeConfig { fee_bps }
        }
    }

    /// Admin sets the arbitration fee configuration for a token — the minimum
    /// fee (in the token's stroops) required to escalate a dispute to
    /// arbitration. Fees are configured per token so tokens with different
    /// decimals and values can each have an appropriate fee.
    pub fn set_arbitration_fee_config(
        env: Env,
        admin: Address,
        token: Address,
        fee_per_case: i128,
    ) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "set_arbitration_fee_config");

        if fee_per_case < 0 {
            soroban_sdk::panic_with_error!(&env, Error::InvalidFeeConfig);
        }

        let config = ArbitrationFeeConfig { fee_per_case };
        env.storage()
            .instance()
            .set(&DataKey2::ArbitrationFeeConfig(token.clone()), &config);

        env.events().publish(
            (
                Symbol::new(&env, "arbitration_fee_config_updated"),
                admin.clone(),
            ),
            (token, fee_per_case),
        );
    }

    /// Get the arbitration fee configuration for a token.
    pub fn get_arbitration_fee_config(env: Env, token: Address) -> ArbitrationFeeConfig {
        env.storage()
            .instance()
            .get(&DataKey2::ArbitrationFeeConfig(token))
            .unwrap_or(ArbitrationFeeConfig { fee_per_case: 0 })
    }

    /// Get the total arbitration fees currently held by the contract for a token.
    /// These funds are reserved for pending arbitration cases and are not
    /// available for instructor withdrawals or emergency sweeps.
    pub fn get_arbitration_fees_held(env: Env, token: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey2::ArbitrationFeesHeld(token))
            .unwrap_or(0)
    }

    /// Admin enables or disables risk-based fee surcharge pricing.
    pub fn set_risk_config_enabled(env: Env, admin: Address, enabled: bool) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "set_risk_config_enabled");
        env.storage()
            .instance()
            .set(&DataKey::RiskConfigEnabled, &enabled);

        env.events().publish(
            (Symbol::new(&env, "risk_config_toggled"), admin.clone()),
            enabled,
        );
    }

    /// Check if risk-based fee pricing is enabled.
    pub fn is_risk_config_enabled(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::RiskConfigEnabled)
            .unwrap_or(false)
    }

    /// Returns `true` if the platform is currently paused, `false` otherwise.
    ///
    /// `PlatformPaused` is set at `init()` and toggled by `pause_platform()` /
    /// `unpause_platform()`. This view lets callers discover the paused state
    /// without attempting an enrollment or reading raw storage.
    pub fn is_platform_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::PlatformPaused)
            .unwrap_or(false)
    }

    /// Admin sets the risk fee configuration — parameters that determine
    /// surcharges based on payment size, customer history, and currency.
    ///
    /// # Panics
    /// - If `large_payment_threshold` is negative
    /// - If any surcharge exceeds 10000 bps (100%)
    /// - If the platform default fee plus all three surcharges exceeds
    ///   10000 bps, i.e. the worst case where every surcharge applies. A
    ///   per-token fee override is not known here, so `compute_fee_breakdown`
    ///   still caps the effective fee at 10000 bps at payment time.
    pub fn set_risk_fee_config(
        env: Env,
        admin: Address,
        large_payment_surcharge_bps: u32,
        large_payment_threshold: i128,
        new_customer_surcharge_bps: u32,
        btc_eth_surcharge_bps: u32,
    ) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "set_risk_fee_config");

        if large_payment_threshold < 0 {
            soroban_sdk::panic_with_error!(&env, Error::InvalidFeeConfig);
        }
        if large_payment_surcharge_bps > Self::MAX_FEE_BPS {
            panic!("large_payment_surcharge_bps cannot exceed 10000");
        }
        if new_customer_surcharge_bps > Self::MAX_FEE_BPS {
            panic!("new_customer_surcharge_bps cannot exceed 10000");
        }
        if btc_eth_surcharge_bps > Self::MAX_FEE_BPS {
            panic!("btc_eth_surcharge_bps cannot exceed 10000");
        }

        // Each term is <= 10000 here, so these additions cannot overflow u32.
        let default_fee_bps: u32 = env
            .storage()
            .instance()
            .get::<DataKey, u32>(&DataKey::DefaultFee)
            .unwrap_or(20u32)
            * 100;
        let worst_case_bps = default_fee_bps
            + large_payment_surcharge_bps
            + new_customer_surcharge_bps
            + btc_eth_surcharge_bps;
        if worst_case_bps > Self::MAX_FEE_BPS {
            panic!("default fee plus risk surcharges cannot exceed 10000 bps");
        }

        let config = RiskFeeConfig {
            large_payment_surcharge_bps,
            large_payment_threshold,
            new_customer_surcharge_bps,
            btc_eth_surcharge_bps,
        };
        env.storage()
            .instance()
            .set(&DataKey::RiskFeeConfig, &config);

        env.events().publish(
            (Symbol::new(&env, "risk_fee_config_updated"), admin.clone()),
            (
                large_payment_surcharge_bps,
                large_payment_threshold,
                new_customer_surcharge_bps,
                btc_eth_surcharge_bps,
            ),
        );
    }

    /// Get the current risk fee configuration.
    ///
    /// Returns the stored configuration, or the default configuration when
    /// none has been set. Risk scoring and fee deduction only apply a stored
    /// configuration; an unset configuration yields no surcharge even though
    /// this getter returns defaults.
    pub fn get_risk_fee_config(env: Env) -> RiskFeeConfig {
        Self::risk_fee_config(&env)
    }

    /// Internal accessor for the stored `RiskFeeConfig`, falling back to the
    /// default configuration when none has been set.
    fn risk_fee_config(env: &Env) -> RiskFeeConfig {
        env.storage()
            .instance()
            .get(&DataKey::RiskFeeConfig)
            .unwrap_or(RiskFeeConfig {
                large_payment_surcharge_bps: 0,
                large_payment_threshold: 1_000_000_000_000, // $100k
                new_customer_surcharge_bps: 0,
                btc_eth_surcharge_bps: 0,
            })
    }

    /// Calculate a risk score and surcharge for a student's payment in `token`.
    ///
    /// The `is_new_customer` / `is_btc_eth` risk flags are derived from
    /// on-chain state via `resolve_risk_flags()` — the same helper that
    /// enroll()/re_enroll() use — rather than trusted from the caller, so
    /// the preview always matches what enrollment actually charges.
    ///
    /// # Arguments
    /// - `student` — the student who would make the payment
    /// - `token` — the payment token address
    /// - `payment_amount` — the payment amount in stroops
    ///
    /// Returns a `RiskScore` with score (0-100) and surcharge_bps to add.
    /// Disabled risk pricing or a missing stored `RiskFeeConfig` yields zero.
    pub fn calculate_risk_score(
        env: Env,
        student: Address,
        token: Address,
        payment_amount: i128,
    ) -> RiskScore {
        let (is_new_customer, is_btc_eth) = Self::resolve_risk_flags(&env, &student, &token);
        Self::risk_score_internal(&env, payment_amount, is_new_customer, is_btc_eth)
    }

    /// Calculate the effective fee for a student's payment, applying
    /// risk-based surcharges exactly as `deduct_fee()` does at enrollment.
    ///
    /// # Arguments
    /// - `student` — the student who would make the payment
    /// - `token` — the token address (for per-token fee config)
    /// - `payment_amount` — the payment amount in stroops
    ///
    /// Returns a `RiskFeeApplied` struct with the full fee breakdown.
    pub fn get_effective_fee_for_payment(
        env: Env,
        student: Address,
        token: Address,
        payment_amount: i128,
    ) -> RiskFeeApplied {
        if payment_amount < 0 {
            panic!("payment_amount cannot be negative");
        }
        let (is_new_customer, is_btc_eth) = Self::resolve_risk_flags(&env, &student, &token);
        Self::compute_fee_breakdown(
            &env,
            &token,
            payment_amount,
            None,
            is_new_customer,
            is_btc_eth,
        )
    }

    /// Single source of truth for the `(is_new_customer, is_btc_eth)` risk
    /// flags applied to a student's payment in `token`. Used by both the fee
    /// previews and enroll()/re_enroll(), so a quote can never diverge from
    /// the actual charge.
    ///
    /// The contract does not yet persist per-student enrollment history or a
    /// BTC/ETH classification for tokens, so enrollment has always charged
    /// both flags as `false`. When that state is added, it must be read here
    /// so previews and enrollment pick it up together.
    fn resolve_risk_flags(_env: &Env, _student: &Address, _token: &Address) -> (bool, bool) {
        (false, false)
    }

    /// Computes the risk score and total surcharge (bps) for a payment.
    /// Surcharges only apply when risk pricing is enabled and a
    /// `RiskFeeConfig` is available (stored override or the default).
    fn risk_score_internal(
        env: &Env,
        payment_amount: i128,
        is_new_customer: bool,
        is_btc_eth: bool,
    ) -> RiskScore {
        let enabled: bool = env
            .storage()
            .instance()
            .get(&DataKey::RiskConfigEnabled)
            .unwrap_or(false);
        // Same config source as `get_risk_fee_config()`: stored override,
        // otherwise the default, so quotes and charges stay in sync.
        let config: Option<RiskFeeConfig> = Some(Self::get_risk_fee_config(env.clone()));

        let (score, surcharge_bps) = risk_surcharge(
            enabled,
            config.as_ref(),
            payment_amount,
            is_new_customer,
            is_btc_eth,
        );

        RiskScore {
            score,
            surcharge_bps,
        }
    }

    /// Computes the full fee breakdown for a payment: per-token base fee
    /// (falling back to `DefaultFee`) plus any risk surcharge, capped at
    /// 100%. Shared by `deduct_fee()` and `get_effective_fee_for_payment()`.
    ///
    /// `course_fee_override_bps` is the course's explicit registration-time
    /// fee override, if any. It acts as a floor on the live fee. Quotes pass
    /// `None` because the public quote API is not course-scoped; the charge
    /// path passes the enrolled course's override. Keeping the floor here
    /// means every fee multiplication goes through the same overflow guard.
    fn compute_fee_breakdown(
        env: &Env,
        token: &Address,
        payment_amount: i128,
        course_fee_override_bps: Option<u32>,
        is_new_customer: bool,
        is_btc_eth: bool,
    ) -> RiskFeeApplied {
        let base_fee_bps = Self::get_fee_config(env.clone(), token.clone()).fee_bps;

        if payment_amount <= 0 {
            return RiskFeeApplied {
                payment_amount,
                base_fee_bps,
                risk_surcharge_bps: 0,
                effective_fee_bps: base_fee_bps,
                platform_fee: 0,
            };
        }

        let risk_surcharge_bps =
            Self::risk_score_internal(env, payment_amount, is_new_customer, is_btc_eth)
                .surcharge_bps;

        // Cap at 100% (10000 bps)
        let effective_fee_bps = base_fee_bps.saturating_add(risk_surcharge_bps).min(10000);

        // Compute platform fee: amount * effective_bps / 10000. Both the
        // quote and the charge use this helper, so overflow always panics
        // with the same message instead of silently quoting a zero fee.
        let mut platform_fee = Self::compute_fee_amount(env, payment_amount, effective_fee_bps);
        if let Some(override_bps) = course_fee_override_bps {
            platform_fee = platform_fee.max(Self::compute_fee_amount(env, payment_amount, override_bps));
        }

        RiskFeeApplied {
            payment_amount,
            base_fee_bps,
            risk_surcharge_bps,
            effective_fee_bps,
            platform_fee,
        }
    }

    /// Single overflow-checked fee multiplication: `amount * fee_bps / 10000`.
    /// Panicking (rather than falling back to zero) keeps quotes honest: if a
    /// charge cannot be computed, the quote must fail too.
    fn compute_fee_amount(env: &Env, amount: i128, fee_bps: u32) -> i128 {
        amount
            .checked_mul(fee_bps as i128)
            .map(|v| v / 10000)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(env, Error::ArithmeticOverflow))
    }

    /// Compute and deduct the platform fee from a payment amount.
    /// Uses per-token fee configuration if set, otherwise falls back to
    /// the global `DefaultFee`. Applies risk-based surcharges when the
    /// risk pricing config is enabled.
    ///
    /// # Arguments
    /// - `env`        — the contract environment
    /// - `token`      — the payment token address
    /// - `amount`     — the total payment amount (in stroops)
    /// - `course_fee_override_bps` — the course's explicit registration-time
    ///   fee override, if any; used as a floor on the live per-token fee
    /// - `is_new_customer` — whether the student has no prior enrollments
    /// - `is_btc_eth` — whether the token is BTC/ETH (higher volatility)
    ///
    /// Returns `(net_amount, fee_amount)` where `net_amount + fee_amount == amount`.
    fn deduct_fee(
        env: &Env,
        token: &Address,
        amount: i128,
        course_fee_override_bps: Option<u32>,
        is_new_customer: bool,
        is_btc_eth: bool,
    ) -> (i128, i128) {
        let (net_amount, platform_fee, breakdown) = Self::calculate_enrollment_split(
            env,
            token,
            amount,
            course_fee_override_bps,
            is_new_customer,
            is_btc_eth,
        );

        if let Some(breakdown) = breakdown {
            // Publish RiskFeeApplied event when a risk surcharge was applied.
            if breakdown.risk_surcharge_bps > 0 {
                env.events()
                    .publish((Symbol::new(env, "risk_fee_applied"),), breakdown);
            }
        }

        (net_amount, platform_fee)
    }

    fn calculate_enrollment_split(
        env: &Env,
        token: &Address,
        amount: i128,
        course_fee_override_bps: Option<u32>,
        is_new_customer: bool,
        is_btc_eth: bool,
    ) -> (i128, i128, Option<RiskFeeApplied>) {
        if amount <= 0 {
            return (0, 0, None);
        }

        let breakdown = Self::compute_fee_breakdown(
            env,
            token,
            amount,
            course_fee_override_bps,
            is_new_customer,
            is_btc_eth,
        );
        let platform_fee = breakdown.platform_fee;
        let net_amount = amount - platform_fee;

        (net_amount, platform_fee, Some(breakdown))
    }

    /// Escalate a dispute to arbitration. The caller must pay the
    /// arbitration fee (set via `set_arbitration_fee_config`).
    /// The fee is transferred from the caller to the contract and held
    /// until the arbitration is resolved.
    pub fn escalate_to_arbitration(env: Env, caller: Address, course_id: String) {
        caller.require_auth();

        let course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CourseNotFound));

        let config: ArbitrationFeeConfig =
            Self::get_arbitration_fee_config(env.clone(), course.token.clone());

        if config.fee_per_case <= 0 {
            soroban_sdk::panic_with_error!(&env, Error::ArbitrationError);
        }

        let course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| panic!("course not found"));

        // Only an enrolled student (current or historical) or the course
        // instructor may open a dispute on a course.
        let is_instructor = caller == course.instructor;
        let is_current_student = env
            .storage()
            .persistent()
            .has(&DataKey::Enrollment(caller.clone(), course_id.clone()));
        let is_historical_student = env.storage().persistent().has(&DataKey::EnrollmentHistory(
            caller.clone(),
            course_id.clone(),
        ));

        if !is_instructor && !is_current_student && !is_historical_student {
            panic!(
                "unauthorized: caller is not enrolled in or the instructor of course {:?}",
                course_id
            );
        }

        if course.status != CourseStatus::Active {
            soroban_sdk::panic_with_error!(&env, HamplardError::CourseNotActive);
        }

        if !env
            .storage()
            .instance()
            .has(&DataKey::ApprovedToken(course.token.clone()))
        {
            soroban_sdk::panic_with_error!(&env, HamplardError::TokenNotApproved);
        }

        // Reject duplicate open cases for the same (caller, course_id) pair.
        let case_key = ArbitrationCaseKey {
            caller: caller.clone(),
            course_id: course_id.clone(),
        };
        if env.storage().persistent().has(&case_key) {
            let existing: ArbitrationCase = env.storage().persistent().get(&case_key).unwrap();
            if existing.status == DisputeStatus::Open {
                soroban_sdk::panic_with_error!(&env, Error::ArbitrationError);
            }
        }

        let token_client = token::Client::new(&env, &course.token);
        token_client.transfer(
            &caller,
            &env.current_contract_address(),
            &config.fee_per_case,
        );

        // Track the fee separately from instructor earnings and platform funds
        // so it can be released only through arbitration resolution.
        Self::credit_arbitration_fees_held(&env, &course.token, config.fee_per_case);

        // Derive a case ID from the course_id (unique per caller+course_id pair).
        let case_id = course_id.clone();

        let record = ArbitrationCase {
            case_id: case_id.clone(),
            caller: caller.clone(),
            course_id: course_id.clone(),
            instructor: course.instructor.clone(),
            token: course.token.clone(),
            fee_paid: config.fee_per_case,
            opened_at_ledger: env.ledger().sequence(),
            status: DisputeStatus::Open,
        };

        Self::persist(&env, &case_key, &record);

        // Track total outstanding arbitration fees for this token
        let arb_key = DataKey2::TotalArbitrationFees(course.token.clone());
        let current_arb: i128 = env.storage().instance().get(&arb_key).unwrap_or(0);
        let new_arb = current_arb
            .checked_add(config.fee_per_case)
            .unwrap_or_else(|| panic!("overflow computing total arbitration fees"));
        env.storage().instance().set(&arb_key, &new_arb);

        env.events().publish(
            (Symbol::new(&env, "dispute_escalated"), course_id.clone()),
            (caller, course_id, config.fee_per_case),
        );
    }

    /// Retrieve a persistent arbitration case record.
    ///
    /// Returns `None` if no case has been opened for this (caller, course_id) pair.
    pub fn get_arbitration_case(
        env: Env,
        caller: Address,
        course_id: String,
    ) -> Option<ArbitrationCase> {
        env.storage()
            .persistent()
            .get(&ArbitrationCaseKey { caller, course_id })
    }

    /// Admin function to resolve an open arbitration case and disburse the fee.
    ///
    /// - `for_student = true`  — refund the fee to the original caller (student won).
    /// - `for_student = false` — transfer the fee to the course instructor (instructor won).
    ///
    /// Panics if the case does not exist or is not `Open`.
    pub fn resolve_arbitration(
        env: Env,
        admin: Address,
        caller: Address,
        course_id: String,
        for_student: bool,
    ) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "resolve_arbitration");
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        let case_key = ArbitrationCaseKey {
            caller: caller.clone(),
            course_id: course_id.clone(),
        };
        let mut case: ArbitrationCase = env
            .storage()
            .persistent()
            .get(&case_key)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::ArbitrationError));

        if case.status != DisputeStatus::Open {
            soroban_sdk::panic_with_error!(&env, Error::ArbitrationError);
        }

        let token_client = token::Client::new(&env, &case.token);

        // Release the reserved fee from tracked arbitration funds before
        // paying it out, so held balances always match open cases.
        Self::debit_arbitration_fees_held(&env, &case.token, case.fee_paid);

        if for_student {
            // Refund the fee to the student who escalated.
            token_client.transfer(
                &env.current_contract_address(),
                &case.caller,
                &case.fee_paid,
            );
            case.status = DisputeStatus::ResolvedForStudent;
        } else {
            // Pay the fee to the instructor.
            token_client.transfer(
                &env.current_contract_address(),
                &case.instructor,
                &case.fee_paid,
            );
            case.status = DisputeStatus::ResolvedForInstructor;
        }

        Self::persist(&env, &case_key, &case);

        // Decrement total outstanding arbitration fees for this token
        let arb_key = DataKey2::TotalArbitrationFees(case.token.clone());
        let current_arb: i128 = env.storage().instance().get(&arb_key).unwrap_or(0);
        let new_arb = current_arb.saturating_sub(case.fee_paid);
        if new_arb == 0 {
            env.storage().instance().remove(&arb_key);
        } else {
            env.storage().instance().set(&arb_key, &new_arb);
        }

        env.events().publish(
            (Symbol::new(&env, "arbitration_resolved"), course_id.clone()),
            (caller, course_id, for_student, admin),
        );
    }

    /// Preview effective payment fee calculation with risk assessment.
    ///
    /// NOTE: This function is read-only and DOES NOT perform any token transfer
    /// or mutate contract state. It calculates the fee breakdown (base fee,
    /// dynamic discount, risk fee, final payment amount) identical to
    /// `get_effective_fee_for_payment`.
    pub fn preview_payment_fee(
        env: Env,
        student: Address,
        token: Address,
        payment_amount: i128,
    ) -> RiskFeeApplied {
        Self::get_effective_fee_for_payment(env, student, token, payment_amount)
    }

    /// Read-only fee preview calculation.
    ///
    /// NOTE: Despite the name, this function DOES NOT transfer tokens or mutate state.
    /// It is a read-only helper identical to `preview_payment_fee` / `get_effective_fee_for_payment`.
    pub fn do_complete_payment(
        env: Env,
        student: Address,
        token: Address,
        payment_amount: i128,
    ) -> RiskFeeApplied {
        Self::get_effective_fee_for_payment(env, student, token, payment_amount)
    }

    // ----------------------------------------------------------
    // WAITLIST MANAGEMENT
    // ----------------------------------------------------------

    /// Student joins the waitlist for a course that has reached capacity.
    /// When a spot opens up (via refund or enrollment expiry), the first
    /// waitlisted student will be automatically promoted to enrolled status.
    ///
    /// # Arguments
    /// - `student`   — student's address (must sign)
    /// - `course_id` — the course to join waitlist for
    pub fn join_waitlist(env: Env, student: Address, course_id: String) {
        student.require_auth();

        let course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::CourseNotFound));

        if Self::is_student_blocked_internal(&env, &student) {
            soroban_sdk::panic_with_error!(&env, Error::StudentBlocked);
        }

        if Self::is_admin(&env, &student) {
            soroban_sdk::panic_with_error!(&env, Error::EntryRestricted);
        }

        if student == course.instructor {
            soroban_sdk::panic_with_error!(&env, Error::EntryRestricted);
        }

        if course.status != CourseStatus::Active {
            soroban_sdk::panic_with_error!(&env, Error::CourseNotActive);
        }

        // Cannot join waitlist if already enrolled
        if env
            .storage()
            .persistent()
            .has(&DataKey::Enrollment(student.clone(), course_id.clone()))
        {
            soroban_sdk::panic_with_error!(&env, Error::AlreadyEnrolled);
        }

        // Check if course is at capacity
        if let Some(cap) = course.max_capacity {
            if course.total_enrollments < cap {
                soroban_sdk::panic_with_error!(&env, Error::WaitlistError);
            }
        } else {
            soroban_sdk::panic_with_error!(&env, Error::WaitlistError);
        }

        let waitlist_key = DataKey::CourseWaitlist(course_id.clone());
        let mut waitlist: Vec<Address> = env
            .storage()
            .persistent()
            .get(&waitlist_key)
            .unwrap_or_else(|| Vec::new(&env));

        // Check if student is already on waitlist
        for i in 0..waitlist.len() {
            if waitlist.get(i).unwrap() == student {
                soroban_sdk::panic_with_error!(&env, Error::WaitlistError);
            }
        }

        if waitlist.len() >= Self::MAX_WAITLIST_ENTRIES {
            soroban_sdk::panic_with_error!(&env, Error::WaitlistError);
        }

        waitlist.push_back(student.clone());
        Self::persist(&env, &waitlist_key, &waitlist);

        env.events().publish(
            (Symbol::new(&env, "waitlist_joined"), course_id.clone()),
            (student, course_id),
        );
    }

    /// Student leaves the waitlist for a course.
    ///
    /// # Arguments
    /// - `student`   — student's address (must sign)
    /// - `course_id` — the course to leave waitlist for
    pub fn leave_waitlist(env: Env, student: Address, course_id: String) {
        student.require_auth();

        let waitlist_key = DataKey::CourseWaitlist(course_id.clone());
        let waitlist: Vec<Address> = env
            .storage()
            .persistent()
            .get(&waitlist_key)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, Error::WaitlistError));

        let mut found_index: Option<u32> = None;
        for i in 0..waitlist.len() {
            if waitlist.get(i).unwrap() == student {
                found_index = Some(i);
                break;
            }
        }

        if found_index.is_none() {
            soroban_sdk::panic_with_error!(&env, Error::WaitlistError);
        }

        // Remove student from waitlist by creating a new vec without them
        let mut new_waitlist = Vec::new(&env);
        for i in 0..waitlist.len() {
            if i != found_index.unwrap() {
                new_waitlist.push_back(waitlist.get(i).unwrap());
            }
        }

        if !new_waitlist.is_empty() {
            Self::persist(&env, &waitlist_key, &new_waitlist);
        } else {
            env.storage().persistent().remove(&waitlist_key);
        }

        env.events().publish(
            (Symbol::new(&env, "waitlist_left"), course_id.clone()),
            (student, course_id),
        );
    }

    /// Get the waitlist for a course.
    ///
    /// # Arguments
    /// - `course_id` — the course to get waitlist for
    ///
    /// # Returns
    /// Ordered list of student addresses on the waitlist (first = next to be promoted)
    pub fn get_waitlist(env: Env, course_id: String) -> Option<Vec<Address>> {
        let waitlist_key = DataKey::CourseWaitlist(course_id);
        env.storage().persistent().get(&waitlist_key)
    }

    /// Internal helper to promote the first waitlisted student to enrolled status
    /// when capacity opens. Called automatically when a refund or expiry creates space.
    fn promote_from_waitlist(env: &Env, course_id: &String) {
        let waitlist_key = DataKey::CourseWaitlist(course_id.clone());
        let waitlist: Option<Vec<Address>> = env.storage().persistent().get(&waitlist_key);

        if let Some(list) = waitlist {
            if !list.is_empty() {
                // Skip stale entries: students who are already enrolled, blocked,
                // or whose enrollment record was otherwise cleaned up. Collect
                // remaining (non-stale, not-yet-promoted) entries.
                let mut remaining = Vec::new(env);
                let mut promoted: Option<Address> = None;

                for i in 0..list.len() {
                    let candidate = list.get(i).unwrap();

                    if promoted.is_some() {
                        // Already found someone to promote; keep the rest.
                        remaining.push_back(candidate);
                        continue;
                    }

                    let already_enrolled = env
                        .storage()
                        .persistent()
                        .has(&DataKey::Enrollment(candidate.clone(), course_id.clone()));
                    let is_blocked = Self::is_student_blocked_internal(env, &candidate);

                    if already_enrolled || is_blocked {
                        // Stale entry — drop it silently.
                        continue;
                    }

                    // This candidate gets the slot.
                    promoted = Some(candidate);
                }

                if !remaining.is_empty() {
                    Self::persist(env, &waitlist_key, &remaining);
                } else {
                    env.storage().persistent().remove(&waitlist_key);
                }

                // Emit a notification event so the student can self-enroll.
                // Directly calling enroll_internal here would attempt a token
                // transfer that the student has not authorised in this transaction
                // (#684), and would also fail for blocked students, low balances
                // and Paused courses (which archive_course requires).
                if let Some(next_student) = promoted {
                    env.events().publish(
                        (Symbol::new(env, "waitlist_ready"), course_id.clone()),
                        (next_student, course_id.clone()),
                    );
                }
            }
        }
    }
}

mod test;

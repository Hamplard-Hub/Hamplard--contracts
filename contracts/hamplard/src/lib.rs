//! # Hamplard Contract — Security Model
//!
//! ## Trust Hierarchy
//!
//! | Role              | Who                    | Capabilities                                                          |
//! |-------------------|------------------------|-----------------------------------------------------------------------|
//! | Admin             | `DataKey::Admin`       | Approve/archive courses, issue & revoke certificates, pause platform, block students  |
//! | Secondary Admin   | `DataKey::SecondaryAdmin` | Required alongside Admin for multi-sig operations (archive, treasury update, admin transfer) |
//! | Instructor        | Course `instructor` field | Register courses, pause/unpause own courses, withdraw earnings, transfer course (with admin co-approval) |
//! | Student           | Any caller             | Enroll in active courses (must sign), batch-enroll (unless blocked by admin)  |
//! | Treasury          | `DataKey::Treasury`    | Passive recipient of platform fee share; cannot initiate any action   |
//!
//! ## Privileged Operations (single admin)
//! - `approve_course` — moves a course from Pending to Active
//! - `transfer_course` — co-approves an instructor-initiated course ownership transfer
//! - `mark_completed` — marks a student enrollment as completed (blocked students cannot proceed)
//! - `issue_certificate` — mints an on-chain certificate of completion (blocked students cannot proceed)
//! - `revoke_certificate` — flags a certificate as revoked (remains on-chain for audit)
//! - `bulk_revoke_course_certificates` — flags every certificate of a course as revoked in one transaction
//! - `pause_platform` / `unpause_platform` — halts or restores all enrollments
//! - `add_approved_token` / `remove_approved_token` — controls which token contracts are accepted
//! - `update_default_fee` / `update_max_courses_limit` — updates global parameters
//! - `block_student` / `unblock_student` — bans or unbans a student from the platform
//! - `get_platform_fee` — retrieves platform fee configuration (admin only)
//! - `withdraw_tokens` — emergency sweep of contract-held tokens (admin only)
//!
//! ## Privileged Operations (multi-sig — both Admin + Secondary Admin required)
//! - `archive_course` — permanent course removal; may trigger student refunds
//! - `transfer_admin` — proposes a new admin pair (new admins must then call `accept_admin`)
//! - `update_treasury` — schedules a new treasury address (takes effect after 100 ledgers)
//! - `set_admin_expiry` — sets a ledger sequence when the admin role expires (blocks all admin operations)
//! - `propose_upgrade` / `upgrade_contract` / `cancel_upgrade` — time-locked contract code
//!   upgrade; see "Contract Upgrades" below
//!
//! ## Student Blocking Policy
//! - `block_student()` called by the admin prevents a student from:
//!   - Enrolling in new courses via `enroll()` / `batch_enroll()` / `re_enroll()`
//!   - Marking an existing enrollment as completed via `mark_completed()`
//!   - Receiving a certificate via `issue_certificate()`
//!   - Requesting or receiving refunds via `request_refund()` / `process_refund()`
//! - Blocking is global and applies indefinitely until `unblock_student()` is called
//! - Blocking does not retroactively revoke already-issued certificates; only forward-looking actions are blocked
//! - A blocked student's existing enrollments are frozen: they cannot transition to Completed status
//!   and therefore cannot receive certificates for that work.
//!
//! ## Admin Expiry Policy
//! - `set_admin_expiry()` called by both admins sets a ledger sequence at which the admin role expires
//! - Once the current ledger sequence reaches or exceeds the expiry value:
//!   - All single-admin operations (`approve_course`, `block_student`, `issue_certificate`, etc.)
//!     automatically fail with "admin role has expired"
//!   - All multi-admin operations (`archive_course`, `transfer_admin`, `set_admin_expiry`, etc.)
//!     automatically fail with "admin role has expired"
//!   - The only recovery is for the expired admins to call `transfer_admin()` to nominate a new pair,
//!     and the new pair calls `accept_admin()` — this resets the admin expiry to None
//! - Expiry may be cleared by calling `set_admin_expiry()` with `None` before it takes effect
//!
//! ## Payment Guarantees
//! - On enrollment the full course price is transferred from the student atomically:
//!   `platform_fee_percent` of the price is forwarded to the treasury address immediately;
//!   the remaining instructor share is held inside the contract and credited to
//!   `DataKey::InstructorEarnings` for pull-based withdrawal.
//! - Revenue split uses integer arithmetic: `platform_amount = price * pct / 100`.
//!   Any remainder (from integer truncation) stays with the instructor share.
//! - The contract does **not** escrow student funds beyond the enrollment transaction;
//!   post-enrollment refunds require admin-initiated archiving with an explicit refund list.
//!
//! ## What This Contract Does NOT Protect Against
//! - **Off-chain content access** — the contract cannot enforce that a student actually
//!   receives course materials after enrolling; content delivery is the backend's responsibility.
//! - **Course quality or accuracy** — admin approval is a policy gate only; the contract
//!   does not validate course content or instructor qualifications.
//! - **Instructor insolvency** — if the instructor's earnings balance is insufficient for a
//!   refund (e.g. concurrent withdrawals), the archive refund will panic. Callers must
//!   ensure balances are adequate before invoking `archive_course` with refunds.
//! - **Token price risk** — payment amounts are fixed in token stroops at enrollment time;
//!   the contract makes no exchange-rate or price guarantees.
//! - **Front-running** — enrollment order is determined by ledger sequence; the contract
//!   does not prevent two students from enrolling in the last seat simultaneously on
//!   different nodes (Soroban consensus resolves ordering).
//! - **Admin key compromise** — a compromised admin key can approve courses, issue
//!   certificates, and withdraw contract tokens. Key rotation requires the two-step
//!   `transfer_admin` / `accept_admin` flow with both current admins signing. Admin expiry
//!   enforces that both admins must cooperate for critical operations; a single compromised
//!   key cannot bypass the expiry check once it takes effect.
//! - **Treasury update delay** — `update_treasury` takes effect 100 ledgers after proposal;
//!   enrollments submitted within that window still route fees to the old treasury.
//!
//! ## Contract Upgrades
//! - The contract can migrate to a new Wasm implementation in place, preserving all
//!   enrollment, certificate, and course data — no redeploy or data loss required to
//!   ship a fix.
//! - Upgrades are two-step and time-locked: `propose_upgrade` (both admins) records the
//!   new Wasm hash and starts a `get_upgrade_timelock()`-ledger review window (default
//!   ~1 day); `upgrade_contract` (both admins) executes it only after that window has
//!   elapsed. `cancel_upgrade` withdraws a pending proposal at any time.
//! - A compromised single admin key cannot upgrade the contract — both admin signatures
//!   are required for every step, and the time-lock gives observers a chance to react to
//!   a malicious proposal before it can take effect.

#![no_std]

#[cfg(test)]
extern crate std;

use soroban_sdk::{
    contract, contractimpl, contracttype, token, Address, BytesN, Env, String, Symbol, Vec,
};

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

// ============================================================
// DATA TYPES
// ============================================================

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
    /// Total number of enrollments (incremented on each enroll)
    pub total_enrollments: u32,
    /// Total active enrollments (enrolled but not completed)
    pub active_enrollments: u32,
    /// Total USDC earned across all enrollments (in stroops)
    pub total_earned: i128,
    /// Course status
    pub status: CourseStatus,
    /// Ledger sequence when the course was registered
    pub created_at_ledger: u32,
    pub max_capacity: Option<u32>,
    /// Optional ledger sequence before which new enrollments are rejected.
    /// Allows instructors/admins to create a grace period after approval before
    /// student intake begins.
    pub enrollment_start_ledger: Option<u32>,
    /// Optional enrollment expiry duration in ledger sequences.
    /// If set, an enrollment is considered inactive (expired) after
    /// `enrolled_at_ledger + enrollment_expiry_ledgers` ledgers.
    /// Expired enrollments cannot be marked completed and are treated
    /// as inactive, freeing the conceptual course slot.
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
    /// When set, enforces a hard limit on certificate issuance to prevent credential dilution.
    pub max_certificates: Option<u32>,
    /// Number of certificates issued for this course
    pub certificates_issued: u32,
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
    /// Optional proof of completion evidence (e.g. hash)
    pub evidence_hash: Option<String>,
    /// Whether this enrollment has been refunded
    pub is_refunded: bool,
    /// Course version active at the time of enrollment
    pub course_version: u32,
    /// Platform's share of `amount_paid`, as actually deducted at
    /// enroll()/re_enroll() time via `deduct_fee()`. Persisted so that a
    /// later refund splits funds in the same proportion that was actually
    /// collected, even if `FeeConfig(token)` or `course.platform_fee_percent`
    /// changes afterward.
    pub platform_amount: i128,
    /// Instructor's share of `amount_paid`, as actually credited to
    /// `InstructorEarnings` at enroll()/re_enroll() time.
    pub instructor_amount: i128,
    /// Unique enrollment reference ID, used by issue_certificate to look up
    /// the enrollment and derive the course_id without caller-supplied course_id.
    pub enrollment_ref: String,
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
    /// Optional Ed25519 signature from the instructor over the certificate
    /// data, allowing external verifiers to cryptographically confirm the
    /// instructor endorsed this certificate without off-chain evidence.
    pub instructor_signature: Option<BytesN<64>>,
    /// Ledger sequence after which a pending revocation becomes permanent.
    /// `Some(seq)` means revocation is pending and challengeable until
    /// the current ledger reaches `seq`. `None` means no pending revocation.
    pub revocation_deadline: Option<u32>,
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
}

/// A refund request record
#[contracttype]
#[derive(Clone)]
pub struct RefundRequest {
    pub student: Address,
    pub course_id: String,
    pub requested_at_ledger: u32,
    pub status: RefundStatus,
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
    /// Total number of certificates issued via `issue_certificate`.
    pub total_certificates: u32,
}

// ============================================================
// STORAGE KEYS
// ============================================================

#[contracttype]
pub enum DataKey {
    /// Course record by course ID
    Course(String),
    /// Immutable instructor address recorded at registration time
    CourseInstructorRef(String),
    /// Enrollment record by (student_address, course_id)
    Enrollment(Address, String),
    /// Certificate record by certificate ID
    Certificate(String),
    /// Admin address — set at init, can approve courses and issue certificates
    Admin,
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
    /// Accumulated instructor earnings per (instructor, token) pair (in stroops)
    InstructorEarnings(Address, Address),
    /// Accumulated instructor earnings across all tokens (in stroops)
    InstructorTotalEarnings(Address),
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
    /// Default minimum ledger sequences required between enroll() and
    /// mark_completed() for newly-registered courses (admin-configurable).
    DefaultMinCompletionLedgers,
    /// Refund window delay in ledger sequences
    RefundWindow,
    /// Refund request record by (student_address, course_id)
    RefundRequest(Address, String),
    /// Blocklist of instructor addresses who are frozen
    InstructorBlocked(Address),
    /// Ordered list of all registered course IDs (on-chain catalog)
    CourseList,
    /// Archived past `Enrollment` records for a (student, course_id) pair,
    /// preserved when a completed student re-enrolls via `re_enroll()`.
    EnrollmentHistory(Address, String),
    /// Aggregate reputation stats for an instructor (total students,
    /// completions, certificates issued) — see `InstructorStats`.
    InstructorStats(Address),
    /// Blocklist of student addresses who are banned from the platform
    StudentBlocked(Address),
    /// Per-token fee configuration (maps token address → FeeConfig)
    FeeConfig(Address),
    /// Arbitration fee configuration
    ArbitrationFeeConfig,
    /// Risk fee configuration for surcharge pricing
    RiskFeeConfig,
    /// Flag indicating whether risk-based fee pricing is enabled
    RiskConfigEnabled,
    /// Total active enrollments across all courses platform-wide
    TotalActiveEnrollments,
    /// Maximum total active enrollments allowed platform-wide
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
    /// Mapping from enrollment_reference (unique ID) to (student, course_id)
    /// Used by issue_certificate to look up enrollments without a caller-supplied course_id.
    EnrollmentByRef(String),
    /// Counter for generating unique enrollment references
    EnrollmentRefCounter,
    /// Global registry of approver addresses with course approval authority
    Approver(Address),
    /// Ledger sequence when init() was called — used to enforce
    /// a governance window before approve_course() can be called.
    InitLedger,
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

#[contract]
pub struct HamplardContract;

#[contractimpl]
impl HamplardContract {
    /// Minimum ledgers before instance storage TTL extension is triggered (~1 year)
    const INSTANCE_TTL_THRESHOLD: u32 = 6_000_000;
    const INSTANCE_TTL_EXTEND_TO: u32 = 6_300_000;
    /// Minimum ledgers before persistent storage TTL extension is triggered (~1 year)
    const PERSISTENT_TTL_THRESHOLD: u32 = 6_000_000;
    const PERSISTENT_TTL_EXTEND_TO: u32 = 6_300_000;
    const MAX_COURSE_ID_LEN: u32 = 256;
    const MAX_COURSE_TITLE_LEN: u32 = 512;
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

    // ----------------------------------------------------------
    // INIT
    // ----------------------------------------------------------

    /// Initialise the contract.
    /// Called once by the deployer immediately after deployment.
    ///
    /// # Arguments
    /// - `admin`                    — admin address (approves courses, issues certificates)
    /// - `treasury`                 — platform treasury address (receives platform fee share)
    /// - `default_fee_pct`          — default platform fee percentage (e.g. 20 = 20%)
    /// - `refund_window_ledgers`    — number of ledger sequences after enrollment during which a
    ///                                refund request is accepted; requests after this window are
    ///                                automatically rejected (e.g. 17_280 ≈ 1 day at 5s/ledger)
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
        admin.require_auth();

        if env.storage().instance().has(&DataKey::Admin) {
            panic!("contract already initialized");
        }

        if default_fee_pct > 100 {
            panic!("fee percentage cannot exceed 100");
        }

        if treasury == env.current_contract_address() {
            panic!("treasury cannot be the contract address");
        }

        if admin == treasury {
            panic!("admin and treasury must be distinct addresses");
        }

        if secondary_admin == treasury {
            panic!("secondary_admin and treasury must be distinct addresses");
        }

        if admin == secondary_admin {
            panic!("admin and secondary_admin must be distinct addresses");
        }

        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        env.storage().instance().set(&DataKey::Admin, &admin);
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
            .unwrap_or_else(|| panic!("course not found"));

        let is_admin = Self::is_admin(&env, &caller);
        let is_instructor = caller == course.instructor;

        if !is_admin && !is_instructor {
            panic!("unauthorized");
        }

        if Self::is_instructor_frozen_internal(&env, &course.instructor) {
            panic!("instructor is frozen");
        }

        if course.status == CourseStatus::Archived {
            panic!("cannot update archived course");
        }

        course.enrollment_start_ledger = enrollment_start_ledger;
        course.last_updated_ledger = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);
        env.storage().persistent().extend_ttl(
            &DataKey::Course(course_id.clone()),
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        env.events().publish(
            (Symbol::new(&env, "enrollment_start_set"), course_id.clone()),
            (course_id, enrollment_start_ledger),
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
    /// - `token`            — USDC Stellar Asset Contract address
    /// - `platform_fee_pct` — optional fee override; pass 0 to use platform default
    /// - `content_hash`     — 32-byte hash of the off-chain course content at registration time
    pub fn register_course(
        env: Env,
        instructor: Address,
        course_id: String,
        price: i128,
        token: Address,
        platform_fee_pct: u32,
        max_capacity: Option<u32>,
        content_hash: BytesN<32>,
    ) -> String {
        instructor.require_auth();

        // Validate that the token address is a contract (not an EOA)
        let token_client = token::Client::new(&env, &token);
        let token_decimals = token_client.decimals();
        
        // Validate token decimal precision matches expected standard (7 for Stellar USDC)
        // This ensures fee calculations are accurate
        const EXPECTED_TOKEN_DECIMALS: u32 = 7;
        if token_decimals != EXPECTED_TOKEN_DECIMALS {
            panic!("token decimal precision must be {} (found {})", EXPECTED_TOKEN_DECIMALS, token_decimals);
        }

        if Self::is_instructor_frozen_internal(&env, &instructor) {
            panic!("instructor is frozen");
        }

        if course_id.is_empty() {
            panic!("course_id cannot be empty");
        }

        if course_id.len() > Self::MAX_COURSE_ID_LEN {
            panic!("course_id exceeds maximum length");
        }

        // `Some(0)` would make the course permanently unenrollable
        // (`total_enrollments >= 0` is always true); use `None` for unlimited.
        if max_capacity == Some(0) {
            panic!("max_capacity must be greater than zero (use None for unlimited)");
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
                panic!("course_id contains invalid characters (must be printable ASCII)");
            }
        }

        if price < 0 {
            panic!("price cannot be negative");
        }

        // A price of exactly 0 is a valid free course. Any non-zero price
        // must be denominated in stroops at the token's expected 7-decimal
        // precision — reject values so small or so large that they signal
        // the price was entered in the wrong unit.
        if price != 0
            && (price < Self::MIN_COURSE_PRICE_STROOPS || price > Self::MAX_COURSE_PRICE_STROOPS)
        {
            panic!("price is outside the expected USDC precision range (0 for free, or 0.01-100000 USDC in stroops)");
        }

        if env
            .storage()
            .persistent()
            .has(&DataKey::Course(course_id.clone()))
        {
            panic!("course already registered");
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
                    panic!("course ID is within archival cooldown period");
                }
            }
        }

        let max_courses: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MaxCoursesPerInstructor)
            .unwrap_or(50);

        let course_count_key = DataKey::InstructorCourseCount(instructor.clone());
        let current_count: u32 = env.storage().instance().get(&course_count_key).unwrap_or(0);

        if current_count >= max_courses {
            panic!("instructor has reached the maximum number of course registrations");
        }

        let pending_count_key = DataKey::InstructorPendingCourseCount(instructor.clone());
        let current_pending_count: u32 = env
            .storage()
            .instance()
            .get(&pending_count_key)
            .unwrap_or(0);

        if current_pending_count >= max_courses {
            panic!("instructor has reached the maximum number of pending course registrations");
        }

        let default_fee = env
            .storage()
            .instance()
            .get::<DataKey, u32>(&DataKey::DefaultFee)
            .unwrap_or(20);

        let fee = if platform_fee_pct == 0 {
            default_fee
        } else {
            if platform_fee_pct > 100 {
                panic!("fee percentage cannot exceed 100");
            }
            if platform_fee_pct < default_fee {
                panic!("fee percentage cannot be below platform minimum");
            }
            platform_fee_pct
        };

        let min_completion_ledgers: u32 = env
            .storage()
            .instance()
            .get(&DataKey::DefaultMinCompletionLedgers)
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
            status: CourseStatus::Pending,
            created_at_ledger: env.ledger().sequence(),
            max_capacity,
            enrollment_start_ledger: None,
            enrollment_expiry_ledgers: None,
            min_completion_ledgers,
            version: 1,
            last_updated_ledger: env.ledger().sequence(),
            expires_at_ledger: None,
            content_hash,
            max_certificates: None,
            certificates_issued: 0,
            prerequisite_course_ids: Vec::new(&env),
            archived_at_ledger: None,
        };

        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);

        env.storage().persistent().extend_ttl(
            &DataKey::Course(course_id.clone()),
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        env.storage().persistent().set(
            &DataKey::CourseInstructorRef(course_id.clone()),
            &instructor,
        );
        env.storage().persistent().extend_ttl(
            &DataKey::CourseInstructorRef(course_id.clone()),
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        // Append to on-chain course catalog
        let mut catalog: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::CourseList)
            .unwrap_or_else(|| Vec::new(&env));
        catalog.push_back(course_id.clone());
        env.storage()
            .persistent()
            .set(&DataKey::CourseList, &catalog);
        env.storage().persistent().extend_ttl(
            &DataKey::CourseList,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        env.storage().instance().set(
            &DataKey::InstructorCourseCount(instructor.clone()),
            &(current_count + 1),
        );
        env.storage()
            .instance()
            .set(&pending_count_key, &(current_pending_count + 1));

        // Append to the per-instructor course list
        let instructor_list_key = DataKey::InstructorCourseList(instructor.clone());
        let mut instructor_courses: Vec<String> = env
            .storage()
            .persistent()
            .get(&instructor_list_key)
            .unwrap_or_else(|| Vec::new(&env));
        instructor_courses.push_back(course_id.clone());
        env.storage()
            .persistent()
            .set(&instructor_list_key, &instructor_courses);
        env.storage().persistent().extend_ttl(
            &instructor_list_key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

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
            env.storage()
                .persistent()
                .set(&DataKey::InstructorRegistry, &registry);
            env.storage().persistent().extend_ttl(
                &DataKey::InstructorRegistry,
                Self::PERSISTENT_TTL_THRESHOLD,
                Self::PERSISTENT_TTL_EXTEND_TO,
            );
        }

        env.events().publish(
            (Symbol::new(&env, "course_registered"), course_id.clone()),
            course_id.clone(),
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
                panic!("governance window has not elapsed");
            }
        }

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| panic!("course not found"));

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
            panic!("course review period has not elapsed");
        }

        if course.status != CourseStatus::Pending {
            panic!("course is not pending approval");
        }

        course.status = CourseStatus::Active;
        course.last_updated_ledger = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);
        env.storage().persistent().extend_ttl(
            &DataKey::Course(course_id.clone()),
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        let pending_key = DataKey::InstructorPendingCourseCount(course.instructor.clone());
        let pending_count: u32 = env.storage().instance().get(&pending_key).unwrap_or(0);
        let new_pending_count = pending_count
            .checked_sub(1)
            .unwrap_or_else(|| panic!("pending course count underflow"));
        env.storage()
            .instance()
            .set(&pending_key, &new_pending_count);

        env.events().publish(
            (Symbol::new(&env, "course_approved"), course_id.clone()),
            (course_id, course.instructor, caller, env.ledger().sequence()),
        );
    }

    /// Admin or approved approver rejects a Pending course, transitioning it to Rejected status.
    ///
    /// # Arguments
    /// - `caller`    — must be admin or an approved approver
    /// - `course_id` — the course to reject
    /// - `reason`    — rejection reason (e.g., "CONTENT_POLICY_VIOLATION", "DUPLICATE_COURSE")
    pub fn reject_course(env: Env, caller: Address, course_id: String, reason: String) {
        caller.require_auth();
        Self::require_admin_or_approver(&env, &caller, "reject_course");
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| panic!("course not found"));

        if course.status != CourseStatus::Pending {
            panic!("course is not pending approval");
        }

        course.status = CourseStatus::Rejected;
        course.last_updated_ledger = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);
        env.storage().persistent().extend_ttl(
            &DataKey::Course(course_id.clone()),
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

    /// Instructor or admin pauses a course.
    /// Existing enrollments are unaffected — students can still access content.
    /// New enrollments are blocked until the course is unpaused.
    pub fn pause_course(env: Env, caller: Address, course_id: String) {
        caller.require_auth();

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| panic!("course not found"));

        let is_admin = Self::is_admin(&env, &caller);
        let is_instructor = caller == course.instructor;

        if !is_admin && !is_instructor {
            panic!("unauthorized");
        }

        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        if course.status != CourseStatus::Active {
            panic!("course is not active");
        }

        course.status = CourseStatus::Paused;
        course.last_updated_ledger = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);
        env.storage().persistent().extend_ttl(
            &DataKey::Course(course_id.clone()),
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

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

    /// Instructor or admin unpauses a Paused course, restoring it to Active.
    pub fn unpause_course(env: Env, caller: Address, course_id: String) {
        caller.require_auth();

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| panic!("course not found"));

        let is_admin = Self::is_admin(&env, &caller);
        let is_instructor = caller == course.instructor;

        if !is_admin && !is_instructor {
            panic!("unauthorized");
        }

        // freeze_instructor() auto-pauses the instructor's courses; a frozen
        // instructor must not be able to undo that by unpausing them.
        if !is_admin && Self::is_instructor_frozen_internal(&env, &course.instructor) {
            panic!("instructor is frozen");
        }

        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        if course.status != CourseStatus::Paused {
            panic!("course is not paused");
        }

        course.status = CourseStatus::Active;
        course.last_updated_ledger = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);
        env.storage().persistent().extend_ttl(
            &DataKey::Course(course_id.clone()),
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

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

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| panic!("course not found"));

        if instructor != course.instructor {
            panic!("unauthorized");
        }

        Self::require_admin(&env, &admin, "transfer_course");
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        if course.status == CourseStatus::Archived {
            panic!("cannot transfer archived course");
        }

        if new_instructor == course.instructor {
            panic!("new instructor must differ from current instructor");
        }

        if Self::is_instructor_frozen_internal(&env, &instructor) {
            panic!("instructor is frozen");
        }

        if Self::is_instructor_frozen_internal(&env, &new_instructor) {
            panic!("instructor is frozen");
        }

        let max_courses: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MaxCoursesPerInstructor)
            .unwrap_or(50);

        let new_count_key = DataKey::InstructorCourseCount(new_instructor.clone());
        let new_count: u32 = env.storage().instance().get(&new_count_key).unwrap_or(0);
        if new_count >= max_courses {
            panic!("instructor has reached the maximum number of course registrations");
        }

        if course.status == CourseStatus::Pending {
            let new_pending_key = DataKey::InstructorPendingCourseCount(new_instructor.clone());
            let new_pending: u32 = env.storage().instance().get(&new_pending_key).unwrap_or(0);
            if new_pending >= max_courses {
                panic!("instructor has reached the maximum number of pending course registrations");
            }
        }

        let previous_instructor = course.instructor.clone();

        course.instructor = new_instructor.clone();
        course.last_updated_ledger = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);
        env.storage().persistent().extend_ttl(
            &DataKey::Course(course_id.clone()),
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        // Keep CourseInstructorRef in sync so enroll() continues to succeed
        // after ownership changes. Enrollment records themselves are untouched.
        env.storage().persistent().set(
            &DataKey::CourseInstructorRef(course_id.clone()),
            &new_instructor,
        );
        env.storage().persistent().extend_ttl(
            &DataKey::CourseInstructorRef(course_id.clone()),
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        let old_count_key = DataKey::InstructorCourseCount(previous_instructor.clone());
        let old_count: u32 = env.storage().instance().get(&old_count_key).unwrap_or(0);
        let decremented_old = old_count
            .checked_sub(1)
            .unwrap_or_else(|| panic!("instructor course count underflow"));
        env.storage()
            .instance()
            .set(&old_count_key, &decremented_old);
        env.storage()
            .instance()
            .set(&new_count_key, &(new_count + 1));

        if course.status == CourseStatus::Pending {
            let old_pending_key =
                DataKey::InstructorPendingCourseCount(previous_instructor.clone());
            let old_pending: u32 = env.storage().instance().get(&old_pending_key).unwrap_or(0);
            let decremented_pending = old_pending
                .checked_sub(1)
                .unwrap_or_else(|| panic!("pending course count underflow"));
            env.storage()
                .instance()
                .set(&old_pending_key, &decremented_pending);

            let new_pending_key = DataKey::InstructorPendingCourseCount(new_instructor.clone());
            let new_pending: u32 = env.storage().instance().get(&new_pending_key).unwrap_or(0);
            env.storage()
                .instance()
                .set(&new_pending_key, &(new_pending + 1));
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
        env.storage()
            .persistent()
            .set(&new_list_key, &new_instructor_courses);
        env.storage().persistent().extend_ttl(
            &new_list_key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

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

    /// Admin archives a course permanently.
    /// Only admin can archive — this is a moderation action.
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
            .unwrap_or_else(|| panic!("course not found"));

        if course.status != CourseStatus::Paused {
            panic!("course must be paused before archiving");
        }

        let mut refund_count = 0u32;
        let mut total_refunded = 0i128;

        if let Some(ref students) = students_to_refund {
            let token_client = token::Client::new(&env, &course.token);

            let treasury: Address = env
                .storage()
                .instance()
                .get(&DataKey::Treasury)
                .unwrap_or_else(|| panic!("treasury not set"));

            for student in students.iter() {
                let enrollment_key = DataKey::Enrollment(student.clone(), course_id.clone());
                if env.storage().persistent().has(&enrollment_key) {
                    let enrollment: Enrollment =
                        env.storage().persistent().get(&enrollment_key).unwrap();

                    if !enrollment.completed && !enrollment.is_refunded {
                        // Use the split actually applied at enroll()/re_enroll()
                        // time (persisted on the enrollment record) rather than
                        // recomputing it from course.platform_fee_percent, which
                        // may have diverged from the FeeConfig (and any risk
                        // surcharge) that deduct_fee() actually applied.
                        let platform_amount = enrollment.platform_amount;
                        let instructor_amount = enrollment.instructor_amount;

                        // Refund platform fee from treasury
                        if platform_amount > 0 {
                            token_client.transfer(&treasury, &student, &platform_amount);
                        }

                        // Refund instructor share from contract-held earnings
                        if instructor_amount > 0 {
                            Self::debit_instructor_earnings(
                                &env,
                                &course.instructor,
                                &course.token,
                                instructor_amount,
                            );
                            token_client.transfer(
                                &env.current_contract_address(),
                                &student,
                                &instructor_amount,
                            );
                        }

                        // Record refund and remove the enrollment record so
                        // `is_enrolled` reflects the student is no longer enrolled.
                        // (Preserve no history here; historical archiving is handled
                        // by re_enroll and EnrollmentHistory when needed.)
                        env.storage().persistent().remove(&enrollment_key);

                        refund_count = refund_count
                            .checked_add(1)
                            .unwrap_or_else(|| panic!("refund count overflow"));
                        total_refunded = total_refunded
                            .checked_add(platform_amount + instructor_amount)
                            .unwrap_or_else(|| panic!("total_refunded overflow"));

                        // Decrement active enrollments
                        if course.active_enrollments > 0 {
                            course.active_enrollments -= 1;
                        }

                        // Decrement platform-wide active enrollment counter
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

                        // Promote from waitlist if capacity just opened
                        Self::promote_from_waitlist(&env, &course_id);
                    }
                }
            }
        }

        if course.active_enrollments > 0 {
            panic!("cannot archive course with active enrollments");
        }

        course.status = CourseStatus::Archived;
        course.archived_at_ledger = Some(env.ledger().sequence());
        course.last_updated_ledger = env.ledger().sequence();

        // Store archived course to enforce cooldown period on re-registration
        env.storage()
            .persistent()
            .set(&DataKey::ArchivedCourse(course_id.clone()), &course);

        // Remove the active course record
        env.storage()
            .persistent()
            .remove(&DataKey::Course(course_id.clone()));

        env.events().publish(
            (Symbol::new(&env, "course_archived"), course_id.clone()),
            (course_id.clone(), admin1.clone(), admin2.clone()),
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
    ) -> u32 {
        caller.require_auth();

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| panic!("course not found"));

        let is_admin = Self::is_admin(&env, &caller);
        let is_instructor = caller == course.instructor;

        if !is_admin && !is_instructor {
            panic!("unauthorized");
        }

        if Self::is_instructor_frozen_internal(&env, &course.instructor) {
            panic!("instructor is frozen");
        }

        if course.status == CourseStatus::Archived {
            panic!("cannot update archived course");
        }

        let mut modified = false;

        if let Some(price) = new_price {
            if price < 0 {
                panic!("price cannot be negative");
            }
            if price != 0
                && (price < Self::MIN_COURSE_PRICE_STROOPS
                    || price > Self::MAX_COURSE_PRICE_STROOPS)
            {
                panic!("price is outside the expected USDC precision range (0 for free, or 0.01-100000 USDC in stroops)");
            }
            course.price = price;
            modified = true;
        }

        if let Some(capacity) = new_max_capacity {
            if capacity == Some(0) {
                panic!("max_capacity must be greater than zero (use None for unlimited)");
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
            .unwrap_or_else(|| panic!("course version overflow"));
        course.last_updated_ledger = env.ledger().sequence();

        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);

        env.events().publish(
            (Symbol::new(&env, "course_updated"), course_id.clone()),
            (course_id, course.version, caller),
        );

        course.version
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
            .unwrap_or_else(|| panic!("course not found"));

        let is_admin = Self::is_admin(&env, &caller);
        let is_instructor = caller == course.instructor;

        if !is_admin && !is_instructor {
            panic!("unauthorized");
        }

        if !is_admin && Self::is_instructor_frozen_internal(&env, &course.instructor) {
            panic!("instructor is frozen");
        }

        course.enrollment_expiry_ledgers = expiry_ledgers;
        course.last_updated_ledger = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);

        env.events().publish(
            (
                Symbol::new(&env, "enrollment_expiry_set"),
                course_id.clone(),
            ),
            (course_id, expiry_ledgers),
        );
    }

    /// Instructor or admin sets or clears the course expiry ledger.
    /// If set to a past ledger sequence, the course will be treated as expired
    /// and new enrollments will be rejected via validate_enrollment() and re_enroll().
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
            .unwrap_or_else(|| panic!("course not found"));

        let is_admin = Self::is_admin(&env, &caller);
        let is_instructor = caller == course.instructor;

        if !is_admin && !is_instructor {
            panic!("unauthorized");
        }

        if Self::is_instructor_frozen_internal(&env, &course.instructor) {
            panic!("instructor is frozen");
        }

        if course.status == CourseStatus::Archived {
            panic!("cannot update archived course");
        }

        course.expires_at_ledger = expires_at_ledger;
        course.last_updated_ledger = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);
        env.storage().persistent().extend_ttl(
            &DataKey::Course(course_id.clone()),
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        env.events().publish(
            (Symbol::new(&env, "course_expiry_set"), course_id.clone()),
            (course_id, expires_at_ledger),
        );
    }

    /// Instructor or admin configures the list of prerequisite course IDs
    /// that a student must have completed — with an issued, non-revoked
    /// certificate — before they may enroll in this course.
    ///
    /// Pass an empty list to remove all prerequisites.
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
            .unwrap_or_else(|| panic!("course not found"));

        let is_admin = Self::is_admin(&env, &caller);
        let is_instructor = caller == course.instructor;

        if !is_admin && !is_instructor {
            panic!("unauthorized");
        }

        if !is_admin && Self::is_instructor_frozen_internal(&env, &course.instructor) {
            panic!("instructor is frozen");
        }

        if course.status == CourseStatus::Archived {
            panic!("cannot update archived course");
        }

        for i in 0..prerequisite_course_ids.len() {
            let prereq_id = prerequisite_course_ids.get(i).unwrap();

            if prereq_id == course_id {
                panic!("course cannot be its own prerequisite");
            }

            if !env
                .storage()
                .persistent()
                .has(&DataKey::Course(prereq_id.clone()))
            {
                panic!("prerequisite course not found");
            }
        }

        course.prerequisite_course_ids = prerequisite_course_ids.clone();
        course.last_updated_ledger = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);
        env.storage().persistent().extend_ttl(
            &DataKey::Course(course_id.clone()),
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        env.events().publish(
            (
                Symbol::new(&env, "prerequisite_courses_set"),
                course_id.clone(),
            ),
            (course_id, prerequisite_course_ids),
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
            .unwrap_or_else(|| panic!("course not found"));

        let is_admin = Self::is_admin(&env, &caller);
        let is_instructor = caller == course.instructor;

        if !is_admin && !is_instructor {
            panic!("unauthorized");
        }

        // A frozen instructor may not alter the content commitment, but an
        // admin may still update it as an administrative intervention.
        if !is_admin && Self::is_instructor_frozen_internal(&env, &course.instructor) {
            panic!("instructor is frozen");
        }

        if course.status == CourseStatus::Archived {
            panic!("cannot update content hash of an archived course");
        }

        course.content_hash = content_hash.clone();
        course.last_updated_ledger = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);
        env.storage().persistent().extend_ttl(
            &DataKey::Course(course_id.clone()),
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

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
            .unwrap_or_else(|| panic!("course not found"));

        let is_admin = Self::is_admin(&env, &caller);
        let is_instructor = caller == course.instructor;

        if !is_admin && !is_instructor {
            panic!("unauthorized");
        }

        if Self::is_instructor_frozen_internal(&env, &course.instructor) {
            panic!("instructor is frozen");
        }

        if course.status == CourseStatus::Archived {
            panic!("cannot update max_certificates of an archived course");
        }

        // Validate: if setting a limit, ensure it's at least as high as current issuance
        if let Some(limit) = max_certs {
            if limit < course.certificates_issued {
                panic!("max_certificates cannot be less than current certificates_issued");
            }
        }

        course.max_certificates = max_certs;
        course.last_updated_ledger = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);
        env.storage().persistent().extend_ttl(
            &DataKey::Course(course_id.clone()),
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        env.events().publish(
            (Symbol::new(&env, "max_certificates_set"), course_id.clone()),
            (course_id, caller, max_certs),
        );
    }

    // ----------------------------------------------------------
    // ENROLLMENT & PAYMENT
    // ----------------------------------------------------------

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
    pub fn enroll(env: Env, student: Address, course_id: String) {
        student.require_auth();
        Self::enroll_internal(&env, &student, &course_id);
    }

    /// Enroll a student in multiple courses atomically.
    /// The entire batch succeeds or the entire batch fails — no partial state.
    ///
    /// # Arguments
    /// - `student`    — student's Stellar address (must sign)
    /// - `course_ids` — list of course IDs to enroll in
    pub fn batch_enroll(env: Env, student: Address, course_ids: Vec<String>) {
        student.require_auth();

        if course_ids.is_empty() {
            panic!("course list cannot be empty");
        }

        // Reject duplicate course IDs within the batch
        for i in 0..course_ids.len() {
            for j in (i + 1)..course_ids.len() {
                if course_ids.get(i).unwrap() == course_ids.get(j).unwrap() {
                    panic!("duplicate course in batch");
                }
            }
        }

        // Validate every course before any mutation
        for i in 0..course_ids.len() {
            let course_id = course_ids.get(i).unwrap();
            Self::validate_enrollment(&env, &student, &course_id);
        }

        // All validations passed — enroll atomically
        for i in 0..course_ids.len() {
            let course_id = course_ids.get(i).unwrap();
            Self::enroll_internal(&env, &student, &course_id);
        }
    }

    fn validate_enrollment(env: &Env, student: &Address, course_id: &String) {
        if env
            .storage()
            .instance()
            .get(&DataKey::PlatformPaused)
            .unwrap_or(false)
        {
            panic!("platform is paused");
        }

        let course =
            Self::get_course_internal(env, course_id).unwrap_or_else(|| panic!("course not found"));

        if Self::is_student_blocked_internal(env, student) {
            panic!("student is blocked");
        }

        if Self::is_instructor_frozen_internal(env, &course.instructor) {
            panic!("instructor is frozen");
        }

        if Self::is_admin(env, student) {
            panic!("admin cannot enroll in courses");
        }

        let registered_instructor: Address = env
            .storage()
            .persistent()
            .get(&DataKey::CourseInstructorRef(course_id.clone()))
            .unwrap_or_else(|| panic!("course instructor reference not found"));

        if registered_instructor != course.instructor {
            panic!("course instructor reference mismatch");
        }

        if *student == course.instructor {
            panic!("instructor cannot enroll in own course");
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
            panic!("course is not available for enrollment");
        }

        if env.ledger().sequence() <= course.created_at_ledger {
            panic!("cannot enroll in the same ledger the course was registered");
        }

        if let Some(enrollment_start_ledger) = course.enrollment_start_ledger {
            if env.ledger().sequence() < enrollment_start_ledger {
                panic!("enrollment has not started for this course");
            }
        }

        if env
            .storage()
            .persistent()
            .has(&DataKey::Enrollment(student.clone(), course_id.clone()))
        {
            panic!("already enrolled in this course");
        }
        if let Some(cap) = course.max_capacity {
            if course.total_enrollments >= cap {
                panic!("course has reached maximum enrollment capacity");
            }
        }

        if let Some(expiry) = course.expires_at_ledger {
            if env.ledger().sequence() >= expiry {
                panic!("course has expired");
            }
        }

        if !env
            .storage()
            .instance()
            .has(&DataKey::ApprovedToken(course.token.clone()))
        {
            panic!("course token is not approved");
        }

        // Validate token decimal precision at enrollment time to ensure fee calculations are accurate
        let token_client = token::Client::new(env, &course.token);
        let token_decimals = token_client.decimals();
        const EXPECTED_TOKEN_DECIMALS: u32 = 7;
        if token_decimals != EXPECTED_TOKEN_DECIMALS {
            panic!("token decimal precision must be {} (found {})", EXPECTED_TOKEN_DECIMALS, token_decimals);
        }

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
                panic!("prerequisite course not completed");
            }
        }

        // Check platform-wide enrollment cap
        if let Some(platform_cap) = env
            .storage()
            .instance()
            .get::<DataKey, u32>(&DataKey::PlatformEnrollmentCap)
        {
            let total_active: u32 = env
                .storage()
                .instance()
                .get(&DataKey::TotalActiveEnrollments)
                .unwrap_or(0);
            if total_active >= platform_cap {
                panic!("platform has reached maximum total enrollment capacity");
            }
        }
    }

    fn enroll_internal(env: &Env, student: &Address, course_id: &String) {
        Self::validate_enrollment(env, student, course_id);

        let mut course =
            Self::get_course_internal(env, course_id).unwrap_or_else(|| panic!("course not found"));
        let token_client = token::Client::new(env, &course.token);

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
            panic!("course is not available for enrollment");
        }

        // Use the centralized fee deduction function that supports:
        // 1. Per-token fee configuration (map token → FeeConfig)
        // 2. Risk-based surcharges for large payments, new customers, and BTC/ETH
        // 3. Publishes RiskFeeApplied event when surcharge applies
        // Risk flags come from the same helper the fee previews use, so a
        // quote from get_effective_fee_for_payment() matches this charge.
        let (is_new_customer, is_btc_eth) = Self::resolve_risk_flags(env, student, &course.token);
        let (instructor_amount, platform_amount) = Self::deduct_fee(
            env,
            &course.token,
            course.price,
            is_new_customer,
            is_btc_eth,
        );

        // Fetch treasury, applying any pending treasury update if effective
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

        // Transfer full price from student to contract, then distribute platform fee
        let actual_amount_paid = if course.price > 0 {
            // Verify actual transfer amount by checking balance before and after
            let balance_before = token_client.balance(&env.current_contract_address());
            token_client.transfer(student, &env.current_contract_address(), &course.price);
            let balance_after = token_client.balance(&env.current_contract_address());
            
            let actual_received = balance_after
                .checked_sub(balance_before)
                .unwrap_or_else(|| panic!("balance overflow during transfer verification"));
            
            if actual_received != course.price {
                panic!("token transfer amount mismatch: expected {}, received {}", course.price, actual_received);
            }

            if platform_amount > 0 {
                token_client.transfer(&env.current_contract_address(), &treasury, &platform_amount);
                env.events().publish(
                    (
                        Symbol::new(&env, "platform_fee_transferred"),
                        course_id.clone(),
                    ),
                    (treasury.clone(), platform_amount, env.ledger().sequence()),
                );
            }

            // Credit instructor earnings — pull-based withdrawal model
            if instructor_amount > 0 {
                Self::credit_instructor_earnings(
                    env,
                    &course.instructor,
                    &course.token,
                    instructor_amount,
                );
                env.events().publish(
                    (
                        Symbol::new(&env, "instructor_payment_transferred"),
                        course_id.clone(),
                    ),
                    (
                        course.instructor.clone(),
                        instructor_amount,
                        env.ledger().sequence(),
                    ),
                );
            }
            
            actual_received
        } else {
            0
        };

        // Record enrollment
        let enrollment_ref = Self::generate_enrollment_ref(&env, &student, &course_id);
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
            platform_amount,
            instructor_amount,
            enrollment_ref: enrollment_ref.clone(),
        };

        env.storage().persistent().set(
            &DataKey::Enrollment(student.clone(), course_id.clone()),
            &enrollment,
        );

        env.storage().persistent().extend_ttl(
            &DataKey::Enrollment(student.clone(), course_id.clone()),
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        // Update course stats
        course.total_enrollments = course
            .total_enrollments
            .checked_add(1)
            .unwrap_or_else(|| panic!("enrollment count overflow"));
        course.active_enrollments = course
            .active_enrollments
            .checked_add(1)
            .unwrap_or_else(|| panic!("active enrollment count overflow"));
        course.total_earned = course
            .total_earned
            .checked_add(course.price)
            .unwrap_or_else(|| panic!("total earned overflow"));
        course.last_updated_ledger = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);

        // Increment platform-wide active enrollment counter
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

        Self::update_instructor_stats(env, &course.instructor, |s| {
            s.total_students = s
                .total_students
                .checked_add(1)
                .unwrap_or_else(|| panic!("instructor stats overflow"));
        });

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
            ),
        );
    }

    /// Student re-enrolls in a course they have already completed.
    ///
    /// Unlike `enroll()`, this is allowed even though a completed
    /// `Enrollment` record already exists for this (student, course_id)
    /// pair. The prior completed record — including its evidence hash and
    /// certificate linkage — is archived to `EnrollmentHistory` before a
    /// fresh `Enrollment` is created, so nothing about the original
    /// completion or any certificate already issued for it is overwritten.
    /// The student is charged again, exactly as for a first-time
    /// enrollment.
    ///
    /// # Arguments
    /// - `student`   — student's Stellar address (must sign)
    /// - `course_id` — the course to re-enroll in
    pub fn re_enroll(env: Env, student: Address, course_id: String) {
        student.require_auth();

        if env
            .storage()
            .instance()
            .get(&DataKey::PlatformPaused)
            .unwrap_or(false)
        {
            panic!("platform is paused");
        }

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| panic!("course not found"));

        if Self::is_student_blocked_internal(&env, &student) {
            panic!("student is blocked");
        }

        if Self::is_instructor_frozen_internal(&env, &course.instructor) {
            panic!("instructor is frozen");
        }

        if student == course.instructor {
            panic!("instructor cannot enroll in own course");
        }

        if course.status != CourseStatus::Active {
            panic!("course is not available for enrollment");
        }

        if !env
            .storage()
            .instance()
            .has(&DataKey::ApprovedToken(course.token.clone()))
        {
            panic!("course token is not approved");
        }

        let enrollment_key = DataKey::Enrollment(student.clone(), course_id.clone());
        let previous_enrollment: Enrollment = env
            .storage()
            .persistent()
            .get(&enrollment_key)
            .unwrap_or_else(|| panic!("no prior enrollment found for this course"));

        if !previous_enrollment.completed {
            panic!("current enrollment has not been completed yet");
        }

        if let Some(cap) = course.max_capacity {
            if course.total_enrollments >= cap {
                panic!("course has reached maximum enrollment capacity");
            }
        }

        if let Some(expiry) = course.expires_at_ledger {
            if env.ledger().sequence() >= expiry {
                panic!("course has expired");
            }
        }

        // Archive the completed enrollment — including its evidence hash
        // and certificate_id linkage — before it is overwritten.
        let history_key = DataKey::EnrollmentHistory(student.clone(), course_id.clone());
        let mut history: Vec<Enrollment> = env
            .storage()
            .persistent()
            .get(&history_key)
            .unwrap_or_else(|| Vec::new(&env));
        history.push_back(previous_enrollment);
        env.storage().persistent().set(&history_key, &history);
        env.storage().persistent().extend_ttl(
            &history_key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        let token_client = token::Client::new(&env, &course.token);

        // Use the centralized fee deduction function that supports:
        // 1. Per-token fee configuration (map token → FeeConfig)
        // 2. Risk-based surcharges for large payments, new customers, and BTC/ETH
        // 3. Publishes RiskFeeApplied event when surcharge applies
        let (is_new_customer, is_btc_eth) = Self::resolve_risk_flags(&env, &student, &course.token);
        let (instructor_amount, platform_amount) = Self::deduct_fee(
            &env,
            &course.token,
            course.price,
            is_new_customer,
            is_btc_eth,
        );

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

        if course.price > 0 {
            // Verify actual transfer amount by checking balance before and after
            let balance_before = token_client.balance(&env.current_contract_address());
            token_client.transfer(&student, &env.current_contract_address(), &course.price);
            let balance_after = token_client.balance(&env.current_contract_address());
            
            let actual_received = balance_after
                .checked_sub(balance_before)
                .unwrap_or_else(|| panic!("balance overflow during transfer verification"));
            
            if actual_received != course.price {
                panic!("token transfer amount mismatch: expected {}, received {}", course.price, actual_received);
            }

            if platform_amount > 0 {
                token_client.transfer(&env.current_contract_address(), &treasury, &platform_amount);
            }

            if instructor_amount > 0 {
                Self::credit_instructor_earnings(
                    &env,
                    &course.instructor,
                    &course.token,
                    instructor_amount,
                );
            }
        }

        let new_enrollment_ref = Self::generate_enrollment_ref(&env, &student, &course_id);
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
            platform_amount,
            instructor_amount,
            enrollment_ref: new_enrollment_ref,
        };

        env.storage()
            .persistent()
            .set(&enrollment_key, &new_enrollment);
        env.storage().persistent().extend_ttl(
            &enrollment_key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        course.total_enrollments = course
            .total_enrollments
            .checked_add(1)
            .unwrap_or_else(|| panic!("enrollment count overflow"));
        course.active_enrollments = course
            .active_enrollments
            .checked_add(1)
            .unwrap_or_else(|| panic!("active enrollment count overflow"));
        course.total_earned = course
            .total_earned
            .checked_add(course.price)
            .unwrap_or_else(|| panic!("total earned overflow"));
        course.last_updated_ledger = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);

        // Increment platform-wide active enrollment counter
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

        Self::update_instructor_stats(&env, &course.instructor, |s| {
            s.total_students = s
                .total_students
                .checked_add(1)
                .unwrap_or_else(|| panic!("instructor stats overflow"));
        });

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

    /// Instructor withdraws accumulated earnings for a given token.
    /// Pass `amount = 0` to withdraw the full available balance.
    pub fn withdraw_earnings(env: Env, instructor: Address, token: Address, amount: i128) {
        instructor.require_auth();

        if amount < 0 {
            panic!("withdrawal amount cannot be negative");
        }

        let earnings_key = DataKey::InstructorEarnings(instructor.clone(), token.clone());
        let balance: i128 = env.storage().persistent().get(&earnings_key).unwrap_or(0);

        let withdraw_amount = if amount == 0 { balance } else { amount };

        if withdraw_amount == 0 {
            return;
        }

        if withdraw_amount > balance {
            panic!("insufficient earnings balance");
        }

        let new_balance = balance
            .checked_sub(withdraw_amount)
            .unwrap_or_else(|| panic!("overflow computing new balance"));

        if new_balance == 0 {
            env.storage().persistent().remove(&earnings_key);
        } else {
            env.storage().persistent().set(&earnings_key, &new_balance);
        }

        let token_client = token::Client::new(&env, &token);
        token_client.transfer(
            &env.current_contract_address(),
            &instructor,
            &withdraw_amount,
        );

        env.events().publish(
            (Symbol::new(&env, "earnings_withdrawn"), instructor.clone()),
            (token, withdraw_amount),
        );
    }

    /// Get accumulated earnings for an instructor and token pair
    pub fn get_instructor_earnings(env: Env, instructor: Address, token: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::InstructorEarnings(instructor, token))
            .unwrap_or(0)
    }

    /// Get accumulated earnings for an instructor across all tokens
    pub fn get_instructor_total_earnings(env: Env, instructor: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::InstructorTotalEarnings(instructor))
            .unwrap_or(0)
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
    /// - `evidence_hash` — non-empty proof of completion; when `None`, the
    ///   student must co-sign instead
    pub fn mark_completed(
        env: Env,
        admin: Address,
        student: Address,
        course_id: String,
        evidence_hash: Option<String>,
    ) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "mark_completed");
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        // Supplying evidence waives the student co-signature, so the evidence
        // must be real — an empty string would bypass it with no evidence.
        match &evidence_hash {
            Some(hash) if hash.len() == 0 => panic!("evidence_hash cannot be empty"),
            Some(_) => {}
            None => student.require_auth(),
        }

        // Check if student is blocked
        if Self::is_student_blocked_internal(&env, &student) {
            panic!("student is blocked and cannot proceed");
        }

        let course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| panic!("course not found"));

        // Validate course is in a valid state for completion (Active or Paused, not Archived)
        if course.status == CourseStatus::Archived {
            panic!("cannot mark completion for archived course");
        }

        let mut enrollment = Self::get_enrollment_internal(&env, &student, &course_id);

        if enrollment.is_refunded {
            panic!("cannot mark refunded enrollment as completed");
        }

        let elapsed = env
            .ledger()
            .sequence()
            .checked_sub(enrollment.enrolled_at_ledger)
            .unwrap_or(0);
        if elapsed < course.min_completion_ledgers {
            panic!("minimum enrollment duration has not elapsed");
        }

        if enrollment.course_id != course_id {
            panic!("enrollment course_id mismatch");
        }

        if enrollment.completed {
            panic!("already marked as completed");
        }

        // Check enrollment expiry — expired enrollments cannot be completed
        if let Some(expiry_ledgers) = course.enrollment_expiry_ledgers {
            let expiry_at = enrollment
                .enrolled_at_ledger
                .checked_add(expiry_ledgers)
                .unwrap_or(u32::MAX);
            if env.ledger().sequence() >= expiry_at {
                panic!("enrollment has expired");
            }
        }

        enrollment.completed = true;
        enrollment.evidence_hash = evidence_hash;

        env.storage().persistent().set(
            &DataKey::Enrollment(student.clone(), course_id.clone()),
            &enrollment,
        );
        env.storage().persistent().extend_ttl(
            &DataKey::Enrollment(student.clone(), course_id.clone()),
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        // Update active enrollments count on course
        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| panic!("course not found"));
        if course.active_enrollments > 0 {
            course.active_enrollments -= 1;
            course.last_updated_ledger = env.ledger().sequence();
            env.storage()
                .persistent()
                .set(&DataKey::Course(course_id.clone()), &course);
        }

        // Decrement platform-wide active enrollment counter
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

        Self::update_instructor_stats(&env, &course.instructor, |s| {
            s.total_completions = s
                .total_completions
                .checked_add(1)
                .unwrap_or_else(|| panic!("instructor stats overflow"));
        });

        let completion_ledger = env.ledger().sequence();
        env.events().publish(
            (Symbol::new(&env, "course_completed"), course_id.clone()),
            (student, admin, completion_ledger),
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
    /// - `expires_at_ledger`    — optional expiry ledger
    /// - `instructor_signature` — optional Ed25519 signature
    pub fn issue_certificate(
        env: Env,
        admin: Address,
        student: Address,
        course_id: String,
        certificate_id: String,
        course_title: String,
        enrollment_reference: String,
        expires_at_ledger: Option<u32>,
        instructor_signature: Option<BytesN<64>>,
    ) -> String {
        admin.require_auth();
        Self::require_admin(&env, &admin, "issue_certificate");
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        if certificate_id.len() == 0 {
            panic!("certificate_id cannot be empty");
        }
        if certificate_id.len() > Self::MAX_COURSE_ID_LEN {
            panic!("certificate_id exceeds maximum length");
        }
        if course_title.len() > Self::MAX_COURSE_TITLE_LEN {
            panic!("course_title exceeds maximum length");
        }
        if enrollment_reference.len() == 0 {
            panic!("enrollment_reference cannot be empty");
        }
        if enrollment_reference.len() > Self::MAX_COURSE_ID_LEN {
            panic!("enrollment_reference exceeds maximum length");
        }

        // Check if student is blocked
        if Self::is_student_blocked_internal(&env, &student) {
            panic!("student is blocked and cannot receive certificates");
        }

        // Student must have completed the course — enrollment is looked up
        // from the authoritative enrollment record for (student, course_id).
        let mut enrollment = Self::get_enrollment_internal(&env, &student, &course_id);
        if enrollment.course_id != course_id {
            panic!("enrollment course_id mismatch");
        }
        if enrollment.is_refunded {
            panic!("cannot issue certificate for refunded enrollment");
        }
        if !enrollment.completed {
            panic!("student has not completed this course");
        }

        if enrollment.certificate_issued {
            panic!("certificate already issued for this enrollment");
        }

        // Certificate ID must be unique
        if env
            .storage()
            .persistent()
            .has(&DataKey::Certificate(certificate_id.clone()))
        {
            panic!("certificate ID already exists");
        }

        let mut course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| panic!("course not found"));

        // Check per-course certificate limit
        if let Some(max_certs) = course.max_certificates {
            if course.certificates_issued >= max_certs {
                panic!("course has reached maximum certificate issuance limit");
            }
        }

        let issued_at_ledger = env.ledger().sequence();
        let certificate = Certificate {
            id: certificate_id.clone(),
            student: enrollment.student.clone(),
            course_id: course_id.clone(),
            course_title,
            enrollment_reference: enrollment_reference.clone(),
            instructor: course.instructor.clone(),
            issued_by: env.current_contract_address(),
            issued_at_ledger,
            revoked: false,
            revoked_by: None,
            revocation_ledger: None,
            revocation_reason: None,
            expires_at_ledger,
            instructor_signature,
            revocation_deadline: None,
        };

        env.storage()
            .persistent()
            .set(&DataKey::Certificate(certificate_id.clone()), &certificate);

        env.storage().persistent().extend_ttl(
            &DataKey::Certificate(certificate_id.clone()),
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        // Mark enrollment as certificate issued
        enrollment.certificate_issued = true;
        enrollment.certificate_id = Some(certificate_id.clone());
        env.storage().persistent().set(
            &DataKey::Enrollment(student.clone(), course_id.clone()),
            &enrollment,
        );

        // Increment certificate count for course
        course.certificates_issued = course
            .certificates_issued
            .checked_add(1)
            .unwrap_or_else(|| panic!("certificate count overflow"));
        course.last_updated_ledger = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);

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
        env.storage()
            .persistent()
            .set(&course_certs_key, &course_certs);
        env.storage().persistent().extend_ttl(
            &course_certs_key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        Self::update_instructor_stats(&env, &certificate.instructor, |s| {
            s.total_certificates = s
                .total_certificates
                .checked_add(1)
                .unwrap_or_else(|| panic!("instructor stats overflow"));
        });

        // The certificate ID is indexed in the topics for efficient filtering;
        // the payload carries the issuance details and audit actor.
        env.events().publish(
            (
                Symbol::new(&env, "certificate_issued"),
                certificate_id.clone(),
            ),
            (student, course_id, admin, issued_at_ledger),
        );

        certificate_id
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
    /// carries the fact that a certificate was issued for it. Because
    /// `issue_certificate` panics whenever `certificate_issued` is already
    /// `true`, a revoked certificate can never be replaced by re-issuing a
    /// fresh certificate against the same enrollment. Certifying the student
    /// again requires a new enrollment (see `re_enroll`), which starts from
    /// a clean `Enrollment` record.
    ///
    /// # Arguments
    /// - `admin`          — must match stored admin
    /// - `certificate_id` — the certificate to revoke
    /// - `reason`         — short reason code (e.g. "ACADEMIC_DISHONESTY", "ISSUED_IN_ERROR")
    pub fn revoke_certificate(env: Env, admin: Address, certificate_id: String, reason: String) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "revoke_certificate");
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
            .unwrap_or_else(|| panic!("certificate not found"));
        assert!(!cert.revoked, "certificate is already revoked");

        Self::apply_revocation(&env, &admin, &certificate_id, &reason);
    }

    /// Admin revokes every certificate issued for a single course in one
    /// atomic transaction.
    ///
    /// Used when a course as a whole is found to be fraudulent or its content
    /// invalid: instead of calling `revoke_certificate()` once per graduate —
    /// operationally infeasible for courses with hundreds of students — the
    /// admin revokes the entire cohort at once.
    ///
    /// Each certificate receives exactly the same pending-revocation semantics
    /// as `revoke_certificate()`: the holder keeps the standard challenge
    /// period and may call `challenge_revocation()` to dispute it. Certificates
    /// that are already revoked are skipped, so the call is safe to retry and
    /// a partially-revoked course can still be completed later.
    ///
    /// # Arguments
    /// - `admin`     — must match stored admin
    /// - `course_id` — the course whose certificates should all be revoked
    ///
    /// # Returns
    /// The number of certificates newly marked for revocation.
    pub fn bulk_revoke_course_certificates(
        env: Env,
        admin: Address,
        course_id: String,
    ) -> u32 {
        admin.require_auth();
        Self::require_admin(&env, &admin, "bulk_revoke_course_certificates");
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        if Self::get_course_internal(&env, &course_id).is_none() {
            panic!("course not found");
        }

        let certificate_ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::CourseCertificates(course_id.clone()))
            .unwrap_or_else(|| Vec::new(&env));

        if certificate_ids.is_empty() {
            panic!("no certificates issued for this course");
        }

        // One fixed reason code keeps the bulk path auditable and lets
        // off-chain indexers tell bulk revocations apart from targeted ones.
        let reason = String::from_str(&env, "BULK_COURSE_REVOCATION");
        let mut revoked_count: u32 = 0;

        for i in 0..certificate_ids.len() {
            let certificate_id = certificate_ids.get(i).unwrap();
            if Self::apply_revocation(&env, &admin, &certificate_id, &reason) {
                revoked_count = revoked_count
                    .checked_add(1)
                    .unwrap_or_else(|| panic!("revoked certificate count overflow"));
            }
        }

        env.events().publish(
            (
                Symbol::new(&env, "course_certificates_revoked"),
                course_id.clone(),
            ),
            (
                admin,
                course_id,
                revoked_count,
                env.ledger().sequence(),
            ),
        );

        revoked_count
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
            .unwrap_or_else(|| panic!("certificate not found"));

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
            .unwrap_or_else(|| panic!("revocation deadline overflow"));

        cert.revoked = true;
        cert.revoked_by = Some(admin.clone());
        cert.revocation_ledger = Some(current_ledger);
        cert.revocation_reason = Some(reason.clone());
        cert.revocation_deadline = Some(deadline);

        env.storage().persistent().set(&certificate_key, &cert);

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
            .unwrap_or_else(|| panic!("certificate not found"));

        // Only the certificate holder may challenge
        if cert.student != student {
            panic!("only the certificate holder may challenge a revocation");
        }

        // A revocation must be pending (revoked == true with a deadline)
        if !cert.revoked {
            panic!("certificate has no pending revocation to challenge");
        }

        let deadline = cert
            .revocation_deadline
            .unwrap_or_else(|| panic!("certificate has no pending revocation to challenge"));

        let current_ledger = env.ledger().sequence();

        // Challenge must be submitted before the deadline
        if current_ledger >= deadline {
            panic!("revocation challenge period has expired");
        }

        // Cancel the pending revocation
        cert.revoked = false;
        cert.revoked_by = None;
        cert.revocation_ledger = None;
        cert.revocation_reason = None;
        cert.revocation_deadline = None;

        env.storage()
            .persistent()
            .set(&DataKey::Certificate(certificate_id.clone()), &cert);

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
        env.storage()
            .instance()
            .set(&DataKey::PlatformPaused, &true);

        env.events()
            .publish((Symbol::new(&env, "platform_paused"), admin.clone()), admin);
    }

    pub fn unpause_platform(env: Env, admin: Address) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "unpause_platform");
        env.storage()
            .instance()
            .set(&DataKey::PlatformPaused, &false);

        env.events().publish(
            (Symbol::new(&env, "platform_unpaused"), admin.clone()),
            admin,
        );
    }

    pub fn withdraw_tokens(
        env: Env,
        admin: Address,
        token: Address,
        amount: i128,
        destination: Address,
    ) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "withdraw_tokens");
        let token_client = token::Client::new(&env, &token);
        token_client.transfer(&env.current_contract_address(), &destination, &amount);

        env.events().publish(
            (Symbol::new(&env, "tokens_withdrawn"), admin.clone()),
            (admin, token, amount, destination),
        );
    }

    /// Propose a new admin address (step 1 of two-step transfer).
    /// The new admin must call accept_admin() to complete the handover.
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
            panic!("proposed admin addresses are identical to current admin addresses");
        }

        if new_admin == new_secondary_admin {
            panic!("admin and secondary_admin must be distinct addresses");
        }

        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);
        env.storage()
            .instance()
            .set(&DataKey::PendingAdmin, &new_admin);
        env.storage()
            .instance()
            .set(&DataKey::PendingSecondaryAdmin, &new_secondary_admin);

        env.events().publish(
            (Symbol::new(&env, "admin_proposed"), new_admin.clone()),
            (new_admin, admin1, admin2),
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
            .unwrap_or_else(|| panic!("no pending admin"));

        let pending_sec: Address = env
            .storage()
            .instance()
            .get(&DataKey::PendingSecondaryAdmin)
            .unwrap_or_else(|| panic!("no pending secondary admin"));

        if pending != new_admin || pending_sec != new_secondary_admin {
            panic!("callers are not the pending admins");
        }

        let previous_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic!("admin not set"));

        env.storage().instance().set(&DataKey::Admin, &new_admin);
        env.storage()
            .instance()
            .set(&DataKey::SecondaryAdmin, &new_secondary_admin);
        env.storage().instance().remove(&DataKey::PendingAdmin);
        env.storage()
            .instance()
            .remove(&DataKey::PendingSecondaryAdmin);
        
        // Clear the admin expiry when new admins take over
        // This ensures the new admin pair starts fresh without inheriting
        // the previous admin's expiry time
        env.storage().instance().remove(&DataKey::AdminExpiresAt);

        let ledger_sequence = env.ledger().sequence();

        env.events().publish(
            (Symbol::new(&env, "admin_transferred"), new_admin.clone()),
            (previous_admin, new_admin.clone(), ledger_sequence),
        );
    }

    /// Update the platform treasury address.
    /// Emits `treasury_updated` immediately so the pending change is
    /// auditable in real time, even though it only takes effect 100
    /// ledgers later (see `TreasuryUpdate`).
    pub fn update_treasury(env: Env, admin1: Address, admin2: Address, new_treasury: Address) {
        admin1.require_auth();
        admin2.require_auth();
        Self::require_multi_admin(&env, &admin1, &admin2);

        if new_treasury == env.current_contract_address() {
            panic!("treasury cannot be the contract address");
        }

        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        let secondary_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::SecondaryAdmin)
            .unwrap();

        if new_treasury == admin {
            panic!("treasury cannot be the admin address");
        }

        if new_treasury == secondary_admin {
            panic!("treasury cannot be the secondary_admin address");
        }

        let current_treasury: Address = env
            .storage()
            .instance()
            .get(&DataKey::Treasury)
            .unwrap_or_else(|| panic!("treasury not set"));

        if new_treasury == current_treasury {
            panic!("new treasury address must differ from current treasury");
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
            .unwrap_or_else(|| panic!("treasury not set"));

        let ledger_sequence = env.ledger().sequence();
        let effective_ledger = ledger_sequence + 100;
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

    /// Update the default platform fee percentage.
    pub fn update_default_fee(env: Env, admin: Address, new_fee_pct: u32) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "update_default_fee");
        if new_fee_pct > 100 {
            panic!("fee percentage cannot exceed 100");
        }
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);
        env.storage()
            .instance()
            .set(&DataKey::DefaultFee, &new_fee_pct);

        env.events().publish(
            (Symbol::new(&env, "default_fee_updated"), admin.clone()),
            (admin, new_fee_pct),
        );
    }

    /// Admin adds a token contract address to the enrollment whitelist.
    pub fn add_approved_token(env: Env, admin: Address, token: Address) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "add_approved_token");
        env.storage()
            .instance()
            .set(&DataKey::ApprovedToken(token.clone()), &true);

        env.events().publish(
            (Symbol::new(&env, "token_whitelisted"), admin.clone()),
            (admin, token),
        );
    }

    /// Admin removes a token contract address from the enrollment whitelist.
    pub fn remove_approved_token(env: Env, admin: Address, token: Address) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "remove_approved_token");
        env.storage()
            .instance()
            .remove(&DataKey::ApprovedToken(token.clone()));

        env.events().publish(
            (
                Symbol::new(&env, "token_removed_from_whitelist"),
                admin.clone(),
            ),
            (admin, token),
        );
    }

    /// Admin updates the maximum number of courses an instructor can register.
    pub fn update_max_courses_limit(env: Env, admin: Address, new_max: u32) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "update_max_courses_limit");
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
            (admin, new_max),
        );
    }

    /// Admin freezes/blocks a specific instructor address.
    /// Automatically pauses all of the instructor's currently Active courses
    /// to prevent students from enrolling in or paying for frozen instructor courses.
    pub fn freeze_instructor(env: Env, admin: Address, instructor: Address) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "freeze_instructor");
        env.storage()
            .instance()
            .set(&DataKey::InstructorBlocked(instructor.clone()), &true);

        // Pause all of the instructor's currently Active courses
        if let Some(course_ids) = env
            .storage()
            .persistent()
            .get::<DataKey, Vec<String>>(&DataKey::InstructorCourseList(instructor.clone()))
        {
            for i in 0..course_ids.len() {
                let course_id = course_ids.get(i).unwrap();
                if let Some(mut course) = env
                    .storage()
                    .persistent()
                    .get::<DataKey, Course>(&DataKey::Course(course_id.clone()))
                {
                    if course.status == CourseStatus::Active {
                        course.status = CourseStatus::Paused;
                        course.last_updated_ledger = env.ledger().sequence();
                        env.storage()
                            .persistent()
                            .set(&DataKey::Course(course_id.clone()), &course);
                        env.storage().persistent().extend_ttl(
                            &DataKey::Course(course_id.clone()),
                            Self::PERSISTENT_TTL_THRESHOLD,
                            Self::PERSISTENT_TTL_EXTEND_TO,
                        );

                        env.events().publish(
                            (Symbol::new(&env, "course_paused"), course_id.clone()),
                            (course_id.clone(), Symbol::new(&env, "instructor_frozen")),
                        );
                    }
                }
            }
        }

        env.events().publish(
            (Symbol::new(&env, "instructor_frozen"), instructor.clone()),
            (instructor, admin),
        );
    }

    /// Admin unfreezes/unblocks a specific instructor address.
    pub fn unfreeze_instructor(env: Env, admin: Address, instructor: Address) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "unfreeze_instructor");
        env.storage()
            .instance()
            .remove(&DataKey::InstructorBlocked(instructor.clone()));
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
            panic!("approver cannot be the admin address");
        }

        // Verify the address is not already an approver
        if Self::is_approver(&env, &approver) {
            panic!("address is already an approver");
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
            panic!("address is not an approver");
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

    /// Admin blocks/bans a specific student address from the platform.
    /// Blocked students cannot enroll in any course.
    pub fn block_student(env: Env, admin: Address, student: Address) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "block_student");
        env.storage()
            .instance()
            .set(&DataKey::StudentBlocked(student.clone()), &true);
        env.events().publish(
            (Symbol::new(&env, "student_blocked"), student.clone()),
            (student, admin),
        );
    }

    /// Admin unblocks a previously blocked student address.
    pub fn unblock_student(env: Env, admin: Address, student: Address) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "unblock_student");
        env.storage()
            .instance()
            .remove(&DataKey::StudentBlocked(student.clone()));
        env.events().publish(
            (Symbol::new(&env, "student_unblocked"), student.clone()),
            (student, admin),
        );
    }

    /// Check if a student is blocked/banned from the platform
    pub fn is_student_blocked(env: Env, student: Address) -> bool {
        Self::is_student_blocked_internal(&env, &student)
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

        if let Some(expiry) = expires_at {
            if expiry <= env.ledger().sequence() {
                panic!("expiry must be in the future");
            }
            env.storage()
                .instance()
                .set(&DataKey::AdminExpiresAt, &expiry);
        } else {
            env.storage().instance().remove(&DataKey::AdminExpiresAt);
        }

        env.events().publish(
            (Symbol::new(&env, "admin_expiry_set"), admin1.clone()),
            (admin1, admin2, expires_at),
        );
    }

    /// Get the admin role expiry ledger sequence, if set.
    pub fn get_admin_expiry(env: Env) -> Option<u32> {
        env.storage().instance().get(&DataKey::AdminExpiresAt)
    }

    /// Set the platform-wide maximum total active enrollment cap.
    /// Pass `None` to remove the cap (unlimited enrollments).
    pub fn set_platform_enrollment_cap(env: Env, admin: Address, cap: Option<u32>) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "set_platform_enrollment_cap");

        if let Some(cap_value) = cap {
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
            (admin, cap),
        );
    }

    /// Get the platform-wide maximum total active enrollment cap, if set.
    pub fn get_platform_enrollment_cap(env: Env) -> Option<u32> {
        env.storage()
            .instance()
            .get(&DataKey::PlatformEnrollmentCap)
    }

    /// Get the current total active enrollments platform-wide.
    pub fn get_total_active_enrollments(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::TotalActiveEnrollments)
            .unwrap_or(0)
    }

    /// Update the minimum review delay (in ledger sequences)
    pub fn update_min_review_delay(env: Env, admin: Address, delay: u32) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "update_min_review_delay");
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);
        env.storage()
            .instance()
            .set(&DataKey::MinReviewDelay, &delay);

        env.events().publish(
            (Symbol::new(&env, "min_review_delay_updated"), admin.clone()),
            (admin, delay),
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
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);
        env.storage()
            .instance()
            .set(&DataKey::DefaultMinCompletionLedgers, &ledgers);

        env.events().publish(
            (
                Symbol::new(&env, "min_completion_ledgers_updated"),
                admin.clone(),
            ),
            (admin, ledgers),
        );
    }

    /// Get the default minimum enrollment duration (in ledger sequences)
    pub fn get_min_completion_ledgers(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::DefaultMinCompletionLedgers)
            .unwrap_or(0)
    }

    /// Update the refund window (in ledger sequences)
    pub fn update_refund_window(env: Env, admin: Address, window: u32) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "update_refund_window");
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);
        env.storage()
            .instance()
            .set(&DataKey::RefundWindow, &window);

        env.events().publish(
            (Symbol::new(&env, "refund_window_updated"), admin.clone()),
            (admin, window),
        );
    }

    /// Get the refund window (in ledger sequences)
    pub fn get_refund_window(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::RefundWindow)
            .unwrap_or(1000)
    }

    /// Update the revocation challenge period (in ledger sequences).
    /// This is the window during which a certificate holder may dispute
    /// a pending revocation after `revoke_certificate` is called.
    pub fn set_revocation_challenge_period(env: Env, admin: Address, period: u32) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "set_revocation_challenge_period");
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

    /// Request a refund for an enrollment within the refund window
    pub fn request_refund(env: Env, student: Address, course_id: String) {
        student.require_auth();
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        // Check if student is blocked
        if Self::is_student_blocked_internal(&env, &student) {
            panic!("student is blocked and cannot request refunds");
        }

        let enrollment = Self::get_enrollment_internal(&env, &student, &course_id);

        if enrollment.completed {
            panic!("already marked as completed");
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

        if elapsed > refund_window {
            panic!("refund window has expired");
        }

        let key = DataKey::RefundRequest(student.clone(), course_id.clone());
        if env.storage().persistent().has(&key) {
            panic!("refund request already exists");
        }

        let request = RefundRequest {
            student: student.clone(),
            course_id: course_id.clone(),
            requested_at_ledger: env.ledger().sequence(),
            status: RefundStatus::Pending,
        };

        env.storage().persistent().set(&key, &request);
        env.storage().persistent().extend_ttl(
            &key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        env.events().publish(
            (Symbol::new(&env, "refund_requested"), course_id.clone()),
            (student, course_id, enrollment.amount_paid),
        );
    }

    /// Admin processes a pending refund request (approve or reject)
    pub fn process_refund(
        env: Env,
        admin: Address,
        student: Address,
        course_id: String,
        approved: bool,
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
            .unwrap_or_else(|| panic!("refund request not found"));

        if request.status != RefundStatus::Pending {
            panic!("refund request is not pending");
        }

        if approved {
            let mut course = Self::get_course_internal(&env, &course_id)
                .unwrap_or_else(|| panic!("course not found"));
            let enrollment_key = DataKey::Enrollment(student.clone(), course_id.clone());
            let mut enrollment = env
                .storage()
                .persistent()
                .get::<DataKey, Enrollment>(&enrollment_key)
                .unwrap_or_else(|| panic!("enrollment not found"));

            // Use the split actually applied at enroll()/re_enroll() time
            // (persisted on the enrollment record) rather than recomputing
            // it from course.platform_fee_percent, which is never consulted
            // at enrollment and may have since diverged from the FeeConfig
            // (and any risk surcharge) that deduct_fee() actually applied.
            let platform_amount = enrollment.platform_amount;
            let instructor_amount = enrollment.instructor_amount;

            let token_client = token::Client::new(&env, &course.token);
            let treasury: Address = env
                .storage()
                .instance()
                .get(&DataKey::Treasury)
                .unwrap_or_else(|| panic!("treasury not set"));

            // Refund platform fee from treasury
            if platform_amount > 0 {
                token_client.transfer(&treasury, &student, &platform_amount);
            }

            // Refund instructor share from contract-held earnings
            if instructor_amount > 0 {
                Self::debit_instructor_earnings(
                    &env,
                    &course.instructor,
                    &course.token,
                    instructor_amount,
                );
                token_client.transfer(
                    &env.current_contract_address(),
                    &student,
                    &instructor_amount,
                );
            }

            // Mark enrollment as refunded and remove the active record so
            // `is_enrolled` reflects that the student is no longer enrolled.
            enrollment.is_refunded = true;
            env.storage().persistent().remove(&enrollment_key);

            // Decrement active enrollments
            if course.active_enrollments > 0 {
                course.active_enrollments -= 1;
            }

            course.last_updated_ledger = env.ledger().sequence();
            env.storage()
                .persistent()
                .set(&DataKey::Course(course_id.clone()), &course);

            // Decrement platform-wide active enrollment counter
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

            request.status = RefundStatus::Approved;
        } else {
            request.status = RefundStatus::Rejected;
        }

        env.storage().persistent().set(&key, &request);

        env.events().publish(
            (Symbol::new(&env, "refund_processed"), course_id.clone()),
            (student, course_id, approved, admin),
        );
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
        env.storage()
            .instance()
            .get(&DataKey::InstructorCourseCount(instructor))
            .unwrap_or(0)
    }

    /// Get the ordered list of course IDs an instructor has registered.
    /// The list is append-only: pausing, unpausing, or archiving a course
    /// changes only its `Course.status` and never removes it from this
    /// list, so it always reflects every course the instructor has ever
    /// registered.
    pub fn get_courses_by_instructor(env: Env, instructor: Address) -> Vec<String> {
        env.storage()
            .persistent()
            .get(&DataKey::InstructorCourseList(instructor))
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
            .unwrap_or_else(|| panic!("overflow computing upgrade effective ledger"));

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
            .unwrap_or_else(|| panic!("no pending upgrade"));

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
            .unwrap_or_else(|| panic!("no pending upgrade"));

        if env.ledger().sequence() < pending.effective_ledger {
            panic!("upgrade time-lock has not elapsed");
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
        }
        Self::get_course_internal(&env, &course_id)
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
        let course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| panic!("course not found"));
        let is_instructor = caller == course.instructor;

        if caller != student && !is_admin && !is_instructor {
            panic!("unauthorized");
        }
        env.storage()
            .persistent()
            .get(&DataKey::Enrollment(student, course_id))
    }

    /// Get the archived enrollment history (past completed attempts) for a
    /// student/course pair, populated by `re_enroll()`. Access follows the
    /// same rules as `get_enrollment`.
    pub fn get_enrollment_history(
        env: Env,
        caller: Address,
        student: Address,
        course_id: String,
    ) -> Vec<Enrollment> {
        caller.require_auth();
        let is_admin = Self::is_admin(&env, &caller);
        let course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| panic!("course not found"));
        let is_instructor = caller == course.instructor;

        if caller != student && !is_admin && !is_instructor {
            panic!("unauthorized");
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
            .unwrap_or_else(|| panic!("certificate not found"));

        let is_admin = Self::is_admin(&env, &caller);
        let is_instructor = caller == cert.instructor;
        let is_student = caller == cert.student;

        if !is_student && !is_admin && !is_instructor {
            panic!("unauthorized");
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

    /// Check whether a student is enrolled in a course
    pub fn is_enrolled(env: Env, student: Address, course_id: String) -> bool {
        env.storage()
            .persistent()
            .has(&DataKey::Enrollment(student, course_id))
    }

    /// Check whether a student has completed a course
    /// Check whether a student has completed a course.
    ///
    /// Returns:
    /// - `None`        — student has no enrollment record for this course
    /// - `Some(false)` — enrolled but not yet completed (or enrollment has expired)
    /// - `Some(true)`  — enrollment exists and is marked completed
    pub fn has_completed(env: Env, student: Address, course_id: String) -> Option<bool> {
        if let Some(enrollment) = env
            .storage()
            .persistent()
            .get::<DataKey, Enrollment>(&DataKey::Enrollment(student, course_id.clone()))
        {
            if enrollment.completed {
                return Some(true);
            }
            // Not yet completed — check if the enrollment window has expired
            if let Some(course) = Self::get_course_internal(&env, &course_id) {
                if let Some(expiry_ledgers) = course.enrollment_expiry_ledgers {
                    let expiry_at = enrollment
                        .enrolled_at_ledger
                        .checked_add(expiry_ledgers)
                        .unwrap_or(u32::MAX);
                    if env.ledger().sequence() >= expiry_at {
                        return Some(false); // Expired — treat as inactive
                    }
                }
            }
            Some(false)
        } else {
            None
        }
    }

    /// Verify a certificate — returns true if it exists, has not been
    /// permanently revoked, and has not expired.
    ///
    /// A certificate with a pending revocation (revoked == true but
    /// revocation_deadline in the future) is still considered valid until
    /// the challenge period expires.
    pub fn verify_certificate(env: Env, certificate_id: String) -> bool {
        if let Some(cert) = env
            .storage()
            .persistent()
            .get::<DataKey, Certificate>(&DataKey::Certificate(certificate_id))
        {
            if cert.revoked {
                // Revocation is permanent only after the challenge period expires
                if let Some(deadline) = cert.revocation_deadline {
                    if env.ledger().sequence() >= deadline {
                        return false;
                    }
                    // Within challenge period — still valid (pending revocation)
                    return true;
                }
                // Revoked without a deadline (legacy or direct revocation)
                return false;
            }
            if let Some(expiry) = cert.expires_at_ledger {
                if env.ledger().sequence() >= expiry {
                    return false;
                }
            }
            true
        } else {
            false
        }
    }

    /// Return a page of registered course IDs from the on-chain catalog.
    ///
    /// # Arguments
    /// - `offset` — zero-based index of the first course to return
    /// - `limit`  — maximum number of course IDs to return in one call
    ///
    /// Returns an empty list when `offset` is beyond the end of the catalog.
    pub fn list_courses(env: Env, offset: u32, limit: u32) -> Vec<String> {
        let catalog: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::CourseList)
            .unwrap_or_else(|| Vec::new(&env));

        let total = catalog.len();
        let start = offset.min(total);
        let end = (start + limit).min(total);

        let mut page = Vec::new(&env);
        for i in start..end {
            page.push_back(catalog.get(i).unwrap());
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

    fn get_course_internal(env: &Env, course_id: &String) -> Option<Course> {
        if let Some(course) = env
            .storage()
            .persistent()
            .get::<DataKey, Course>(&DataKey::Course(course_id.clone()))
        {
            return Some(course);
        }
        
        // If not found in active courses, check archived courses
        env.storage()
            .persistent()
            .get::<DataKey, Course>(&DataKey::ArchivedCourse(course_id.clone()))
    }

    fn get_enrollment_internal(env: &Env, student: &Address, course_id: &String) -> Enrollment {
        env.storage()
            .persistent()
            .get(&DataKey::Enrollment(student.clone(), course_id.clone()))
            .unwrap_or_else(|| panic!("enrollment not found"))
    }

    fn is_instructor_frozen_internal(env: &Env, instructor: &Address) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::InstructorBlocked(instructor.clone()))
            .unwrap_or(false)
    }

    fn is_student_blocked_internal(env: &Env, student: &Address) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::StudentBlocked(student.clone()))
            .unwrap_or(false)
    }

    fn is_admin(env: &Env, caller: &Address) -> bool {
        let admin: Option<Address> = env.storage().instance().get(&DataKey::Admin);
        admin.map(|a| a == *caller).unwrap_or(false)
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

    fn require_admin(env: &Env, caller: &Address, operation: &str) {
        if !Self::is_admin(env, caller) {
            panic!("unauthorized: {} - caller is not admin", operation);
        }

        // Check if admin role has expired
        if let Some(expires_at) = env
            .storage()
            .instance()
            .get::<DataKey, u32>(&DataKey::AdminExpiresAt)
        {
            if env.ledger().sequence() >= expires_at {
                panic!("admin role has expired");
            }
        }
    }

    /// Require that caller is either admin or an approved approver
    fn require_admin_or_approver(env: &Env, caller: &Address, operation: &str) {
        if !Self::is_admin_or_approver(env, caller) {
            panic!("unauthorized: {} - caller is not admin or approver", operation);
        }

        // Check if admin role has expired (approvers don't expire)
        if Self::is_admin(env, caller) {
            if let Some(expires_at) = env
                .storage()
                .instance()
                .get::<DataKey, u32>(&DataKey::AdminExpiresAt)
            {
                if env.ledger().sequence() >= expires_at {
                    panic!("admin role has expired");
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

        if (*caller1 == admin && *caller2 == secondary_admin)
            || (*caller1 == secondary_admin && *caller2 == admin)
        {
            // ok
        } else {
            panic!("unauthorized: requires both admin signatures");
        }

        // Check if admin role has expired
        if let Some(expires_at) = env
            .storage()
            .instance()
            .get::<DataKey, u32>(&DataKey::AdminExpiresAt)
        {
            if env.ledger().sequence() >= expires_at {
                panic!("admin role has expired");
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

    /// Generate a unique enrollment reference and store the mapping
    /// from enrollment_reference -> (student, course_id).
    fn generate_enrollment_ref(env: &Env, student: &Address, course_id: &String) -> String {
        let counter: u32 = env
            .storage()
            .instance()
            .get(&DataKey::EnrollmentRefCounter)
            .unwrap_or(1);
        env.storage()
            .instance()
            .set(&DataKey::EnrollmentRefCounter, &(counter + 1));

        let enrollment_ref = Self::counter_to_string(env, counter);

        // Store the mapping
        env.storage().persistent().set(
            &DataKey::EnrollmentByRef(enrollment_ref.clone()),
            &(student.clone(), course_id.clone()),
        );
        env.storage().persistent().extend_ttl(
            &DataKey::EnrollmentByRef(enrollment_ref.clone()),
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

        enrollment_ref
    }

    fn credit_instructor_earnings(env: &Env, instructor: &Address, token: &Address, amount: i128) {
        let key = DataKey::InstructorEarnings(instructor.clone(), token.clone());
        let current: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        let new_balance = current
            .checked_add(amount)
            .unwrap_or_else(|| panic!("overflow computing instructor earnings"));
        env.storage().persistent().set(&key, &new_balance);
        env.storage().persistent().extend_ttl(
            &key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );
        // Also update the per-instructor total earnings accumulator
        let total_key = DataKey::InstructorTotalEarnings(instructor.clone());
        let current_total: i128 = env.storage().persistent().get(&total_key).unwrap_or(0);
        let new_total = current_total
            .checked_add(amount)
            .unwrap_or_else(|| panic!("overflow computing instructor total earnings"));
        env.storage().persistent().set(&total_key, &new_total);
        env.storage().persistent().extend_ttl(
            &total_key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );
    }

    fn debit_instructor_earnings(env: &Env, instructor: &Address, token: &Address, amount: i128) {
        let key = DataKey::InstructorEarnings(instructor.clone(), token.clone());
        let current: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        if amount > current {
            panic!("insufficient instructor earnings for refund");
        }
        let new_balance = current - amount;
        if new_balance == 0 {
            env.storage().persistent().remove(&key);
        } else {
            env.storage().persistent().set(&key, &new_balance);
        }
        // Also decrement the per-instructor total earnings accumulator
        let total_key = DataKey::InstructorTotalEarnings(instructor.clone());
        let current_total: i128 = env.storage().persistent().get(&total_key).unwrap_or(0);
        if amount > current_total {
            panic!("inconsistent instructor total earnings state");
        }
        let new_total = current_total - amount;
        if new_total == 0 {
            env.storage().persistent().remove(&total_key);
        } else {
            env.storage().persistent().set(&total_key, &new_total);
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
        env.storage().persistent().set(&key, &stats);
        env.storage().persistent().extend_ttl(
            &key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );
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
            panic!("fee_bps cannot exceed 10000");
        }

        let config = FeeConfig { fee_bps };
        env.storage()
            .instance()
            .set(&DataKey::FeeConfig(token.clone()), &config);

        env.events().publish(
            (Symbol::new(&env, "fee_config_updated"), admin.clone()),
            (token, fee_bps),
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
            FeeConfig {
                fee_bps: default_pct
                    .checked_mul(100)
                    .expect("DefaultFee out of expected range [0, 100]: overflow would indicate validation bug"),
            }
        }
    }

    /// Admin sets the arbitration fee configuration — the minimum fee
    /// (in stroops) required to escalate a dispute to arbitration.
    pub fn set_arbitration_fee_config(env: Env, admin: Address, fee_per_case: i128) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "set_arbitration_fee_config");

        if fee_per_case < 0 {
            panic!("fee_per_case cannot be negative");
        }

        let config = ArbitrationFeeConfig { fee_per_case };
        env.storage()
            .instance()
            .set(&DataKey::ArbitrationFeeConfig, &config);

        env.events().publish(
            (
                Symbol::new(&env, "arbitration_fee_config_updated"),
                admin.clone(),
            ),
            fee_per_case,
        );
    }

    /// Get the current arbitration fee configuration.
    pub fn get_arbitration_fee_config(env: Env) -> ArbitrationFeeConfig {
        env.storage()
            .instance()
            .get(&DataKey::ArbitrationFeeConfig)
            .unwrap_or(ArbitrationFeeConfig { fee_per_case: 0 })
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

    /// Admin sets the risk fee configuration — parameters that determine
    /// surcharges based on payment size, customer history, and currency.
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
            panic!("large_payment_threshold cannot be negative");
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
    pub fn get_risk_fee_config(env: &Env) -> RiskFeeConfig {
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
        let (is_new_customer, is_btc_eth) = Self::resolve_risk_flags(&env, &student, &token);
        Self::compute_fee_breakdown(&env, &token, payment_amount, is_new_customer, is_btc_eth)
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
    /// `RiskFeeConfig` has been stored.
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
        let config: Option<RiskFeeConfig> = env.storage().instance().get(&DataKey::RiskFeeConfig);

        let config = match config {
            Some(config) if enabled => config,
            _ => {
                return RiskScore {
                    score: 0,
                    surcharge_bps: 0,
                }
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

        RiskScore {
            score,
            surcharge_bps,
        }
    }

    /// Computes the full fee breakdown for a payment: per-token base fee
    /// (falling back to `DefaultFee`) plus any risk surcharge, capped at
    /// 100%. Shared by `deduct_fee()` and `get_effective_fee_for_payment()`.
    fn compute_fee_breakdown(
        env: &Env,
        token: &Address,
        payment_amount: i128,
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

        // Compute platform fee: amount * effective_bps / 10000
        let platform_fee = payment_amount
            .checked_mul(effective_fee_bps as i128)
            .map(|v| v / 10000)
            .unwrap_or_else(|| panic!("overflow computing platform fee"));

        RiskFeeApplied {
            payment_amount,
            base_fee_bps,
            risk_surcharge_bps,
            effective_fee_bps,
            platform_fee,
        }
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
    /// - `is_new_customer` — whether the student has no prior enrollments
    /// - `is_btc_eth` — whether the token is BTC/ETH (higher volatility)
    ///
    /// Returns `(net_amount, fee_amount)` where `net_amount + fee_amount == amount`.
    fn deduct_fee(
        env: &Env,
        token: &Address,
        amount: i128,
        is_new_customer: bool,
        is_btc_eth: bool,
    ) -> (i128, i128) {
        if amount <= 0 {
            return (0, 0);
        }

        let breakdown = Self::compute_fee_breakdown(env, token, amount, is_new_customer, is_btc_eth);
        let platform_fee = breakdown.platform_fee;
        let net_amount = amount - platform_fee;

        // Publish RiskFeeApplied event when a risk surcharge was applied
        if breakdown.risk_surcharge_bps > 0 {
            env.events().publish((Symbol::new(env, "risk_fee_applied"),), breakdown);
        }

        (net_amount, platform_fee)
    }

    /// Escalate a dispute to arbitration. The caller must pay the
    /// arbitration fee (set via `set_arbitration_fee_config`).
    /// The fee is transferred from the caller to the contract and held
    /// until the arbitration is resolved.
    pub fn escalate_to_arbitration(env: Env, caller: Address, course_id: String) {
        caller.require_auth();

        let config: ArbitrationFeeConfig = env
            .storage()
            .instance()
            .get(&DataKey::ArbitrationFeeConfig)
            .unwrap_or(ArbitrationFeeConfig { fee_per_case: 0 });

        if config.fee_per_case <= 0 {
            panic!("arbitration fee not configured");
        }

        let course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| panic!("course not found"));

        let token_client = token::Client::new(&env, &course.token);
        token_client.transfer(
            &caller,
            &env.current_contract_address(),
            &config.fee_per_case,
        );

        env.events().publish(
            (Symbol::new(&env, "dispute_escalated"), course_id.clone()),
            (caller, course_id, config.fee_per_case),
        );
    }

    /// Complete a payment with a risk assessment.
    /// This is a helper that calculates the fee breakdown for a given
    /// payment and returns the `RiskFeeApplied` result.
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
            .unwrap_or_else(|| panic!("course not found"));

        if Self::is_student_blocked_internal(&env, &student) {
            panic!("student is blocked");
        }

        if Self::is_admin(&env, &student) {
            panic!("admin cannot join waitlist");
        }

        if student == course.instructor {
            panic!("instructor cannot join waitlist for own course");
        }

        if course.status != CourseStatus::Active {
            panic!("course is not active");
        }

        // Cannot join waitlist if already enrolled
        if env
            .storage()
            .persistent()
            .has(&DataKey::Enrollment(student.clone(), course_id.clone()))
        {
            panic!("already enrolled in this course");
        }

        // Check if course is at capacity
        if let Some(cap) = course.max_capacity {
            if course.total_enrollments < cap {
                panic!("course has not reached capacity - enroll directly instead");
            }
        } else {
            panic!("course has no capacity limit - enroll directly instead");
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
                panic!("already on waitlist");
            }
        }

        waitlist.push_back(student.clone());
        env.storage().persistent().set(&waitlist_key, &waitlist);
        env.storage().persistent().extend_ttl(
            &waitlist_key,
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );

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
        let mut waitlist: Vec<Address> = env
            .storage()
            .persistent()
            .get(&waitlist_key)
            .unwrap_or_else(|| panic!("no waitlist exists for this course"));

        let mut found_index: Option<u32> = None;
        for i in 0..waitlist.len() {
            if waitlist.get(i).unwrap() == student {
                found_index = Some(i);
                break;
            }
        }

        if found_index.is_none() {
            panic!("not on waitlist");
        }

        // Remove student from waitlist by creating a new vec without them
        let mut new_waitlist = Vec::new(&env);
        for i in 0..waitlist.len() {
            if i != found_index.unwrap() {
                new_waitlist.push_back(waitlist.get(i).unwrap());
            }
        }

        if new_waitlist.len() > 0 {
            env.storage().persistent().set(&waitlist_key, &new_waitlist);
            env.storage().persistent().extend_ttl(
                &waitlist_key,
                Self::PERSISTENT_TTL_THRESHOLD,
                Self::PERSISTENT_TTL_EXTEND_TO,
            );
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

        if let Some(mut list) = waitlist {
            if list.len() > 0 {
                let next_student = list.get(0).unwrap();
                
                // Remove from waitlist
                let mut new_waitlist = Vec::new(env);
                for i in 1..list.len() {
                    new_waitlist.push_back(list.get(i).unwrap());
                }

                if new_waitlist.len() > 0 {
                    env.storage().persistent().set(&waitlist_key, &new_waitlist);
                    env.storage().persistent().extend_ttl(
                        &waitlist_key,
                        Self::PERSISTENT_TTL_THRESHOLD,
                        Self::PERSISTENT_TTL_EXTEND_TO,
                    );
                } else {
                    env.storage().persistent().remove(&waitlist_key);
                }

                // Automatically enroll the student
                // We skip validation checks since the student already passed them when joining waitlist
                Self::enroll_internal(env, &next_student, course_id);

                env.events().publish(
                    (Symbol::new(env, "waitlist_promoted"), course_id.clone()),
                    (next_student, course_id.clone()),
                );
            }
        }
    }
}

mod test;

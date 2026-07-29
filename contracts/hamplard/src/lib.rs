//! # Hamplard Contract — Security Model
//!
//! ## Trust Hierarchy
//!
//! | Role              | Who                    | Capabilities                                                          |
//! |-------------------|------------------------|-----------------------------------------------------------------------|
//! | Admin             | `DataKey::Admin`       | Approve/archive courses, issue & revoke certificates, pause platform  |
//! | Secondary Admin   | `DataKey::SecondaryAdmin` | Required alongside Admin for multi-sig operations (archive, treasury update, admin transfer) |
//! | Instructor        | Course `instructor` field | Register courses, pause/unpause own courses, withdraw earnings     |
//! | Student           | Any caller             | Enroll in active courses (must sign), batch-enroll                   |
//! | Treasury          | `DataKey::Treasury`    | Passive recipient of platform fee share; cannot initiate any action   |
//!
//! ## Privileged Operations (single admin)
//! - `approve_course` — moves a course from Pending to Active
//! - `mark_completed` — marks a student enrollment as completed
//! - `issue_certificate` — mints an on-chain certificate of completion
//! - `revoke_certificate` — flags a certificate as revoked (remains on-chain for audit)
//! - `pause_platform` / `unpause_platform` — halts or restores all enrollments
//! - `add_approved_token` / `remove_approved_token` — controls which token contracts are accepted
//! - `update_default_fee` / `update_max_courses_limit` — updates global parameters
//! - `get_platform_fee` — retrieves platform fee configuration (admin only)
//! - `withdraw_tokens` — emergency sweep of contract-held tokens (admin only)
//!
//! ## Privileged Operations (multi-sig — both Admin + Secondary Admin required)
//! - `archive_course` — permanent course removal; may trigger student refunds
//! - `transfer_admin` — proposes a new admin pair (new admins must then call `accept_admin`)
//! - `update_treasury` — schedules a new treasury address (takes effect after 100 ledgers)
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
//!   `transfer_admin` / `accept_admin` flow with both current admins signing.
//! - **Treasury update delay** — `update_treasury` takes effect 100 ledgers after proposal;
//!   enrollments submitted within that window still route fees to the old treasury.

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
    /// Ledger sequence of the most recent state-changing write to this record
    /// (registration, approval, pause/unpause, archival, capacity update, etc.).
    pub last_updated_ledger: u32,
    /// SHA-256 (or equivalent 32-byte) hash of the off-chain course content
    /// (syllabus, video manifest, etc.) at registration time.
    /// Allows students and auditors to verify that off-chain materials have not
    /// been silently changed after enrollment by comparing against the stored
    /// commitment. Updated only by the instructor or admin via
    /// `update_content_hash()`.
    pub content_hash: BytesN<32>,
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
    /// Course version active at the time of enrollment
    pub course_version: u32,
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
    /// Whether this certificate has been revoked (e.g. cheating)
    pub revoked: bool,
    /// Admin address that performed the revocation, if revoked
    pub revoked_by: Option<Address>,
    /// Ledger sequence when the revocation occurred, if revoked
    pub revoked_at_ledger: Option<u32>,
    /// Reason code supplied by the revoking admin, if revoked
    pub revocation_reason: Option<String>,
    /// Optional ledger sequence when the certificate expires
    pub expires_at_ledger: Option<u32>,
    /// Optional Ed25519 signature from the instructor over the certificate
    /// data, allowing external verifiers to cryptographically confirm the
    /// instructor endorsed this certificate without off-chain evidence.
    pub instructor_signature: Option<BytesN<64>>,
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
        let _ = token_client.decimals();

        if Self::is_instructor_frozen_internal(&env, &instructor) {
            panic!("instructor is frozen");
        }

        if course_id.len() > Self::MAX_COURSE_ID_LEN {
            panic!("course_id exceeds maximum length");
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
            last_updated_ledger: env.ledger().sequence(),
            content_hash,
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
        env.storage().instance().set(
            &pending_count_key,
            &(current_pending_count + 1),
        );

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

        env.events().publish(
            (Symbol::new(&env, "course_registered"), course_id.clone()),
            course_id.clone(),
        );

        course_id
    }

    /// Admin approves a Pending course, making it Active and enrollable.
    ///
    /// # Arguments
    /// - `admin`     — must match the stored admin address
    /// - `course_id` — the course to approve
    pub fn approve_course(env: Env, admin: Address, course_id: String) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "approve_course");
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

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

        let pending_key = DataKey::InstructorPendingCourseCount(course.instructor.clone());
        let pending_count: u32 = env.storage().instance().get(&pending_key).unwrap_or(0);
        let new_pending_count = pending_count
            .checked_sub(1)
            .unwrap_or_else(|| panic!("pending course count underflow"));
        env.storage().instance().set(&pending_key, &new_pending_count);

        env.events().publish(
            (Symbol::new(&env, "course_approved"), course_id.clone()),
            (course_id, course.instructor, admin, env.ledger().sequence()),
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

        env.events().publish(
            (Symbol::new(&env, "course_paused"), course_id.clone()),
            course_id,
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

        env.events().publish(
            (Symbol::new(&env, "course_unpaused"), course_id.clone()),
            course_id,
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

        if let Some(ref students) = students_to_refund {
            let token_client = token::Client::new(&env, &course.token);
            let platform_fee_pct = course.platform_fee_percent as i128;

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

                    if !enrollment.completed {
                        let platform_amount = enrollment
                            .amount_paid
                            .checked_mul(platform_fee_pct)
                            .map(|v| v / 100)
                            .unwrap_or_else(|| panic!("overflow computing platform fee"));

                        let instructor_amount = enrollment.amount_paid - platform_amount;

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

                        // Remove enrollment
                        env.storage().persistent().remove(&enrollment_key);

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
                    }
                }
            }
        }

        if course.active_enrollments > 0 {
            panic!("cannot archive course with active enrollments");
        }

        course.status = CourseStatus::Archived;

        course.last_updated_ledger = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);

        env.events().publish(
            (Symbol::new(&env, "course_archived"), course_id.clone()),
            (course_id, admin1, admin2),
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

        let mut modified = false;

        if let Some(price) = new_price {
            if price < 0 {
                panic!("price cannot be negative");
            }
            if price != 0
                && (price < Self::MIN_COURSE_PRICE_STROOPS || price > Self::MAX_COURSE_PRICE_STROOPS)
            {
                panic!("price is outside the expected USDC precision range (0 for free, or 0.01-100000 USDC in stroops)");
            }
            course.price = price;
            modified = true;
        }

        if let Some(capacity) = new_max_capacity {
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
        course.enrollment_start_ledger = enrollment_start_ledger;
        course.last_updated_ledger = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);

        env.events().publish(
            (Symbol::new(&env, "enrollment_start_set"), course_id.clone()),
            (course_id, enrollment_start_ledger),
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
            .unwrap_or_else(|| panic!("course not found"));

        let is_admin = Self::is_admin(&env, &caller);
        let is_instructor = caller == course.instructor;

        if !is_admin && !is_instructor {
            panic!("unauthorized");
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

        if !env
            .storage()
            .instance()
            .has(&DataKey::ApprovedToken(course.token.clone()))
        {
            panic!("course token is not approved");
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
            panic!("course is not available for enrollment");
        }

        // Use the centralized fee deduction function that supports:
        // 1. Per-token fee configuration (map token → FeeConfig)
        // 2. Risk-based surcharges for large payments, new customers, and BTC/ETH
        // 3. Publishes RiskFeeApplied event when surcharge applies
        let (instructor_amount, platform_amount) = Self::deduct_fee(
            env,
            &course.token,
            course.price,
            false, // is_new_customer — not tracked at enrollment; always false
            false, // is_btc_eth — not tracked; always false
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
        if course.price > 0 {
            token_client.transfer(student, &env.current_contract_address(), &course.price);

            if platform_amount > 0 {
                token_client.transfer(&env.current_contract_address(), &treasury, &platform_amount);
                env.events().publish(
                    (Symbol::new(&env, "platform_fee_transferred"), course_id.clone()),
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
                    (Symbol::new(&env, "instructor_payment_transferred"), course_id.clone()),
                    (course.instructor.clone(), instructor_amount, env.ledger().sequence()),
                );
            }
        }

        // Record enrollment
        let enrollment = Enrollment {
            student: student.clone(),
            course_id: course_id.clone(),
            amount_paid: course.price,
            enrolled_at_ledger: env.ledger().sequence(),
            completed: false,
            certificate_issued: false,
            certificate_id: None,
            evidence_hash: None,
            course_version: course.version,
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
        let (instructor_amount, platform_amount) = Self::deduct_fee(
            &env,
            &course.token,
            course.price,
            false, // is_new_customer — not tracked at re-enrollment; always false
            false, // is_btc_eth — not tracked; always false
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
            token_client.transfer(&student, &env.current_contract_address(), &course.price);

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

        let new_enrollment = Enrollment {
            student: student.clone(),
            course_id: course_id.clone(),
            amount_paid: course.price,
            enrolled_at_ledger: env.ledger().sequence(),
            completed: false,
            certificate_issued: false,
            certificate_id: None,
            evidence_hash: None,
            course_version: course.version,
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

    // ----------------------------------------------------------
    // COURSE COMPLETION & CERTIFICATES
    // ----------------------------------------------------------

    /// Admin marks a student's enrollment as completed.
    /// This is called by the admin after the backend verifies the student
    /// has finished all lessons and passed all assignments.
    ///
    /// # Arguments
    /// - `admin`     — must match stored admin
    /// - `student`   — the student's address
    /// - `course_id` — the course completed
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

        if evidence_hash.is_none() {
            student.require_auth();
        }

        let mut enrollment = Self::get_enrollment_internal(&env, &student, &course_id);

        let course_for_check = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| panic!("course not found"));
        
        // Validate course is in a valid state for completion (Active or Paused, not Archived)
        if course_for_check.status == CourseStatus::Archived {
            panic!("cannot mark completion for archived course");
        }
        
        let elapsed = env
            .ledger()
            .sequence()
            .checked_sub(enrollment.enrolled_at_ledger)
            .unwrap_or(0);
        if elapsed < course_for_check.min_completion_ledgers {
            panic!("minimum enrollment duration has not elapsed");
        }

        if enrollment.course_id != course_id {
            panic!("enrollment course_id mismatch");
        }

        if enrollment.completed {
            panic!("already marked as completed");
        }

        // Check enrollment expiry — expired enrollments cannot be completed
        let course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| panic!("course not found"));
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

        env.events().publish(
            (Symbol::new(&env, "course_completed"), course_id.clone()),
            (student, admin),
        );
    }

    /// Issue an on-chain certificate to a student who has completed a course.
    /// Certificates are permanent, verifiable proofs of skill attainment.
    ///
    /// Admin calls this after `mark_completed`. The certificate ID must be
    /// unique (e.g. generated by the backend as UUID or hash).
    ///
    /// # Arguments
    /// - `admin`          — must match stored admin
    /// - `certificate_id` — unique certificate identifier
    /// - `student`        — the student's address
    /// - `course_id`      — the completed course
    /// - `course_title`   — short title stored on-chain for verifiability
    pub fn issue_certificate(
        env: Env,
        admin: Address,
        certificate_id: String,
        course_id: String,
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

        let student = Address::from_string(&enrollment_reference);

        // Student must have completed the course
        let mut enrollment = Self::get_enrollment_internal(&env, &student, &course_id);
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

        let course = Self::get_course_internal(&env, &course_id)
            .unwrap_or_else(|| panic!("course not found"));

        let issued_at_ledger = env.ledger().sequence();
        let certificate = Certificate {
            id: certificate_id.clone(),
            student: enrollment.student.clone(),
            course_id: course_id.clone(),
            course_title,
            enrollment_reference: enrollment_reference.clone(),
            instructor: course.instructor,
            issued_by: env.current_contract_address(),
            issued_at_ledger,
            revoked: false,
            revoked_by: None,
            revoked_at_ledger: None,
            revocation_reason: None,
            expires_at_ledger,
            instructor_signature,
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
    /// Revoked certificates remain on-chain for audit purposes but are flagged.
    /// The revoking admin's address, the ledger sequence, and a reason code are
    /// all persisted so the revocation is fully auditable after the fact.
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

        let mut cert = env
            .storage()
            .persistent()
            .get::<DataKey, Certificate>(&DataKey::Certificate(certificate_id.clone()))
            .unwrap_or_else(|| panic!("certificate not found"));

        if cert.revoked {
            panic!("certificate is already revoked");
        }

        cert.revoked = true;
        cert.revoked_by = Some(admin.clone());
        cert.revoked_at_ledger = Some(env.ledger().sequence());
        cert.revocation_reason = Some(reason.clone());

        env.storage()
            .persistent()
            .set(&DataKey::Certificate(certificate_id.clone()), &cert);

        env.events().publish(
            (
                Symbol::new(&env, "certificate_revoked"),
                certificate_id.clone(),
            ),
            (certificate_id, admin, reason),
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
    pub fn freeze_instructor(env: Env, admin: Address, instructor: Address) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "freeze_instructor");
        env.storage()
            .instance()
            .set(&DataKey::InstructorBlocked(instructor.clone()), &true);
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
        env.storage()
            .instance()
            .get(&DataKey::AdminExpiresAt)
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
            env.storage().instance().remove(&DataKey::PlatformEnrollmentCap);
        }
        
        env.events().publish(
            (Symbol::new(&env, "platform_enrollment_cap_set"), admin.clone()),
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

    /// Request a refund for an enrollment within the refund window
    pub fn request_refund(env: Env, student: Address, course_id: String) {
        student.require_auth();

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
            let enrollment = env
                .storage()
                .persistent()
                .get::<DataKey, Enrollment>(&enrollment_key)
                .unwrap_or_else(|| panic!("enrollment not found"));

            let platform_fee_pct = course.platform_fee_percent as i128;
            let platform_amount = enrollment
                .amount_paid
                .checked_mul(platform_fee_pct)
                .map(|v| v / 100)
                .unwrap_or_else(|| panic!("overflow computing platform fee"));

            let instructor_amount = enrollment.amount_paid - platform_amount;

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

            // Remove enrollment
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

    /// Get a certificate by ID
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
            cert.student.require_auth();
        }
        cert
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
            Some(enrollment.completed)
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

    /// Verify a certificate — returns true if it exists and has not been revoked
    pub fn verify_certificate(env: Env, certificate_id: String) -> bool {
        if let Some(cert) = env
            .storage()
            .persistent()
            .get::<DataKey, Certificate>(&DataKey::Certificate(certificate_id))
        {
            if cert.revoked {
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
        env.storage()
            .persistent()
            .get(&DataKey::Course(course_id.clone()))
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
        env.storage()
            .instance()
            .get::<DataKey, FeeConfig>(&DataKey::FeeConfig(token))
            .unwrap_or(FeeConfig { fee_bps: 2000 })
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
            (Symbol::new(&env, "arbitration_fee_config_updated"), admin.clone()),
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
            (large_payment_surcharge_bps, large_payment_threshold, new_customer_surcharge_bps, btc_eth_surcharge_bps),
        );
    }

    /// Get the current risk fee configuration.
    pub fn get_risk_fee_config(env: Env) -> RiskFeeConfig {
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

    /// Calculate a risk score and surcharge for a given payment.
    ///
    /// # Arguments
    /// - `payment_amount` — the payment amount in stroops
    /// - `is_new_customer` — whether the student has no prior enrollments
    /// - `is_btc_eth` — whether the payment is in BTC/ETH (higher volatility)
    ///
    /// Returns a `RiskScore` with score (0-100) and surcharge_bps to add.
    pub fn calculate_risk_score(
        env: Env,
        payment_amount: i128,
        is_new_customer: bool,
        is_btc_eth: bool,
    ) -> RiskScore {
        let config: RiskFeeConfig = Self::get_risk_fee_config(env);

        if !env.storage().instance().get(&DataKey::RiskConfigEnabled).unwrap_or(false) {
            return RiskScore {
                score: 0,
                surcharge_bps: 0,
            };
        }

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

        RiskScore { score, surcharge_bps }
    }

    /// Calculate the effective fee for a payment, applying risk-based surcharges.
    ///
    /// # Arguments
    /// - `token` — the token address (for per-token fee config)
    /// - `payment_amount` — the payment amount in stroops
    /// - `is_new_customer` — whether the student has no prior enrollments
    /// - `is_btc_eth` — whether the payment is in BTC/ETH
    ///
    /// Returns a `RiskFeeApplied` struct with the full fee breakdown.
    pub fn get_effective_fee_for_payment(
        env: Env,
        token: Address,
        payment_amount: i128,
        is_new_customer: bool,
        is_btc_eth: bool,
    ) -> RiskFeeApplied {
        let fee_config = Self::get_fee_config(env.clone(), token);
        let base_fee_bps = fee_config.fee_bps;

        // Only apply risk surcharge if risk config is enabled
        let risk_surcharge_bps = if Self::is_risk_config_enabled(env.clone()) {
            let risk_score = Self::calculate_risk_score(
                env.clone(),
                payment_amount,
                is_new_customer,
                is_btc_eth,
            );
            risk_score.surcharge_bps
        } else {
            0
        };

        let effective_fee_bps = base_fee_bps.saturating_add(risk_surcharge_bps);
        // Cap at 100% (10000 bps)
        let effective_fee_bps = if effective_fee_bps > 10000 { 10000 } else { effective_fee_bps };

        // Compute fee: amount * bps / 10000
        let platform_fee = payment_amount
            .checked_mul(effective_fee_bps as i128)
            .map(|v| v / 10000)
            .unwrap_or(0);

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

        let fee_config = Self::get_fee_config(env.clone(), token.clone());
        let mut effective_bps = fee_config.fee_bps;

        // Apply risk surcharge when risk config is enabled
        let mut risk_surcharge_bps: u32 = 0;
        if Self::is_risk_config_enabled(env.clone()) {
            if let Some(risk_config) = env
                .storage()
                .instance()
                .get::<DataKey, RiskFeeConfig>(&DataKey::RiskFeeConfig)
            {
                // Large payment surcharge
                if amount > risk_config.large_payment_threshold {
                    risk_surcharge_bps = risk_surcharge_bps.saturating_add(
                        risk_config.large_payment_surcharge_bps,
                    );
                }
                // New customer surcharge
                if is_new_customer {
                    risk_surcharge_bps = risk_surcharge_bps.saturating_add(
                        risk_config.new_customer_surcharge_bps,
                    );
                }
                // BTC/ETH surcharge
                if is_btc_eth {
                    risk_surcharge_bps = risk_surcharge_bps.saturating_add(
                        risk_config.btc_eth_surcharge_bps,
                    );
                }

                effective_bps = effective_bps.saturating_add(risk_surcharge_bps);
                // Cap at 100% (10000 bps)
                if effective_bps > 10000 {
                    effective_bps = 10000;
                }
            }
        }

        // Compute platform fee: amount * effective_bps / 10000
        let platform_fee = amount
            .checked_mul(effective_bps as i128)
            .map(|v| v / 10000)
            .unwrap_or(0);
        let net_amount = amount - platform_fee;

        // Publish RiskFeeApplied event when a risk surcharge was applied
        if risk_surcharge_bps > 0 {
            env.events().publish(
                (Symbol::new(env, "risk_fee_applied"),),
                RiskFeeApplied {
                    payment_amount: amount,
                    base_fee_bps: fee_config.fee_bps,
                    risk_surcharge_bps,
                    effective_fee_bps: effective_bps,
                    platform_fee,
                },
            );
        }

        (net_amount, platform_fee)
    }

    /// Escalate a dispute to arbitration. The caller must pay the
    /// arbitration fee (set via `set_arbitration_fee_config`).
    /// The fee is transferred from the caller to the contract and held
    /// until the arbitration is resolved.
    pub fn escalate_to_arbitration(
        env: Env,
        caller: Address,
        course_id: String,
    ) {
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
        token_client.transfer(&caller, &env.current_contract_address(), &config.fee_per_case);

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
        token: Address,
        payment_amount: i128,
        is_new_customer: bool,
        is_btc_eth: bool,
    ) -> RiskFeeApplied {
        Self::get_effective_fee_for_payment(env, token, payment_amount, is_new_customer, is_btc_eth)
    }
}

mod test;

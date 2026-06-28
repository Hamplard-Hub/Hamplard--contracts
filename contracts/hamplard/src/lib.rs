#![no_std]

use soroban_sdk::{
    contract, contractimpl, contracttype, token, Address, Env, String, Vec,
};

// ============================================================
// EVENT SYSTEM
// ============================================================

/// Centralized event emission helper module.
/// Ensures consistent event structure with ledger sequence, actor, and operation details.
mod events {
    use soroban_sdk::{Address, Env, String, Symbol};

    /// Emit a course registration event with full audit trail
    pub fn course_registered(
        env: &Env,
        actor: &Address,
        course_id: &String,
        instructor: &Address,
        price: i128,
        token: &Address,
        fee_percent: u32,
    ) {
        env.events().publish(
            (Symbol::new(env, "course_registered"), course_id.clone()),
            (
                actor.clone(),
                course_id.clone(),
                instructor.clone(),
                price,
                token.clone(),
                fee_percent,
                env.ledger().sequence(),
            ),
        );
    }

    /// Emit a course approval event
    pub fn course_approved(env: &Env, admin: &Address, course_id: &String) {
        env.events().publish(
            (Symbol::new(env, "course_approved"), course_id.clone()),
            (admin.clone(), course_id.clone(), env.ledger().sequence()),
        );
    }

    /// Emit a course pause event
    pub fn course_paused(env: &Env, caller: &Address, course_id: &String) {
        env.events().publish(
            (Symbol::new(env, "course_paused"), course_id.clone()),
            (caller.clone(), course_id.clone(), env.ledger().sequence()),
        );
    }

    /// Emit a course unpause event
    pub fn course_unpaused(env: &Env, caller: &Address, course_id: &String) {
        env.events().publish(
            (Symbol::new(env, "course_unpaused"), course_id.clone()),
            (caller.clone(), course_id.clone(), env.ledger().sequence()),
        );
    }

    /// Emit a course archive event with refund details
    pub fn course_archived(
        env: &Env,
        admins: (&Address, &Address),
        course_id: &String,
        refund_count: u32,
        total_refunded: i128,
    ) {
        env.events().publish(
            (Symbol::new(env, "course_archived"), course_id.clone()),
            (
                admins.0.clone(),
                admins.1.clone(),
                course_id.clone(),
                refund_count,
                total_refunded,
                env.ledger().sequence(),
            ),
        );
    }

    /// Emit a student enrollment event with payment split details
    pub fn student_enrolled(
        env: &Env,
        student: &Address,
        course_id: &String,
        amount_paid: i128,
        platform_fee: i128,
        instructor_fee: i128,
    ) {
        env.events().publish(
            (Symbol::new(env, "student_enrolled"), course_id.clone()),
            (
                student.clone(),
                course_id.clone(),
                amount_paid,
                platform_fee,
                instructor_fee,
                env.ledger().sequence(),
            ),
        );
    }

    /// Emit a course completion event
    pub fn course_completed(
        env: &Env,
        admin: &Address,
        student: &Address,
        course_id: &String,
        has_evidence: bool,
    ) {
        env.events().publish(
            (Symbol::new(env, "course_completed"), course_id.clone()),
            (
                admin.clone(),
                student.clone(),
                course_id.clone(),
                has_evidence,
                env.ledger().sequence(),
            ),
        );
    }

    /// Emit a certificate issuance event
    pub fn certificate_issued(
        env: &Env,
        admin: &Address,
        certificate_id: &String,
        student: &Address,
        course_id: &String,
        course_title: &String,
    ) {
        env.events().publish(
            (Symbol::new(env, "certificate_issued"), certificate_id.clone()),
            (
                admin.clone(),
                certificate_id.clone(),
                student.clone(),
                course_id.clone(),
                course_title.clone(),
                env.ledger().sequence(),
            ),
        );
    }

    /// Emit a certificate revocation event with reason
    pub fn certificate_revoked(
        env: &Env,
        admin: &Address,
        certificate_id: &String,
        student: &Address,
        course_id: &String,
        reason: &String,
    ) {
        env.events().publish(
            (Symbol::new(env, "certificate_revoked"), certificate_id.clone()),
            (
                admin.clone(),
                certificate_id.clone(),
                student.clone(),
                course_id.clone(),
                reason.clone(),
                env.ledger().sequence(),
            ),
        );
    }

    /// Emit a platform pause event
    pub fn platform_paused(env: &Env, admin: &Address) {
        env.events().publish(
            (Symbol::new(env, "platform_paused"), Symbol::new(env, "system")),
            (admin.clone(), env.ledger().sequence()),
        );
    }

    /// Emit a platform unpause event
    pub fn platform_unpaused(env: &Env, admin: &Address) {
        env.events().publish(
            (Symbol::new(env, "platform_unpaused"), Symbol::new(env, "system")),
            (admin.clone(), env.ledger().sequence()),
        );
    }

    /// Emit a token withdrawal event
    pub fn tokens_withdrawn(
        env: &Env,
        admin: &Address,
        token: &Address,
        amount: i128,
        destination: &Address,
    ) {
        env.events().publish(
            (Symbol::new(env, "tokens_withdrawn"), token.clone()),
            (
                admin.clone(),
                token.clone(),
                amount,
                destination.clone(),
                env.ledger().sequence(),
            ),
        );
    }

    /// Emit an admin transfer proposal event
    pub fn admin_transfer_proposed(
        env: &Env,
        proposer1: &Address,
        proposer2: &Address,
        new_admin: &Address,
        new_secondary_admin: &Address,
    ) {
        env.events().publish(
            (Symbol::new(env, "admin_transfer_proposed"), new_admin.clone()),
            (
                proposer1.clone(),
                proposer2.clone(),
                new_admin.clone(),
                new_secondary_admin.clone(),
                env.ledger().sequence(),
            ),
        );
    }

    /// Emit an admin transfer acceptance event
    pub fn admin_transfer_accepted(
        env: &Env,
        new_admin: &Address,
        new_secondary_admin: &Address,
    ) {
        env.events().publish(
            (Symbol::new(env, "admin_transfer_accepted"), new_admin.clone()),
            (
                new_admin.clone(),
                new_secondary_admin.clone(),
                env.ledger().sequence(),
            ),
        );
    }

    /// Emit a treasury update event
    pub fn treasury_updated(
        env: &Env,
        admin1: &Address,
        admin2: &Address,
        new_treasury: &Address,
        effective_ledger: u32,
    ) {
        env.events().publish(
            (Symbol::new(env, "treasury_updated"), new_treasury.clone()),
            (
                admin1.clone(),
                admin2.clone(),
                new_treasury.clone(),
                effective_ledger,
                env.ledger().sequence(),
            ),
        );
    }

    /// Emit a default fee update event
    pub fn default_fee_updated(env: &Env, admin: &Address, new_fee_pct: u32) {
        env.events().publish(
            (Symbol::new(env, "default_fee_updated"), Symbol::new(env, "system")),
            (admin.clone(), new_fee_pct, env.ledger().sequence()),
        );
    }

    /// Emit a token whitelist addition event
    pub fn token_whitelisted(env: &Env, admin: &Address, token: &Address) {
        env.events().publish(
            (Symbol::new(env, "token_whitelisted"), token.clone()),
            (admin.clone(), token.clone(), env.ledger().sequence()),
        );
    }

    /// Emit a token whitelist removal event
    pub fn token_removed_from_whitelist(env: &Env, admin: &Address, token: &Address) {
        env.events().publish(
            (Symbol::new(env, "token_removed_from_whitelist"), token.clone()),
            (admin.clone(), token.clone(), env.ledger().sequence()),
        );
    }

    /// Emit a platform initialization event
    pub fn platform_initialized(
        env: &Env,
        admin: &Address,
        secondary_admin: &Address,
        treasury: &Address,
        default_fee_pct: u32,
    ) {
        env.events().publish(
            (Symbol::new(env, "platform_initialized"), admin.clone()),
            (
                admin.clone(),
                secondary_admin.clone(),
                treasury.clone(),
                default_fee_pct,
                env.ledger().sequence(),
            ),
        );
    }
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
    /// USDC price per enrollment (in stroops, 7 decimal places)
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
    /// Optional proof of completion evidence (e.g. hash)
    pub evidence_hash: Option<String>,
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
    /// Instructor's address (for attribution)
    pub instructor: Address,
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
}

/// Pending platform treasury update with effective ledger sequence
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct TreasuryUpdate {
    pub address: Address,
    pub effective_ledger: u32,
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
    /// Secondary admin address for multi-sig operations
    SecondaryAdmin,
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
}

// ============================================================
// CONTRACT
// ============================================================

#[contract]
pub struct HamplardContract;

#[contractimpl]
impl HamplardContract {

    const INSTANCE_TTL_THRESHOLD: u32 = 100_000;
    const INSTANCE_TTL_EXTEND_TO:  u32 = 6_300_000;
    const MAX_COURSE_ID_LEN:       u32 = 256;
    const MAX_COURSE_TITLE_LEN:    u32 = 512;

    // ----------------------------------------------------------
    // INIT
    // ----------------------------------------------------------

    /// Initialise the contract.
    /// Called once by the deployer immediately after deployment.
    ///
    /// # Arguments
    /// - `admin`            — admin address (approves courses, issues certificates)
    /// - `treasury`         — platform treasury address (receives platform fee share)
    /// - `default_fee_pct`  — default platform fee percentage (e.g. 20 = 20%)
    pub fn init(env: Env, admin: Address, secondary_admin: Address, treasury: Address, default_fee_pct: u32) {
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

        env.storage().instance().extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::SecondaryAdmin, &secondary_admin);
        env.storage().instance().set(&DataKey::Treasury, &treasury);
        env.storage().instance().set(&DataKey::PlatformPaused, &false);
        env.storage().instance().set(&DataKey::DefaultFee, &default_fee_pct);

        // Emit comprehensive initialization event with full audit trail
        events::platform_initialized(&env, &admin, &secondary_admin, &treasury, default_fee_pct);
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
    pub fn register_course(
        env: Env,
        instructor: Address,
        course_id: String,
        price: i128,
        token: Address,
        platform_fee_pct: u32,
    ) -> String {
        instructor.require_auth();

        if course_id.len() > Self::MAX_COURSE_ID_LEN {
            panic!("course_id exceeds maximum length");
        }

        if price < 0 {
            panic!("price cannot be negative");
        }

        if env
            .storage()
            .persistent()
            .has(&DataKey::Course(course_id.clone()))
        {
            panic!("course already registered");
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
            platform_fee_pct
        };

        let course = Course {
            id: course_id.clone(),
            instructor: instructor.clone(),
            price,
            platform_fee_percent: fee,
            token: token.clone(),
            total_enrollments: 0,
            active_enrollments: 0,
            total_earned: 0,
            status: CourseStatus::Pending,
            created_at_ledger: env.ledger().sequence(),
        };

        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);

        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Course(course_id.clone()), 100_000, 6_300_000);

        // Emit enhanced event with full audit trail and payment details
        events::course_registered(&env, &instructor, &course_id, &instructor, price, &token, fee);

        course_id
    }

    /// Admin approves a Pending course, making it Active and enrollable.
    ///
    /// # Arguments
    /// - `admin`     — must match the stored admin address
    /// - `course_id` — the course to approve
    pub fn approve_course(env: Env, admin: Address, course_id: String) {
        admin.require_auth();
        Self::require_admin(&env, &admin);
        env.storage().instance().extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        let mut course = Self::get_course_internal(&env, &course_id);

        if course.status != CourseStatus::Pending {
            panic!("course is not pending approval");
        }

        course.status = CourseStatus::Active;
        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);

        // Emit enhanced approval event with ledger sequence and admin info
        events::course_approved(&env, &admin, &course_id);
    }

    /// Instructor or admin pauses a course.
    /// Existing enrollments are unaffected — students can still access content.
    /// New enrollments are blocked until the course is unpaused.
    pub fn pause_course(env: Env, caller: Address, course_id: String) {
        caller.require_auth();

        let mut course = Self::get_course_internal(&env, &course_id);

        let is_admin = Self::is_admin(&env, &caller);
        let is_instructor = caller == course.instructor;

        if !is_admin && !is_instructor {
            panic!("unauthorized");
        }

        env.storage().instance().extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        if course.status != CourseStatus::Active {
            panic!("course is not active");
        }

        course.status = CourseStatus::Paused;
        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);

        // Emit enhanced pause event with actor and ledger sequence
        events::course_paused(&env, &caller, &course_id);
    }

    /// Instructor or admin unpauses a Paused course, restoring it to Active.
    pub fn unpause_course(env: Env, caller: Address, course_id: String) {
        caller.require_auth();

        let mut course = Self::get_course_internal(&env, &course_id);

        let is_admin = Self::is_admin(&env, &caller);
        let is_instructor = caller == course.instructor;

        if !is_admin && !is_instructor {
            panic!("unauthorized");
        }

        env.storage().instance().extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        if course.status != CourseStatus::Paused {
            panic!("course is not paused");
        }

        course.status = CourseStatus::Active;
        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);

        // Emit enhanced unpause event with actor and ledger sequence
        events::course_unpaused(&env, &caller, &course_id);
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
        env.storage().instance().extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        let mut course = Self::get_course_internal(&env, &course_id);

        if course.status != CourseStatus::Paused {
            panic!("course must be paused before archiving");
        }

        let mut total_refunded: i128 = 0;
        let mut refund_count: u32 = 0;

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
                    let enrollment: Enrollment = env
                        .storage()
                        .persistent()
                        .get(&enrollment_key)
                        .unwrap();
                    
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
                            total_refunded += platform_amount;
                        }

                        // Refund instructor share from instructor
                        if instructor_amount > 0 {
                            token_client.transfer(&course.instructor, &student, &instructor_amount);
                            total_refunded += instructor_amount;
                        }

                        // Remove enrollment
                        env.storage().persistent().remove(&enrollment_key);

                        // Decrement active enrollments
                        if course.active_enrollments > 0 {
                            course.active_enrollments -= 1;
                        }

                        refund_count += 1;
                    }
                }
            }
        }

        if course.active_enrollments > 0 {
            panic!("cannot archive course with active enrollments");
        }

        course.status = CourseStatus::Archived;

        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);

        // Emit enhanced archive event with refund details for audit trail
        events::course_archived(&env, (&admin1, &admin2), &course_id, refund_count, total_refunded);
    }

    // ----------------------------------------------------------
    // ENROLLMENT & PAYMENT
    // ----------------------------------------------------------

    /// Student enrolls in a course and pays the fee.
    ///
    /// The payment is split automatically:
    ///   - Platform fee  → treasury address
    ///   - Instructor fee → instructor address
    ///
    /// Both transfers happen in the same transaction — no escrow needed.
    /// A student cannot enroll in the same course twice.
    ///
    /// # Arguments
    /// - `student`   — student's Stellar address (must sign)
    /// - `course_id` — the course to enroll in
    pub fn enroll(env: Env, student: Address, course_id: String) {
        student.require_auth();

        if env.storage().instance().get(&DataKey::PlatformPaused).unwrap_or(false) {
            panic!("platform is paused");
        }

        let mut course = Self::get_course_internal(&env, &course_id);

        if course.status != CourseStatus::Active {
            panic!("course is not available for enrollment");
        }

        // Prevent duplicate enrollment
        if env
            .storage()
            .persistent()
            .has(&DataKey::Enrollment(student.clone(), course_id.clone()))
        {
            panic!("already enrolled in this course");
        }

        // Validate course token against the admin-approved whitelist
        if !env
            .storage()
            .instance()
            .has(&DataKey::ApprovedToken(course.token.clone()))
        {
            panic!("course token is not approved");
        }

        let token_client = token::Client::new(&env, &course.token);

        // Calculate revenue split (overflow-safe)
        let pct = course.platform_fee_percent as i128;
        let platform_amount = course
            .price
            .checked_mul(pct)
            .map(|v| v / 100)
            .unwrap_or_else(|| panic!("overflow computing platform fee"));
        let instructor_amount = course.price - platform_amount;

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

        // Perform transfers atomically:
        // First transfer the full price from the student to the contract's own address.
        if course.price > 0 {
            token_client.transfer(&student, &env.current_contract_address(), &course.price);

            // Distribute platform fee to treasury
            if platform_amount > 0 {
                token_client.transfer(&env.current_contract_address(), &treasury, &platform_amount);
            }

            // Distribute instructor share directly to instructor
            if instructor_amount > 0 {
                token_client.transfer(&env.current_contract_address(), &course.instructor, &instructor_amount);
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
            evidence_hash: None,
        };

        env.storage()
            .persistent()
            .set(
                &DataKey::Enrollment(student.clone(), course_id.clone()),
                &enrollment,
            );

        env.storage().persistent().extend_ttl(
            &DataKey::Enrollment(student.clone(), course_id.clone()),
            100_000,
            6_300_000,
        );

        // Update course stats
        course.total_enrollments += 1;
        course.active_enrollments += 1;
        course.total_earned += course.price;
        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);

        // Calculate payment split for event
        let platform_amount = (course.price * course.platform_fee_percent as i128) / 100;
        let instructor_amount = course.price - platform_amount;

        // Emit enhanced enrollment event with payment split details
        events::student_enrolled(&env, &student, &course_id, course.price, platform_amount, instructor_amount);
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
        Self::require_admin(&env, &admin);
        env.storage().instance().extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        if evidence_hash.is_none() {
            student.require_auth();
        }

        let mut enrollment = Self::get_enrollment_internal(&env, &student, &course_id);

        if enrollment.completed {
            panic!("already marked as completed");
        }

        let has_evidence = evidence_hash.is_some();
        enrollment.completed = true;
        enrollment.evidence_hash = evidence_hash;

        env.storage()
            .persistent()
            .set(
                &DataKey::Enrollment(student.clone(), course_id.clone()),
                &enrollment,
            );

        // Update active enrollments count on course
        let mut course = Self::get_course_internal(&env, &course_id);
        if course.active_enrollments > 0 {
            course.active_enrollments -= 1;
            env.storage()
                .persistent()
                .set(&DataKey::Course(course_id.clone()), &course);
        }

        // Emit enhanced completion event with evidence status
        events::course_completed(&env, &admin, &student, &course_id, has_evidence);
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
        student: Address,
        course_id: String,
        course_title: String,
    ) -> String {
        admin.require_auth();
        Self::require_admin(&env, &admin);
        env.storage().instance().extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        if certificate_id.len() > Self::MAX_COURSE_ID_LEN {
            panic!("certificate_id exceeds maximum length");
        }
        if course_title.len() > Self::MAX_COURSE_TITLE_LEN {
            panic!("course_title exceeds maximum length");
        }

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

        let course = Self::get_course_internal(&env, &course_id);

        let certificate = Certificate {
            id: certificate_id.clone(),
            student: student.clone(),
            course_id: course_id.clone(),
            course_title,
            instructor: course.instructor,
            issued_at_ledger: env.ledger().sequence(),
            revoked: false,
            revoked_by: None,
            revoked_at_ledger: None,
            revocation_reason: None,
        };

        env.storage()
            .persistent()
            .set(&DataKey::Certificate(certificate_id.clone()), &certificate);

        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Certificate(certificate_id.clone()), 100_000, 6_300_000);

        // Mark enrollment as certificate issued
        enrollment.certificate_issued = true;
        env.storage()
            .persistent()
            .set(
                &DataKey::Enrollment(student.clone(), course_id.clone()),
                &enrollment,
            );

        // Emit enhanced certificate issuance event with all details
        events::certificate_issued(&env, &admin, &certificate_id, &student, &course_id, &certificate.course_title);

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
        Self::require_admin(&env, &admin);
        env.storage().instance().extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

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

        // Emit enhanced revocation event with reason and all audit trail details
        events::certificate_revoked(&env, &admin, &certificate_id, &cert.student, &cert.course_id, &reason);
    }

    // ----------------------------------------------------------
    // ADMIN MANAGEMENT
    // ----------------------------------------------------------

    pub fn pause_platform(env: Env, admin: Address) {
        admin.require_auth();
        Self::require_admin(&env, &admin);
        env.storage().instance().set(&DataKey::PlatformPaused, &true);

        // Emit platform pause event with admin and ledger sequence
        events::platform_paused(&env, &admin);
    }

    pub fn unpause_platform(env: Env, admin: Address) {
        admin.require_auth();
        Self::require_admin(&env, &admin);
        env.storage().instance().set(&DataKey::PlatformPaused, &false);

        // Emit platform unpause event with admin and ledger sequence
        events::platform_unpaused(&env, &admin);
    }

    pub fn withdraw_tokens(env: Env, admin: Address, token: Address, amount: i128, destination: Address) {
        admin.require_auth();
        Self::require_admin(&env, &admin);
        let token_client = token::Client::new(&env, &token);
        token_client.transfer(&env.current_contract_address(), &destination, &amount);

        // Emit token withdrawal event with admin, amount, and destination
        events::tokens_withdrawn(&env, &admin, &token, amount, &destination);
    }

    /// Propose a new admin address (step 1 of two-step transfer).
    /// The new admin must call accept_admin() to complete the handover.
    pub fn transfer_admin(env: Env, admin1: Address, admin2: Address, new_admin: Address, new_secondary_admin: Address) {
        admin1.require_auth();
        admin2.require_auth();
        Self::require_multi_admin(&env, &admin1, &admin2);
        env.storage().instance().extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);
        env.storage().instance().set(&DataKey::PendingAdmin, &new_admin);
        env.storage().instance().set(&DataKey::PendingSecondaryAdmin, &new_secondary_admin);

        // Emit admin transfer proposal event with both proposers and new admins
        events::admin_transfer_proposed(&env, &admin1, &admin2, &new_admin, &new_secondary_admin);
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

        env.storage().instance().set(&DataKey::Admin, &new_admin);
        env.storage().instance().set(&DataKey::SecondaryAdmin, &new_secondary_admin);
        env.storage().instance().remove(&DataKey::PendingAdmin);
        env.storage().instance().remove(&DataKey::PendingSecondaryAdmin);

        // Emit admin transfer acceptance event with new admin details
        events::admin_transfer_accepted(&env, &new_admin, &new_secondary_admin);
    }

    /// Update the platform treasury address.
    pub fn update_treasury(env: Env, admin1: Address, admin2: Address, new_treasury: Address) {
        admin1.require_auth();
        admin2.require_auth();
        Self::require_multi_admin(&env, &admin1, &admin2);
        
        if new_treasury == env.current_contract_address() {
            panic!("treasury cannot be the contract address");
        }

        env.storage().instance().extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        let effective_ledger = env.ledger().sequence() + 100;
        let update = TreasuryUpdate {
            address: new_treasury.clone(),
            effective_ledger,
        };
        env.storage().instance().set(&DataKey::PendingTreasury, &update);

        // Emit treasury update event with effective ledger sequence
        events::treasury_updated(&env, &admin1, &admin2, &new_treasury, effective_ledger);
    }

    /// Update the default platform fee percentage.
    pub fn update_default_fee(env: Env, admin: Address, new_fee_pct: u32) {
        admin.require_auth();
        Self::require_admin(&env, &admin);
        if new_fee_pct > 100 {
            panic!("fee percentage cannot exceed 100");
        }
        env.storage().instance().extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);
        env.storage().instance().set(&DataKey::DefaultFee, &new_fee_pct);

        // Emit default fee update event
        events::default_fee_updated(&env, &admin, new_fee_pct);
    }

    /// Admin adds a token contract address to the enrollment whitelist.
    pub fn add_approved_token(env: Env, admin: Address, token: Address) {
        admin.require_auth();
        Self::require_admin(&env, &admin);
        env.storage().instance().set(&DataKey::ApprovedToken(token.clone()), &true);

        // Emit token whitelist addition event
        events::token_whitelisted(&env, &admin, &token);
    }

    /// Admin removes a token contract address from the enrollment whitelist.
    pub fn remove_approved_token(env: Env, admin: Address, token: Address) {
        admin.require_auth();
        Self::require_admin(&env, &admin);
        env.storage().instance().remove(&DataKey::ApprovedToken(token.clone()));

        // Emit token removal from whitelist event
        events::token_removed_from_whitelist(&env, &admin, &token);
    }

    // ----------------------------------------------------------
    // READ-ONLY QUERIES
    // ----------------------------------------------------------

    /// Get a course record by ID
    pub fn get_course(env: Env, course_id: String) -> Course {
        Self::get_course_internal(&env, &course_id)
    }

    /// Get an enrollment record for a student + course pair
    pub fn get_enrollment(env: Env, student: Address, course_id: String) -> Enrollment {
        Self::get_enrollment_internal(&env, &student, &course_id)
    }

    /// Get a certificate by ID
    pub fn get_certificate(env: Env, certificate_id: String) -> Certificate {
        env.storage()
            .persistent()
            .get::<DataKey, Certificate>(&DataKey::Certificate(certificate_id))
            .unwrap_or_else(|| panic!("certificate not found"))
    }

    /// Check whether a student is enrolled in a course
    pub fn is_enrolled(env: Env, student: Address, course_id: String) -> bool {
        env.storage()
            .persistent()
            .has(&DataKey::Enrollment(student, course_id))
    }

    /// Check whether a student has completed a course
    pub fn has_completed(env: Env, student: Address, course_id: String) -> bool {
        if let Some(enrollment) = env
            .storage()
            .persistent()
            .get::<DataKey, Enrollment>(&DataKey::Enrollment(student, course_id))
        {
            enrollment.completed
        } else {
            false
        }
    }

    /// Verify a certificate — returns true if it exists and has not been revoked
    pub fn verify_certificate(env: Env, certificate_id: String) -> bool {
        if let Some(cert) = env
            .storage()
            .persistent()
            .get::<DataKey, Certificate>(&DataKey::Certificate(certificate_id))
        {
            !cert.revoked
        } else {
            false
        }
    }

    /// Get the current platform fee percentage
    pub fn get_platform_fee(env: Env) -> u32 {
        env.storage()
            .instance()
            .get::<DataKey, u32>(&DataKey::DefaultFee)
            .unwrap_or(20)
    }

    // ----------------------------------------------------------
    // INTERNAL HELPERS
    // ----------------------------------------------------------

    fn get_course_internal(env: &Env, course_id: &String) -> Course {
        env.storage()
            .persistent()
            .get(&DataKey::Course(course_id.clone()))
            .unwrap_or_else(|| panic!("course not found"))
    }

    fn get_enrollment_internal(env: &Env, student: &Address, course_id: &String) -> Enrollment {
        env.storage()
            .persistent()
            .get(&DataKey::Enrollment(student.clone(), course_id.clone()))
            .unwrap_or_else(|| panic!("enrollment not found"))
    }

    fn is_admin(env: &Env, caller: &Address) -> bool {
        let admin: Option<Address> = env.storage().instance().get(&DataKey::Admin);
        admin.map(|a| a == *caller).unwrap_or(false)
    }

    fn require_admin(env: &Env, caller: &Address) {
        if !Self::is_admin(env, caller) {
            panic!("unauthorized: caller is not admin");
        }
    }

    fn require_multi_admin(env: &Env, caller1: &Address, caller2: &Address) {
        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        let secondary_admin: Address = env.storage().instance().get(&DataKey::SecondaryAdmin).unwrap();

        if (*caller1 == admin && *caller2 == secondary_admin) || (*caller1 == secondary_admin && *caller2 == admin) {
            // ok
        } else {
            panic!("unauthorized: requires both admin signatures");
        }
    }
}

mod test;

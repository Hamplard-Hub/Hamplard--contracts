#![no_std]

use soroban_sdk::{contract, contractimpl, contracttype, token, Address, Env, String, Symbol, Vec};

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
    pub max_capacity: Option<u32>,
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
    /// Accumulated instructor earnings per (instructor, token) pair (in stroops)
    InstructorEarnings(Address, Address),
    /// Number of courses registered by an instructor
    InstructorCourseCount(Address),
    /// Maximum number of courses an instructor can register
    MaxCoursesPerInstructor,
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
    pub fn init(
        env: Env,
        admin: Address,
        secondary_admin: Address,
        treasury: Address,
        default_fee_pct: u32,
        max_courses_per_instructor: u32,
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
        max_capacity: Option<u32>,
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
            token,
            total_enrollments: 0,
            active_enrollments: 0,
            total_earned: 0,
            status: CourseStatus::Pending,
            created_at_ledger: env.ledger().sequence(),
            max_capacity, // ← ADD THIS
        };

        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);

        env.storage().persistent().extend_ttl(
            &DataKey::Course(course_id.clone()),
            Self::PERSISTENT_TTL_THRESHOLD,
            Self::PERSISTENT_TTL_EXTEND_TO,
        );
        env.storage().instance().set(
            &DataKey::InstructorCourseCount(instructor.clone()),
            &(current_count + 1),
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

        let mut course = Self::get_course_internal(&env, &course_id);

        if course.status != CourseStatus::Pending {
            panic!("course is not pending approval");
        }

        course.status = CourseStatus::Active;
        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);

        env.events().publish(
            (Symbol::new(&env, "course_approved"), course_id.clone()),
            course_id,
        );
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

        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        if course.status != CourseStatus::Active {
            panic!("course is not active");
        }

        course.status = CourseStatus::Paused;
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

        let mut course = Self::get_course_internal(&env, &course_id);

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

        let mut course = Self::get_course_internal(&env, &course_id);

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

        env.events().publish(
            (Symbol::new(&env, "course_archived"), course_id.clone()),
            course_id,
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

        let course = Self::get_course_internal(env, course_id);

        if *student == course.instructor {
            panic!("instructor cannot enroll in own course");
        }

        if course.status != CourseStatus::Active {
            panic!("course is not available for enrollment");
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
    }

    fn enroll_internal(env: &Env, student: &Address, course_id: &String) {
        Self::validate_enrollment(env, student, course_id);

        let mut course = Self::get_course_internal(env, course_id);
        let token_client = token::Client::new(env, &course.token);

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

        // Transfer full price from student to contract, then distribute platform fee
        if course.price > 0 {
            token_client.transfer(student, &env.current_contract_address(), &course.price);

            if platform_amount > 0 {
                token_client.transfer(&env.current_contract_address(), &treasury, &platform_amount);
            }

            // Credit instructor earnings — pull-based withdrawal model
            if instructor_amount > 0 {
                Self::credit_instructor_earnings(
                    env,
                    &course.instructor,
                    &course.token,
                    instructor_amount,
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
        env.storage()
            .persistent()
            .set(&DataKey::Course(course_id.clone()), &course);

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

        if enrollment.completed {
            panic!("already marked as completed");
        }

        enrollment.completed = true;
        enrollment.evidence_hash = evidence_hash;

        env.storage().persistent().set(
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

        env.events().publish(
            (Symbol::new(&env, "course_completed"), course_id.clone()),
            student,
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
        student: Address,
        course_id: String,
        course_title: String,
        enrollment_reference: String,
    ) -> String {
        admin.require_auth();
        Self::require_admin(&env, &admin, "issue_certificate");
        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

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
            enrollment_reference,
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

        env.events().publish(
            (
                Symbol::new(&env, "certificate_issued"),
                certificate_id.clone(),
            ),
            (student, course_id),
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
    }

    pub fn unpause_platform(env: Env, admin: Address) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "unpause_platform");
        env.storage()
            .instance()
            .set(&DataKey::PlatformPaused, &false);
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
            new_admin,
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

        env.storage()
            .instance()
            .extend_ttl(Self::INSTANCE_TTL_THRESHOLD, Self::INSTANCE_TTL_EXTEND_TO);

        let effective_ledger = env.ledger().sequence() + 100;
        let update = TreasuryUpdate {
            address: new_treasury,
            effective_ledger,
        };
        env.storage()
            .instance()
            .set(&DataKey::PendingTreasury, &update);
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
    }

    /// Admin adds a token contract address to the enrollment whitelist.
    pub fn add_approved_token(env: Env, admin: Address, token: Address) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "add_approved_token");
        env.storage()
            .instance()
            .set(&DataKey::ApprovedToken(token), &true);
    }

    /// Admin removes a token contract address from the enrollment whitelist.
    pub fn remove_approved_token(env: Env, admin: Address, token: Address) {
        admin.require_auth();
        Self::require_admin(&env, &admin, "remove_approved_token");
        env.storage()
            .instance()
            .remove(&DataKey::ApprovedToken(token));
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
    }

    /// Get the current per-instructor course registration limit.
    pub fn get_max_courses_limit(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::MaxCoursesPerInstructor)
            .unwrap_or(50)
    }

    /// Get the number of courses an instructor has registered.
    pub fn get_instructor_course_count(env: Env, instructor: Address) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::InstructorCourseCount(instructor))
            .unwrap_or(0)
    }

    // ----------------------------------------------------------
    // READ-ONLY QUERIES
    // ----------------------------------------------------------

    /// Get a course record by ID
    pub fn get_course(env: Env, course_id: String) -> Course {
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
    pub fn get_enrollment(env: Env, caller: Address, student: Address, course_id: String) -> Option<Enrollment> {
        caller.require_auth();
        let is_admin = Self::is_admin(&env, &caller);
        let course = Self::get_course_internal(&env, &course_id);
        let is_instructor = caller == course.instructor;
        
        if caller != student && !is_admin && !is_instructor {
            panic!("unauthorized");
        }
        env.storage()
            .persistent()
            .get(&DataKey::Enrollment(student, course_id))
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

    fn require_admin(env: &Env, caller: &Address, operation: &str) {
        if !Self::is_admin(env, caller) {
            panic!("unauthorized: {} - caller is not admin", operation);
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
}

mod test;

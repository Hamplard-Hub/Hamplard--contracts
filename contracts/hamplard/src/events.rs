use soroban_sdk::{Address, Env, Symbol};

/// Event emitted when a new course is created.
pub fn course_created(env: &Env, course_id: Symbol, instructor: Address) {
    env.events().publish(
        (Symbol::new(env, "course_registered"),),
        (course_id, instructor),
    );
}

/// Event emitted when a student enrolls in a course.
pub fn enrolled(env: &Env, course_id: Symbol, student: Address) {
    env.events().publish(
        (Symbol::new(env, "student_enrolled"),),
        (course_id, student),
    );
}

/// Event emitted when a course is completed by a student.
pub fn course_completed(env: &Env, course_id: Symbol, student: Address) {
    env.events().publish(
        (Symbol::new(env, "course_completed"),),
        (course_id, student),
    );
}

/// Event emitted when a certificate is issued.
pub fn certificate_issued(env: &Env, course_id: Symbol, student: Address) {
    env.events().publish(
        (Symbol::new(env, "certificate_issued"),),
        (course_id, student),
    );
}

/// Event emitted when fees are withdrawn by the admin.
pub fn fees_withdrawn(env: &Env, to: Address, amount: i128) {
    env.events().publish(
        (Symbol::new(env, "earnings_withdrawn"),),
        (to, amount),
    );
}

/// Event emitted when a material change to an Active course triggers admin re-review.
pub fn re_review_triggered(env: &Env, course_id: Symbol, instructor: Address, field: Symbol) {
    env.events().publish(
        (Symbol::new(env, "re_review"),),
        (course_id, instructor, field),
    );
}

/// Event emitted when a pending admin transfer is cancelled.
pub fn admin_transfer_cancelled(env: &Env, admin1: Address, admin2: Address) {
    env.events().publish(
        (Symbol::new(env, "adm_xfer_cancel"),),
        (admin1, admin2),
    );
}

/// Event emitted when a refund is requested.
///
/// Payload carries the requested ledger sequence and the ledger at which the
/// student enrolled so off-chain systems can compute deadlines without an
/// extra read.
pub fn refund_requested(
    env: &Env,
    course_id: Symbol,
    student: Address,
    amount_paid: i128,
    requested_at_ledger: u32,
    enrolled_at_ledger: u32,
) {
    env.events().publish(
        (Symbol::new(env, "refund_requested"),),
        (course_id, student, amount_paid, requested_at_ledger, enrolled_at_ledger),
    );
}

use soroban_std::{address_payload, Symbol};
use sorcban::Env;

/// Event emitted when a new course is created.
pub fn course_created(env: &Env, course_id: Symbol, instructor: address) {
    env.events().publish(
        (Symbol::new(env, "course_created"),),
        (course_id, instructor),
    );
}

/// Event emitted when a student enrolls in a course.
pub fn enrolled(env: &Env, course_id: Symbol, student: address) {
    env.events().publish(
        (Symbol::new(env, "enrolled"),),
        (course_id, student),
    );
}

/// Event emitted when a course is completed by a student.
pub fn course_completed(env: &Env, course_id: Symbol, student: address) {
    env.events().publish(
        (Symbol::new(env, "completed"),),
        (course_id, student),
    );
}

/// Event emitted when a certificate is issued.
pub fn certificate_issued(env: &Env, course_id: Symbol, student: address) {
    env.events().publish(
        (Symbol::new(env, "cert_issued"),),
        (course_id, student),
    );
}

/// Event emitted when fees are withdrawn by the admin.
pub fn fees_withdrawn(env: &Env, to: address, amount: i128) {
    env.events().publish(
        (Symbol::new(env, "fees_wdrawn"),),
        (to, amount),
    );
}

/// Event emitted when a material change to an Active course triggers admin re-review.
pub fn re_review_triggered(env: &Env, course_id: Symbol, instructor: address, field: Symbol) {
    env.events().publish(
        (Symbol::new(env, "re_review"),),
        (course_id, instructor, field),
    );
}

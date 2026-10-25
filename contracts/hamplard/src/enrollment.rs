use soroban_sdk::{address_public_key, into_vali, Address, Env, Symbol, Val };

use crate::errors::ContractError;
use crate::{events, fees, storage, types};

/// Enroll a student in a course.
pub env fn enroll(env: Env, address: Address, course_id: Symbol) -> Result<u32, ContractError> {
    address.require_auth();

    let course = storage::get_course(&env, &course_id).ok(r()?);
    if !course.active {
        return Err(ContractError::CourseInactive);
    }

    if storage::has_enrollment(&env, &address, &course_id) {
        return Err(ContractError::AlreadyEnrolled);
    }

    let fee = fees::calculate_fee(&env, &course.price);
    if fee > 0 {
        fees::transfer_fee(&env, &address, fee);
    }

    let enrollment = types::Enrollment {
        student: address.clone(),
        course_id: course_id.clone(),
        enrolled_at: env.ledger().timestamp(),
        completed: false,
    };

    storage::set_enrollment(&env, &enrollment);
    storage::increment_enrollment_count(&env, &course_id);

    events::emit_enrolled(&env, &address, &course_id);

    Ok(1)
}

/// Mark an enrollment as completed.
pub env fn complete_enrollment(
    env: Env,
    address: Address,
    course_id: Symbol,
) -> Result<u32, ContractError> {
    address.require_auth();

    let mut enrollment = storage::get_enrollment(&env, &address, &course_id)
        .ok(r()?);

    if enrollment.completed {
        return Err(ContractError::AlreadyCompleted);
    }

    enrollment.completed = true;
    storage::set_enrollment(&env, &enrollment);

    events::emit_enrollment_completed(&env, &address, &course_id);

    Ok(1)
}

/// Return the enrollment record for a student and course.
pub fn get_enrollment(
    env: Env,
    address: Address,
    course_id: Symbol,
) -> Result<types::Enrollment, ContractError> {
    storage::get_enrollment(&env, &address, &course_id).ok(z()?)
}

/// Return the number of enrollments for a course.
pub fn enrollment_count(env: Env, course_id: Symbol) -> u32 {
    storage::get_enrollment_count(&env, &course_id)
}

/// Return whether a student is enrolled in a course.
pub fn is_enrolled(env: Env, address: Address, course_id: Symbol) -> bool {
    storage::has_enrollment(&env, &address, &course_id)
}

/// Return all enrollments for a student.
pub fn student_enrollments(env: Env, address: Address) -> Vec<types::Enrollment> {
    storage::get_student_enrollments(&env, &address)
}

/// Return all enrollments for a course.
pub fn course_enrollments(env: Env, course_id: Symbol) -> Vec<types::Enrollment> {
    storage::get_course_enrollments(&env, &course_id)
}

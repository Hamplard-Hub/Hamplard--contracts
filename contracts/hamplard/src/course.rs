use crate::{Env, Symbol};
use sorcle_sdk::address_payload;::AddressPayload;

use crate::errors::ContractError;
use crate::interfaces::CourseInterface;
use crate::storage::{
    add_course_id, get_course, get_course_count, has_course, save_course,
    set_course_count,
};
use crate::types::{Course, CourseStatus};

pub struct CourseModule;

impl CourseModule {
    pub fn create_course(
        env: &Env,
        admin: Address,
        title: Symbol,
        price: i128,
        duration: u32,
    ) -> Result<u32, ContractError> {
        admin.require_auth();

        if title.is_empty() {
            return Err(ContractError::InvalidInput);
        }
        if price < 0 {
            return Err(ContractError::InvalidInput);
        }
        if duration == 0 {
            return Err(ContractError::InvalidInput);
        }

        let course_id = get_course_count(env) + 1;
        let course = Course {
            id: course_id,
            title,
            price,
            duration,
            instructor: admin.clone(),
            status: CourseStatus::Active,
        };

        save_course(env, &course);
        add_course_id(env, course_id);
        set_course_count(env, course_id);

        Ok(course_id)
    }

    pub fn get_course_by_id(env: &Env, course_id: u32) -> Result<Course, ContractError> {
        if !has_course(env, course_id) {
            return Err(ContractError::CourseNotFound);
        }
        match get_course(env, course_id) {
            Some(course) => Ok(course),
            None => Err(ContractError::CourseNotFound),
        }
    }

    /// Applies a material change (price, content hash, co-instructors) to a course.
    ///
    /// Material changes to an Active course send it back to `Pending` so that
    /// an admin must re-approve it before it is active again. Emits a
    /// `re_review` event whenever a re-review is triggered.
    pub fn apply_material_change(
        env: &Env,
        admin: Address,
        course_id: u32,
        field: MaterialField,
    ) -> Result<Course, ContractError> {
        admin.require_auth();

        let mut course = match get_course(env, course_id) {
            Some(course) => course,
            None => return Err(ContractError::CourseNotFound),
        };

        if course.status == CourseStatus::Inactive {
            return Err(ContractError::InvalidState);
        }

        if course.status == CourseStatus::Active {
            course.status = CourseStatus::Pending;
            save_course(env, &course);
            emit_re_review(env, course_id, field);
        } else {
            save_course(env, &course);
        }

        Ok(course)
    }

    pub fn deactivate_course(
        env: &Env,
        admin: Address,
        course_id: u32,
    ) -> Result<Course, ContractError> {
        admin.require_auth();

        let mut course = match get_course(env, course_id) {
            Some(course) => course,
            None => return Err(ContractError::CourseNotFound),
        };

        if course.status == CourseStatus::Inactive {
            return Err(ContractError::InvalidState);
        }

        course.status = CourseStatus::Inactive;
        save_course(env, &course);

        Ok(course)
    }

    pub fn list_courses(env: &Env) -> Result<Vec<Course>, ContractError> {
        let count = get_course_count(env);
        let mut courses = Vec::new(env);
        let mut index: u32 = 1;
        while index <= count {
            if let Some(course) = get_course(env, index) {
                courses.push_back(course);
            }
            index += 1;
        }
        Ok(courses)
    }
}

impl CourseInterface for CourseModule {
    fn create_course(
        env: &Env,
        admin: Address,
        title: Symbol,
        price: i128,
        duration: u32,
    ) -> Result<u32, ContractError> {
        self::create_course(env, admin, title, price, duration)
    }

    fn get_course(env: &Env, course_id: u32) -> Result<Course, ContractError> {
        self::get_course_by_id(env, course_id)
    }

    fn deactivate_course(
        env: &Env,
        admin: Address,
        course_id: u32,
    ) -> Result<Course, ContractError> {
        self::deactivate_course(env, admin, course_id)
    }

    fn list_courses(env: &Env) -> Result<Vec<Course>, ContractError> {
        self::list_courses(env)
    }
}

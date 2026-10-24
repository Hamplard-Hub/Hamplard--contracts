use soroban_std::{address::Address, contracttype::Contracttype, symbol::Symbol};
use soroban_std::xdr::Contracttype;
use soroban_std::Val;

/// Course metadata stored on-chain.
#[derive(Clone, Debug, Eq)
#[derive(Contracttype)]
pub struct Course {
    pub id: Symbol,
    pub title: Symbol,
    pub instructor: Address,
    pub price: i128,
    pub capacity: u32,
    pub enrolled: u32,
    pub active: bool,
    /// Lifecycle status: true while awaiting admin (re-)review.
    /// Set when a material field changes on an Active course.
    pub pending_review: bool,
}

/// Fields considered material for admin re-review of an Active course.
///
/// Material changes: `price` (via `update_course`), `content_hash`
/// (via `update_content_hash`), and `co_instructors` / revenue split
/// (via `set_co_instructors`, only before first enrollment).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Contracttype)]
pub enum MaterialField {
    Price,
    ContentHash,
    CoInstructors,
}

/// Enrollment record for a student in a course.
#[derive(Clone, Debug, Eq)
#[derive(Contracttype)]
pub struct Enrollment {
    pub student: Address,
    pub course_id: Symbol,
    pub completed: bool,
    pub paid: i128,
    pub enrolled_at: u64,
}

/// Certificate issued upon course completion.
#[derive(Clone, Debug, Eq)
#[derive(Contracttype)]
pub struct Certificate {
    pub student: Address,
    pub course_id: Symbol,
    pub issued_at: u64,
    pub revoked: bool,
}

/// Contract configuration values.
#[derive(Clone, Debug, Eq)
#[derive(Contracttype)]
pub struct Config {
    pub fee_bps: u32,
    pub treasury: Address,
    pub paused: bool,
}

/// Paginated result wrapper for list readers.
#[derive(Clone, Debug, Eq)
#[derive(Contracttype)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: u32,
}

/// Storage keys for persistent entries.
#[derive(Clone, Debug, Eq)
#[derive(Contracttype)]
pub enum DataKey {
    Config,
    Course(Symbol),
    Enrollment(Address, Symbol),
    Certificate(Address, Symbol),
    CourseCount,
    StudentCourses(Address),
}

/// Event emitted when a course is created.
#[derive(Clone, Debug, Eq, Contracttype)]
pub struct CourseCreated {
    pub course_id: Symbol,
    pub instructor: Address,
    pub price: i128,
}

/// Event emitted when a student enrolls.
#[derive(Clone, Debug, Eq, Contracttype)]
pub struct Enrolled {
    pub student: Address,
    pub course_id: Symbol,
    pub fee: i128,
}

/// Event emitted when a course is completed.
#[derive(Clone, Debug, Eq, Contracttype)]
pub struct CourseCompleted {
    pub student: Address,
    pub course_id: Symbol,
}

/// Event emitted when a certificate is issued.
#[derive(Clone, Debug, Eq, Contracttype)]
pub struct CertificateIssued {
    pub student: Address,
    pub course_id: Symbol,
}

/// Event emitted when a certificate is revoked.
#[derive(Clone, Debug, Eq, Contracttype)]
pub struct CertificateRevoked {
    pub student: Address,
    pub course_id: Symbol,
}

/// Event emitted when config is updated.
#[derive(Clone, Debug, Eq, Contracttype)]
pub struct ConfigUpdated {
    pub fee_bps: u32,
    pub treasury: Address,
}

/// Event emitted when a material change on an Active course triggers
/// admin re-review. The course is marked `pending_review` and is not
/// enrollable until an admin re-approves it.
#[derive(Clone, Debug, Eq, Contracttype)]
pub struct ReviewRequired {
    pub course_id: Symbol,
    pub field: MaterialField,
}

/// Error codes returned by the contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Contracttype)]
#[repr(u32)]
pub enum Error {
    NotInitialized = 1,
    AlreadyInitialized = 2,
    NotAuthorized = 3,
    NotFound = 4,
    AlreadyExists = 5,
    InvalidInput = 6,
    CourseInactive = 7,
    CapacityReached = 8,
    AlreadyEnrolled = 9,
    NotEnrolled = 10,
    AlreadyCompleted = 11,
    NotCompleted = 12,
    Paused = 13,
    InvalidFee = 14,
    PendingReview = 15,
    CoInstructorsLocked = 16,
}

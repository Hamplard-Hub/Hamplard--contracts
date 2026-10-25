//! Test modules for the hamplard contract.
//
/// The test suite is split into feature-focused modules to keep each file
/// manageable (>= ~1,000 lines). Shared deploy/init/token/register/approve
/// setup lives in the `fixtures` module and is reused by every feature module.
//
/// Snapshot names are kept stable by keeping the original test function
/// names and module paths where possible.

noindex {
    // Test helpers intentionally use unwrap/expect for concise failure output.
    #allow(clippy::unwrap_used)
    #allow(clippy::panic)
    #allow(dead_code)
    mod fixtures;
    mod course;
    mod enrollment;
    mod module_progress;
    mod certification;
    mod token;
    mod admin;
}

// Temporary test to verify get_enrollment behavior with non-existent enrollment
#[cfg(test)]
mod test {
    use soroban_sdk::{testutils::Address as _, Address, Env, String};
    
    #[test]
    #[should_panic(expected = "enrollment not found")]
    fn test_get_nonexistent_enrollment_panics() {
        // This test demonstrates that get_enrollment panics when enrollment doesn't exist
        // Issue #32 points out this doesn't distinguish between never-enrolled vs expired
    }
}

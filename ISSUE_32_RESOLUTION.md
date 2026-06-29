# Issue #32 Resolution: get_enrollment() Ambiguous Return Handling

## Issue Description
The `get_enrollment()` query function returns storage data directly without distinguishing between:
1. **Never enrolled** - student never signed up for the course
2. **Expired enrollment** - enrollment record existed but has exceeded TTL and been garbage collected

The current implementation panics with "enrollment not found" in both cases, making it impossible for callers to determine the actual state.

## Current Implementation Analysis

### Function Implementation (Lines 1092-1094, 1156-1161)

```rust
// Public API
pub fn get_enrollment(env: Env, student: Address, course_id: String) -> Enrollment {
    Self::get_enrollment_internal(&env, &student, &course_id)
}

// Internal implementation
fn get_enrollment_internal(env: &Env, student: &Address, course_id: &String) -> Enrollment {
    env.storage()
        .persistent()
        .get(&DataKey::Enrollment(student.clone(), course_id.clone()))
        .unwrap_or_else(|| panic!("enrollment not found"))  // ❌ ISSUE HERE
}
```

### Current Behavior
- ❌ **Returns:** `Enrollment` (always expects data to exist)
- ❌ **On missing data:** Panics with "enrollment not found"
- ❌ **Cannot distinguish:** Never-enrolled vs expired record
- ❌ **No graceful handling:** Callers cannot check existence before querying

### Comparison with Related Functions

The codebase **already has examples** of better patterns:

#### 1. `is_enrolled()` - Existence Check (Lines 1105-1108)
```rust
pub fn is_enrolled(env: Env, student: Address, course_id: String) -> bool {
    env.storage()
        .persistent()
        .has(&DataKey::Enrollment(student, course_id))
}
```
✅ Returns `bool` - gracefully handles missing data

#### 2. `has_completed()` - Safe Query with Option (Lines 1112-1121)
```rust
pub fn has_completed(env: Env, student: Address, course_id: String) -> bool {
    if let Some(enrollment) = env
        .storage()
        .persistent()
        .get::<DataKey, Enrollment>(&DataKey::Enrollment(student, course_id))
    {
        enrollment.completed
    } else {
        false  // ✅ Gracefully returns false if not found
    }
}
```
✅ Uses `Option<Enrollment>` - distinguishes between exists/not-exists

## Problem Impact

### For Integrating Systems
1. **Cannot differentiate states:**
   - Did the student never enroll?
   - Did the enrollment record expire?
   - Is the data corrupted?

2. **Cannot implement safe retry logic:**
   - Every query must be wrapped in panic handling
   - Cannot pre-check before querying

3. **Breaks idempotency patterns:**
   - Cannot safely query to check if enrollment exists
   - Must use separate `is_enrolled()` call first

### For UI/Frontend Integration
```javascript
// Current problematic pattern
try {
    const enrollment = await contract.get_enrollment({student, course_id});
    // Success - enrollment exists
} catch (e) {
    if (e.includes("enrollment not found")) {
        // But WHY was it not found?
        // - Never enrolled?
        // - Record expired?
        // - Invalid course_id?
        // No way to tell!
    }
}
```

## Recommended Solution

### Option 1: Return `Option<Enrollment>` (Preferred)

```rust
/// Get an enrollment record for a student + course pair
/// Returns None if the student has never enrolled or the record has expired
pub fn get_enrollment(env: Env, student: Address, course_id: String) -> Option<Enrollment> {
    env.storage()
        .persistent()
        .get(&DataKey::Enrollment(student, course_id))
}
```

**Benefits:**
- ✅ Idiomatic Rust pattern
- ✅ Graceful handling of missing data
- ✅ Consistent with `has_completed()` pattern
- ✅ Allows callers to distinguish presence/absence
- ✅ No breaking panic behavior

**Usage:**
```rust
if let Some(enrollment) = contract.get_enrollment(&env, &student, &course_id) {
    // Handle existing enrollment
    assert_eq!(enrollment.amount_paid, expected_amount);
} else {
    // Handle non-existent enrollment
    // Could be never-enrolled or expired
    // Caller can check is_enrolled() if needed to distinguish
}
```

### Option 2: Add Separate Safe Query Function

Keep existing `get_enrollment()` for internal use, add new safe query:

```rust
/// Get an enrollment record safely - returns None if not found
pub fn try_get_enrollment(env: Env, student: Address, course_id: String) -> Option<Enrollment> {
    env.storage()
        .persistent()
        .get(&DataKey::Enrollment(student, course_id))
}

/// Get an enrollment record - panics if not found (legacy behavior)
pub fn get_enrollment(env: Env, student: Address, course_id: String) -> Enrollment {
    Self::try_get_enrollment(env.clone(), student, course_id)
        .unwrap_or_else(|| panic!("enrollment not found"))
}
```

**Benefits:**
- ✅ Backward compatible - existing behavior preserved
- ✅ Provides safe alternative for new code
- ❌ API inconsistency - two ways to do same thing

## Distinguishing Never-Enrolled vs Expired

Even with `Option<Enrollment>`, callers still cannot definitively distinguish between:
- **Never enrolled** - record never created
- **Expired** - record created but TTL exceeded

### Mitigation Strategy

Callers can use a combination approach:

```rust
// Check 1: Does enrollment exist?
let enrollment_opt = contract.get_enrollment(&env, &student, &course_id);

match enrollment_opt {
    Some(enrollment) => {
        // Enrollment record exists and is active
        handle_active_enrollment(enrollment);
    }
    None => {
        // Could be never-enrolled OR expired
        // Additional context needed:
        
        // Check course stats for historical enrollment count
        let course = contract.get_course(&env, &course_id);
        
        // Check if student appears in any course events
        // (Events have independent TTL and may persist longer)
        
        // For most use cases: treat as "not enrolled" regardless of reason
        handle_not_enrolled();
    }
}
```

### Why Full Distinction is Impossible

Soroban's storage model does not preserve "tombstones" for expired entries:
- Once TTL expires, data is garbage collected
- No record that data ever existed
- No timestamp of expiration
- This is by design for efficiency

**Workaround:** Event emission provides persistent audit trail (if needed):
```rust
// When enrollment is created (already implemented)
env.events().publish(
    (Symbol::new(env, "student_enrolled"), course_id.clone()),
    (student.clone(), course_id.clone(), amount_paid, enrolled_at_ledger)
);
```

Events can be indexed off-chain to maintain historical enrollment records beyond TTL.

## Implementation Plan

### Step 1: Update Function Signature
```rust
pub fn get_enrollment(env: Env, student: Address, course_id: String) -> Option<Enrollment> {
    env.storage()
        .persistent()
        .get(&DataKey::Enrollment(student, course_id))
}
```

### Step 2: Update Internal Usage

Update all internal calls to handle `Option`:

**File:** `contracts/hamplard/src/lib.rs`

**Line 751:** `mark_completed()`
```rust
// Before
let mut enrollment = Self::get_enrollment_internal(&env, &student, &course_id);

// After
let mut enrollment = Self::get_enrollment_internal(&env, &student, &course_id)
    .unwrap_or_else(|| panic!("enrollment not found"));
```

**Line 814:** `issue_certificate()`
```rust
// Before
let mut enrollment = Self::get_enrollment_internal(&env, &student, &course_id);

// After
let mut enrollment = Self::get_enrollment_internal(&env, &student, &course_id)
    .unwrap_or_else(|| panic!("enrollment not found"));
```

### Step 3: Update Tests

Update all test expectations to handle `Option`:

```rust
// Before
let enrollment = client.get_enrollment(&student, &course_id);
assert_eq!(enrollment.amount_paid, price);

// After
let enrollment = client.get_enrollment(&student, &course_id)
    .expect("enrollment should exist");
assert_eq!(enrollment.amount_paid, price);
```

### Step 4: Update Documentation

Add clear documentation about the return behavior:

```rust
/// Get an enrollment record for a student + course pair
/// 
/// Returns `Some(Enrollment)` if the record exists and has not expired.
/// Returns `None` if:
/// - The student has never enrolled in this course
/// - The enrollment record has exceeded its TTL and been garbage collected
/// 
/// To check only existence without retrieving data, use `is_enrolled()`.
/// To distinguish between never-enrolled and expired, query historical events off-chain.
pub fn get_enrollment(env: Env, student: Address, course_id: String) -> Option<Enrollment>
```

## Testing Strategy

### Test Cases to Add

1. **Test getting existing enrollment**
   ```rust
   #[test]
   fn test_get_enrollment_existing() {
       // Setup and enroll
       let enrollment = client.get_enrollment(&student, &course_id);
       assert!(enrollment.is_some());
   }
   ```

2. **Test getting non-existent enrollment**
   ```rust
   #[test]
   fn test_get_enrollment_never_enrolled() {
       let enrollment = client.get_enrollment(&student, &course_id);
       assert!(enrollment.is_none());
   }
   ```

3. **Test getting enrollment after massive ledger advance**
   ```rust
   #[test]
   fn test_get_enrollment_potentially_expired() {
       // Enroll, then advance ledger beyond TTL
       env.ledger().with_mut(|l| l.sequence_number += 10_000_000);
       
       // Depending on TTL extension logic, may return None
       let enrollment = client.get_enrollment(&student, &course_id);
       // Test appropriate behavior
   }
   ```

## Current TTL Configuration

From `lib.rs` lines 154-155:
```rust
const PERSISTENT_TTL_THRESHOLD: u32 = 6_000_000;  // ~1 year
const PERSISTENT_TTL_EXTEND_TO: u32 = 6_300_000;  // ~1 year
```

Enrollments are set with extended TTL on creation (lines 642-645):
```rust
env.storage().persistent().extend_ttl(
    &DataKey::Enrollment(student.clone(), course_id.clone()),
    Self::PERSISTENT_TTL_THRESHOLD,
    Self::PERSISTENT_TTL_EXTEND_TO,
);
```

**Note:** Enrollment TTL is NOT extended on subsequent operations (mark_completed, issue_certificate), which means:
- Enrollments can expire even if student completed course
- Certificates persist independently with their own TTL
- This is likely intentional design to reduce costs

## Alternative: Status Enum (More Complex)

If precise state tracking is required, consider adding an enrollment status:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnrollmentStatus {
    Active,
    Completed,
    Expired,
    Withdrawn,
}

pub struct Enrollment {
    pub student: Address,
    pub course_id: String,
    pub amount_paid: i128,
    pub enrolled_at_ledger: u32,
    pub status: EnrollmentStatus,  // New field
    pub certificate_issued: bool,
    pub evidence_hash: Option<String>,
}
```

This would require:
- Schema migration
- Additional state management logic
- More complex expired state handling
- Still cannot recover expired data that's garbage collected

**Verdict:** Not recommended due to complexity and limited benefit over `Option` approach.

## Recommendation Summary

**Implement Option 1:** Change `get_enrollment()` to return `Option<Enrollment>`

**Rationale:**
1. ✅ Follows Rust best practices
2. ✅ Consistent with similar query functions in codebase (`has_completed`)
3. ✅ Enables graceful error handling
4. ✅ Callers can distinguish presence/absence
5. ✅ Simple implementation with minimal changes
6. ✅ Allows callers to implement appropriate handling logic

**Migration Note:** This is a breaking API change. Existing contracts/clients will need to update their calls to handle `Option`.

## Files to Modify

1. **contracts/hamplard/src/lib.rs**
   - Line 1092-1094: Update `get_enrollment()` signature and implementation
   - Line 1156-1161: Update `get_enrollment_internal()` to return `Option`
   - Line 751: Update `mark_completed()` to handle Option
   - Line 814: Update `issue_certificate()` to handle Option

2. **contracts/hamplard/src/test.rs**
   - Update all test calls to handle `Option` return type
   - Add new tests for non-existent enrollment cases

## Conclusion

**Issue Status:** Valid - `get_enrollment()` does not gracefully handle missing data

**Recommended Action:** Change return type to `Option<Enrollment>`

**Priority:** Medium - Affects API usability and integrating systems

**Breaking Change:** Yes - requires updates to all callers

**Complexity:** Low - straightforward implementation

The fix aligns with Rust idioms and existing patterns in the codebase while providing callers with the ability to handle missing enrollments gracefully.

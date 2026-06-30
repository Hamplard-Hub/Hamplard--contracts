with open("contracts/hamplard/src/test.rs", "r") as f:
    content = f.read()

# Fix 1: register_course calls with 5 args (missing max_capacity) -> add &None
# The pattern is: register_course(&instructor, &course_id, &price, &token_id, &0u32)
# Need to add , &None)
# Lines 2436 and 2477 (the new test code we added for issues 47 and 50)
content = content.replace(
    'client.register_course(&instructor, &course_id, &100_000_000, &token_id, &0u32);',
    'client.register_course(&instructor, &course_id, &100_000_000, &token_id, &0u32, &None);'
)

# Fix 2: get_enrollment returns Option<Enrollment> now, so .amount_paid needs .unwrap()
# Lines 2384, 2388, 2392
content = content.replace(
    '''    // Student can access
    let enrollment = client.get_enrollment(&student, &student, &course_id);
    assert_eq!(enrollment.amount_paid, 100_000_000);

    // Instructor can access
    let enrollment2 = client.get_enrollment(&instructor, &student, &course_id);
    assert_eq!(enrollment2.amount_paid, 100_000_000);

    // Admin can access
    let enrollment3 = client.get_enrollment(&admin, &student, &course_id);
    assert_eq!(enrollment3.amount_paid, 100_000_000);''',
    '''    // Student can access
    let enrollment = client.get_enrollment(&student, &student, &course_id).unwrap();
    assert_eq!(enrollment.amount_paid, 100_000_000);

    // Instructor can access
    let enrollment2 = client.get_enrollment(&instructor, &student, &course_id).unwrap();
    assert_eq!(enrollment2.amount_paid, 100_000_000);

    // Admin can access
    let enrollment3 = client.get_enrollment(&admin, &student, &course_id).unwrap();
    assert_eq!(enrollment3.amount_paid, 100_000_000);'''
)

# Fix 3: issue_certificate is now 7 args (added enrollment_reference)
# Upstream HEAD didn't change the test calls - the HEAD was OK. So the new lib.rs has a new signature.
# Let's check what lib.rs line 856 says - issue_certificate now has an extra `enrollment_reference` param
# All calls to issue_certificate with 5 user args need a 6th arg `enrollment_reference`
# The pattern is:
#   client.issue_certificate(&admin, &cert_id, &student, &course_id, &title);
# becomes:
#   client.issue_certificate(&admin, &cert_id, &student, &course_id, &title, &String::from_str(&env, ""));
# 
# But the HEAD tests also have 5 user args so this is an upstream change we need to handle.
# Let's find all issue_certificate calls and add the extra arg.

import re

# Pattern for inline (one-liner) issue_certificate
content = re.sub(
    r'client\.issue_certificate\((&\w+), (&\w+), (&\w+), (&\w+), (&\w+)\);',
    r'client.issue_certificate(\1, \2, \3, \4, \5, &String::from_str(&env, ""));',
    content
)

# Also fix the TTL test get_enrollment with old 2-arg signature at line 1498
content = content.replace(
    '    let enrollment = client.get_enrollment(&student, &course_id);\n    assert_eq!(enrollment.unwrap().amount_paid, 100_000_000);\n    assert!(client.is_enrolled(&student, &course_id));',
    '    let enrollment = client.get_enrollment(&student, &student, &course_id).unwrap();\n    assert_eq!(enrollment.amount_paid, 100_000_000);\n    assert!(client.is_enrolled(&student, &course_id));'
)

with open("contracts/hamplard/src/test.rs", "w") as f:
    f.write(content)

print("Done!")

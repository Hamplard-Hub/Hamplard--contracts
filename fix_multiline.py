import re

with open("contracts/hamplard/src/test.rs", "r") as f:
    content = f.read()

# Fix multi-line issue_certificate calls that have 5 args (missing enrollment_reference)
# Pattern: client.issue_certificate(\n        &X,\n        &Y,\n        &Z,\n        &W,\n        &V,\n    );
# The function takes: admin, cert_id, student, course_id, course_title, enrollment_reference

# Find all multi-line issue_certificate calls and add the missing argument
# We identify them by the closing pattern: 5-arg close -> ),\n    );
def fix_issue_cert(m):
    block = m.group(0)
    # Count the args by counting comma-delimited lines inside the parentheses
    # We add &String::from_str(&env, "") before the closing );
    if 'String::from_str(&env, "")' in block:
        # Already fixed
        return block
    return block[:-8] + '        &String::from_str(&env, ""),\n    );\n'

# Match multi-line issue_certificate calls
pattern = r'client\.issue_certificate\(\n(?:        [^\n]+,\n){5}    \);\n'
content = re.sub(pattern, fix_issue_cert, content)

with open("contracts/hamplard/src/test.rs", "w") as f:
    f.write(content)
print("Done!")

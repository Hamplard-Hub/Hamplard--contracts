import re

with open("contracts/hamplard/src/test.rs", "r") as f:
    content = f.read()

# Fix the conflict at 303
content = content.replace('''<<<<<<< HEAD
    let enrollment = client.get_enrollment(&student, &String::from_str(&env, "COURSE-FASHION-001"));
    let enrollment = enrollment.unwrap();
=======
    let enrollment = client.get_enrollment(&student, &student, &String::from_str(&env, "COURSE-FASHION-001"));
>>>>>>> 7b3b92e (Fix issues #47, #48, #49, #50)''', 
'''    let enrollment = client.get_enrollment(&student, &student, &String::from_str(&env, "COURSE-FASHION-001")).unwrap();''')

# Fix the conflict at 321
content = content.replace('''<<<<<<< HEAD
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
=======
    let (env, contract_id, token_id, admin, _sec_admin, treasury, instructor) = setup();
>>>>>>> 7b3b92e (Fix issues #47, #48, #49, #50)''', 
'''    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();''')

# Fix the conflict at 344
content = content.replace('''<<<<<<< HEAD
    let enrollment = client.get_enrollment(&student, &String::from_str(&env, "COURSE-FREE-001"));
    assert_eq!(enrollment.unwrap().amount_paid, 0);
=======
    let enrollment = client.get_enrollment(&student, &student, &String::from_str(&env, "COURSE-FREE-001"));
    assert_eq!(enrollment.amount_paid, 0);
>>>>>>> 7b3b92e (Fix issues #47, #48, #49, #50)''', 
'''    let enrollment = client.get_enrollment(&student, &student, &String::from_str(&env, "COURSE-FREE-001")).unwrap();
    assert_eq!(enrollment.amount_paid, 0);''')

# Fix the conflict at 360
content = content.replace('''<<<<<<< HEAD
    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();
=======
    let (env, contract_id, token_id, admin, _sec_admin, treasury, instructor) = setup();
>>>>>>> 7b3b92e (Fix issues #47, #48, #49, #50)''', 
'''    let (env, contract_id, token_id, admin, _sec_admin, _treasury, instructor) = setup();''')

# Fix the conflict at 609
content = content.replace('''<<<<<<< HEAD
    let enrollment = client.get_enrollment(&student, &course_id);
    assert!(enrollment.unwrap().certificate_issued);
=======
    let enrollment = client.get_enrollment(&student, &student, &course_id);
    assert!(enrollment.certificate_issued);
>>>>>>> 7b3b92e (Fix issues #47, #48, #49, #50)''', 
'''    let enrollment = client.get_enrollment(&student, &student, &course_id).unwrap();
    assert!(enrollment.certificate_issued);''')

# Fix the conflict at 905
content = content.replace('''<<<<<<< HEAD
    let enrollment = client.get_enrollment(&student, &course_id);
    assert_eq!(enrollment.unwrap().evidence_hash, Some(hash));
=======
    let enrollment = client.get_enrollment(&student, &student, &course_id);
    assert_eq!(enrollment.evidence_hash, Some(hash));
>>>>>>> 7b3b92e (Fix issues #47, #48, #49, #50)''', 
'''    let enrollment = client.get_enrollment(&student, &student, &course_id).unwrap();
    assert_eq!(enrollment.evidence_hash, Some(hash));''')

with open("contracts/hamplard/src/test.rs", "w") as f:
    f.write(content)

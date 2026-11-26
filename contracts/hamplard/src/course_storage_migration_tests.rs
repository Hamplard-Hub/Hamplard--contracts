use super::*;

#[test]
fn legacy_course_record_is_migrated_with_defaults() {
    let env = Env::default();
    let contract_id = Address::generate(&env);
    let course_id = String::from_str(&env, "LEGACY-COURSE");
    let instructor = Address::generate(&env);
    let token = Address::generate(&env);

    let course = Course {
        id: course_id.clone(),
        instructor,
        price: 1_000_000,
        platform_fee_percent: 10,
        token,
        total_enrollments: 2,
        active_enrollments: 1,
        total_earned: 2_000_000,
        total_refunded: 400_000,
        refund_count: 1,
        status: CourseStatus::Active,
        created_at_ledger: 1,
        max_capacity: Some(10),
        capacity_mode: CapacityMode::Concurrent,
        enrollment_start_ledger: None,
        enrollment_expiry_ledgers: None,
        min_completion_ledgers: 0,
        version: 1,
        last_updated_ledger: 1,
        expires_at_ledger: None,
        content_hash: BytesN::from_array(&env, &[0; 32]),
        max_certificates: None,
        certificates_issued: 0,
        certificates_revoked: 0,
        prerequisite_course_ids: Vec::new(&env),
        archived_at_ledger: None,
        categories: Vec::from_array(&env, [String::from_str(&env, "legacy-category")]),
        co_instructors: Vec::new(&env),
        paused_by: PausedBy::None,
    };

    let value: Val = course.into_val(&env);
    let mut legacy_fields = Map::<Symbol, Val>::try_from_val(&env, &value).unwrap();
    for field in [
        "total_refunded",
        "refund_count",
        "capacity_mode",
        "categories",
        "co_instructors",
        "certificates_revoked",
        "paused_by",
    ]
    .iter()
    {
        legacy_fields.remove(Symbol::new(&env, field));
    }

    let key = DataKey::Course(course_id.clone());
    env.as_contract(&contract_id, || {
        env.storage().persistent().set(&key, &legacy_fields);
        HamplardContract::migrate_course_record(&env, &key);

        let migrated: Course = env.storage().persistent().get(&key).unwrap();
        assert_eq!(migrated.id, course_id);
        assert_eq!(migrated.capacity_mode, CapacityMode::Lifetime);
        assert_eq!(migrated.total_refunded, 0);
        assert_eq!(migrated.refund_count, 0);
        assert!(migrated.categories.is_empty());
        assert!(migrated.co_instructors.is_empty());
    });
}

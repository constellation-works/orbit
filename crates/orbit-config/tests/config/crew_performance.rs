// `workflow.crew_performance_types`: the ordered task-type list admits and
// rejects through the same registry row a config load uses.

/// The admitted type list one load of `body` gives, or its error.
fn load_types(body: &str) -> Result<Vec<CrewPerformanceType>, String> {
    load_config(body).map(|config| config.snapshot.crew_performance_types().to_vec())
}

#[test]
fn crew_performance_types_default_to_empty_and_admit_an_ordered_list() {
    assert_eq!(load_types("").expect("empty loads"), Vec::new());
    let types = load_types(
        "[[workflow.crew_performance_types]]\n\
         name = \"review\"\n\
         tags = [\"no-diff-expected\"]\n\
         [[workflow.crew_performance_types]]\n\
         name = \"ui\"\n\
         tags = [\"ui\"]\n\
         paths = [\"web/**\"]\n",
    )
    .expect("valid list loads");
    assert_eq!(types.len(), 2);
    assert_eq!(types[0].name, "review");
    assert_eq!(types[1].paths, ["web/**"]);
}

#[test]
fn crew_performance_types_reject_duplicate_reserved_and_matchless_entries() {
    let duplicate = load_types(
        "[[workflow.crew_performance_types]]\nname = \"review\"\ntags = [\"a\"]\n\
         [[workflow.crew_performance_types]]\nname = \"review\"\ntags = [\"b\"]\n",
    )
    .expect_err("duplicate names are refused");
    assert!(duplicate.contains("review"), "{duplicate}");
    let reserved =
        load_types("[[workflow.crew_performance_types]]\nname = \"other\"\ntags = [\"a\"]\n")
            .expect_err("other is refused");
    assert!(reserved.contains("other"), "{reserved}");
    let matchless = load_types("[[workflow.crew_performance_types]]\nname = \"empty\"\n")
        .expect_err("an entry with neither tags nor paths is refused");
    assert!(matchless.contains("empty"), "{matchless}");
}

#[test]
fn classification_takes_the_first_match_and_falls_back_to_other() {
    let types = vec![
        CrewPerformanceType {
            name: "review".into(),
            tags: vec!["no-diff-expected".into()],
            paths: Vec::new(),
        },
        CrewPerformanceType {
            name: "ui".into(),
            tags: vec!["ui".into()],
            paths: vec!["web/**".into()],
        },
    ];
    // Tag match beats a later path match; first entry wins.
    assert_eq!(
        classify_task_type(
            &types,
            &["no-diff-expected".into()],
            &["file:web/app.rs".into()],
        ),
        "review"
    );
    assert_eq!(
        classify_task_type(&types, &[], &["file:web/app.rs".into()]),
        "ui"
    );
    assert_eq!(classify_task_type(&types, &[], &["dir:web".into()]), "ui");
    // A canonical symbol selector matches by its backing file.
    assert_eq!(
        classify_task_type(&types, &[], &["symbol:web/src/lib.rs#App:struct".into()],),
        "ui"
    );
    // A symbol selector without `#<name>:<kind>` names no path.
    assert_eq!(
        classify_task_type(&types, &[], &["symbol:web::App".into()]),
        "other"
    );
    // Anchor-less selectors (module:, command:) never match a path glob.
    let wildcard_types = vec![CrewPerformanceType {
        name: "wildcard".into(),
        tags: Vec::new(),
        paths: vec!["*".into()],
    }];
    assert_eq!(
        classify_task_type(&wildcard_types, &[], &["module:orbit_core".into()]),
        "other"
    );
    assert_eq!(
        classify_task_type(&wildcard_types, &[], &["command:task".into()]),
        "other"
    );
    assert_eq!(classify_task_type(&[], &["ui".into()], &[]), "other");
}

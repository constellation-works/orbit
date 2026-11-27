#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use orbit_core::application::task::TaskAddParams;
use orbit_core::runtime::OrbitRuntime;
use orbit_core::runtime::context::ActorIdentity;
use orbit_core::test_support::fixture_engine::RunLifecycle;
use orbit_types::task::{
    DefectSeverity, RegressionClassification, RegressionEscape, RegressionNature, SeveritySource,
    TASK_ARTIFACT_SCHEMA_VERSION, TASK_COMMENTS_FILE_NAME, TASK_EVENTS_FILE_NAME, TaskCommentRowV2,
    TaskEventRowV2, TaskRelation, TaskRelationType, TaskStatus,
};
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::json;

use crate::isolated_cli_fixture::Fixture;

/// `orbit codebase analytics` prints the tool's rankings, and `orbit codebase
/// show` follows a ranked path's rate to the tasks behind it.
#[test]
fn codebase_cli_ranks_paths_and_lists_the_tasks_behind_a_count() {
    if crate::isolated_test::run_in_child(
        "codebase_cli::codebase_cli_ranks_paths_and_lists_the_tasks_behind_a_count",
    ) {
        return;
    }

    let fixture = Fixture::new();
    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    let (regressed, regression) = {
        // A managed run exports worker authority that would bind the fixture
        // runtime to the live store.
        let _authority = orbit_common::test_env::unset(
            orbit_common::test_env::INHERITED_AUTHORITY_ENV
                .iter()
                .copied(),
        );
        let runtime = OrbitRuntime::from_roots(&fixture.root, workspace_root)
            .unwrap()
            .with_actor(ActorIdentity::human("codebase-fixture"));
        let add = |title: &str, status: TaskStatus, relations: Vec<TaskRelation>| {
            runtime
                .add_task(TaskAddParams {
                    title: title.into(),
                    description: "codebase analytics fixture".into(),
                    context_files: vec!["file:src/lib.rs".into()],
                    relations,
                    status: Some(status),
                    ..Default::default()
                })
                .unwrap()
                .id
                .to_string()
        };
        let regressed = add("regressed", TaskStatus::Done, vec![]);
        add("clean", TaskStatus::Done, vec![]);
        let regression = add(
            "regression",
            TaskStatus::Backlog,
            vec![TaskRelation {
                relation_type: TaskRelationType::RegressionFrom,
                target: regressed.parse().unwrap(),
                regression: Some(RegressionClassification {
                    nature: RegressionNature::Unclassified,
                    escape: RegressionEscape::Unclassified,
                    severity: DefectSeverity::Medium,
                    severity_source: SeveritySource::Filer,
                    sites: vec!["symbol:src/lib.rs#parse:function".into()],
                }),
            }],
        );
        (regressed, regression)
    };

    let selection = ["--window", "30d", "--min-support", "1"];
    let mut analytics =
        fixture.json(&[&["codebase", "analytics"][..], &selection, &["--json"]].concat());
    assert_eq!(
        analytics["regressions"]["rows"][0]["path"], "src/lib.rs",
        "{analytics}"
    );
    assert_eq!(analytics["regressions"]["rows"][0]["numerator"], 1);
    assert_eq!(analytics["regressions"]["rows"][0]["denominator"], 2);
    let input = json!({"action": "analytics", "window": "30d", "min_support": 1 }).to_string();
    let mut tool = fixture.json(&["tool", "run", "orbit.codebase", "--full", "--input", &input]);
    for payload in [&mut analytics, &mut tool] {
        let object = payload.as_object_mut().unwrap();
        object.remove("computed_at");
        object.remove("since");
    }
    assert_eq!(analytics, tool, "the command prints the tool's payload");

    let detail = fixture.json(
        &[
            &["codebase", "show", "src/lib.rs"][..],
            &selection,
            &["--json"],
        ]
        .concat(),
    );
    let regressions = &detail["rankings"]["regressions"];
    assert_eq!(regressions["rank"], 1);
    assert_eq!(regressions["numerator"], 1);
    assert_eq!(regressions["tasks"][0]["task_id"], regressed.as_str());
    assert_eq!(
        regressions["tasks"][0]["regression_tasks"],
        json!([regression])
    );
    assert_eq!(regressions["tasks"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        regressions["tasks"][0]["sites"],
        json!(["symbol:src/lib.rs#parse:function"])
    );

    assert_eq!(detail["absent_at_head"], true);
    assert_eq!(analytics["regressions"]["rows"][0]["absent_at_head"], true);
    let text = fixture
        .command(&[&["codebase", "analytics"][..], &selection].concat())
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(
        String::from_utf8(text)
            .unwrap()
            .contains("src/lib.rs [absent at HEAD]")
    );

    // The human view lists the regressed task with the task that regressed it.
    let human = fixture
        .command(&[&["codebase", "show", "src/lib.rs"][..], &selection].concat())
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human = String::from_utf8(human).unwrap();
    assert!(
        human.lines().any(|line| {
            line.trim_start().starts_with(regressed.as_str()) && line.contains(regression.as_str())
        }),
        "the regressed task and its regression are listed:\n{human}"
    );

    assert!(human.contains("absent at HEAD"));

    // The human view gives each link's dimensions, and the regression flags
    // filter what the rankings count.
    let link = human
        .lines()
        .find(|line| line.trim_start().starts_with(regression.as_str()))
        .unwrap_or_else(|| panic!("a line per regression link:\n{human}"));
    for dimension in ["unclassified", "other", "operator"] {
        assert!(link.contains(dimension), "{dimension} in {link}");
    }
    // It names the sites that attribute the task to the path, and lists each
    // site under the path with its regressed-task count.
    assert!(
        human
            .lines()
            .any(|line| line.trim() == "sites: symbol:src/lib.rs#parse:function"),
        "the regressed task's sites are listed:\n{human}"
    );
    assert!(
        human.lines().any(|line| line
            .trim_start()
            .starts_with("parse (function)  1 regressed tasks")),
        "the path's sites are listed with their counts:\n{human}"
    );
    let filtered = fixture.json(
        &[
            &["codebase", "analytics"][..],
            &selection,
            &[
                "--nature",
                "unclassified",
                "--detected-by",
                "operator",
                "--json",
            ],
        ]
        .concat(),
    );
    assert_eq!(filtered["regression_filter"]["nature"], "unclassified");
    assert_eq!(filtered["regressions"]["matching_links"], 1, "{filtered}");
    assert_eq!(filtered["regressions"]["rows"][0]["numerator"], 1);
    let excluded = fixture.json(
        &[
            &["codebase", "analytics"][..],
            &selection,
            &["--nature", "broke_existing", "--json"],
        ]
        .concat(),
    );
    assert_eq!(excluded["regressions"]["matching_links"], 0, "{excluded}");
    let unclassified = excluded["regressions"]["breakdowns"]["nature"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["value"] == "unclassified")
        .unwrap();
    assert_eq!(
        unclassified["links"], 1,
        "a breakdown counts past its own filter"
    );
    let refused = fixture
        .command(&["codebase", "analytics", "--layer", "frontend"])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let refused = String::from_utf8_lossy(&refused);
    assert!(
        refused.contains("frontend"),
        "an unknown layer is refused: {refused}"
    );

    let refused = fixture
        .command(&["codebase", "show", "src/absent.rs"])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let refused = String::from_utf8_lossy(&refused);
    assert!(
        refused.contains("src/absent.rs"),
        "a path nothing touched is refused by name: {refused}"
    );
}

/// A path's show marks the absent partner of each coupled pair, in JSON and in the text view.
#[test]
fn codebase_cli_marks_the_absent_partner_in_a_path_show() {
    if crate::isolated_test::run_in_child(
        "codebase_cli::codebase_cli_marks_the_absent_partner_in_a_path_show",
    ) {
        return;
    }

    let fixture = Fixture::new();
    for path in ["src/live.rs", "src/gone.rs"] {
        let file = fixture.repo.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, "coupling fixture\n").unwrap();
    }

    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    {
        let _authority = orbit_common::test_env::unset(
            orbit_common::test_env::INHERITED_AUTHORITY_ENV
                .iter()
                .copied(),
        );
        let runtime = OrbitRuntime::from_roots(&fixture.root, workspace_root)
            .unwrap()
            .with_actor(ActorIdentity::human("codebase-fixture"));
        let task = runtime
            .add_task(TaskAddParams {
                title: "coupled paths".into(),
                description: "codebase analytics fixture".into(),
                context_files: vec!["file:src/live.rs".into(), "file:src/gone.rs".into()],
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .unwrap();
        let reservation = runtime
            .reserve_task_locks(
                json!({"task_ids": [task.id.to_string()], "ttl_seconds": 600}),
                None,
                None,
                None,
            )
            .unwrap();
        assert_eq!(reservation["reserved"], true, "{reservation}");
    }
    fs::remove_file(fixture.repo.join("src/gone.rs")).unwrap();

    let selection = ["--window", "30d", "--min-support", "1"];
    let detail = fixture.json(
        &[
            &["codebase", "show", "src/live.rs"][..],
            &selection,
            &["--json"],
        ]
        .concat(),
    );
    assert!(detail.get("absent_at_head").is_none(), "{detail}");
    let pairs = detail["rankings"]["change_coupling"]["pairs"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(pairs.len(), 1, "{detail}");
    assert_eq!(
        pairs[0]["absent_paths_at_head"],
        json!(["src/gone.rs"]),
        "{detail}"
    );

    let human = fixture
        .command(&[&["codebase", "show", "src/live.rs"][..], &selection].concat())
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human = String::from_utf8(human).unwrap();
    assert!(
        human.contains("src/gone.rs [absent at HEAD]"),
        "the show text labels the absent partner of the pair:\n{human}"
    );
}

/// A width-limited analytics table keeps both pair member identities and the absence marker visible.
#[cfg(unix)]
#[test]
fn codebase_cli_marks_the_absent_coupling_member_at_terminal_width() {
    if crate::isolated_test::run_in_child(
        "codebase_cli::codebase_cli_marks_the_absent_coupling_member_at_terminal_width",
    ) {
        return;
    }

    let first = "crates/orbit-core/src/application/codebase/sections/long_left_subsystem/analytics_path/first_absent_member.rs";
    let second = "crates/orbit-core/src/application/codebase/sections/long_right_subsystem/analytics_path/second_present_member.rs";
    let (plain, piped_table, rendered, analytics) =
        run_coupling_pair_analytics(first, second, true, false);

    let pair = &analytics["change_coupling"]["rows"][0];
    assert_eq!(pair["paths"], json!([first, second]), "{analytics}");
    assert_eq!(pair["absent_paths_at_head"], json!([first]), "{analytics}");
    assert_eq!(pair["numerator"], 1, "{analytics}");
    assert_eq!(pair["denominator"], 1, "{analytics}");
    assert_eq!(pair["rate"], 1.0, "{analytics}");

    assert!(
        plain.contains(&format!("{first} [absent at HEAD] <-> {second}")),
        "untruncated plain analytics text retains both paths and the absence note:\n{plain}"
    );
    assert!(
        plain.contains("100%"),
        "untruncated plain analytics text retains the rate:\n{plain}"
    );
    assert!(
        piped_table.contains(&format!("{first} [absent at HEAD] <-> {second}")),
        "untruncated table analytics text retains both paths and the absence note:\n{piped_table}"
    );
    assert!(
        piped_table.contains("FIRST")
            && piped_table.contains("100%")
            && piped_table.contains("1/1"),
        "untruncated table analytics text retains absence indicator and rate:\n{piped_table}"
    );

    let absent_row = rendered
        .lines()
        .find(|line| line.contains("FIRST"))
        .unwrap_or_else(|| panic!("terminal table marks the first path as absent:\n{rendered}"));
    assert!(
        absent_row.contains('…'),
        "the pair paths are shortened while the absent member remains visible: {absent_row}"
    );
    assert!(
        absent_row.contains("irst_absent_member.rs"),
        "the shortened row preserves the first member's identity: {absent_row}"
    );
    assert!(
        absent_row.contains(" <-> "),
        "the shortened row preserves the pair separator: {absent_row}"
    );
    assert!(
        absent_row.contains("ond_present_member.rs"),
        "the shortened row preserves the second member's identity: {absent_row}"
    );
    assert!(
        absent_row.contains("100%") && absent_row.contains("1/1"),
        "the shortened row retains the coupling rate and count: {absent_row}"
    );
    assert!(
        rendered.contains("ABSENT MEMBER"),
        "the terminal table labels its absent-member column:\n{rendered}"
    );

    // Even when both members share a deep prefix and identical filename, both remain distinguishable.
    let left_mod =
        "crates/orbit-core/src/application/codebase/sections/left_shared_subsystem/mod.rs";
    let right_mod =
        "crates/orbit-core/src/application/codebase/sections/right_shared_subsystem/mod.rs";
    let (plain_mod, _, rendered_mod, analytics_mod) =
        run_coupling_pair_analytics(left_mod, right_mod, false, true);
    assert_eq!(
        analytics_mod["change_coupling"]["rows"][0]["absent_paths_at_head"],
        json!([right_mod]),
        "{analytics_mod}"
    );
    assert!(
        plain_mod.contains(&format!("{left_mod} <-> {right_mod} [absent at HEAD]")),
        "untruncated shared-filename pair retains full paths and absence note:\n{plain_mod}"
    );
    let mod_row = rendered_mod
        .lines()
        .find(|line| line.contains("SECOND"))
        .unwrap_or_else(|| {
            panic!("terminal table marks the second path as absent:\n{rendered_mod}")
        });
    assert!(
        mod_row.contains("left_share")
            && mod_row.contains("right_shar")
            && mod_row.matches("/mod.rs").count() == 2
            && mod_row.contains(" <-> "),
        "the shortened row disambiguates shared-filename members: {mod_row}"
    );
}

/// Ten coupling rows widen `#` to 2, so at 80 columns `PATHS` shrinks to the
/// even width 48 (odd pair budget 43): shared-prefix, shared-filename members
/// must still disambiguate instead of collapsing to fragments that differ by a
/// single prefix character (ORB-27878).
#[cfg(unix)]
#[test]
fn codebase_cli_disambiguates_shared_filename_pairs_at_even_column_width() {
    if crate::isolated_test::run_in_child(
        "codebase_cli::codebase_cli_disambiguates_shared_filename_pairs_at_even_column_width",
    ) {
        return;
    }

    let left_mod =
        "crates/orbit-core/src/application/codebase/sections/left_shared_subsystem/mod.rs";
    let right_mod =
        "crates/orbit-core/src/application/codebase/sections/right_shared_subsystem/mod.rs";
    let left_design = "docs/design/auto-tasks/2_design.md";
    let right_design = "docs/design/model-ranking/2_design.md";
    // `shared_alpha` rides two reservations so one pair lands at 1/2 (50%):
    // non-uniform RATE and COUNT columns survive uniform suppression, keeping
    // all five columns and `PATHS` at the even width 48.
    let filler: Vec<(String, String)> = (0..6)
        .map(|index| {
            (
                format!("src/filler_{index:02}_alpha.rs"),
                format!("src/filler_{index:02}_beta.rs"),
            )
        })
        .collect();
    let mut pairs: Vec<(&str, &str, bool, bool)> = vec![
        (left_mod, right_mod, false, true),
        (left_design, right_design, false, false),
        ("src/shared_alpha.rs", "src/shared_beta.rs", false, false),
        ("src/shared_alpha.rs", "src/shared_gamma.rs", false, false),
    ];
    for (first, second) in &filler {
        pairs.push((first.as_str(), second.as_str(), false, false));
    }
    let (_, _, rendered, analytics) = run_coupling_pairs_analytics(&pairs);

    let rows = analytics["change_coupling"]["rows"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(rows.len(), 10, "{analytics}");
    assert!(
        rows.iter()
            .any(|row| row["absent_paths_at_head"] == json!([right_mod])),
        "the ten-row analytics keeps the absent mod member: {analytics}"
    );
    assert!(
        rows.iter()
            .any(|row| row["numerator"] == json!(1) && row["denominator"] == json!(2)),
        "a half-rate pair keeps the RATE and COUNT columns visible: {analytics}"
    );

    let mod_row = rendered
        .lines()
        .find(|line| line.contains("SECOND"))
        .unwrap_or_else(|| {
            panic!("terminal table marks the second mod path as absent:\n{rendered}")
        });
    assert!(
        mod_row.contains("left_")
            && mod_row.contains("right_")
            && mod_row.matches("/mod.rs").count() == 2
            && mod_row.contains(" <-> "),
        "the even-width row keeps both shared-filename member identities: {mod_row}"
    );

    let design_row = rendered
        .lines()
        .find(|line| line.contains("design.md"))
        .unwrap_or_else(|| panic!("terminal table keeps the design pair's filename:\n{rendered}"));
    assert!(
        design_row.contains("auto-tasks")
            && design_row.contains("model-ran")
            && design_row.contains(" <-> "),
        "the even-width row keeps both design feature names: {design_row}"
    );

    assert!(
        rendered
            .lines()
            .filter(|line| !line.trim().is_empty())
            .all(|line| line.chars().count() <= 80),
        "every rendered terminal line stays within 80 columns:\n{rendered}"
    );
}

#[cfg(unix)]
fn run_coupling_pair_analytics(
    first: &str,
    second: &str,
    remove_first: bool,
    remove_second: bool,
) -> (String, String, String, serde_json::Value) {
    run_coupling_pairs_analytics(&[(first, second, remove_first, remove_second)])
}

/// Each `(first, second, remove_first, remove_second)` tuple reserves one
/// coupling pair; removals mark members absent at HEAD. Multiple pairs widen
/// the `#` column once the table reaches 10 rows, which shrinks `PATHS` to an
/// even width and exercises the odd pair budget.
#[cfg(unix)]
fn run_coupling_pairs_analytics(
    pairs: &[(&str, &str, bool, bool)],
) -> (String, String, String, serde_json::Value) {
    use std::io::Read as _;
    use std::os::fd::FromRawFd as _;
    use std::process::{Command, Stdio};

    let fixture = Fixture::new();
    for (first, second, _, _) in pairs {
        for path in [first, second] {
            let file = fixture.repo.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, "coupling fixture\n").unwrap();
        }
    }

    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    {
        let _authority = orbit_common::test_env::unset(
            orbit_common::test_env::INHERITED_AUTHORITY_ENV
                .iter()
                .copied(),
        );
        let runtime = OrbitRuntime::from_roots(&fixture.root, workspace_root)
            .unwrap()
            .with_actor(ActorIdentity::human("codebase-fixture"));
        for (index, (first, second, _, _)) in pairs.iter().enumerate() {
            let task = runtime
                .add_task(TaskAddParams {
                    title: format!("coupled paths {index}"),
                    description: "codebase analytics fixture".into(),
                    context_files: vec![format!("file:{first}"), format!("file:{second}")],
                    status: Some(TaskStatus::Backlog),
                    ..Default::default()
                })
                .unwrap();
            let id = task.id.to_string();
            let reservation = runtime
                .reserve_task_locks(
                    json!({"task_ids": [id], "ttl_seconds": 600}),
                    None,
                    None,
                    None,
                )
                .unwrap();
            assert_eq!(reservation["reserved"], true, "{reservation}");
            // History keeps released rows, so release at once: later pairs
            // may share a file without tripping the live lock.
            let reservation_id = reservation["reservation_id"].as_str().unwrap().to_string();
            let released = orbit_core::runtime::stored::locks::release(
                &runtime,
                json!({ "reservation_id": reservation_id }),
                None,
                None,
            )
            .unwrap();
            assert_eq!(released["released"], true, "{released}");
        }
    }
    for (first, second, remove_first, remove_second) in pairs {
        if *remove_first {
            fs::remove_file(fixture.repo.join(first)).unwrap();
        }
        if *remove_second {
            fs::remove_file(fixture.repo.join(second)).unwrap();
        }
    }

    let analytics = fixture.json(&[
        "codebase",
        "analytics",
        "--window",
        "30d",
        "--min-support",
        "1",
        "--json",
    ]);

    let run_untruncated = |format: &str| {
        let out = fixture
            .command(&[
                "--format",
                format,
                "codebase",
                "analytics",
                "--window",
                "30d",
                "--min-support",
                "1",
            ])
            .env("NO_COLOR", "1")
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        String::from_utf8(out).unwrap()
    };
    let plain = run_untruncated("plain");
    let piped_table = run_untruncated("table");

    let mut master_fd = -1;
    let mut slave_fd = -1;
    let mut dimensions = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: openpty initializes both descriptors on success and reads a valid winsize.
    let rc = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::addr_of_mut!(dimensions),
        )
    };
    assert_eq!(rc, 0, "openpty: {}", std::io::Error::last_os_error());
    // SAFETY: openpty returned two fresh, distinct descriptors, each wrapped once.
    let mut master = unsafe { fs::File::from_raw_fd(master_fd) };
    // SAFETY: openpty returned two fresh, distinct descriptors, each wrapped once.
    let slave = unsafe { fs::File::from_raw_fd(slave_fd) };
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("orbit"));
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let child = command
        .current_dir(&fixture.repo)
        .env("HOME", &fixture.home)
        .env("USERPROFILE", &fixture.home)
        .env("XDG_CONFIG_HOME", fixture.home.join("xdg"))
        .env("TERM", "xterm-256color")
        .env("COLUMNS", "80")
        .env("NO_COLOR", "1")
        .arg("--root")
        .arg(&fixture.root)
        .args([
            "codebase",
            "analytics",
            "--window",
            "30d",
            "--min-support",
            "1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::from(slave))
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(command);
    let mut rendered = Vec::new();
    let mut buffer = [0; 2048];
    loop {
        match master.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => rendered.extend_from_slice(&buffer[..read]),
            Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
            Err(error) => panic!("read terminal output: {error}"),
        }
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "analytics command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rendered = String::from_utf8(rendered).unwrap();
    (plain, piped_table, rendered, analytics)
}

/// The public CLI reads landed paths rather than inferring them from context.
#[test]
fn codebase_related_cli_reports_landed_markdown_partner() {
    if crate::isolated_test::run_in_child(
        "codebase_cli::codebase_related_cli_reports_landed_markdown_partner",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let shown = fixture.json(&["workspace", "show", "--json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    let task = {
        let _authority = orbit_common::test_env::unset(
            orbit_common::test_env::INHERITED_AUTHORITY_ENV
                .iter()
                .copied(),
        );
        let runtime = OrbitRuntime::from_roots(&fixture.root, workspace_root)
            .unwrap()
            .with_actor(ActorIdentity::human("codebase-fixture"));
        runtime
            .add_task(TaskAddParams {
                title: "landed fixture".into(),
                context_files: vec!["file:src/lib.rs".into()],
                status: Some(TaskStatus::Done),
                ..Default::default()
            })
            .unwrap()
            .id
            .to_string()
    };
    fs::create_dir_all(fixture.repo.join("src")).unwrap();
    fs::write(fixture.repo.join("src/lib.rs"), "pub fn example() {}\n").unwrap();
    fs::write(fixture.repo.join("design.md"), "design\n").unwrap();
    crate::git_repo::run(&fixture.repo, &["add", "src/lib.rs", "design.md"]);
    crate::git_repo::run(
        &fixture.repo,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "landed fixture",
        ],
    );
    let commit = crate::git_repo::run(&fixture.repo, &["rev-parse", "HEAD"]);
    record_landing(&fixture.root, &task, &commit);
    let payload = fixture.json(&[
        "codebase",
        "related",
        "src/lib.rs",
        "--top",
        "1",
        "--min-support",
        "1",
        "--json",
    ]);
    assert_eq!(payload["window"], "30d");
    assert_eq!(payload["rows"].as_array().unwrap().len(), 1);
    let partner = &payload["rows"][0];
    assert_eq!(partner["path"], "design.md");
    assert_eq!(partner["jaccard"], 1.0);
    assert_eq!(partner["conditional_rate"], 1.0);
    assert_eq!(partner["support"]["both"], 1);
    assert_eq!(partner["footprint_miss_count"], 1);
    assert!(partner["reservation_coupling"].is_null());
    assert_eq!(partner["tasks"][0]["task_id"], task);

    // At the default minimum the file's one landed task cannot reach it: the
    // command says so and names the minimum that lists a partner.
    let output = fixture
        .command(&["codebase", "related", "src/lib.rs", "--format", "plain"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "src/lib.rs changed in 1 landed task in this window, fewer than the minimum of 15"
        ) && stderr.contains("rerun with --min-support 1"),
        "{stderr}"
    );
}

/// The bundle directory of task `id` under `root`.
fn bundle(root: &Path, id: &str) -> Option<PathBuf> {
    for entry in fs::read_dir(root).ok()? {
        let path = entry.ok()?.path();
        if !path.is_dir() {
            continue;
        }
        if path.file_name().is_some_and(|name| name == id)
            && path.join(TASK_EVENTS_FILE_NAME).is_file()
        {
            return Some(path);
        }
        if let Some(found) = bundle(&path, id) {
            return Some(found);
        }
    }
    None
}

fn append_row(path: PathBuf, row: &impl serde::Serialize) {
    writeln!(
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap(),
        "{}",
        serde_json::to_string(row).unwrap()
    )
    .unwrap();
}

/// Record a verified local landing of `commit` for task `id`.
fn record_landing(root: &Path, id: &str, commit: &str) {
    let path = bundle(root, id).unwrap().join(TASK_EVENTS_FILE_NAME);
    let rows = fs::read_to_string(&path).unwrap().lines().count();
    let row = TaskEventRowV2 { schema_version: TASK_ARTIFACT_SCHEMA_VERSION, event_id: format!("EV-{:04}", rows+1), at: chrono::Utc::now(), by: "system".into(), event_type: "landing_completed".into(), note: Some(json!({"delivery":"local_candidate","candidate":commit,"verified":"landing_ref_contains_candidate"}).to_string()), from_status: None, to_status: Some(TaskStatus::Done), operation_id: None, decision: None, surface: None, caller: None };
    append_row(path, &row);
}

/// `orbit agents scorecard --role pilots` grades a task-pilot crew's
/// selectors against the landed diff, in its JSON and in its text table, and
/// `orbit codebase analytics` no longer carries a task-pilot ranking.
#[test]
fn pilot_cli_ranks_task_pilot_crews_against_the_landed_diff() {
    if crate::isolated_test::run_in_child(
        "codebase_cli::pilot_cli_ranks_task_pilot_crews_against_the_landed_diff",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let shown = fixture.json(&["workspace", "show", "--json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    let now = chrono::Utc::now();
    let task = {
        let _authority = orbit_common::test_env::unset(
            orbit_common::test_env::INHERITED_AUTHORITY_ENV
                .iter()
                .copied(),
        );
        let runtime = OrbitRuntime::from_roots(&fixture.root, workspace_root)
            .unwrap()
            .with_actor(ActorIdentity::human("codebase-fixture"));
        let started = now - chrono::Duration::minutes(10);
        let run = JobRun {
            build: None,
            run_id: "jrun-pilot-cli".into(),
            job_id: "task_pilot_pipeline".into(),
            attempt: 1,
            state: JobRunState::Success,
            scheduled_at: started,
            created_at: started,
            started_at: Some(started),
            finished_at: Some(now),
            duration_ms: None,
            pid: None,
            pid_start_time: None,
            input: None,
            retry_source_run_id: None,
            knowledge_metrics: None,
            resolved_crew: Some("sol".into()),
            crew_model: Some("task-model".into()),
            steps: Vec::new(),
            executed_on: None,
        };
        runtime
            .sqlite_store()
            .unwrap()
            .write(|tx| {
                tx.job_run()
                    .put(&runtime.workspace_id().unwrap(), &run, None)
            })
            .unwrap();
        // The piloted task's crew is distinct from the pilot activity's draw.
        let key = orbit_types::workflow::CrewPurpose::TaskPilot.config_key();
        let mut state = orbit_types::workflow::PipelineState::new(
            run.run_id.clone(),
            run.job_id.clone(),
            json!({}),
        );
        state.activity_crew_draws.insert(
            key.into(),
            orbit_types::workflow::ActivityCrewDraw {
                crew: "luna".into(),
                source: key.into(),
                eligible_pool: Vec::new(),
                auto_effort: None,
            },
        );
        runtime.write_run_state(&run.run_id, &state).unwrap();
        runtime
            .insert_invocation_trace_record(
                &orbit_core::test_support::fixture_store::InvocationInsertParams {
                    agent_execution_id: None,
                    job_run_id: run.run_id.clone(),
                    account: None,
                    activity_id: "pilot".into(),
                    agent: "fixture".into(),
                    provider: None,
                    model: Some("luna-model".into()),
                    task_ids: Vec::new(),
                    trace: Default::default(),
                },
            )
            .unwrap();
        runtime
            .add_task(TaskAddParams {
                title: "piloted fixture".into(),
                status: Some(TaskStatus::Done),
                ..Default::default()
            })
            .unwrap()
            .id
            .to_string()
    };
    let before = ["file:src/lib.rs"];
    let after = ["dir:src", "file:docs/unused.md"];
    let receipt = TaskCommentRowV2 {
        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        comment_id: "C-0001".into(),
        at: now - chrono::Duration::minutes(5),
        by: "task-pilot".into(),
        body: json!({
            "assessment": {"context_files_before": before, "context_files_after": after},
            "context_files_before": before,
            "pilot_run_id": "jrun-pilot-cli",
        })
        .to_string(),
        operation_id: Some(format!("{:064x}", 1)),
    };
    append_row(
        bundle(&fixture.root, &task)
            .unwrap()
            .join(TASK_COMMENTS_FILE_NAME),
        &receipt,
    );
    fs::create_dir_all(fixture.repo.join("src")).unwrap();
    fs::write(fixture.repo.join("src/lib.rs"), "pub fn example() {}\n").unwrap();
    fs::write(fixture.repo.join("src/main.rs"), "fn main() {}\n").unwrap();
    crate::git_repo::run(&fixture.repo, &["add", "src"]);
    crate::git_repo::run(
        &fixture.repo,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "piloted fixture",
        ],
    );
    let commit = crate::git_repo::run(&fixture.repo, &["rev-parse", "HEAD"]);
    record_landing(&fixture.root, &task, &commit);

    let codebase = fixture.json(&[
        "codebase",
        "analytics",
        "--window",
        "30d",
        "--min-support",
        "1",
        "--json",
    ]);
    assert!(codebase.get("task_pilot").is_none(), "{codebase}");

    let selection = [
        "agents",
        "scorecard",
        "--role",
        "pilots",
        "--window",
        "30d",
        "--min-support",
        "1",
    ];
    let payload = fixture.json(&[&selection[..], &["--json"]].concat());
    let row = &payload["rows"][0];
    assert_eq!(
        (row["crew"].as_str(), row["model"].as_str(), &row["units"]),
        (Some("luna"), Some("luna-model"), &json!(1)),
        "{payload}"
    );
    let stat = |key: &str| {
        row["stats"]
            .as_array()
            .unwrap()
            .iter()
            .find(|stat| stat["key"] == key)
            .unwrap_or_else(|| panic!("{key}: {row}"))["value"]
            .clone()
    };
    assert_eq!(
        stat("recall_percent"),
        100,
        "dir:src covers both landed files"
    );
    assert_eq!(
        stat("precision_percent"),
        50,
        "docs/unused.md covered nothing"
    );
    assert_eq!(stat("filer_f1_percent"), 66.67);

    let human = fixture
        .command(&selection)
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human = String::from_utf8(human).unwrap();
    assert!(
        human
            .lines()
            .any(|line| line.contains("luna") && line.contains("luna-model")),
        "the text table ranks the crew:\n{human}"
    );
}

/// Analytics is history-only, and old done tasks cannot add delivery I/O to
/// a related query. Git's own trace records identity resolution independently
/// of the observer diagnostic, without timing assertions or synthetic load.
/// However many tasks a scan observes, it parses the job catalog at most once
/// (ORB-16938: a 7-minute scan reparsed it for every task).
#[test]
fn codebase_delivery_reads_are_lazy_and_window_bounded() {
    if crate::isolated_test::run_in_child(
        "codebase_cli::codebase_delivery_reads_are_lazy_and_window_bounded",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let shown = fixture.json(&["workspace", "show", "--json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    let jobs = fixture.root.join("resources/jobs");
    fs::create_dir_all(&jobs).unwrap();
    fs::write(jobs.join("codebase_fixture.yaml"),
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: codebase_fixture\nspec:\n  state: enabled\n  task_delivery: {}\n  steps: []\n").unwrap();
    crate::git_repo::run(
        &fixture.repo,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/codebase.git",
        ],
    );
    let seed = |title: &str, status: TaskStatus, at: chrono::DateTime<chrono::Utc>| {
        let _authority = orbit_common::test_env::unset(
            orbit_common::test_env::INHERITED_AUTHORITY_ENV
                .iter()
                .copied(),
        );
        let runtime = OrbitRuntime::from_roots(&fixture.root, workspace_root)
            .unwrap()
            .with_actor(ActorIdentity::human("codebase-fixture"));
        let task = runtime
            .add_task(TaskAddParams {
                title: title.into(),
                status: Some(status),
                context_files: vec!["file:src/lib.rs".into()],
                ..Default::default()
            })
            .unwrap()
            .id
            .to_string();
        // No host checkpoint is present: observing the run must still resolve
        // its repository and report unavailable delivery rather than a landing.
        orbit_core::test_support::EngineHost::new(&runtime)
            .insert_job_run(
                "codebase_fixture",
                1,
                at,
                Some(json!({"task_ids": [task]})),
                None,
            )
            .unwrap();
        let path = bundle(&fixture.root, &task)
            .unwrap()
            .join(TASK_EVENTS_FILE_NAME);
        let rows = fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| {
                let mut row: TaskEventRowV2 = serde_json::from_str(line).unwrap();
                row.at = at;
                serde_json::to_string(&row).unwrap()
            })
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(path, format!("{rows}\n")).unwrap();
    };
    let now = chrono::Utc::now();
    seed("recent one", TaskStatus::Done, now);
    seed("recent two", TaskStatus::Done, now);
    seed("unfinished", TaskStatus::Backlog, now);
    let run = |verb: &str, window: &str, expected_observations: usize, expected_git: usize| {
        let trace = fixture._temp.path().join("git-trace.log");
        fs::write(&trace, "").unwrap();
        let mut args = vec!["codebase", verb];
        if verb != "analytics" {
            args.push("src/lib.rs");
        }
        args.extend(["--window", window, "--min-support", "1", "--json"]);
        let output = fixture
            .command(&args)
            .env("GIT_TRACE", &trace)
            .env(
                "RUST_LOG",
                "orbit_core::application::job::run::delivery=debug,\
                 orbit_core::application::job::catalog=debug",
            )
            .assert()
            .success()
            .get_output()
            .clone();
        let git = fs::read_to_string(trace).unwrap();
        if verb != "related" {
            assert!(git.trim().is_empty(), "{verb} must not spawn Git: {git}");
        }
        assert_eq!(
            git.lines()
                .filter(|line| line.contains("config --get remote.origin.url"))
                .count(),
            expected_git,
            "{verb}/{window}: repository identity must resolve at most once: {git}"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            stderr
                .lines()
                .filter(|line| line.contains("codebase_delivery_observation=true"))
                .count(),
            expected_observations,
            "{verb}/{window}: delivery observation must be lazy and window-bounded: {stderr}"
        );
        assert!(
            stderr
                .lines()
                .filter(|line| line.contains("job_catalog_load=true"))
                .count()
                <= 1,
            "{verb}/{window}: one scan must parse the job catalog at most once: {stderr}"
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    run("analytics", "30d", 0, 0);
    run("show", "30d", 0, 0);
    let before = run("related", "30d", 2, 1);
    // A small fixed fixture proves the bound; it does not attempt to create
    // CPU load or reproduce a timing race on a shared host.
    for title in ["old one", "old two", "old three"] {
        seed(title, TaskStatus::Done, now - chrono::Duration::days(40));
    }
    run("analytics", "30d", 0, 0);
    run("show", "30d", 0, 0);
    let after = run("related", "30d", 2, 1);
    assert_eq!(before["rows"], after["rows"]);
    assert_eq!(
        before["unavailable_landed_tasks"],
        after["unavailable_landed_tasks"]
    );
    run("related", "all", 5, 1);
}

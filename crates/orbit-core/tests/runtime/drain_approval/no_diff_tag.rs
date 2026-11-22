//! A pilot verdict that a task expects no repository diff tags it
//! `no-diff-expected`, so its empty `context_files` stop blocking admission
//! [ORB-26836]. A verdict that says the work already landed, or a finding task
//! with no selectors, keeps its hold.

use orbit_types::task::{NO_DIFF_EXPECTED_TAG, ReadinessStage, readiness_gaps};

use super::*;

const NO_DIFF_EVIDENCE: &str = "The deliverable is a filed task list and a report artifact.";

fn tags_of(ws: &Workspace, task: &Task) -> Vec<String> {
    ws.runtime.get_task(&task.id).unwrap().tags
}

fn blocking_gaps(ws: &Workspace, task: &Task) -> Vec<String> {
    let task = ws.runtime.get_task(&task.id).unwrap();
    readiness_gaps(
        ReadinessStage::Proposed,
        &task.tags,
        &task.context_files,
        task.complexity,
    )
    .into_iter()
    .filter(|gap| gap.is_blocking())
    .map(|gap| gap.code.as_str().to_string())
    .collect()
}

fn audit_task(ws: &Workspace, title: &str, tags: &[&str]) -> Task {
    ws.task(title, tags, &["file:README.md"], TaskComplexity::Low)
}

fn a_no_diff_verdict_tags_the_task_and_clears_the_context_files_gap() {
    let ws = Workspace::new();
    let task = audit_task(&ws, "audit", &["auto-task:pilot-block-patterns"]);
    let before = ws.runtime.get_task(&task.id).unwrap();
    assert!(!before.tags.iter().any(|tag| tag == NO_DIFF_EXPECTED_TAG));

    apply_verified_outside(&ws, vec![verified_no_diff(&task, NO_DIFF_EVIDENCE)]);

    let after = ws.runtime.get_task(&task.id).unwrap();
    assert_eq!(after.status, TaskStatus::Proposed, "no authority promotes");
    assert!(after.context_files.is_empty(), "{after:?}");
    assert_eq!(
        after
            .tags
            .iter()
            .filter(|tag| *tag == NO_DIFF_EXPECTED_TAG)
            .count(),
        1,
        "{:?}",
        after.tags
    );
    assert!(
        after
            .tags
            .iter()
            .any(|tag| tag == "auto-task:pilot-block-patterns"),
        "the pilot keeps the task's own tags: {:?}",
        after.tags
    );
    assert_eq!(blocking_gaps(&ws, &task), Vec::<String>::new());
}

fn a_drain_approves_the_task_its_pilot_tagged_in_the_same_pass() {
    let ws = Workspace::new();
    let drain = ws.drain();
    let task = audit_task(&ws, "audit", &[]);
    let selection = ws.select(&drain);
    assert_task_ids(&selection, &[&task]);
    let pilot = ws.pilot_child(&drain, &selection["task_ids"]);
    let applied = ws.pilot_ok(
        &drain,
        &pilot,
        vec![verified_no_diff(&task, NO_DIFF_EVIDENCE)],
    );
    assert_classification(&applied, "drain_approval", "verified_ready");
    assert_eq!(applied["drain_approval"][0]["approved"], true, "{applied}");
    assert_status(&ws, &task, TaskStatus::Backlog);
    assert!(tags_of(&ws, &task).contains(&NO_DIFF_EXPECTED_TAG.to_string()));
}

fn a_verdict_that_is_not_a_no_diff_deliverable_leaves_the_task_untagged_and_gapped() {
    let ws = Workspace::new();
    let landed = ws.commit("the fix");
    let cited = audit_task(&ws, "cites a commit", &[]);
    let typed = audit_task(&ws, "typed already landed", &[]);
    let duplicate = audit_task(&ws, "typed duplicate", &[]);
    let release = audit_task(&ws, "release action", &[]);
    let finding = audit_task(&ws, "review finding", &["delivery-code-review"]);
    let unfound = ws.task("no selectors found", &[], &[], TaskComplexity::Low);
    let mut not_found = assessment(&unfound);
    not_found["context_files_after"] = json!([]);
    let mut already_landed = verified_no_diff(&typed, NO_DIFF_EVIDENCE);
    already_landed["already_landed"] = json!({"evidence": "The change is on the base branch."});
    let mut duplicate_of = verified_no_diff(&duplicate, NO_DIFF_EVIDENCE);
    duplicate_of["duplicate_of"] = json!({"task_id": cited.id, "evidence": "Same work."});
    let mut release_action = verified_no_diff(&release, NO_DIFF_EVIDENCE);
    release_action["release_action_required"] =
        json!({"action": "publish the tag", "evidence": "The tag is recorded but unpublished."});
    apply_verified_outside(
        &ws,
        vec![
            verified_no_diff(&cited, &format!("Commit {landed} already carries it.")),
            already_landed,
            duplicate_of,
            release_action,
            verified_no_diff(
                &finding,
                "The failing test does not exist at this revision.",
            ),
        ],
    );
    for task in [&cited, &typed, &duplicate, &release, &finding, &unfound] {
        assert!(
            !tags_of(&ws, task).contains(&NO_DIFF_EXPECTED_TAG.to_string()),
            "{} must stay untagged",
            task.title
        );
        assert_eq!(
            blocking_gaps(&ws, task),
            vec!["missing_context_files".to_string()],
            "{} keeps its gap",
            task.title
        );
    }
}

fn a_task_with_selectors_or_the_tag_is_left_as_it_was() {
    let ws = Workspace::new();
    let scoped = audit_task(&ws, "has context files", &[]);
    let tagged = audit_task(&ws, "already tagged", &[NO_DIFF_EXPECTED_TAG, "chore"]);
    apply_verified_outside(
        &ws,
        vec![
            assessment(&scoped),
            verified_no_diff(&tagged, NO_DIFF_EVIDENCE),
        ],
    );
    let scoped_after = ws.runtime.get_task(&scoped.id).unwrap();
    assert_eq!(scoped_after.context_files, vec!["file:README.md"]);
    assert!(
        !scoped_after
            .tags
            .iter()
            .any(|tag| tag == NO_DIFF_EXPECTED_TAG)
    );
    let mut tags = tags_of(&ws, &tagged);
    tags.sort();
    assert_eq!(
        tags,
        vec!["chore".to_string(), NO_DIFF_EXPECTED_TAG.to_string()]
    );
}

fn a_no_diff_verdict_naming_a_known_non_commit_object_tags_and_approves() {
    let ws = Workspace::new();
    let drain = ws.drain();
    let readme_blob = ws.git(&["rev-parse", "HEAD:README.md"]);
    let root_tree = ws.git(&["rev-parse", "HEAD^{tree}"]);
    let blob_task = audit_task(&ws, "audit readme blob checksum", &[]);
    let tree_task = audit_task(&ws, "audit root tree checksum", &[]);
    let selection = ws.select(&drain);
    assert_task_ids(&selection, &[&blob_task, &tree_task]);
    let pilot = ws.pilot_child(&drain, &selection["task_ids"]);

    let blob_assessment = verified_no_diff(
        &blob_task,
        &format!("Deliver a report artifact recording the README blob checksum {readme_blob}."),
    );
    let mut tree_assessment = verified_no_diff(&tree_task, NO_DIFF_EVIDENCE);
    tree_assessment["assessment_rationale"] = json!(format!(
        "Inspected tree {root_tree}; no repository diff is expected."
    ));

    let applied = ws.pilot_ok(&drain, &pilot, vec![blob_assessment, tree_assessment]);
    for entry in applied["drain_approval"].as_array().unwrap() {
        assert_eq!(entry["classification"], "verified_ready", "{applied}");
        assert_eq!(entry["approved"], true, "{applied}");
    }
    for task in [&blob_task, &tree_task] {
        assert_status(&ws, task, TaskStatus::Backlog);
        assert!(tags_of(&ws, task).contains(&NO_DIFF_EXPECTED_TAG.to_string()));
        assert_eq!(blocking_gaps(&ws, task), Vec::<String>::new());
    }
}

fn non_commit_object_checks_preserve_commit_unknown_and_finding_holds() {
    let ws = Workspace::new();
    let landed = ws.commit("the fix");
    let readme_blob = ws.git(&["rev-parse", "HEAD:README.md"]);
    let drain = ws.drain();

    let mixed = audit_task(&ws, "cites blob and commit", &[]);
    let unknown = audit_task(&ws, "cites unknown sha", &[]);
    let typed = audit_task(&ws, "typed already landed with blob", &[]);
    let finding_with_blob =
        audit_task(&ws, "review finding naming blob", &["delivery-code-review"]);
    let finding_with_commit = audit_task(
        &ws,
        "review finding citing commit",
        &["auto-task:delivery-code-review"],
    );

    let mut already_landed =
        verified_no_diff(&typed, &format!("Deliver a report for blob {readme_blob}."));
    already_landed["already_landed"] = json!({"evidence": "The change is on the base branch."});

    assert_verified_no_diff_outside(
        &ws,
        &drain,
        vec![
            verified_no_diff(
                &mixed,
                &format!("Blob {readme_blob} changed in commit {landed}."),
            ),
            verified_no_diff(&unknown, "Report cites unverified sha 0badc0ffee1."),
            already_landed,
            verified_no_diff(
                &finding_with_blob,
                &format!("README blob {readme_blob} already matches."),
            ),
            verified_no_diff(
                &finding_with_commit,
                &format!("Commit {landed} already carries the fix."),
            ),
        ],
        &[&finding_with_commit],
        &[
            (&mixed, Some(landed.as_str())),
            (&unknown, Some("0badc0ffee1")),
            (&typed, Some(readme_blob.as_str())),
            (&finding_with_blob, Some(readme_blob.as_str())),
        ],
    );

    for task in [&mixed, &unknown, &typed, &finding_with_blob] {
        assert!(
            !tags_of(&ws, task).contains(&NO_DIFF_EXPECTED_TAG.to_string()),
            "{} must stay untagged",
            task.title
        );
        assert_eq!(
            blocking_gaps(&ws, task),
            vec!["missing_context_files".to_string()],
            "{} keeps its gap",
            task.title
        );
    }
}

fn mixed_hex_prefix(full: &str) -> &str {
    let len = (7..full.len())
        .find(|&len| {
            let prefix = &full[..len];
            prefix.bytes().any(|byte| byte.is_ascii_digit())
                && prefix.bytes().any(|byte| byte.is_ascii_alphabetic())
        })
        .expect("full OID has a mixed digit/letter prefix");
    &full[..len]
}

fn hex_ref_names_do_not_hide_commit_or_unknown_citations() {
    let ws = Workspace::new();
    let landed = ws.commit("the fix");
    let readme_blob = ws.git(&["rev-parse", "HEAD:README.md"]);
    let commit_prefix = mixed_hex_prefix(&landed);
    let blob_prefix = mixed_hex_prefix(&readme_blob);
    assert_ne!(commit_prefix, blob_prefix);

    // Shadow the commit abbreviation and an unknown hex token with tags to a
    // blob, and shadow the blob abbreviation with a tag to a commit.
    ws.git(&["tag", commit_prefix, &readme_blob]);
    ws.git(&["tag", "0badc0ffee1", &readme_blob]);
    ws.git(&["tag", blob_prefix, &landed]);

    let drain = ws.drain();
    let shadowed_commit = audit_task(&ws, "cites commit prefix shadowed by blob tag", &[]);
    let shadowed_unknown = audit_task(&ws, "cites unknown hex shadowed by blob tag", &[]);
    let shadowed_blob = audit_task(&ws, "cites blob prefix shadowed by commit tag", &[]);

    let selection = ws.select(&drain);
    assert_task_ids(
        &selection,
        &[&shadowed_commit, &shadowed_unknown, &shadowed_blob],
    );
    let pilot = ws.pilot_child(&drain, &selection["task_ids"]);
    let applied = ws.pilot_ok(
        &drain,
        &pilot,
        vec![
            verified_no_diff(
                &shadowed_commit,
                &format!("Commit {commit_prefix} already carries it."),
            ),
            verified_no_diff(
                &shadowed_unknown,
                "Report cites unverified sha 0badc0ffee1.",
            ),
            verified_no_diff(
                &shadowed_blob,
                &format!("Deliver a report artifact recording blob checksum {blob_prefix}."),
            ),
        ],
    );

    let approvals = applied["drain_approval"].as_array().unwrap();
    assert_eq!(approvals.len(), 3, "{applied}");
    let approval_for = |task: &Task| {
        approvals
            .iter()
            .find(|entry| entry["task_id"] == task.id)
            .unwrap_or_else(|| panic!("missing approval entry for {}: {applied}", task.id))
    };

    let next = ws.select(&drain);
    for (task, cited) in [
        (&shadowed_commit, commit_prefix),
        (&shadowed_unknown, "0badc0ffee1"),
    ] {
        let entry = approval_for(task);
        assert_eq!(
            entry["classification"], "pilot_verified_no_diff",
            "{applied}"
        );
        assert_eq!(entry["decision"], "withhold", "{applied}");
        assert_status(&ws, task, TaskStatus::Proposed);
        assert!(
            !tags_of(&ws, task).contains(&NO_DIFF_EXPECTED_TAG.to_string()),
            "{} must stay untagged",
            task.title
        );
        assert_eq!(
            blocking_gaps(&ws, task),
            vec!["missing_context_files".to_string()],
            "{} keeps its gap",
            task.title
        );
        assert_held(&next, task, "pilot_verified_no_diff");
        assert_held_detail(&next, task, cited);
    }

    let blob_entry = approval_for(&shadowed_blob);
    assert_eq!(blob_entry["classification"], "verified_ready", "{applied}");
    assert_eq!(blob_entry["approved"], true, "{applied}");
    assert_status(&ws, &shadowed_blob, TaskStatus::Backlog);
    assert!(tags_of(&ws, &shadowed_blob).contains(&NO_DIFF_EXPECTED_TAG.to_string()));
    assert_eq!(blocking_gaps(&ws, &shadowed_blob), Vec::<String>::new());
}

fn duplicate_loose_object_for_ambiguity(repo: &Path, oid: &str) {
    let (dir, rest) = oid.split_at(2);
    let source = repo.join(".git/objects").join(dir).join(rest);
    assert!(
        source.is_file(),
        "expected loose object {}",
        source.display()
    );
    let alt_last = if oid.ends_with('0') { '1' } else { '0' };
    let mut alt_rest = rest[..rest.len() - 1].to_string();
    alt_rest.push(alt_last);
    let duplicate = repo.join(".git/objects").join(dir).join(alt_rest);
    std::fs::copy(&source, &duplicate).unwrap();
}

fn covering_proof_resolves_unique_commit_prefix_under_blob_tag_and_holds_invalid_citations() {
    let ws = Workspace::new();
    let older = ws.commit("older readme change");
    let culprit = ws.commit("the culprit");
    let fix = ws.commit("the fix");
    let other_path = ws.commit_file("other.txt", "unrelated\n", "unrelated path change");
    let ambig = ws.commit("ambiguous prefix commit");
    // File the creation task before `src/new.rs` exists so it receives an
    // operator creation grant, then land `src/new.rs`.
    let creation = ws.creation_task("shadowed landed creation");
    std::fs::create_dir_all(ws.repo.join("src")).unwrap();
    let created = ws.commit_file(
        "src/new.rs",
        "pub fn delivered() {}\n",
        "creation delivered",
    );
    let readme_blob = ws.git(&["rev-parse", "HEAD:README.md"]);

    let older_prefix = mixed_hex_prefix(&older);
    let culprit_prefix = mixed_hex_prefix(&culprit);
    let fix_prefix = mixed_hex_prefix(&fix);
    let other_prefix = mixed_hex_prefix(&other_path);
    let ambig_prefix = mixed_hex_prefix(&ambig);
    let created_prefix = mixed_hex_prefix(&created);

    // Shadow valid commit prefixes with same-name tags pointing to a blob,
    // shadow an unknown hex token with a tag pointing to the covering commit,
    // and make `ambig_prefix` ambiguous in the object database even with a
    // same-name tag pointing to the commit.
    for prefix in [
        older_prefix,
        culprit_prefix,
        fix_prefix,
        other_prefix,
        created_prefix,
    ] {
        ws.git(&["tag", prefix, &readme_blob]);
    }
    ws.git(&["tag", "0badc0ffee1", &fix]);
    duplicate_loose_object_for_ambiguity(&ws.repo, &ambig);
    ws.git(&["tag", ambig_prefix, &ambig]);

    let drain = ws.drain();
    let description = format!("Introduced by {culprit_prefix} on the base branch.");
    let finding = |title: &str| ws.readme_finding(title, description.clone(), Vec::new());

    let covered = finding("covered via pending drain");
    let cites_culprit = finding("cites shadowed culprit");
    let cites_older = finding("cites shadowed older commit");
    let cites_other_path = finding("cites shadowed unrelated path");
    let cites_unknown = finding("cites unknown hex shadowed by commit tag");

    assert_verified_no_diff_outside(
        &ws,
        &drain,
        verified_cases(&[
            (&covered, format!("Commit {fix_prefix} repaired README.md.")),
            (
                &cites_culprit,
                format!("Commit {culprit_prefix} is the culprit."),
            ),
            (
                &cites_older,
                format!("Commit {older_prefix} touched README.md earlier."),
            ),
            (
                &cites_other_path,
                format!("Commit {other_prefix} touched other.txt."),
            ),
            (
                &cites_unknown,
                "Commit 0badc0ffee1 supposedly repaired README.md.".into(),
            ),
        ]),
        &[&covered],
        &[
            (&cites_culprit, Some("do not count")),
            (&cites_older, Some("do not count")),
            (&cites_other_path, Some("do not count")),
            (&cites_unknown, Some("0badc0ffee1")),
        ],
    );
    let archive_comment = ws.last_comment(&covered);
    assert!(
        archive_comment.message.contains(fix_prefix),
        "archive comment names the cited prefix: {archive_comment:?}"
    );

    // Audited pilot application under drain approval closes a finding whose
    // covering commit prefix is shadowed by a blob tag, holds an ambiguous hex
    // prefix even when a same-name tag points to that commit, and proves
    // operator-authorized creation work citing blob-tag-shadowed prefixes.
    let audited_covered = finding("covered via audited pilot apply");
    let cites_ambiguous = finding("cites ambiguous hex prefix");
    let selection = ws.select(&drain);
    assert_task_ids(&selection, &[&creation, &audited_covered, &cites_ambiguous]);
    let pilot = ws.pilot_child(&drain, &selection["task_ids"]);
    let applied = ws.pilot_ok(
        &drain,
        &pilot,
        vec![
            verified_no_diff(
                &creation,
                &format!("Complete scope delivered by {fix_prefix} and {created_prefix}."),
            ),
            verified_no_diff(
                &audited_covered,
                &format!("Commit {fix_prefix} repaired README.md."),
            ),
            verified_no_diff(
                &cites_ambiguous,
                &format!("Commit {ambig_prefix} repaired README.md."),
            ),
        ],
    );
    let approvals = applied["drain_approval"].as_array().unwrap();
    let approval_for = |task: &Task| {
        approvals
            .iter()
            .find(|entry| entry["task_id"] == task.id)
            .unwrap_or_else(|| panic!("missing approval entry for {}: {applied}", task.id))
    };
    let covered_decision = approval_for(&audited_covered);
    assert_eq!(
        covered_decision["classification"], "pilot_verified_no_diff",
        "{applied}"
    );
    assert_eq!(covered_decision["closed"], true, "{applied}");
    assert_eq!(covered_decision["covering_commits"], json!([fix_prefix]));
    assert_status(&ws, &audited_covered, TaskStatus::Archived);

    let ambig_decision = approval_for(&cites_ambiguous);
    assert_eq!(
        ambig_decision["classification"], "pilot_verified_no_diff",
        "{applied}"
    );
    assert_eq!(ambig_decision["closed"], false, "{applied}");
    assert!(
        ambig_decision["not_closed_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("not a commit") && reason.contains(ambig_prefix)),
        "{applied}"
    );
    assert_status(&ws, &cites_ambiguous, TaskStatus::Proposed);

    let creation_decision = approval_for(&creation);
    assert_eq!(
        creation_decision["classification"], "already_landed",
        "{applied}"
    );
    assert_eq!(
        creation_decision["evidence"]["covering_commits"],
        json!([fix_prefix, created_prefix])
    );
    assert_status(&ws, &creation, TaskStatus::Proposed);
}

crate::scenario_table!(
    a_no_diff_verdict_tags_the_task_scenarios,
    a_no_diff_verdict_tags_the_task_and_clears_the_context_files_gap,
    a_drain_approves_the_task_its_pilot_tagged_in_the_same_pass,
    a_no_diff_verdict_naming_a_known_non_commit_object_tags_and_approves,
    a_verdict_that_is_not_a_no_diff_deliverable_leaves_the_task_untagged_and_gapped,
    non_commit_object_checks_preserve_commit_unknown_and_finding_holds,
    hex_ref_names_do_not_hide_commit_or_unknown_citations,
    covering_proof_resolves_unique_commit_prefix_under_blob_tag_and_holds_invalid_citations,
    a_task_with_selectors_or_the_tag_is_left_as_it_was
);

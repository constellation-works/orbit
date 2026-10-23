//! A later authenticated prerequisite read fails after a successful PR-status read.
use super::*;

/// A BLOCKED pull request whose only failure is an optional check sends
/// enabled auto-fix completion to the infrastructure prerequisite reads. The
/// combined-status read fails once at the TLS handshake; the pull request is
/// then mergeable on its next observation.
fn prerequisite_forge(f: &Fixture) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let root = f._root.path();
    let calls = root.join("prerequisite-calls");
    let lost = root.join("lost-status");
    let merged = root.join("merged");
    let file = |name: &str, value: Value| {
        let path = root.join(name);
        std::fs::write(&path, value.to_string()).unwrap();
        path
    };
    let head = &f.head;
    let repo = json!({"id":500,"full_name":"owner/repository","url":"https://api.github.com/repos/owner/repository"});
    let repository = file("prerequisite-repo.json", repo.clone());
    let pull = file(
        "prerequisite-pull.json",
        json!({"number":17,"state":"open","merged":false,
            "head":{"sha":head,"ref":f.branch,"repo":repo},
            "base":{"sha":f.base,"ref":"main","repo":repo}}),
    );
    let classic = file(
        "prerequisite-classic.json",
        json!({"data":{"repository":{"pullRequest":{"headRefOid":head,"baseRefName":"main",
            "reviewDecision":null,"baseRef":{"branchProtectionRule":{"id":"RULE-1",
            "requiredStatusChecks":[{"context":"native","app":null}]}}}}}}),
    );
    let required = file(
        "prerequisite-required.json",
        json!({"data":{"repository":{"pullRequest":{"headRefOid":head,"commits":{"nodes":[{"commit":{
        "oid":head,"statusCheckRollup":{"contexts":{"pageInfo":{"hasNextPage":false},"nodes":[
            {"__typename":"CheckRun","name":"native","status":"COMPLETED","conclusion":"SUCCESS","isRequired":true},
            {"__typename":"CheckRun","name":"Read state","status":"COMPLETED","conclusion":"FAILURE","isRequired":false}
        ]}}}}]}}}}}),
    );
    let checks = file(
        "prerequisite-checks.json",
        json!({"total_count":2,"check_runs":[
            {"id":1,"name":"native","head_sha":head,"app":{"id":900},"status":"completed","conclusion":"success"},
            {"id":2,"name":"Read state","head_sha":head,"app":{"id":900},"status":"completed","conclusion":"failure"}
        ]}),
    );
    let statuses = file(
        "prerequisite-statuses.json",
        json!({"sha":head,"repository":repo,"total_count":0,"statuses":[]}),
    );
    let mut answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    answer["isDraft"] = json!(false);
    answer["mergeStateStatus"] = json!("BLOCKED");
    answer["statusCheckRollup"] = json!([
        {"name":"native","status":"COMPLETED","conclusion":"SUCCESS"},
        {"name":"Read state","status":"COMPLETED","conclusion":"FAILURE"}
    ]);
    std::fs::write(&f.answer, answer.to_string()).unwrap();
    answer["mergeStateStatus"] = json!("CLEAN");
    answer["statusCheckRollup"] =
        json!([{"name":"native","status":"COMPLETED","conclusion":"SUCCESS"}]);
    let clean = file("prerequisite-clean.json", answer.clone());
    answer["state"] = json!("MERGED");
    answer["mergeCommit"] = json!({"oid":head});
    answer["mergedAt"] = json!("2026-10-17T12:00:00Z");
    let landed = file("prerequisite-merged.json", answer);
    let cat = |path: &PathBuf| format!("cat '{}'", path.display());
    let gh = root.join("bin/gh");
    std::fs::write(
        &gh,
        format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{calls}'
case "$*" in
 'pr view'*) if [ -f '{merged}' ]; then {landed}; elif [ -f '{lost}' ]; then {clean}; else {answer}; fi;;
 'repo view'*) echo '{{"nameWithOwner":"owner/repository"}}';;
 'api graphql'*statusCheckRollup*) {required};;
 'api graphql'*requiredStatusChecks*) {classic};;
 'api graphql'*) echo '{{"data":{{"repository":{{"autoMergeAllowed":false,"mergeCommitAllowed":true,"rebaseMergeAllowed":false,"squashMergeAllowed":false,"pullRequest":{{"baseRefName":"main","baseRef":{{"branchProtectionRule":null}}}}}}}}}}';;
 'api repos/{{owner}}/{{repo}}') {repository};;
 'api repos/{{owner}}/{{repo}}/pulls/17') {pull};;
 'api repos/{{owner}}/{{repo}}/pulls/17/merge'*) touch '{merged}'; printf '{{"merged":true,"sha":"{head}"}}';;
 'api repos/{{owner}}/{{repo}}/rules/branches/'*) echo '[]';;
 'api repos/{{owner}}/{{repo}}/commits/'*'/check-runs'*) {checks};;
 'api repos/{{owner}}/{{repo}}/commits/'*'/status'*)
    if [ ! -f '{lost}' ]; then touch '{lost}'; echo 'Get "https://api.github.com/repos/owner/repository/commits/{head}/status?per_page=100": net/http: TLS handshake timeout' >&2; exit 1; fi
    {statuses};;
 *) exit 99;;
esac
"#,
            calls = calls.display(),
            merged = merged.display(),
            lost = lost.display(),
            landed = cat(&landed),
            clean = cat(&clean),
            answer = cat(&f.answer),
            required = cat(&required),
            classic = cat(&classic),
            repository = cat(&repository),
            pull = cat(&pull),
            checks = cat(&checks),
            statuses = cat(&statuses),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    calls
}

/// [ORB-16307] The original incident failed the later combined-status read,
/// outside the PR-status read that unchanged-tail continuation first covered.
pub(crate) fn later_prerequisite_read_failure_resumes_only_completion() {
    let (mut f, checkout) = fixture();
    configure_review(&mut f, &checkout);
    let calls = prerequisite_forge(&f);
    let state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    let receipts = f
        .runtime
        .get_task_artifact_manifest(&f.task)
        .unwrap()
        .into_iter()
        .filter(|a| {
            a.path.starts_with("validation/")
                || a.path == orbit_types::workflow::REVIEW_GATE_ARTIFACT
        })
        .map(|a| (a.path, a.sha256))
        .collect::<Vec<_>>();
    assert!(
        receipts
            .iter()
            .any(|(path, _)| path == orbit_types::workflow::REVIEW_GATE_ARTIFACT)
    );

    let (outcome, _) = run_tail(&f);
    let outcome = outcome.unwrap();
    assert!(outcome.success, "{outcome:?}");

    let requests = std::fs::read_to_string(calls).unwrap();
    let requests = requests.lines().collect::<Vec<_>>();
    let position = |pattern: &str| requests.iter().position(|l| l.contains(pattern));
    let failed = position("/status?").expect("the combined-status read ran");
    assert!(
        position("pr view").is_some_and(|read| read < failed)
            && position("/check-runs?").is_some_and(|read| read < failed),
        "the failure follows a successful PR-status read: {requests:#?}"
    );
    assert_eq!(
        requests.iter().filter(|l| l.contains("/status?")).count(),
        1,
        "the failed read is not retried in-process; the resumed observation is fresh"
    );
    assert!(
        requests[failed..]
            .iter()
            .filter(|l| l.starts_with("pr view"))
            .count()
            >= 2,
        "fresh hosted observation, then merged readback: {requests:#?}"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|l| l.contains("/pulls/17/merge"))
            .count(),
        1,
        "one merge"
    );
    assert!(!requests.iter().any(|l| l.starts_with("pr create")));

    assert_eq!(
        f.runtime.get_task(&f.task).unwrap().status,
        TaskStatus::Done
    );
    assert_eq!(git(&checkout, &["rev-parse", "HEAD"]), f.head);
    assert_eq!(
        std::fs::read_to_string(f._root.path().join("pushes"))
            .unwrap()
            .lines()
            .count(),
        1,
        "one implementation publication; completion never pushes"
    );
    let after = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    for (index, output) in &state.step_outputs {
        assert_eq!(after.step_output(*index), Some(output), "immutable prefix");
    }
    let manifest = f.runtime.get_task_artifact_manifest(&f.task).unwrap();
    for (path, sha256) in &receipts {
        assert!(
            manifest
                .iter()
                .any(|a| &a.path == path && &a.sha256 == sha256),
            "unchanged host receipt {path}"
        );
    }
    let recovery = after.final_recovery.unwrap();
    assert!(matches!(
        recovery.decision,
        Some(orbit_types::workflow::FinalRecoveryDecision::Resume { .. })
    ));
    assert!(recovery.repair_commit.is_none());
    assert!(state.completion_deadlines.is_empty());
    assert_eq!(
        after.completion_deadlines.len(),
        1,
        "the resumed observation keeps the first attempt's cumulative budget"
    );
    let history = f.runtime.get_task_history(&f.task).unwrap();
    for event in [
        orbit_types::workflow::PR_AUTO_FIX_ADMITTED_EVENT,
        orbit_types::workflow::PR_BASE_UPDATE_RESERVED_EVENT,
        orbit_types::workflow::PR_INFRASTRUCTURE_RESERVED_EVENT,
    ] {
        assert!(
            !history.iter().any(|row| row.event == event),
            "no repair, rerun or base update: {event}"
        );
    }
    assert_eq!(
        history
            .iter()
            .filter(|row| row.event == orbit_types::workflow::PR_COMPLETION_MUTATION_STARTED_EVENT)
            .count(),
        1,
        "only the resumed merge is a completion mutation"
    );
}

/// The same failed read after a durable mutation reservation is not a
/// read-only observation, so the host never resumes the unchanged tail.
pub(crate) fn prerequisite_read_after_mutation_provenance_never_resumes() {
    use orbit_types::task::TaskHistoryEntry;
    let (f, _) = fixture();
    let calls = prerequisite_forge(&f);
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                append_history: vec![TaskHistoryEntry {
                    at: Utc::now(),
                    by: "system".into(),
                    event: orbit_types::workflow::PR_COMPLETION_MUTATION_STARTED_EVENT.into(),
                    note: Some(json!({"run_id":f.source.run_id,"operation":"merge"}).to_string()),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                }],
                ..Default::default()
            },
        )
        .unwrap();
    let (outcome, _) = run_tail(&f);
    assert!(outcome.is_err());
    let requests = std::fs::read_to_string(calls).unwrap();
    assert!(requests.contains("/status?"), "{requests}");
    assert!(!requests.contains("/pulls/17/merge"), "{requests}");
    assert!(
        !matches!(
            f.runtime
                .read_run_state(&f.source.run_id)
                .unwrap()
                .unwrap()
                .final_recovery
                .and_then(|r| r.decision),
            Some(orbit_types::workflow::FinalRecoveryDecision::Resume { .. })
        ),
        "no unchanged-tail continuation"
    );
    assert_ne!(
        f.runtime.get_task(&f.task).unwrap().status,
        TaskStatus::Done
    );
}

/// Substitute `gh` for one completion observation. `required` starts BLOCKED so
/// the required-checks query runs; `merge` starts CLEAN so the first provider
/// read after status is the merge-capability query. Required-check failures
/// happen once; merge-capability failures persist until a fresh PR observation
/// so this fixture still exercises workflow recovery after retry exhaustion.
fn observation_forge(f: &Fixture, mode: &str, kind: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let root = f._root.path();
    let calls = root.join("observation-calls");
    let lost = root.join("lost-read");
    let resumed = root.join("resumed-read");
    let merged = root.join("merged");
    let file = |name: &str, value: Value| {
        let path = root.join(name);
        std::fs::write(&path, value.to_string()).unwrap();
        path
    };
    let head = &f.head;
    let mut answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    answer["isDraft"] = json!(false);
    answer["mergeStateStatus"] = json!("BLOCKED");
    answer["statusCheckRollup"] = json!([
        {"name":"native","status":"COMPLETED","conclusion":"SUCCESS"}
    ]);
    let blocked = file("observation-blocked.json", answer.clone());
    answer["mergeStateStatus"] = json!("CLEAN");
    let clean = file("observation-clean.json", answer.clone());
    answer["state"] = json!("MERGED");
    answer["mergeCommit"] = json!({"oid":head});
    answer["mergedAt"] = json!("2026-10-17T12:00:00Z");
    let landed = file("observation-merged.json", answer);
    let required = file(
        "observation-required.json",
        json!({"data":{"repository":{"pullRequest":{"headRefOid":head,"commits":{"nodes":[{"commit":{
            "oid":head,"statusCheckRollup":{"contexts":{"pageInfo":{"hasNextPage":false},"nodes":[
                {"__typename":"CheckRun","name":"native","status":"COMPLETED","conclusion":"SUCCESS","isRequired":true}
            ]}}
        }}]}}}}}),
    );
    let cat = |path: &PathBuf| format!("cat '{}'", path.display());
    let gh = root.join("bin/gh");
    std::fs::write(
        &gh,
        format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{calls}'
fault_read() {{
    if [ -f '{lost}' ] && {{ [ '{mode}' != merge ] || [ -f '{resumed}' ]; }}; then
        return 1
    fi
    touch '{lost}'
    case '{kind}' in
        tls)
            echo 'Get "https://api.github.com/repos/owner/repository/commits/abc/status?per_page=100": tls: handshake timeout' >&2
            exit 1 ;;
        http500)
            echo 'gh: HTTP 500' >&2
            exit 1 ;;
        forbidden)
            echo 'gh: Resource not accessible by integration (HTTP 403)' >&2
            exit 1 ;;
        credentials)
            echo 'gh: Bad credentials (HTTP 401)' >&2
            exit 1 ;;
        malformed)
            printf '%s\n' 'not-json'
            exit 0 ;;
        *) echo "unknown fault {kind}" >&2; exit 98 ;;
    esac
}}
case "$*" in
 'pr view'*)
    if [ -f '{lost}' ]; then touch '{resumed}'; fi
    if [ -f '{merged}' ]; then {landed}
    elif [ -f '{lost}' ] || [ '{mode}' = merge ]; then {clean}
    else {blocked}
    fi ;;
 'repo view'*) echo '{{"nameWithOwner":"owner/repository"}}' ;;
 'api graphql'*'isRequired(pullRequestNumber'*)
    if [ '{mode}' = required ]; then fault_read || true; fi
    {required} ;;
 'api graphql'*autoMergeAllowed*)
    if [ '{mode}' = merge ]; then fault_read || true; fi
    echo '{{"data":{{"repository":{{"autoMergeAllowed":false,"mergeCommitAllowed":true,"rebaseMergeAllowed":false,"squashMergeAllowed":false,"pullRequest":{{"baseRefName":"main","baseRef":{{"branchProtectionRule":null}}}}}}}}}}' ;;
 'api repos/{{owner}}/{{repo}}/pulls/17/merge'*) touch '{merged}'; printf '{{"merged":true,"sha":"{head}"}}' ;;
 *) echo "unknown gh $*" >&2; exit 99 ;;
esac
"#,
            calls = calls.display(),
            lost = lost.display(),
            resumed = resumed.display(),
            kind = kind,
            merged = merged.display(),
            landed = cat(&landed),
            mode = mode,
            clean = cat(&clean),
            blocked = cat(&blocked),
            required = cat(&required),
            head = head,
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    calls
}

fn assert_resumed_merge(
    f: &Fixture,
    checkout: &std::path::Path,
    calls: PathBuf,
    token: &str,
    reread: bool,
) {
    let state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    let (outcome, _) = run_tail(f);
    let outcome = outcome.unwrap();
    assert!(outcome.success, "{outcome:?}");
    let requests = std::fs::read_to_string(calls).unwrap();
    let requests = requests.lines().collect::<Vec<_>>();
    let failed = requests
        .iter()
        .position(|line| line.contains(token))
        .unwrap_or_else(|| panic!("the {token} read ran: {requests:#?}"));
    let next_view = requests[failed + 1..]
        .iter()
        .position(|line| line.starts_with("pr view"))
        .unwrap_or_else(|| panic!("fresh observation after the failed read: {requests:#?}"));
    let fresh = failed + 1 + next_view;
    assert_eq!(
        requests[failed + 1..fresh]
            .iter()
            .filter(|line| line.contains(token))
            .count(),
        if reread { 2 } else { 0 },
        "merge capabilities exhaust three attempts; deadline-bounded required checks resume immediately: {requests:#?}"
    );
    assert!(
        !requests[failed + 1..fresh]
            .iter()
            .any(|line| line.contains("/pulls/17/merge")),
        "exhausted reads never count as passing checks: {requests:#?}"
    );
    let later = requests[fresh..]
        .iter()
        .filter(|line| line.contains(token))
        .count();
    assert_eq!(
        later,
        usize::from(reread),
        "the resumed completion reads the provider again only when that read is still required: {requests:#?}"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|line| line.contains("/pulls/17/merge"))
            .count(),
        1,
        "one merge after the fresh observation"
    );
    assert_eq!(
        f.runtime.get_task(&f.task).unwrap().status,
        TaskStatus::Done
    );
    assert_eq!(git(checkout, &["rev-parse", "HEAD"]), f.head);
    assert_eq!(
        std::fs::read_to_string(f._root.path().join("pushes"))
            .unwrap()
            .lines()
            .count(),
        1,
        "completion never pushes"
    );
    let after = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    for (index, output) in &state.step_outputs {
        assert_eq!(after.step_output(*index), Some(output), "immutable prefix");
    }
    let recovery = after.final_recovery.unwrap();
    assert!(matches!(
        recovery.decision,
        Some(orbit_types::workflow::FinalRecoveryDecision::Resume { .. })
    ));
    assert!(state.completion_deadlines.is_empty());
    assert_eq!(
        after.completion_deadlines.len(),
        1,
        "the resumed observation keeps the first attempt's cumulative budget"
    );
    assert_eq!(
        f.runtime
            .get_task_history(&f.task)
            .unwrap()
            .iter()
            .filter(|row| row.event == orbit_types::workflow::PR_COMPLETION_MUTATION_STARTED_EVENT)
            .count(),
        1,
        "only the resumed merge is a completion mutation"
    );
}

/// [ORB-16340] A REST TLS handshake timeout on the required-checks read, and an
/// persistent HTTP 5xx on the merge-capability read, resume only that completion.
pub(crate) fn transient_required_check_and_merge_capability_reads_resume_completion() {
    for (mode, kind, token, reread) in [
        ("required", "tls", "isRequired(pullRequestNumber", false),
        ("merge", "http500", "autoMergeAllowed", true),
    ] {
        let (mut f, checkout) = fixture();
        configure_review(&mut f, &checkout);
        let calls = observation_forge(&f, mode, kind);
        assert_resumed_merge(&f, &checkout, calls, token, reread);
    }
}

/// Auth, permission, other permanent 4xx answers, and a malformed provider
/// body stay terminal. None of them resumes completion or merges.
pub(crate) fn permanent_required_check_and_merge_capability_answers_do_not_resume() {
    for (mode, kind, token) in [
        ("required", "forbidden", "isRequired(pullRequestNumber"),
        ("merge", "credentials", "autoMergeAllowed"),
        ("required", "malformed", "isRequired(pullRequestNumber"),
    ] {
        let (mut f, checkout) = fixture();
        configure_review(&mut f, &checkout);
        let calls = observation_forge(&f, mode, kind);
        let (outcome, _) = run_tail(&f);
        assert!(outcome.is_err(), "{kind} on {mode} resumed completion");
        let requests = std::fs::read_to_string(calls).unwrap();
        assert!(
            requests.contains(token),
            "{kind} never reached {token}: {requests}"
        );
        assert!(
            !requests.contains("/pulls/17/merge"),
            "{kind} merged: {requests}"
        );
        assert!(
            !matches!(
                f.runtime
                    .read_run_state(&f.source.run_id)
                    .unwrap()
                    .unwrap()
                    .final_recovery
                    .and_then(|recovery| recovery.decision),
                Some(orbit_types::workflow::FinalRecoveryDecision::Resume { .. })
            ),
            "{kind} was classified as a read-only observation retry"
        );
        assert_ne!(
            f.runtime.get_task(&f.task).unwrap().status,
            TaskStatus::Done
        );
    }
}

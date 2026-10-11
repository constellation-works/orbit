//! [ORB-15570] A claimed leaf whose before-landing review holds for named
//! evidence has already opened its pull request, which the hold leaves open.
//! The release carries it, the owner keeps it with the held candidate, and
//! the owner's run that resumes that candidate once the evidence arrived
//! closes it when its own pull request opens, so the task never has two
//! open Orbit pull requests.
//!
//! Git runs for real against a bare `origin`, and a substitute `gh` first on
//! the isolated child's `PATH` serves any number of pull requests from files
//! beside it, so the engine's own provider adapter lists, creates, views and
//! closes them.

use super::claimed_owed_evidence::{
    artifact, attach_result, ci_fast, codeql_rule, owed, owner_action, owner_checkout,
    owner_resumes, publish_origin, report,
};
use super::claimed_review::{REVIEW_CREW, ReviewedLeaf};
use super::*;

use orbit_types::task::HostOs;
use orbit_types::workflow::{REVIEW_EVIDENCE_HOLD_ARTIFACT, ReviewEvidenceHold, ReviewVerdict};

/// The pull request the leaf opened before its before-landing review.
const LEAF_PR: &str = "41";

/// The `gh` surface the provider adapter calls, over the pull requests kept
/// as `prs/<number>/{head,base,state}` under `__FORGE__`.
const FAKE_GH: &str = r#"#!/bin/sh
set -eu
forge='__FORGE__'

# The value following flag `$1` among the remaining arguments.
flag() {
    name=$1; shift
    while [ $# -gt 0 ]; do
        if [ "$1" = "$name" ]; then printf '%s' "$2"; return; fi
        shift
    done
}

url() { printf 'https://github.com/owner/repository/pull/%s' "$1"; }

case "$1 ${2:-}" in
    "pr list")
        head=$(flag --head "$@")
        printf '['
        sep=
        for dir in "$forge"/prs/*; do
            [ "$(cat "$dir/state")" = OPEN ] || continue
            [ "$(cat "$dir/head")" = "$head" ] || continue
            printf '%s{"number":%s,"title":"Delivery","headRefName":"%s","author":{"login":"orbit"}}' \
                "$sep" "${dir##*/}" "$head"
            sep=,
        done
        printf ']\n' ;;
    "pr create")
        last=$(ls "$forge/prs" | sort -n | tail -n 1)
        number=$(( ${last:-0} + 1 ))
        mkdir -p "$forge/prs/$number"
        flag --head "$@" > "$forge/prs/$number/head"
        flag --base "$@" > "$forge/prs/$number/base"
        printf 'OPEN' > "$forge/prs/$number/state"
        url "$number"; echo ;;
    "pr view")
        number=${3##*/}
        dir="$forge/prs/$number"
        printf '{"number":%s,"title":"Delivery","body":"","state":"%s","headRefName":"%s","baseRefName":"%s","files":[],"commits":[],"url":"%s"}\n' \
            "$number" "$(cat "$dir/state")" "$(cat "$dir/head")" "$(cat "$dir/base")" "$(url "$number")" ;;
    "pr close")
        flag --comment "$@" > "$forge/prs/$3/comment"
        printf 'CLOSED' > "$forge/prs/$3/state" ;;
    *) echo "fake gh: unsupported call: $*" >&2; exit 2 ;;
esac
"#;

/// Install the substitute `gh` in `HOME/bin`, which leads this child's
/// `PATH`, with `head`'s pull request open; returns its forge directory.
fn forge_with_open_pull_request(root: &Path, head: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let forge = root.join("forge");
    let opened = forge.join("prs").join(LEAF_PR);
    std::fs::create_dir_all(&opened).unwrap();
    std::fs::write(opened.join("head"), head).unwrap();
    std::fs::write(opened.join("base"), "main").unwrap();
    std::fs::write(opened.join("state"), "OPEN").unwrap();
    let bin = PathBuf::from(std::env::var("HOME").unwrap()).join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let gh = bin.join("gh");
    std::fs::write(&gh, FAKE_GH.replace("__FORGE__", forge.to_str().unwrap())).unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    forge
}

/// The pull requests the forge holds open, by number.
fn open_pull_requests(forge: &Path) -> Vec<String> {
    let mut open = std::fs::read_dir(forge.join("prs"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|dir| std::fs::read_to_string(dir.join("state")).unwrap() == "OPEN")
        .map(|dir| dir.file_name().unwrap().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    open.sort();
    open
}

/// A Mac leaf pushed its branch, opened #41 and held in its before-landing
/// review for the Linux CodeQL run it owes. Its release names #41 on that
/// branch, and the owner keeps it with the held candidate, which it keeps
/// from the branch the leaf published it to for evidence. Once the owner's
/// result arrives, the owner's own run resumes the held candidate, publishes
/// on its own branch, and its `pr_open` closes #41 as superseded.
#[test]
fn a_before_landing_evidence_hold_leaves_its_task_one_open_pull_request() {
    if !isolated(
        module_path!(),
        "a_before_landing_evidence_hold_leaves_its_task_one_open_pull_request",
    ) {
        return;
    }
    let mut leaf = ReviewedLeaf::admit_from(
        &format!(
            "[review]\nbefore_landing = true\n\n[operation]\nreview_crew = \"{REVIEW_CREW}\"\n{}",
            codeql_rule("**/*.rs")
        ),
        "",
    );
    leaf.bound = leaf.bound.clone().with_host_os(Some(HostOs::Macos));
    leaf.gate_input["before_landing"] = json!(true);
    let (root, origin) = publish_origin(&leaf);

    // The leaf's `push` and `pr_open`, before its before-landing review.
    let branch = format!("orbit/{}", leaf.task);
    let follower_repo = leaf.pair.follower_repo.clone();
    git(&follower_repo, &["push", "-q", "origin", &branch]);
    let pushed = git(&follower_repo, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    let forge = forge_with_open_pull_request(&root, &branch);
    let mut state = leaf
        .pair
        .follower
        .read_run_state(&leaf.leaf)
        .unwrap()
        .unwrap_or_else(|| PipelineState::new(leaf.leaf.clone(), LEAF_JOB.to_string(), json!({})));
    state.pipeline["push"] = json!({"branch": branch, "local_sha": pushed});
    state.pipeline["pr_open"] =
        json!({"decision": "performed", "pr_number": LEAF_PR, "head": branch});
    leaf.pair
        .follower
        .write_run_state(&leaf.leaf, &state)
        .unwrap();

    let admitted = leaf.admit_review();
    assert_eq!(admitted["timing"], "before-landing", "{admitted}");
    leaf.put_report(&report(
        &admitted,
        ReviewVerdict::Incomplete,
        Vec::new(),
        vec![ci_fast()],
        Some("Rust CodeQL cannot run on macOS"),
    ));
    let refused = leaf.settle().expect_err("missing evidence cannot land");
    assert!(refused.contains("review_awaiting_evidence"), "{refused}");
    let hold: ReviewEvidenceHold = artifact(&leaf, REVIEW_EVIDENCE_HOLD_ARTIFACT).unwrap();
    assert_eq!(hold.requirements, vec![owed()]);
    let evidence_branch = format!("orbit-evidence/{branch}");
    assert_eq!(
        hold.published_ref,
        Some(format!("refs/heads/{evidence_branch}"))
    );
    leaf.leaf_holds(&hold);
    leaf.pair.pass(&leaf.drain);
    assert_eq!(leaf.pair.owner_status(&leaf.task), "in-progress");

    // The release names the leaf's open pull request, and the owner keeps it
    // with the held candidate.
    let (claim_id, _) = leaf.claim();
    let release = leaf
        .pair
        .wire
        .calls("orbit.drain.claim.settle")
        .into_iter()
        .rev()
        .find(|settle| settle["claim_id"] == claim_id.as_str())
        .map(|settle| settle["settlement"]["Release"].clone())
        .expect("the leaf's release was delivered");
    assert_eq!(
        release["held_pull_request"],
        json!({"number": LEAF_PR, "head": branch}),
        "{release}"
    );
    let owner = leaf.pair.wire.owner.clone();
    let kept = owner
        .kept_claim_candidate(&leaf.task)
        .unwrap()
        .expect("the owner keeps the held candidate");
    assert_eq!(kept.candidate.branch, evidence_branch);
    assert_eq!(kept.candidate.head_sha, hold.candidate.commit);
    assert_eq!(kept.candidate.pull_request.as_deref(), Some(LEAF_PR));
    assert_eq!(
        kept.candidate.pull_request_head.as_deref(),
        Some(branch.as_str())
    );

    owner_checkout(&leaf, &origin);
    attach_result(&leaf, &owner, &hold);
    assert_eq!(leaf.pair.owner_status(&leaf.task), "backlog");

    let resumed = owner_resumes(&leaf, ReviewTiming::BeforeLanding);
    let output = &resumed.output;
    assert_eq!(output["outcome"], "resumed_held", "{output}");
    assert_eq!(
        output["source_branch"],
        evidence_branch.as_str(),
        "{output}"
    );
    assert_eq!(output["prior_pull_request"], LEAF_PR, "{output}");
    assert_eq!(
        output["prior_pull_request_head"],
        branch.as_str(),
        "{output}"
    );

    // The held candidate's summary, which the delivery gate reads.
    owner
        .update_task_with_identity(
            &leaf.task,
            orbit_core::application::task::TaskUpdateParams {
                execution_summary: Some(
                    "Outcome: success\nResumed the held candidate after its evidence arrived."
                        .into(),
                ),
                ..Default::default()
            },
            Some("codex".into()),
            None,
        )
        .unwrap();
    // The run's `push` and `pr_open`; its before-landing review follows.
    let head = git(&resumed.worktree, &["rev-parse", "--abbrev-ref", "HEAD"])
        .trim()
        .to_string();
    assert_ne!(head, branch, "the run publishes on its own branch");
    git(&resumed.worktree, &["push", "-q", "origin", &head]);
    let opened = owner_action(
        &owner,
        "pr_open",
        &json!({
            "job_run_id": resumed.run, "completed_task_ids": [leaf.task],
            "workspace_path": resumed.worktree, "head": head,
            "base": "main", "base_ref": "main", "base_sha": resumed.base_sha,
            "base_sync": "local", "candidate_resume": output,
        }),
    );
    assert_eq!(opened["decision"], "performed", "{opened}");
    let replacement = opened["pr_number"].as_str().unwrap().to_string();
    assert_ne!(replacement, LEAF_PR, "{opened}");
    let superseded = &opened["superseded_pull_request"];
    assert_eq!(superseded["number"], LEAF_PR, "{opened}");
    assert_eq!(superseded["decision"], "closed", "{opened}");
    assert_eq!(open_pull_requests(&forge), vec![replacement]);
    let comment = std::fs::read_to_string(forge.join("prs").join(LEAF_PR).join("comment")).unwrap();
    assert!(
        comment.contains(&format!("#{}", opened["pr_number"].as_str().unwrap())),
        "{comment}"
    );
}

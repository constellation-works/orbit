//! A newer merged delivery supersedes older PRs through public settlement.
//!
//! `TaskPrForge::close_pull_request` explicitly keeps the head branch; the
//! settlement uses only that operation and never deletes branches.

use super::*;

#[test]
fn settlement_records_each_superseded_pr_and_keeps_unrelated_prs_open() {
    if !super::super::dispatch_admission::isolated(
        "task_external_merge::pr_supersession::settlement_records_each_superseded_pr_and_keeps_unrelated_prs_open",
    ) {
        return;
    }
    for owner_pass in [false, true] {
        let fx = Fixture::new();
        let task = fx.add_task("Newer delivery merged", TaskStatus::Blocked, &[11, 12]);
        let other = fx.add_task("Unrelated delivery", TaskStatus::Review, &[21]);
        fx.forge.merge_recent(12, &task, BOT, &fx.base);
        {
            let mut state = fx.forge.0.lock().unwrap();
            // #13 was not recorded on the task: discover every older delivery,
            // rather than closing just the PR the latest run remembered.
            for (number, branch_task, author) in [
                (11, &task, BOT),
                (13, &task, BOT),
                (20, &task, "alice"),
                (21, &other, BOT),
            ] {
                state.open.insert(
                    number,
                    ForgePullRequest {
                        number,
                        head_branch: format!("orbit/{branch_task}-{number:08x}"),
                        body: format!("Delivers {branch_task}; mentions {task}."),
                        author: author.to_string(),
                    },
                );
            }
        }

        if owner_pass {
            let settlements = fx.runtime.settle_externally_merged_tasks().unwrap();
            assert_eq!(settlements.len(), 1);
            assert_eq!(settlements[0].pr_number, 12);
        } else {
            assert_eq!(
                fx.runtime
                    .settle_externally_merged_task(&task)
                    .unwrap()
                    .pr_number,
                12
            );
        }
        assert_eq!(fx.status(&task), TaskStatus::Done);
        let state = fx.forge.0.lock().unwrap();
        assert_eq!(state.closed, vec![11, 13]);
        assert_eq!(state.open.keys().copied().collect::<Vec<_>>(), vec![20, 21]);
        for number in [11, 13] {
            let comment = &state.closure_comments[&number];
            assert!(comment.contains("superseded by #12"), "{comment}");
            let history = fx.runtime.get_task_history(&task).unwrap();
            let entries = history
                .iter()
                .filter(|entry| entry.event == "pull_request_closed")
                .filter(|entry| {
                    entry
                        .note
                        .as_deref()
                        .unwrap_or_default()
                        .starts_with(&format!("Closed pull request #{number}:"))
                })
                .collect::<Vec<_>>();
            assert_eq!(entries.len(), 1, "each closure is durable task history");
            assert!(
                entries[0]
                    .note
                    .as_deref()
                    .unwrap()
                    .contains("superseded by #12")
            );
        }
    }
}

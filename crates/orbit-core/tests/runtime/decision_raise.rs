//! Agent-raised decision requests: an agent raises a bounded multiple-choice
//! question on a human-crew task through `orbit.decision.raise`; only the
//! operator answers; the answer lands in the task's history, completes the
//! task and unblocks its `blocked_by` dependents.

use std::path::PathBuf;

use orbit_core::application::task::TaskAddParams;
use orbit_core::{ActorIdentity, OrbitError, OrbitRuntime, TaskComplexity, TaskStatus};
use orbit_types::task::{
    DECISION_ANSWERED_EVENT, DecisionRequestState, TaskRelation, TaskRelationType,
};
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::git_test_support;

const CONFIG: &str = "[workflow]\ndefault_crew = \"sol\"\n\n\
     [crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n\n\
     [crews.dk]\nkind = \"human\"\ndescription = \"Daniel's own work\"\n";

struct Fixture {
    _root: TempDir,
    global: PathBuf,
    repo: PathBuf,
    runtime: OrbitRuntime,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let global = root.path().join("global");
        let repo = root.path().join("repo");
        let workspace = repo.join(".orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("config.toml"), CONFIG).unwrap();
        std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
        std::fs::write(repo.join("src/lib.rs"), "pub fn price() {}\n").unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "fixture@example.com"],
            &["config", "user.name", "Fixture"],
            &["config", "commit.gpgsign", "false"],
            &["add", "-A"],
            &["commit", "-q", "-m", "fixture"],
        ] {
            let status = git_test_support::command()
                .args(args)
                .current_dir(&repo)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        }
        let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
        Self {
            _root: root,
            global,
            repo,
            runtime,
        }
    }

    fn operator(&self) -> OrbitRuntime {
        OrbitRuntime::from_roots(&self.global, &self.repo.join(".orbit"))
            .unwrap()
            .with_actor(ActorIdentity::human("human:operator"))
    }

    fn task(&self, title: &str, crew: &str) -> String {
        self.task_with(title, crew, Vec::new())
    }

    fn task_with(&self, title: &str, crew: &str, relations: Vec<TaskRelation>) -> String {
        self.runtime
            .add_task(TaskAddParams {
                title: title.into(),
                description: "Fixture work".into(),
                acceptance_criteria: vec!["Done".into()],
                complexity: TaskComplexity::Low,
                context_files: vec!["file:src/lib.rs".into()],
                crew: Some(crew.into()),
                status: Some(TaskStatus::Backlog),
                relations,
                ..Default::default()
            })
            .unwrap()
            .id
            .to_string()
    }

    /// An agent's `orbit.decision.raise` call.
    fn raise(&self, filer: &str, input: Value) -> Result<Value, OrbitError> {
        let mut input = input;
        input["model"] = json!(filer);
        self.runtime.run_tool("orbit.decision.raise", input)
    }
}

fn options() -> Value {
    json!([
        {"label": "Keep the price", "consequence": "No change to model_prices.yaml."},
        {"label": "Raise the price 10%", "consequence": "The unblocked task edits model_prices.yaml.", "recommended": true},
        {"label": "Drop the model", "consequence": "The unblocked task removes the entry."},
    ])
}

fn raise_input(task: &str, question: &str) -> Value {
    json!({"task_id": task, "question": question, "options": options(), "topic": "pricing/sign-off"})
}

#[test]
fn equivalent_questions_are_deduplicated_only_within_their_task() {
    let fx = Fixture::new();
    let first = fx.task("First decision", "dk");
    let second = fx.task("Second decision", "dk");
    let dependent = |title: &str, decision: &str| {
        fx.task_with(
            title,
            "sol",
            vec![TaskRelation {
                relation_type: TaskRelationType::BlockedBy,
                target: decision.into(),
                regression: None,
            }],
        )
    };
    let first_dependent = dependent("First dependent", &first);
    let second_dependent = dependent("Second dependent", &second);
    let first_request = fx
        .raise("claude", raise_input(&first, "Approve the price?"))
        .unwrap();
    let retry = fx
        .raise("codex", raise_input(&first, "  APPROVE the price? "))
        .unwrap();
    assert_eq!(retry["deduplicated"], true);
    assert_eq!(retry["id"], first_request["id"]);
    let second_request = fx
        .raise("claude", raise_input(&second, "  APPROVE the price? "))
        .unwrap();
    assert_eq!(second_request["deduplicated"], false, "{second_request}");
    assert_ne!(second_request["id"], first_request["id"]);
    assert_eq!(
        fx.runtime.get_task_decision_requests(&first).unwrap().len(),
        1
    );
    assert_eq!(
        fx.runtime
            .get_task_decision_requests(&second)
            .unwrap()
            .len(),
        1
    );

    fx.operator()
        .answer_decision_request_as_human(
            first_request["id"].as_str().unwrap(),
            "1",
            None,
            "human:operator".into(),
            None,
        )
        .unwrap();
    assert_eq!(
        fx.runtime.get_task(&first).unwrap().status,
        TaskStatus::Done
    );
    assert_eq!(
        fx.runtime.get_task(&second).unwrap().status,
        TaskStatus::Backlog
    );
    assert_eq!(
        fx.runtime.get_task_decision_requests(&second).unwrap()[0].state,
        DecisionRequestState::Open
    );
    for (dependent, decision, expected) in [
        (&first_dependent, &first, TaskStatus::Done),
        (&second_dependent, &second, TaskStatus::Backlog),
    ] {
        let task = fx.runtime.get_task(dependent).unwrap();
        assert_eq!(
            fx.runtime
                .dependency_status_index([&task])
                .unwrap()
                .get(decision)
                .copied(),
            Some(expected)
        );
    }
}

#[test]
fn an_agent_raises_a_decision_and_only_the_operator_answers_it() {
    let fx = Fixture::new();
    let decision = fx.task("Sign off the price", "dk");
    let dependent = fx.task_with(
        "Apply the price",
        "sol",
        vec![TaskRelation {
            relation_type: TaskRelationType::BlockedBy,
            target: decision.clone(),
            regression: None,
        }],
    );

    // Raise: the recommended option is first and marked.
    let raised = fx
        .raise("claude", raise_input(&decision, "Which price do we ship?"))
        .unwrap();
    assert_eq!(raised["deduplicated"], false, "{raised}");
    assert_eq!(raised["gate"], "agent_request");
    assert_eq!(raised["state"], "open");
    assert_eq!(raised["recommended"], "1");
    assert_eq!(raised["topic"], "pricing/sign-off");
    assert_eq!(raised["answers"].as_array().unwrap().len(), 3);
    assert_eq!(raised["answers"][0]["label"], "Raise the price 10%");
    let id = raised["id"].as_str().unwrap().to_string();

    // The same open question is deduplicated, with different spacing and case.
    let again = fx
        .raise(
            "codex",
            raise_input(&decision, "  which PRICE do we   ship? "),
        )
        .unwrap();
    assert_eq!(again["deduplicated"], true, "{again}");
    assert_eq!(again["id"], id.as_str());
    assert_eq!(
        fx.runtime
            .get_task_decision_requests(&decision)
            .unwrap()
            .len(),
        1
    );

    // A task holds one open request: a different question is refused.
    let second = fx
        .raise("claude", raise_input(&decision, "Another question?"))
        .expect_err("one open request per task");
    assert!(second.to_string().contains("already holds"), "{second}");

    // The topic is an exact filter.
    let by_topic = fx
        .runtime
        .run_tool("orbit.decision.list", json!({"topic": "pricing/sign-off"}))
        .unwrap();
    assert_eq!(by_topic["count"], 1, "{by_topic}");
    let other_topic = fx
        .runtime
        .run_tool("orbit.decision.list", json!({"topic": "pricing"}))
        .unwrap();
    assert_eq!(other_topic["count"], 0, "{other_topic}");

    // An agent cannot answer, as a tool or as a writer label.
    let tool = fx
        .runtime
        .run_tool("orbit.decision.answer", json!({"id": id, "answer": "1"}));
    assert!(tool.is_err(), "answer is not a tool: {tool:?}");
    let as_agent = fx
        .runtime
        .answer_decision_request_as_human(&id, "1", None, "claude".into(), None)
        .expect_err("an agent label is refused");
    assert!(as_agent.to_string().contains("automation"), "{as_agent}");
    assert_eq!(
        fx.runtime.get_task(&decision).unwrap().status,
        TaskStatus::Backlog
    );

    // The dependent waits while the decision is open.
    let blocker = |fx: &Fixture| {
        let task = fx.runtime.get_task(&dependent).unwrap();
        fx.runtime
            .dependency_status_index([&task])
            .unwrap()
            .get(&decision)
            .copied()
    };
    assert_eq!(blocker(&fx), Some(TaskStatus::Backlog));

    // The operator answers: recorded, task done, dependent's blocker satisfied.
    let answered = fx
        .operator()
        .answer_decision_request_as_human(
            &id,
            "2",
            Some("Ship it.".into()),
            "human:operator".into(),
            Some("cli".into()),
        )
        .unwrap();
    assert_eq!(answered.state, DecisionRequestState::Answered);
    let resolution = answered.resolution.unwrap();
    assert_eq!(resolution.answer_id, "2");
    assert_eq!(resolution.answered_by, "human:operator");
    assert_eq!(
        fx.runtime.get_task(&decision).unwrap().status,
        TaskStatus::Done
    );
    assert_eq!(blocker(&fx), Some(TaskStatus::Done));

    let history = fx.runtime.get_task_history(&decision).unwrap();
    let entry = history
        .iter()
        .find(|entry| entry.event == DECISION_ANSWERED_EVENT)
        .expect("the answer is in the task history");
    assert_eq!(entry.by, "human:operator");
    let comments = fx.runtime.get_task_comments(&decision).unwrap();
    let comment = comments
        .iter()
        .find(|comment| comment.message.starts_with("Decision answer:"))
        .expect("the chosen option is commented");
    assert!(
        comment.message.contains("Keep the price")
            && comment.message.contains("No change to model_prices.yaml."),
        "{}",
        comment.message
    );
    assert!(comment.message.contains("Ship it."), "{}", comment.message);

    // A closed request cannot be answered again.
    fx.operator()
        .answer_decision_request_as_human(&id, "1", None, "human:operator".into(), None)
        .expect_err("already answered");
}

#[test]
fn concurrent_raises_for_distinct_tasks_do_not_deadlock_on_bundle_locks() {
    use std::sync::{Arc, Barrier};

    let fx = Fixture::new();
    let first = fx.task("First concurrent decision", "dk");
    let second = fx.task("Second concurrent decision", "dk");
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for (task, question) in [
        (first.clone(), "First concurrent question?"),
        (second.clone(), "Second concurrent question?"),
    ] {
        let runtime = fx.runtime.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            let mut input = raise_input(&task, question);
            input["model"] = json!("claude");
            orbit_core::test_support::with_decision_raise_checkpoint(
                move || {
                    barrier.wait();
                },
                || runtime.run_tool("orbit.decision.raise", input),
            )
        }));
    }
    let mut raised = Vec::new();
    for handle in handles {
        let result = handle.join().expect("raise thread panicked");
        let value = result.unwrap_or_else(|error| {
            panic!("concurrent raise must finish without a lock timeout: {error}")
        });
        assert_eq!(value["deduplicated"], false, "{value}");
        raised.push(value);
    }
    assert_ne!(raised[0]["id"], raised[1]["id"]);
    assert_eq!(
        fx.runtime.get_task_decision_requests(&first).unwrap().len(),
        1
    );
    assert_eq!(
        fx.runtime
            .get_task_decision_requests(&second)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn concurrent_same_question_raises_on_one_task_deduplicate() {
    use std::sync::{Arc, Barrier};

    let fx = Fixture::new();
    let task = fx.task("Shared concurrent decision", "dk");
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let runtime = fx.runtime.clone();
        let barrier = Arc::clone(&barrier);
        let task = task.clone();
        handles.push(std::thread::spawn(move || {
            let mut input = raise_input(&task, "Same concurrent question?");
            input["model"] = json!("claude");
            orbit_core::test_support::with_decision_raise_checkpoint(
                move || {
                    barrier.wait();
                },
                || runtime.run_tool("orbit.decision.raise", input),
            )
        }));
    }
    let mut raised = Vec::new();
    for handle in handles {
        raised.push(handle.join().expect("raise thread panicked").unwrap());
    }
    assert_eq!(raised[0]["id"], raised[1]["id"], "{raised:?}");
    assert_eq!(
        raised
            .iter()
            .filter(|value| value["deduplicated"] == true)
            .count(),
        1,
        "{raised:?}"
    );
    assert_eq!(
        fx.runtime.get_task_decision_requests(&task).unwrap().len(),
        1
    );
}

#[test]
fn raising_is_refused_for_bad_input_agent_crews_and_past_the_caps() {
    let fx = Fixture::new();

    // Not a human-crew task.
    let agent_task = fx.task("Agent work", "sol");
    let refused = fx
        .raise("claude", raise_input(&agent_task, "Who decides?"))
        .expect_err("only a human-crew task holds a decision");
    assert!(refused.to_string().contains("human crew"), "{refused}");

    // Option shapes.
    let human = fx.task("Human work", "dk");
    let option = |label: &str, recommended: bool| json!({"label": label, "consequence": "Does it.", "recommended": recommended});
    for (options, expected) in [
        (json!([option("A", true)]), "two to four options"),
        (
            json!([
                option("A", true),
                option("B", false),
                option("C", false),
                option("D", false),
                option("E", false)
            ]),
            "two to four options",
        ),
        (
            json!([option("A", false), option("B", false)]),
            "exactly one option",
        ),
        (
            json!([option("A", true), option("B", true)]),
            "exactly one option",
        ),
        (
            json!([option("A\nmore", true), option("B", false)]),
            "one line",
        ),
    ] {
        let refused = fx
            .raise(
                "claude",
                json!({"task_id": human, "question": "Q?", "options": options}),
            )
            .expect_err("malformed options are refused");
        assert!(
            refused.to_string().contains(expected),
            "{expected}: {refused}"
        );
    }
    let bad_topic = fx
        .raise(
            "claude",
            json!({"task_id": human, "question": "Q?", "options": options(), "topic": "has space"}),
        )
        .expect_err("a topic is a token");
    assert!(bad_topic.to_string().contains("topic"), "{bad_topic}");
    assert!(
        fx.runtime
            .get_task_decision_requests(&human)
            .unwrap()
            .is_empty(),
        "nothing was written"
    );

    // Per-filer cap: five open requests, the sixth is refused.
    let mut tasks = Vec::new();
    for index in 0..21 {
        tasks.push(fx.task(&format!("Decision {index}"), "dk"));
    }
    for (index, task) in tasks.iter().take(5).enumerate() {
        fx.raise("claude", raise_input(task, &format!("Question {index}?")))
            .unwrap();
    }
    let capped = fx
        .raise("claude", raise_input(&tasks[5], "Question 5?"))
        .expect_err("per-filer cap");
    assert!(capped.to_string().contains("`claude`"), "{capped}");
    assert!(
        fx.runtime
            .get_task_decision_requests(&tasks[5])
            .unwrap()
            .is_empty(),
        "a refused raise writes nothing"
    );
    // A repeat of an open question still dedupes past the cap.
    let repeat = fx
        .raise("claude", raise_input(&tasks[0], "Question 0?"))
        .unwrap();
    assert_eq!(repeat["deduplicated"], true, "{repeat}");

    // Per-workspace cap: twenty open requests across filers, the next is refused.
    let mut next = 5;
    for filer in ["codex", "gemini", "grok"] {
        for _ in 0..5 {
            fx.raise(
                filer,
                raise_input(&tasks[next], &format!("Question {next}?")),
            )
            .unwrap();
            next += 1;
        }
    }
    assert_eq!(next, 20);
    let full = fx
        .raise("muse", raise_input(&tasks[20], "Question 20?"))
        .expect_err("per-workspace cap");
    assert!(full.to_string().contains("workspace"), "{full}");

    // Answering one frees a slot.
    let open = fx
        .runtime
        .list_decision_requests(None, Some(&tasks[0]), false)
        .unwrap();
    fx.operator()
        .answer_decision_request_as_human(&open[0].id, "1", None, "human:operator".into(), None)
        .unwrap();
    fx.raise("muse", raise_input(&tasks[20], "Question 20?"))
        .unwrap();
}

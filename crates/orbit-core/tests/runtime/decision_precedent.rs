//! Curated decision rules: consolidation proposes markdown and writes nothing,
//! and only an operator-approved current auto rule answers a matching request.

use std::path::PathBuf;

use orbit_core::application::task::{
    ConsolidationAnswer, CorpusDecisionDocument, CorpusDigest, CorpusRuleStatus, TaskAddParams,
    citable_decision_rules, consolidate_decision_corpus, corpus_content_digest,
    parse_decision_rules,
};
use orbit_core::{ActorIdentity, OrbitError, OrbitRuntime, TaskComplexity, TaskStatus};
use orbit_types::task::{DecisionRequestState, TaskRelation, TaskRelationType};
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::git_test_support;

const CONFIG: &str = "[workflow]\ndefault_crew = \"sol\"\n\n\
     [crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n\n\
     [crews.dk]\nkind = \"human\"\ndescription = \"Daniel's own work\"\n";

const TODAY: &str = "2026-10-31";
const FUTURE: &str = "2027-06-01";

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

    fn task(&self, title: &str) -> String {
        self.task_with(title, Vec::new())
    }

    fn task_with(&self, title: &str, relations: Vec<TaskRelation>) -> String {
        self.runtime
            .add_task(TaskAddParams {
                title: title.into(),
                description: "Fixture work".into(),
                acceptance_criteria: vec!["Done".into()],
                complexity: TaskComplexity::Low,
                context_files: vec!["file:src/lib.rs".into()],
                crew: Some("dk".into()),
                status: Some(TaskStatus::Backlog),
                relations,
                ..Default::default()
            })
            .unwrap()
            .id
            .to_string()
    }

    fn raise_as(&self, model: &str, input: Value) -> Result<Value, OrbitError> {
        let mut input = input;
        input["model"] = json!(model);
        self.runtime.run_tool("orbit.decision.raise", input)
    }

    fn raise(&self, input: Value) -> Result<Value, OrbitError> {
        self.raise_as("claude", input)
    }

    fn write_rules(&self, markdown: &str) {
        let dir = self.repo.join("docs/agents/corpus/decisions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pricing.md"), markdown).unwrap();
    }

    fn rule_bytes(&self) -> Vec<u8> {
        std::fs::read(self.repo.join("docs/agents/corpus/decisions/pricing.md")).unwrap()
    }
}

fn options() -> Value {
    json!([
        {"label": "Keep the price", "consequence": "No change to the price table."},
        {"label": "Raise the price 10%", "consequence": "The unblocked task edits the price table.", "recommended": true},
        {"label": "Drop the model", "consequence": "The unblocked task removes the entry."},
    ])
}

fn raise_input(task: &str, question: &str, topic: Option<&str>) -> Value {
    let mut input = json!({"task_id": task, "question": question, "options": options()});
    if let Some(topic) = topic {
        input["topic"] = json!(topic);
    }
    input
}

fn rule_doc(answer: &str, status: &str, review_by: &str, auto: bool, digest: &str) -> String {
    let auto_by = if auto {
        "\nauto_set_by: \"operator\""
    } else {
        ""
    };
    let auto_word = if auto { "true" } else { "false" };
    format!(
        "---\ntype: decision\napplies_to: [agent_implement]\nstatus: active\nlinks: []\n---\n\n# Pricing\n\n## Scope\n\nCurated rules.\n\n## Entries\n\n## Rules\n\n```yaml\nid: \"sign-off\"\nquestion: \"Which price do we ship?\"\nanswer: \"{answer}\"\nwhy: \"The operator chose this option.\"\ncitations:\n  - task: \"T-1\"\n    date: \"2026-01-01\"\n    option: \"{answer}\"\ndepends_on:\n  - kind: \"path\"\n    name: \"src/lib.rs\"\n    digest: \"{digest}\"\nstatus: \"{status}\"\nreview_by: \"{review_by}\"\nauto: {auto_word}{auto_by}\n```\n"
    )
}

fn digest_of(name: &str, digest: &str) -> CorpusDigest {
    CorpusDigest {
        kind: "path".into(),
        name: name.into(),
        digest: digest.into(),
    }
}

fn document(markdown: &str) -> CorpusDecisionDocument {
    CorpusDecisionDocument {
        path: "decisions/pricing.md".into(),
        markdown: markdown.into(),
    }
}

fn answer(task: &str, option: &str, date: &str) -> ConsolidationAnswer {
    ConsolidationAnswer {
        task_id: task.into(),
        topic: "pricing/sign-off".into(),
        question: "Which price do we ship?".into(),
        answer_id: option.into(),
        note: Some("Ship the chosen price.".into()),
        answered_on: date.into(),
    }
}

fn resulting_rules(
    original: &str,
    proposals: &[orbit_core::application::task::ProposedCorpusDocument],
) -> Vec<orbit_core::application::task::CorpusDecisionRule> {
    let markdown = proposals
        .iter()
        .find(|proposal| proposal.path == "decisions/pricing.md")
        .map(|proposal| proposal.markdown.as_str())
        .unwrap_or(original);
    parse_decision_rules(markdown)
}

#[test]
fn consolidation_supersedes_contradictions_and_marks_stale_rules() {
    let digest = corpus_content_digest(b"pub fn price() {}\n");
    let current = rule_doc("1", "current", FUTURE, true, &digest);
    let before = current.clone();
    let documents = [document(&current)];

    let contradicted = consolidate_decision_corpus(
        &documents,
        &[answer("T-2", "2", "2026-10-01")],
        &[digest_of("src/lib.rs", &digest)],
        TODAY,
    );
    assert_eq!(
        documents[0].markdown, before,
        "consolidation must not edit the input"
    );
    let rules = resulting_rules(&current, &contradicted);
    let kept = rules.iter().find(|rule| rule.id == "sign-off").unwrap();
    assert_eq!(kept.status, CorpusRuleStatus::Current);
    assert_eq!(kept.answer, "2");
    assert!(!kept.auto);
    assert!(kept.auto_set_by.is_none());
    assert!(kept.citations.iter().any(|citation| citation.task == "T-2"));
    let superseded = rules
        .iter()
        .find(|rule| rule.status == CorpusRuleStatus::Superseded)
        .unwrap();
    assert!(superseded.id.starts_with("sign-off--superseded-"));
    assert!(!superseded.auto);
    assert!(superseded.auto_set_by.is_none());

    let agreed = consolidate_decision_corpus(
        &documents,
        &[answer("T-3", "1", "2026-10-02")],
        &[digest_of("src/lib.rs", &digest)],
        TODAY,
    );
    let agreed_rules = resulting_rules(&current, &agreed);
    let agreed_rule = agreed_rules
        .iter()
        .find(|rule| rule.id == "sign-off")
        .unwrap();
    assert_eq!(agreed_rule.status, CorpusRuleStatus::Current);
    assert!(agreed_rule.auto);
    assert_eq!(agreed_rule.auto_set_by.as_deref(), Some("operator"));
    assert!(
        agreed_rule
            .citations
            .iter()
            .any(|citation| citation.task == "T-3")
    );

    let drifted = consolidate_decision_corpus(
        &documents,
        &[],
        &[digest_of("src/lib.rs", "different")],
        TODAY,
    );
    let drifted_rules = resulting_rules(&current, &drifted);
    assert_eq!(
        drifted_rules
            .iter()
            .find(|rule| rule.id == "sign-off")
            .unwrap()
            .status,
        CorpusRuleStatus::Stale
    );

    let expired = rule_doc("1", "current", "2020-01-01", true, &digest);
    let aged = consolidate_decision_corpus(
        &[document(&expired)],
        &[],
        &[digest_of("src/lib.rs", &digest)],
        TODAY,
    );
    let aged_rules = resulting_rules(&expired, &aged);
    assert_eq!(
        aged_rules
            .iter()
            .find(|rule| rule.id == "sign-off")
            .unwrap()
            .status,
        CorpusRuleStatus::Stale
    );

    let already = rule_doc("1", "superseded", "2020-01-01", false, &digest);
    let stayed = consolidate_decision_corpus(
        &[document(&already)],
        &[answer("T-4", "2", "2026-10-03")],
        &[digest_of("src/lib.rs", &digest)],
        TODAY,
    );
    let stayed_rules = resulting_rules(&already, &stayed);
    assert!(
        stayed_rules
            .iter()
            .all(|rule| rule.id != "sign-off" || rule.status == CorpusRuleStatus::Superseded)
    );
    assert!(
        stayed_rules
            .iter()
            .all(|rule| rule.status == CorpusRuleStatus::Superseded)
    );

    let fresh = consolidate_decision_corpus(&[], &[answer("T-5", "1", "2026-10-04")], &[], TODAY);
    let created = fresh
        .iter()
        .find(|proposal| proposal.path == "decisions/pricing.md")
        .unwrap();
    let created_rules = parse_decision_rules(&created.markdown);
    assert_eq!(created_rules.len(), 1);
    assert_eq!(created_rules[0].status, CorpusRuleStatus::Current);
    assert!(!created_rules[0].auto);
    assert!(created_rules[0].auto_set_by.is_none());
    assert!(created.markdown.contains("## Scope"));
    assert!(created.markdown.contains("## Entries"));

    let citable = citable_decision_rules(&current, TODAY, &[digest_of("src/lib.rs", &digest)]);
    assert_eq!(citable.len(), 1);
    assert!(citable_decision_rules(&current, TODAY, &[digest_of("src/lib.rs", "nope")]).is_empty());
    assert!(
        citable_decision_rules(&expired, TODAY, &[digest_of("src/lib.rs", &digest)]).is_empty()
    );
    assert!(
        citable_decision_rules(
            &rule_doc("1", "stale", FUTURE, true, &digest),
            TODAY,
            &[digest_of("src/lib.rs", &digest)]
        )
        .is_empty()
    );
    assert!(
        citable_decision_rules(
            &rule_doc("1", "superseded", FUTURE, true, &digest),
            TODAY,
            &[digest_of("src/lib.rs", &digest)]
        )
        .is_empty()
    );
}

#[test]
fn an_operator_approved_auto_rule_answers_and_can_be_reversed() {
    let fx = Fixture::new();
    let digest = corpus_content_digest(b"pub fn price() {}\n");
    fx.write_rules(&rule_doc("1", "current", FUTURE, false, &digest));

    let waiting = fx.task("Waiting on a price");
    let open = fx
        .raise(raise_input(
            &waiting,
            "Which price do we ship?",
            Some("pricing/sign-off"),
        ))
        .unwrap();
    assert_eq!(open["state"], "open", "{open}");
    assert!(open.get("resolution").is_none(), "{open}");

    let agent_set = fx
        .runtime
        .set_decision_rule_auto("pricing/sign-off", true, "claude")
        .expect_err("an agent cannot set auto");
    assert!(agent_set.to_string().contains("operator"), "{agent_set}");
    let precedent_set = fx
        .runtime
        .set_decision_rule_auto("pricing/sign-off", true, "precedent")
        .expect_err("precedent cannot set auto");
    assert!(
        precedent_set.to_string().contains("operator"),
        "{precedent_set}"
    );
    let tool_set = fx.runtime.run_tool(
        "orbit.decision.rule_auto",
        json!({"rule": "pricing/sign-off", "auto": "on"}),
    );
    assert!(
        tool_set.is_err(),
        "rule-auto is not an agent tool: {tool_set:?}"
    );
    let still = parse_decision_rules(&String::from_utf8(fx.rule_bytes()).unwrap());
    assert!(
        !still
            .iter()
            .find(|rule| rule.id == "sign-off")
            .unwrap()
            .auto
    );

    let duplicate = fx
        .raise(raise_input(
            &fx.task("Duplicate price question"),
            "Which price do we ship?",
            Some("pricing/sign-off"),
        ))
        .unwrap();
    assert_eq!(duplicate["deduplicated"], true, "{duplicate}");
    assert_eq!(duplicate["state"], "open", "{duplicate}");

    fx.operator()
        .set_decision_rule_auto("pricing/sign-off", true, "human:operator")
        .unwrap();
    let armed = parse_decision_rules(&String::from_utf8(fx.rule_bytes()).unwrap());
    let armed_rule = armed.iter().find(|rule| rule.id == "sign-off").unwrap();
    assert!(armed_rule.auto);
    assert_eq!(armed_rule.auto_set_by.as_deref(), Some("operator"));

    let still_open = fx
        .raise(raise_input(
            &fx.task("Still the open question"),
            "Which price do we ship?",
            Some("pricing/sign-off"),
        ))
        .unwrap();
    assert_eq!(still_open["deduplicated"], true, "{still_open}");
    assert_eq!(still_open["state"], "open", "{still_open}");

    let decision = fx.task("Sign off a different wording");
    let dependent = fx
        .runtime
        .add_task(TaskAddParams {
            title: "Apply the price".into(),
            description: "Fixture work".into(),
            acceptance_criteria: vec!["Done".into()],
            complexity: TaskComplexity::Low,
            context_files: vec!["file:src/lib.rs".into()],
            crew: Some("sol".into()),
            status: Some(TaskStatus::Backlog),
            relations: vec![TaskRelation {
                relation_type: TaskRelationType::BlockedBy,
                target: decision.clone(),
                regression: None,
            }],
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    let raised = fx
        .raise(raise_input(
            &decision,
            "Do we raise this year's price?",
            Some("pricing/sign-off"),
        ))
        .unwrap();
    assert_eq!(raised["deduplicated"], false, "{raised}");
    assert_eq!(raised["state"], "answered", "{raised}");
    assert_eq!(raised["resolution"]["answered_by"], "precedent");
    assert_eq!(raised["resolution"]["answer_id"], "1");
    assert_eq!(raised["resolution"]["precedent"]["rule_id"], "sign-off");
    assert_eq!(raised["resolution"]["precedent"]["operator_approved"], true);
    assert_eq!(
        raised["resolution"]["precedent"]["citations"][0]["task"],
        "T-1"
    );
    let request_id = raised["id"].as_str().unwrap();
    assert_eq!(
        fx.runtime.get_task(&decision).unwrap().status,
        TaskStatus::Done
    );
    let comments = fx.runtime.get_task_comments(&decision).unwrap();
    let notice = comments
        .iter()
        .find(|comment| comment.by == "precedent")
        .unwrap();
    assert!(notice.message.contains("sign-off"), "{}", notice.message);
    assert!(notice.message.contains("operator"), "{}", notice.message);
    assert!(
        notice
            .message
            .contains(&format!("orbit decision reverse {request_id}")),
        "{}",
        notice.message
    );
    assert!(notice.message.contains("T-1"), "{}", notice.message);
    let blocker = fx.runtime.get_task(&dependent).unwrap();
    assert_eq!(
        fx.runtime
            .dependency_status_index([&blocker])
            .unwrap()
            .get(&decision)
            .copied(),
        Some(TaskStatus::Done)
    );

    let agent_reverse = fx
        .runtime
        .reverse_precedent_answer(request_id, "claude")
        .expect_err("an agent cannot reverse");
    assert!(
        agent_reverse.to_string().contains("operator"),
        "{agent_reverse}"
    );
    let tool_reverse = fx
        .runtime
        .run_tool("orbit.decision.reverse", json!({"id": request_id}));
    assert!(
        tool_reverse.is_err(),
        "reverse is not an agent tool: {tool_reverse:?}"
    );
    assert_eq!(
        fx.runtime.get_task(&decision).unwrap().status,
        TaskStatus::Done
    );

    let reopened = fx
        .operator()
        .reverse_precedent_answer(request_id, "human:operator")
        .unwrap();
    assert_eq!(reopened.state, DecisionRequestState::Open);
    assert!(reopened.resolution.is_none());
    assert_eq!(
        fx.runtime.get_task(&decision).unwrap().status,
        TaskStatus::Backlog
    );
    let blocker = fx.runtime.get_task(&dependent).unwrap();
    assert_eq!(
        fx.runtime
            .dependency_status_index([&blocker])
            .unwrap()
            .get(&decision)
            .copied(),
        Some(TaskStatus::Backlog)
    );
    let again = fx
        .raise(raise_input(
            &fx.task("Repeat the answered wording"),
            "Do we raise this year's price?",
            Some("pricing/sign-off"),
        ))
        .unwrap();
    assert_eq!(again["deduplicated"], true, "{again}");
    assert_eq!(again["state"], "open", "{again}");

    let as_precedent = fx
        .runtime
        .answer_decision_request_as_human(request_id, "1", None, "precedent".into(), None)
        .expect_err("precedent is not a human answer");
    assert!(
        as_precedent.to_string().contains("precedent"),
        "{as_precedent}"
    );
    let as_agent = fx
        .runtime
        .answer_decision_request_as_human(request_id, "1", None, "claude".into(), None)
        .expect_err("an agent label is refused");
    assert!(as_agent.to_string().contains("automation"), "{as_agent}");

    let bytes = fx.rule_bytes();
    fx.operator()
        .answer_decision_request_as_human(
            open["id"].as_str().unwrap(),
            "2",
            Some("Ship a different price.".into()),
            "human:operator".into(),
            Some("cli".into()),
        )
        .unwrap();
    let proposal = fx.runtime.consolidate_decision_precedent(None).unwrap();
    assert_eq!(fx.rule_bytes(), bytes, "consolidate writes no corpus file");
    assert!(proposal["count"].as_u64().unwrap() >= 1, "{proposal}");
    let markdown = proposal["proposals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["path"] == "docs/agents/corpus/decisions/pricing.md")
        .unwrap()["markdown"]
        .as_str()
        .unwrap();
    let proposed = parse_decision_rules(markdown);
    let current = proposed.iter().find(|rule| rule.id == "sign-off").unwrap();
    assert_eq!(current.answer, "2");
    assert_eq!(current.status, CorpusRuleStatus::Current);
    assert!(!current.auto, "consolidation must not set auto");
    assert!(
        proposed
            .iter()
            .any(|rule| rule.status == CorpusRuleStatus::Superseded)
    );
}

#[test]
fn non_auto_stale_and_ambiguous_rules_do_not_answer() {
    let fx = Fixture::new();
    let digest = corpus_content_digest(b"pub fn price() {}\n");
    let cases = [
        (
            rule_doc("1", "current", FUTURE, false, &digest),
            Some("pricing/sign-off"),
            "auto off",
        ),
        (
            rule_doc("1", "stale", FUTURE, true, &digest),
            Some("pricing/sign-off"),
            "stale",
        ),
        (
            rule_doc("1", "superseded", FUTURE, true, &digest),
            Some("pricing/sign-off"),
            "superseded",
        ),
        (
            rule_doc("1", "current", "2020-01-01", true, &digest),
            Some("pricing/sign-off"),
            "past review",
        ),
        (
            rule_doc("1", "current", FUTURE, true, "not-the-digest"),
            Some("pricing/sign-off"),
            "changed dependency",
        ),
        (
            rule_doc("9", "current", FUTURE, true, &digest),
            Some("pricing/sign-off"),
            "option not offered",
        ),
        (
            rule_doc("1", "current", FUTURE, true, &digest),
            None,
            "no topic",
        ),
    ];
    // A filer may hold five open agent requests. Rotate families so that cap
    // is not what these cases exercise.
    const FILERS: [&str; 5] = ["claude", "codex", "gemini", "grok", "muse"];
    for (index, (markdown, topic, label)) in cases.into_iter().enumerate() {
        fx.write_rules(&markdown);
        let task = fx.task(&format!("case {index}"));
        let raised = fx
            .raise_as(
                FILERS[index % FILERS.len()],
                raise_input(&task, &format!("Question {index} for {label}?"), topic),
            )
            .unwrap();
        assert_eq!(raised["state"], "open", "{label}: {raised}");
        assert!(raised.get("resolution").is_none(), "{label}: {raised}");
        assert_eq!(
            fx.runtime.get_task(&task).unwrap().status,
            TaskStatus::Backlog
        );
    }

    let ambiguous = format!(
        "{}\n```yaml\nid: \"drop-model\"\nquestion: \"Drop it?\"\nanswer: \"3\"\nwhy: \"Also approved.\"\ncitations: []\ndepends_on: []\nstatus: \"current\"\nreview_by: \"{FUTURE}\"\nauto: true\nauto_set_by: \"operator\"\n```\n",
        rule_doc("1", "current", FUTURE, true, &digest)
    );
    fx.write_rules(&ambiguous);
    let task = fx.task("ambiguous topic");
    let raised = fx
        .raise_as(
            "gemini",
            raise_input(
                &task,
                "Which file-level price rule applies?",
                Some("pricing"),
            ),
        )
        .unwrap();
    assert_eq!(raised["state"], "open", "{raised}");
    assert_eq!(
        fx.runtime.get_task(&task).unwrap().status,
        TaskStatus::Backlog
    );
}

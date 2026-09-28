#![allow(missing_docs)]

mod run {
    #![allow(missing_docs)]

    use std::collections::{HashSet, VecDeque};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use orbit_common::OrbitError;
    use orbit_tools::{
        OrbitBuiltinAction, OrbitTaskScope, OrbitToolHost, ReservationOwnerContext, ToolContext,
        ToolRegistry,
    };
    use orbit_types::workflow::activity_job::OnDenial;
    use serde_json::{Value, json};

    use super::super::super::agent_loop::*;
    use super::super::super::audit::NullSink;
    use super::super::super::session::Session;
    use super::super::super::{
        ContentBlock, LoopTransport, Message, MessageRole, StopReason, TransportError, TurnRequest,
        TurnResponse, TurnUsage,
    };

    #[derive(Default)]
    struct RecordingTransport {
        advertised: Mutex<Vec<Vec<String>>>,
        calls: Mutex<usize>,
    }

    impl RecordingTransport {
        fn advertised(&self) -> Vec<Vec<String>> {
            self.advertised.lock().expect("advertised mutex").clone()
        }
    }

    impl LoopTransport for RecordingTransport {
        fn provider(&self) -> &str {
            "test"
        }

        fn model(&self) -> &str {
            "test-model"
        }

        fn send_turn(&self, req: &TurnRequest<'_>) -> Result<TurnResponse, TransportError> {
            self.advertised
                .lock()
                .expect("advertised mutex")
                .push(req.tools.iter().map(|tool| tool.name.clone()).collect());

            let mut calls = self.calls.lock().expect("calls mutex");
            let call_index = *calls;
            *calls += 1;

            let (content, stop_reason) = if call_index == 0 {
                (
                    vec![ContentBlock::ToolUse {
                        id: "call-1".to_string(),
                        name: "orbit.task.show".to_string(),
                        input: json!({ "id": "T-test" }),
                    }],
                    StopReason::ToolUse,
                )
            } else {
                (
                    vec![ContentBlock::Text {
                        text: "done".to_string(),
                    }],
                    StopReason::EndTurn,
                )
            };

            Ok(TurnResponse {
                content,
                stop_reason,
                usage: TurnUsage::default(),
                raw_request_body: Vec::new(),
                raw_response_body: Vec::new(),
                endpoint: String::new(),
                http_status: 200,
            })
        }
    }

    #[derive(Default)]
    struct DenialContinueTransport {
        calls: Mutex<usize>,
    }

    impl LoopTransport for DenialContinueTransport {
        fn provider(&self) -> &str {
            "test"
        }

        fn model(&self) -> &str {
            "test-model"
        }

        fn send_turn(&self, req: &TurnRequest<'_>) -> Result<TurnResponse, TransportError> {
            let mut calls = self.calls.lock().expect("calls mutex");
            let call_index = *calls;
            *calls += 1;

            let (content, stop_reason) = if call_index == 0 {
                (
                    vec![ContentBlock::ToolUse {
                        id: "denied-1".to_string(),
                        name: "orbit.task.delete".to_string(),
                        input: json!({ "path": "/tmp/blocked.txt" }),
                    }],
                    StopReason::ToolUse,
                )
            } else {
                let last_message = req.messages.last().expect("tool result user message");
                assert_eq!(last_message.role, MessageRole::User);
                let [
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    },
                ] = last_message.content.as_slice()
                else {
                    panic!("expected one tool_result block");
                };
                assert_eq!(tool_use_id, "denied-1");
                assert!(*is_error);
                let payload: Value =
                    serde_json::from_str(content).expect("denial tool result is json");
                assert_eq!(payload["error"]["code"], "tool_denied");
                assert_eq!(payload["tool_name"], "orbit.task.delete");
                assert_eq!(payload["tool_use_id"], "denied-1");

                (
                    vec![ContentBlock::Text {
                        text: "done".to_string(),
                    }],
                    StopReason::EndTurn,
                )
            };

            Ok(TurnResponse {
                content,
                stop_reason,
                usage: TurnUsage::default(),
                raw_request_body: Vec::new(),
                raw_response_body: Vec::new(),
                endpoint: String::new(),
                http_status: 200,
            })
        }
    }

    /// Replies with one fixed tool-use response per turn, then `end_turn`.
    struct ScriptedTransport {
        first_turn: Vec<ContentBlock>,
        first_stop: StopReason,
        calls: Mutex<usize>,
    }

    impl ScriptedTransport {
        fn new(first_turn: Vec<ContentBlock>, first_stop: StopReason) -> Self {
            Self {
                first_turn,
                first_stop,
                calls: Mutex::new(0),
            }
        }

        fn calls(&self) -> usize {
            *self.calls.lock().expect("calls mutex")
        }
    }

    impl LoopTransport for ScriptedTransport {
        fn provider(&self) -> &str {
            "test"
        }

        fn model(&self) -> &str {
            "test-model"
        }

        fn send_turn(&self, _req: &TurnRequest<'_>) -> Result<TurnResponse, TransportError> {
            let mut calls = self.calls.lock().expect("calls mutex");
            let call_index = *calls;
            *calls += 1;
            let (content, stop_reason) = if call_index == 0 {
                (self.first_turn.clone(), self.first_stop)
            } else {
                (
                    vec![ContentBlock::Text {
                        text: "done".to_string(),
                    }],
                    StopReason::EndTurn,
                )
            };
            Ok(TurnResponse {
                content,
                stop_reason,
                usage: TurnUsage::default(),
                raw_request_body: Vec::new(),
                raw_response_body: Vec::new(),
                endpoint: String::new(),
                http_status: 200,
            })
        }
    }

    /// Sleeps on every `orbit.task.show` and records which ids it executed.
    struct SlowOrbitHost {
        delay: Duration,
        executed: Mutex<Vec<String>>,
    }

    impl SlowOrbitHost {
        fn new(delay: Duration) -> Self {
            Self {
                delay,
                executed: Mutex::new(Vec::new()),
            }
        }

        fn executed(&self) -> Vec<String> {
            self.executed.lock().expect("executed mutex").clone()
        }
    }

    impl OrbitToolHost for SlowOrbitHost {
        fn execute(
            &self,
            action: OrbitBuiltinAction,
            input: Value,
            _agent: Option<String>,
            _model: Option<String>,
            _reservation_owner: Option<ReservationOwnerContext>,
        ) -> Result<Value, OrbitError> {
            assert_eq!(action, OrbitBuiltinAction::TaskShow);
            let id = input["id"].as_str().unwrap_or_default().to_string();
            self.executed
                .lock()
                .expect("executed mutex")
                .push(id.clone());
            std::thread::sleep(self.delay);
            Ok(json!({ "id": id }))
        }

        fn task_scope(&self) -> OrbitTaskScope {
            OrbitTaskScope {
                orbit_root: None,
                task_id: None,
                run_id: None,
            }
        }
    }

    fn task_show_call(call_id: &str, task_id: &str) -> ContentBlock {
        ContentBlock::ToolUse {
            id: call_id.to_string(),
            name: "orbit.task.show".to_string(),
            input: json!({ "id": task_id }),
        }
    }

    fn run_with_slow_host(
        session: &mut Session,
        transport: &ScriptedTransport,
        host: Arc<SlowOrbitHost>,
    ) -> Result<LoopOutcome, AgentLoopError> {
        let cfg = AgentLoopConfig::new_for_run("run-test")
            .with_allowlist(vec!["orbit.task.show".to_string()])
            .with_wall_clock_timeout(Duration::from_millis(50))
            .with_max_iterations(3);
        let mut registry = ToolRegistry::new();
        registry.register_builtins();
        let tool_ctx = ToolContext {
            allowed_tools: vec!["orbit.task.show".to_string()],
            orbit_host: Some(host),
            ..Default::default()
        };
        AgentLoop::run(
            session,
            &cfg,
            transport,
            &registry,
            &tool_ctx,
            &NullSink,
            "show the tasks",
        )
    }

    /// Returns `(tool_use_id, is_error, parsed content)` for each block of the
    /// session's final message, which must be a user tool-result turn.
    fn trailing_tool_results(session: &Session) -> Vec<(String, bool, Value)> {
        let last = session.history().last().expect("history is not empty");
        assert_eq!(last.role, MessageRole::User);
        last.content
            .iter()
            .map(|block| match block {
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => (
                    tool_use_id.clone(),
                    *is_error,
                    serde_json::from_str(content).expect("tool result is json"),
                ),
                other => panic!("expected tool_result block, got {other:?}"),
            })
            .collect()
    }

    struct FakeOrbitHost;

    impl OrbitToolHost for FakeOrbitHost {
        fn execute(
            &self,
            action: OrbitBuiltinAction,
            input: Value,
            _agent: Option<String>,
            _model: Option<String>,
            _reservation_owner: Option<ReservationOwnerContext>,
        ) -> Result<Value, OrbitError> {
            assert_eq!(action, OrbitBuiltinAction::TaskShow);
            assert_eq!(input["id"], "T-test");
            Ok(json!({ "id": "T-test" }))
        }

        fn task_scope(&self) -> OrbitTaskScope {
            OrbitTaskScope {
                orbit_root: None,
                task_id: Some("T-test".to_string()),
                run_id: None,
            }
        }
    }

    #[test]
    fn wildcard_allowlist_advertises_and_executes_task_show() {
        let mut session = Session::new("test", "test-model", "", None);
        let cfg = AgentLoopConfig::new_for_run("run-test")
            .with_allowlist(vec!["orbit.task.*".to_string()])
            .with_max_iterations(3);
        let mut registry = ToolRegistry::new();
        registry.register_builtins();
        let tool_ctx = ToolContext {
            allowed_tools: vec!["orbit.task.*".to_string()],
            orbit_host: Some(Arc::new(FakeOrbitHost)),
            ..Default::default()
        };
        let transport = RecordingTransport::default();
        let sink = NullSink;

        let outcome = AgentLoop::run(
            &mut session,
            &cfg,
            &transport,
            &registry,
            &tool_ctx,
            &sink,
            "show the task",
        )
        .expect("wildcard should allow orbit.task.show");

        assert_eq!(outcome.final_message, "done");
        assert!(
            outcome
                .trace
                .iter()
                .all(|iteration| iteration.policy_denials.is_empty())
        );
        assert!(
            transport
                .advertised()
                .first()
                .expect("first request")
                .iter()
                .any(|name| name == "orbit.task.show")
        );
    }

    #[test]
    fn continue_on_denial_returns_structured_tool_result_error() {
        let mut session = Session::new("test", "test-model", "", None);
        let cfg = AgentLoopConfig::new_for_run("run-test")
            .with_advertised_tools(vec!["orbit.task.delete".to_string()])
            .with_on_denial(OnDenial::Continue)
            .with_max_iterations(3);
        let mut registry = ToolRegistry::new();
        registry.register_builtins();
        let tool_ctx = ToolContext::default();
        let transport = DenialContinueTransport::default();
        let sink = NullSink;

        let outcome = AgentLoop::run(
            &mut session,
            &cfg,
            &transport,
            &registry,
            &tool_ctx,
            &sink,
            "try deleting",
        )
        .expect("continue should feed denial back to model");

        assert_eq!(outcome.final_message, "done");
        assert_eq!(outcome.trace.len(), 2);
        assert_eq!(
            outcome.trace[0].policy_denials,
            vec!["orbit.task.delete".to_string()]
        );
    }

    #[test]
    fn expired_budget_stops_later_tool_dispatch() {
        let mut session = Session::new("test", "test-model", "", None);
        let transport = ScriptedTransport::new(
            vec![
                task_show_call("call-1", "T-a"),
                task_show_call("call-2", "T-b"),
            ],
            StopReason::ToolUse,
        );
        let host = Arc::new(SlowOrbitHost::new(Duration::from_millis(150)));

        let result = run_with_slow_host(&mut session, &transport, Arc::clone(&host));

        assert!(
            matches!(result, Err(AgentLoopError::Timeout { .. })),
            "expected Timeout, got {result:?}"
        );
        assert_eq!(host.executed(), vec!["T-a".to_string()]);
        assert_eq!(transport.calls(), 1, "no turn may start after expiry");

        let results = trailing_tool_results(&session);
        assert_eq!(results.len(), 2, "every tool_use keeps a paired result");
        let (id, is_error, payload) = &results[0];
        assert_eq!(id, "call-1");
        assert!(!is_error);
        assert_eq!(payload["id"], "T-a");
        let (id, is_error, payload) = &results[1];
        assert_eq!(id, "call-2");
        assert!(is_error);
        assert_eq!(payload["error"]["code"], "wall_clock_timeout");
    }

    #[test]
    fn slow_tool_in_final_response_returns_timeout() {
        let mut session = Session::new("test", "test-model", "", None);
        let transport = ScriptedTransport::new(
            vec![
                ContentBlock::Text {
                    text: "checking".to_string(),
                },
                task_show_call("call-1", "T-a"),
            ],
            StopReason::EndTurn,
        );
        let host = Arc::new(SlowOrbitHost::new(Duration::from_millis(150)));

        let result = run_with_slow_host(&mut session, &transport, Arc::clone(&host));

        assert!(
            matches!(result, Err(AgentLoopError::Timeout { .. })),
            "expected Timeout, got {result:?}"
        );
        assert_eq!(host.executed(), vec!["T-a".to_string()]);

        let results = trailing_tool_results(&session);
        assert_eq!(results.len(), 1);
        let (id, is_error, payload) = &results[0];
        assert_eq!(id, "call-1");
        assert!(!is_error);
        assert_eq!(payload["id"], "T-a");
    }

    /// Replays scripted responses in order, asserting every request it
    /// receives is a well-formed transcript.
    struct TranscriptTransport {
        responses: Mutex<VecDeque<(Vec<ContentBlock>, StopReason)>>,
        requests: Mutex<Vec<Vec<Message>>>,
    }

    impl TranscriptTransport {
        fn new(responses: Vec<(Vec<ContentBlock>, StopReason)>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<Vec<Message>> {
            self.requests.lock().expect("requests mutex").clone()
        }
    }

    impl LoopTransport for TranscriptTransport {
        fn provider(&self) -> &str {
            "test"
        }

        fn model(&self) -> &str {
            "test-model"
        }

        fn send_turn(&self, req: &TurnRequest<'_>) -> Result<TurnResponse, TransportError> {
            assert_matched_transcript(req.messages);
            self.requests
                .lock()
                .expect("requests mutex")
                .push(req.messages.to_vec());
            let (content, stop_reason) = self
                .responses
                .lock()
                .expect("responses mutex")
                .pop_front()
                .expect("scripted response available");
            Ok(TurnResponse {
                content,
                stop_reason,
                usage: TurnUsage::default(),
                raw_request_body: Vec::new(),
                raw_response_body: Vec::new(),
                endpoint: String::new(),
                http_status: 200,
            })
        }
    }

    /// Roles alternate, and every assistant tool_use is answered by a
    /// tool_result in the next message — what provider encoders require.
    fn assert_matched_transcript(messages: &[Message]) {
        for pair in messages.windows(2) {
            assert_ne!(pair[0].role, pair[1].role, "roles must alternate");
        }
        for (index, message) in messages.iter().enumerate() {
            if message.role != MessageRole::Assistant {
                continue;
            }
            let requested: HashSet<&str> = message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolUse { id, .. } => Some(id.as_str()),
                    _ => None,
                })
                .collect();
            if requested.is_empty() {
                continue;
            }
            let answered: HashSet<&str> = messages
                .get(index + 1)
                .map(|next| {
                    next.content
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::ToolResult { tool_use_id, .. } => {
                                Some(tool_use_id.as_str())
                            }
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            assert_eq!(requested, answered, "unanswered tool_use ids");
        }
    }

    fn tool_use(id: &str, name: &str) -> ContentBlock {
        ContentBlock::ToolUse {
            id: id.to_string(),
            name: name.to_string(),
            input: json!({ "id": "T-test" }),
        }
    }

    fn text(value: &str) -> ContentBlock {
        ContentBlock::Text {
            text: value.to_string(),
        }
    }

    fn tool_results(message: &Message) -> Vec<(String, bool, Value)> {
        message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => Some((
                    tool_use_id.clone(),
                    *is_error,
                    serde_json::from_str(content).expect("tool result is json"),
                )),
                _ => None,
            })
            .collect()
    }

    #[derive(Default)]
    struct CountingOrbitHost {
        executions: AtomicUsize,
    }

    impl OrbitToolHost for CountingOrbitHost {
        fn execute(
            &self,
            action: OrbitBuiltinAction,
            _input: Value,
            _agent: Option<String>,
            _model: Option<String>,
            _reservation_owner: Option<ReservationOwnerContext>,
        ) -> Result<Value, OrbitError> {
            assert_eq!(action, OrbitBuiltinAction::TaskShow);
            self.executions.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "id": "T-test" }))
        }

        fn task_scope(&self) -> OrbitTaskScope {
            OrbitTaskScope {
                orbit_root: None,
                task_id: Some("T-test".to_string()),
                run_id: None,
            }
        }
    }

    struct Harness {
        registry: ToolRegistry,
        host: Arc<CountingOrbitHost>,
        tool_ctx: ToolContext,
    }

    impl Harness {
        fn new() -> Self {
            let mut registry = ToolRegistry::new();
            registry.register_builtins();
            let host = Arc::new(CountingOrbitHost::default());
            let tool_ctx = ToolContext {
                allowed_tools: vec!["orbit.task.show".to_string()],
                orbit_host: Some(host.clone()),
                ..Default::default()
            };
            Self {
                registry,
                host,
                tool_ctx,
            }
        }

        fn executions(&self) -> usize {
            self.host.executions.load(Ordering::SeqCst)
        }

        fn send(
            &self,
            session: &mut Session,
            transport: &TranscriptTransport,
            prompt: &str,
        ) -> Result<LoopOutcome, AgentLoopError> {
            let cfg = AgentLoopConfig::new_for_run("run-test")
                .with_allowlist(vec!["orbit.task.show".to_string()])
                .with_advertised_tools(vec![
                    "orbit.task.show".to_string(),
                    "orbit.task.delete".to_string(),
                ])
                .with_max_iterations(3);
            session.send(
                &cfg,
                transport,
                &self.registry,
                &self.tool_ctx,
                &NullSink,
                prompt,
            )
        }
    }

    #[test]
    fn terminate_on_denial_preserves_executed_results_for_continuation() {
        let harness = Harness::new();
        let mut session = Session::new("test", "test-model", "", None);
        let transport = TranscriptTransport::new(vec![
            (
                vec![
                    tool_use("allowed-1", "orbit.task.show"),
                    tool_use("denied-2", "orbit.task.delete"),
                    tool_use("after-3", "orbit.task.show"),
                ],
                StopReason::ToolUse,
            ),
            (vec![text("resumed")], StopReason::EndTurn),
        ]);

        let err = harness
            .send(&mut session, &transport, "show then delete")
            .expect_err("denial terminates the turn");
        assert!(matches!(
            err,
            AgentLoopError::PolicyDenied { ref tool_name, iteration: 1 }
                if tool_name == "orbit.task.delete"
        ));
        assert_eq!(harness.executions(), 1, "only the allowed call runs");

        let results = tool_results(session.history().last().expect("tool results"));
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].0, "allowed-1");
        assert!(!results[0].1);
        assert_eq!(results[0].2["id"], "T-test");
        assert_eq!(results[1].0, "denied-2");
        assert!(results[1].1);
        assert_eq!(results[1].2["error"]["code"], "tool_denied");
        assert_eq!(results[2].0, "after-3");
        assert!(results[2].1);
        assert_eq!(results[2].2["error"]["code"], "tool_not_executed");

        let outcome = harness
            .send(&mut session, &transport, "carry on")
            .expect("continuation sends a matched transcript");
        assert_eq!(outcome.final_message, "resumed");
        assert_eq!(harness.executions(), 1, "continuation replays no effects");

        let requests = transport.requests();
        assert_eq!(requests.len(), 2);
        let resumed_prompt = requests[1].last().expect("resumed prompt");
        assert_eq!(resumed_prompt.role, MessageRole::User);
        assert!(matches!(
            resumed_prompt.content.last(),
            Some(ContentBlock::Text { text }) if text == "carry on"
        ));
    }

    #[test]
    fn single_denial_terminate_leaves_no_unanswered_tool_use() {
        let harness = Harness::new();
        let mut session = Session::new("test", "test-model", "", None);
        let transport = TranscriptTransport::new(vec![
            (
                vec![tool_use("denied-1", "orbit.task.delete")],
                StopReason::ToolUse,
            ),
            (vec![text("ok")], StopReason::EndTurn),
        ]);

        harness
            .send(&mut session, &transport, "delete it")
            .expect_err("denial terminates the turn");
        assert_eq!(harness.executions(), 0);
        assert_matched_transcript(session.history());

        harness
            .send(&mut session, &transport, "never mind")
            .expect("continuation sends a matched transcript");
        assert_eq!(harness.executions(), 0);
    }

    #[test]
    fn terminal_stop_reason_with_tool_use_records_results() {
        let harness = Harness::new();
        let mut session = Session::new("test", "test-model", "", None);
        let transport = TranscriptTransport::new(vec![
            (
                vec![text("partial"), tool_use("call-1", "orbit.task.show")],
                StopReason::MaxTokens,
            ),
            (vec![text("resumed")], StopReason::EndTurn),
        ]);

        let outcome = harness
            .send(&mut session, &transport, "show it")
            .expect("terminal stop ends the turn");
        assert_eq!(outcome.terminate_reason, TerminateReason::MaxTokens);
        assert_eq!(outcome.final_message, "partial");
        assert_eq!(harness.executions(), 1);
        assert_matched_transcript(session.history());

        harness
            .send(&mut session, &transport, "continue")
            .expect("continuation sends a matched transcript");
        assert_eq!(harness.executions(), 1, "continuation replays no effects");
    }
}

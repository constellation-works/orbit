#![allow(missing_docs)]

mod run {
    #![allow(missing_docs)]

    use super::super::super::agent_loop::*;
    use super::super::super::audit::NullSink;
    use super::super::super::session::Session;
    use super::super::super::{
        ContentBlock, LoopTransport, Message, MessageRole, StopReason, TransportError, TurnRequest,
        TurnResponse, TurnUsage,
    };
    use orbit_common::OrbitError;
    use orbit_tools::{
        OrbitBuiltinAction, OrbitTaskScope, OrbitToolHost, ReservationOwnerContext, ToolContext,
        ToolRegistry,
    };

    use serde_json::{Value, json};
    use std::collections::{HashSet, VecDeque};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

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
}

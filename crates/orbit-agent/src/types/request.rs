#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentOperation {
    Activity { activity_id: String },
    Job { job_id: String, activity_id: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentRequest {
    pub operation: AgentOperation,
    pub envelope_json: Vec<u8>,
    /// When `true`, the agent CLI should produce verbose/debug output on stderr.
    pub verbose: bool,
    /// Optional opaque provider conversation handle to resume when supported.
    pub conversation_ref: Option<String>,
}

impl AgentRequest {
    pub fn activity(activity_id: impl Into<String>, envelope_json: Vec<u8>) -> Self {
        Self {
            operation: AgentOperation::Activity {
                activity_id: activity_id.into(),
            },
            envelope_json,
            verbose: false,
            conversation_ref: None,
        }
    }

    pub fn job(
        job_id: impl Into<String>,
        activity_id: impl Into<String>,
        envelope_json: Vec<u8>,
    ) -> Self {
        Self {
            operation: AgentOperation::Job {
                job_id: job_id.into(),
                activity_id: activity_id.into(),
            },
            envelope_json,
            verbose: false,
            conversation_ref: None,
        }
    }

    pub fn with_conversation_ref(mut self, conversation_ref: Option<String>) -> Self {
        self.conversation_ref = conversation_ref
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        self
    }
}

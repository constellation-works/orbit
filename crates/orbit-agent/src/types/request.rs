#[derive(Debug, Clone, PartialEq)]
pub struct AgentRequest {
    pub envelope_json: Vec<u8>,
    /// Optional opaque provider conversation handle to resume when supported.
    pub conversation_ref: Option<String>,
}

impl AgentRequest {
    pub fn activity(envelope_json: Vec<u8>) -> Self {
        Self {
            envelope_json,
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

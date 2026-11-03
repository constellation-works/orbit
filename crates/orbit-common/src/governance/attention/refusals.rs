/// Refuse widening a workspace-bound attention session with its wire code.
pub fn attention_scope_refusal() -> crate::OrbitError {
    attention_wire_refusal(
        "scope_not_permitted",
        "A bound MCP session may read only its workspace and cannot select all or host",
    )
}

/// Refuse the aggregator-reserved input from an ordinary caller.
pub fn attention_candidate_read_refusal() -> crate::OrbitError {
    attention_wire_refusal(
        "candidate_read_not_permitted",
        "Candidate reads are reserved for the federated aggregator",
    )
}

fn attention_wire_refusal(code: &str, message: &str) -> crate::OrbitError {
    crate::OrbitError::RemoteTool {
        code: crate::RemoteToolCode::Other(code.into()),
        message: message.into(),
        payload: serde_json::json!({"code": code, "message": message}),
    }
}

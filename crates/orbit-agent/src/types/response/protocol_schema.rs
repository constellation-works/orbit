//! Defines the response envelope schema for provider transports that constrain
//! output and the parser that validates it.

use serde_json::{Value, json};

/// The only protocol version Orbit speaks. Shared with [`super::envelope`] so
/// the schema handed to a provider and the parser that validates the reply
/// cannot disagree about what version means.
pub const RESPONSE_ENVELOPE_SCHEMA_VERSION: u32 = 1;

/// The recognized `status` tokens. All three *terminate* the protocol; which
/// of them may checkpoint a step is a control-plane decision owned by the CLI
/// runner ([ORB-10733]), not by this frame.
pub const RESPONSE_ENVELOPE_STATUSES: [&str; 3] = ["success", "failed", "timeout"];

/// Orbit's response envelope as a JSON Schema, for providers that can
/// constrain their own output.
///
/// # Status/error correlation
///
/// The `allOf` → `anyOf` nesting below ties `status` to `error` at the
/// provider boundary: `success` carries no error object (absent or `null`,
/// matching the parser), while `failed` and `timeout` must carry an error
/// object whose `code` and `message` are non-blank. The conditional stays
/// nested rather than at the top level, and uses only the Claude
/// structured-output subset: `anyOf`/`allOf` and `const` are supported, and
/// non-blank is expressed with a `pattern` because that subset rejects
/// string-length constraints (`minLength`/`maxLength`) with a 400 error
/// (see "JSON Schema limitations" in the Claude structured-outputs docs).
/// The `super::envelope` parser still validates the same correlation when
/// parsing provider output, as the second layer for transports without a
/// schema.
///
/// `durationMs` is not declared here and passes through as an unconstrained
/// additional property.
pub fn response_envelope_json_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "schemaVersion": {
                "const": RESPONSE_ENVELOPE_SCHEMA_VERSION,
                "description": "Orbit response protocol version. Always 1.",
            },
            "status": {
                "type": "string",
                "enum": RESPONSE_ENVELOPE_STATUSES,
                "description":
                    "Terminal outcome of the invocation. Use \"failed\" when the work could \
                     not be completed; do not report \"success\" for partial work.",
            },
            "result": {
                "type": "object",
                "description":
                    "Result payload. Always an object, never null — use {} for activities \
                     whose output is purely durable side effects.",
            },
            "error": {
                "description":
                    "Present only when status is \"failed\" or \"timeout\". Object with \
                     non-empty \"code\" and \"message\". Omit this field when status is \
                     \"success\".",
                "properties": {
                    "code": {
                        "type": "string",
                        "pattern": "\\S",
                        "description": "Stable machine-readable error code.",
                    },
                    "message": {
                        "type": "string",
                        "pattern": "\\S",
                        "description": "Human-readable error message.",
                    },
                },
                "required": ["code", "message"],
            },
        },
        "required": ["schemaVersion", "status", "result"],
        // Nested conditional (see the doc comment above): exactly one branch
        // matches the declared `status`, and each branch pins the `error`
        // shape that status allows. `error: null` is accepted on success
        // because the parser treats JSON null as "no error".
        "allOf": [{
            "anyOf": [
                {
                    "properties": {
                        "status": { "const": "success" },
                        "error": { "type": "null" },
                    },
                    "required": ["status"],
                },
                {
                    "properties": {
                        "status": { "const": "failed" },
                        "error": {
                            "type": "object",
                            "properties": {
                                "code": { "type": "string", "pattern": "\\S" },
                                "message": { "type": "string", "pattern": "\\S" },
                            },
                            "required": ["code", "message"],
                        },
                    },
                    "required": ["status", "error"],
                },
                {
                    "properties": {
                        "status": { "const": "timeout" },
                        "error": {
                            "type": "object",
                            "properties": {
                                "code": { "type": "string", "pattern": "\\S" },
                                "message": { "type": "string", "pattern": "\\S" },
                            },
                            "required": ["code", "message"],
                        },
                    },
                    "required": ["status", "error"],
                },
            ],
        }],
    })
}

/// The schema serialized for use as a CLI argument value.
pub fn response_envelope_json_schema_arg() -> String {
    // The schema is a closed literal above, so this cannot fail; serializing a
    // `Value` only errors on non-string map keys or a custom `Serialize` impl.
    response_envelope_json_schema().to_string()
}

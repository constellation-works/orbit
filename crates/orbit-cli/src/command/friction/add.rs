use serde_json::Value;

use super::support::record_payload;
use crate::command::CommandOut;

pub(super) fn render(value: &Value) -> CommandOut {
    record_payload(value)
}

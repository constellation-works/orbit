use orbit_common::governance::friction::FRICTION_LIST_RESPONSE_MODE_WITH_NOTES;
use serde_json::{Map, Value};

use super::support::{value_string, value_string_list};
use crate::command::{CommandOut, Payload};
use crate::output::common::payload::Block;

pub(super) fn prepare(input: &mut Map<String, Value>) -> Result<(), orbit_core::Error> {
    // The table renderer shows the tool's guidance notes.
    input.insert(
        "response_mode".to_string(),
        Value::String(FRICTION_LIST_RESPONSE_MODE_WITH_NOTES.to_string()),
    );
    Ok(())
}

pub(super) fn render(value: &Value) -> CommandOut {
    let Some((records, notes)) = split_list_payload(value) else {
        return Ok(Payload::document(value.clone()).into());
    };

    use crate::output::common::table::{Column, Table};
    // `orbit friction show <id>` prints a record in full.
    let mut table = Table::new(vec![
        Column::new("ID").fixed(),
        Column::new("STATUS").fixed(),
        Column::new("MODEL").fixed(),
        Column::new("TAGS"),
        Column::new("TASK").fixed(),
        Column::new("TITLE"),
    ])
    .empty_message("no friction records matching the given filters");
    for record in &records {
        table.add_row(vec![
            value_string(record, "id"),
            value_string(record, "status"),
            value_string(record, "model"),
            value_string_list(record, "tags"),
            value_string(record, "during_task"),
            value_string(record, "title"),
        ]);
    }
    if notes.is_empty() {
        return Ok(Payload::list(records, table).into());
    }
    let mut blocks: Vec<Block> = notes
        .iter()
        .map(|note| Block::text(format!("note: {note}")))
        .collect();
    blocks.push(Block::table(table));
    Ok(Payload::blocks(Value::Array(records), blocks).into())
}

/// The tool's opt-in notes envelope is projected back to the historical record
/// array for JSON output while retaining notes in the human rendering.
fn split_list_payload(value: &Value) -> Option<(Vec<Value>, Vec<String>)> {
    match value {
        Value::Array(records) => Some((records.clone(), Vec::new())),
        Value::Object(object) => {
            let records = object.get("records").and_then(Value::as_array)?.clone();
            let notes = object
                .get("notes")
                .and_then(Value::as_array)
                .map(|notes| {
                    notes
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            Some((records, notes))
        }
        _ => None,
    }
}

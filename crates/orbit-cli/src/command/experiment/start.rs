use serde_json::Value;

use super::support::experiment_blocks;
use crate::output::common::payload::Block;

pub(super) fn blocks(value: &Value) -> Vec<Block> {
    experiment_blocks(value)
}

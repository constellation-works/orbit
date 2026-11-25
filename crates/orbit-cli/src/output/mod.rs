//! CLI output formatting, color, table, JSON, pipe, and sink rendering; imports `orbit-cmd`, `orbit-common`, and `orbit-core`.

pub mod color;
pub mod json;
pub mod payload;
pub mod pipe;
pub mod render;
pub mod sink;
pub mod table;

#[cfg(test)]
mod tests;

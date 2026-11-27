//! Production output modules at the integration rendering boundary.
#![allow(dead_code, clippy::print_stdout, clippy::print_stderr)]
#[path = "../../src/output/attention.rs"]
pub mod attention;
#[path = "../../src/output/common/color.rs"]
pub mod color;
#[path = "../../src/output/config.rs"]
pub mod config;
#[path = "../../src/output/doctor.rs"]
pub mod doctor;
#[path = "../../src/output/common/json.rs"]
pub mod json;

#[path = "../../src/output/notice.rs"]
pub mod notice;

#[path = "../../src/output/common/payload.rs"]
pub mod payload;
#[path = "../../src/output/common/pipe.rs"]
pub mod pipe;
#[path = "../../src/output/common/render.rs"]
pub mod render;
#[path = "../../src/output/common/sink.rs"]
pub mod sink;
#[path = "../../src/output/common/table.rs"]
pub mod table;

/// Mirrors `crate::output::common` so the production paths resolve here too.
pub mod common {
    pub use super::{color, json, payload, pipe, render, sink, table};
}

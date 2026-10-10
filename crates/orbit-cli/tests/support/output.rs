//! Production output modules at the integration rendering boundary.
#![allow(dead_code, clippy::print_stdout, clippy::print_stderr)]
#[path = "../../src/output/color.rs"]
pub mod color;
#[path = "../../src/output/json.rs"]
pub mod json;
#[path = "../../src/output/payload.rs"]
pub mod payload;
#[path = "../../src/output/pipe.rs"]
pub mod pipe;
#[path = "../../src/output/render.rs"]
pub mod render;
#[path = "../../src/output/sink.rs"]
pub mod sink;
#[path = "../../src/output/table.rs"]
pub mod table;

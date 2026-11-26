//! CLI output formatting, color, table, JSON, pipe, and sink rendering; imports `orbit-cmd`, `orbit-common`, and `orbit-core`.
//!
//! Files directly under this module are named for the top-level commands whose
//! output they hold; code two or more commands share lives under `common/`.
//! This file holds the module's wiring only (STD-02 R48).

pub mod attention;
pub mod common;
pub mod config;
pub mod doctor;

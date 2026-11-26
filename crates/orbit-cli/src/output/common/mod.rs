//! Output code that two or more commands share: the sink, tables, color,
//! payloads, the renderer, JSON payloads, and the closed-stdout guard.

pub mod color;
pub mod json;
pub mod payload;
pub mod pipe;
pub mod render;
pub mod sink;
pub mod table;

#[cfg(test)]
mod tests;

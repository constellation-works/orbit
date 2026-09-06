//! Legacy cursor projections; Store owns their file persistence.
pub use orbit_store::compose::auto_task::{cursor_state_path, load_cursor_state};
pub use orbit_types::workflow::{AutoTaskCursor, AutoTaskCursorState};

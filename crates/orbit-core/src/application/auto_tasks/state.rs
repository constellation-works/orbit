//! Legacy cursor projections; Store owns their file persistence.

pub use orbit_store::compose::auto_task::{cursor_state_path, cursor_store};
pub use orbit_types::workflow::{AutoTaskCursor, AutoTaskCursorState};

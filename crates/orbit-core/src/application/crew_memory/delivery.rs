//! Rendering a memory space for delivery (slice M2): the newest revision of
//! a model's space, plus the global memory when the arm asks for it, as the
//! data block an agent envelope carries. Only the `memory` experiment
//! overlay calls this; nothing else delivers memory to a prompt.

use orbit_common::OrbitError;
use orbit_types::workflow::{
    CREW_MEMORY_MAX_CONTENT_BYTES, CrewMemoryBlock, CrewMemoryDelivery, CrewMemoryNote,
    TagInstructionRole,
};

use super::MemoryKey;
use super::operations::parse_revision_files;
use crate::OrbitRuntime;

impl OrbitRuntime {
    /// Render `key`'s newest revision, and the global memory's when
    /// `global`, for a dispatch in `role`. A space with no revision renders
    /// empty; that is still a delivery and is recorded.
    pub(crate) fn render_crew_memory_delivery(
        &self,
        role: TagInstructionRole,
        key: &MemoryKey,
        global: bool,
    ) -> Result<CrewMemoryDelivery, OrbitError> {
        let store = self.crew_memory_store()?;
        let workspace_id = self.workspace_id()?;
        let mut block = CrewMemoryBlock::default();
        let mut revised_at = None;
        if let Some(row) = store.crew_memory_revision(&workspace_id, key.as_str(), None)? {
            block.model = parse_revision_files(&row)?
                .into_iter()
                .map(|file| CrewMemoryNote {
                    path: file.path,
                    text: file.text,
                })
                .collect();
            block
                .model
                .sort_by(|left, right| left.path.cmp(&right.path));
            block.revision.model = Some(row.revision);
            revised_at = Some(row.created_at);
        }
        if global
            && let Some(row) =
                store.crew_memory_revision(&workspace_id, MemoryKey::global().as_str(), None)?
        {
            block.global = parse_revision_files(&row)?
                .into_iter()
                .map(|file| file.text)
                .collect::<Vec<_>>()
                .join("\n\n");
            block.revision.global = Some(row.revision);
            revised_at = revised_at.max(Some(row.created_at));
        }
        if block.content_bytes() > CREW_MEMORY_MAX_CONTENT_BYTES {
            return Err(OrbitError::PolicyDenied(format!(
                "crew memory `{key}` renders {} content bytes, over the {CREW_MEMORY_MAX_CONTENT_BYTES}-byte delivery limit",
                block.content_bytes()
            )));
        }
        Ok(CrewMemoryDelivery::render(
            role,
            key.as_str(),
            global,
            revised_at.map(|at| at.to_rfc3339()),
            block,
        ))
    }
}

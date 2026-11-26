use orbit_types::task::Task;

/// How one task id resolved against this machine's task ownership registry.
///
/// Every variant fails closed: only [`Self::Resolved`] carries a body, and
/// neither of the other two may be read as a satisfied prerequisite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisteredTaskResolution {
    /// A workspace registered on this machine owns the id, and its bundle was
    /// read from the path the registry binds it to.
    Resolved(Box<Task>),
    /// The id is well formed and belongs to a task-id prefix this registry has
    /// never issued or registered, so its authority is another host's. This
    /// machine cannot read the body and must not invent one.
    ForeignAuthority,
    /// The id belongs to a prefix this registry knows, but no readable task is
    /// bound to it here — deleted, never published, or not yet imported.
    Missing,
}

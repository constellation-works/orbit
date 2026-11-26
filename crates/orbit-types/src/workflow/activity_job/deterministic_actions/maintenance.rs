#![expect(
    clippy::arbitrary_source_item_ordering,
    reason = "macro_rules! maintenance_actions must precede its pub(super) use re-export in lexical macro scope"
)]

macro_rules! maintenance_actions {
    ($collect:ident, $state:tt) => {
        $collect! {
            $state;
            core {
                CrewMemoryDream = 8 => "crew_memory_dream",
                StoreGc = 42 => "store_gc",
                TaskPublication = 43 => "task_publication",
            }
            engine {
                WorktreeGc = 21 => "worktree_gc",
            }
        }
    };
}

pub(super) use maintenance_actions;

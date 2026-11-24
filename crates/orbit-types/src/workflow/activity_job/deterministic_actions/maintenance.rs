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

//! Background worker that drains an mpsc channel of `EmbedJob`s and feeds
//! them into a long-lived embedder. Long-lived hosts (MCP, dashboard) start
//! this worker so task mutations enqueue best-effort indexing without
//! blocking the caller. Short-lived CLI runtimes use [`EmbedWorker::disabled`]
//! instead: process exit can interrupt a detached companion, so CLI refresh
//! is `orbit semantic index`. Failures log at debug and never propagate.
//!
//! The companion itself belongs to the host's [`EmbedderPool`], which the
//! query path draws from too — indexing a mutation and answering a query use
//! the same warm child rather than one companion each.

use std::sync::Arc;
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread;

use orbit_types::task::Task;

use crate::EmbedderPool;

use super::store::VectorStore;

const EMBED_BATCH_SIZE: usize = 16;

#[derive(Debug, Clone)]
pub struct EmbedJob {
    pub task: Task,
    pub force: bool,
}

#[derive(Clone)]
pub struct EmbedWorker {
    sender: Option<SyncSender<EmbedJob>>,
}

impl EmbedWorker {
    /// Incremental indexer for a process that retains this runtime.
    ///
    /// The companion is created lazily on the first job — by `embedders`, so a
    /// missing install fails before spawn retry and a host that already warmed
    /// a companion for a query reuses it here.
    pub fn start(store: VectorStore, embedders: Arc<EmbedderPool>) -> Self {
        Self::spawn(store, embedders)
    }

    /// No background thread and no companion. `enqueue` is a silent no-op.
    pub fn disabled() -> Self {
        Self { sender: None }
    }

    fn spawn(store: VectorStore, embedders: Arc<EmbedderPool>) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<EmbedJob>(128);
        thread::spawn(move || {
            while let Ok(first) = receiver.recv() {
                let mut batch = vec![first];
                while batch.len() < EMBED_BATCH_SIZE {
                    match receiver.try_recv() {
                        Ok(job) => batch.push(job),
                        Err(_) => break,
                    }
                }
                let embedder = match embedders.embedder(crate::DEFAULT_MODEL) {
                    Ok(embedder) => embedder,
                    Err(error) => {
                        orbit_common::tracing::debug!(
                            target: "orbit.search.indexer",
                            error = %error,
                            "semantic indexing skipped because embedder initialization failed",
                        );
                        continue;
                    }
                };
                for job in &batch {
                    if let Err(error) = store.index_task(&job.task, embedder.as_ref(), job.force) {
                        orbit_common::tracing::debug!(
                            target: "orbit.search.indexer",
                            task_id = job.task.id.as_str(),
                            error = %error,
                            "semantic indexing failed after task mutation",
                        );
                    }
                }
            }
        });
        Self {
            sender: Some(sender),
        }
    }

    pub fn enqueue(&self, task: Task) {
        let Some(sender) = self.sender.as_ref() else {
            return;
        };
        match sender.try_send(EmbedJob { task, force: false }) {
            Ok(()) => {}
            Err(TrySendError::Full(job)) => {
                orbit_common::tracing::debug!(
                    target: "orbit.search.indexer",
                    task_id = job.task.id.as_str(),
                    "semantic indexing queue is full; dropping task update",
                );
            }
            Err(TrySendError::Disconnected(_)) => {
                orbit_common::tracing::debug!(
                    target: "orbit.search.indexer",
                    "semantic indexing queue is disconnected; dropping task update",
                );
            }
        }
    }
}

//! Background worker that drains an mpsc channel of `EmbedJob`s and feeds
//! them into a long-lived embedder. Long-lived hosts (MCP, dashboard) start
//! this worker so task mutations enqueue best-effort indexing without
//! blocking the caller. Short-lived CLI runtimes use [`EmbedWorker::disabled`]
//! instead: process exit can interrupt a detached companion, so CLI refresh
//! is `orbit semantic index`. Failures log at debug and never propagate.

use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread;

use orbit_types::task::Task;

use crate::SubprocessEmbedder;
use crate::embedder::Embedder;

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
    /// The companion is created lazily on the first job so a missing install
    /// fails before spawn retry, matching [`SubprocessEmbedder::quiet_with_model`].
    pub fn start(store: VectorStore) -> Self {
        Self::spawn(store, None)
    }

    /// No background thread and no companion. `enqueue` is a silent no-op.
    pub fn disabled() -> Self {
        Self { sender: None }
    }

    /// Incremental indexer with a caller-supplied embedder. Used by tests so
    /// indexing is observed through the vector store without a real companion.
    #[cfg(test)]
    pub(crate) fn start_with_embedder(store: VectorStore, embedder: Box<dyn Embedder>) -> Self {
        Self::spawn(store, Some(embedder))
    }

    fn spawn(store: VectorStore, seeded_embedder: Option<Box<dyn Embedder>>) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<EmbedJob>(128);
        thread::spawn(move || {
            let mut embedder = seeded_embedder;
            while let Ok(first) = receiver.recv() {
                let mut batch = vec![first];
                while batch.len() < EMBED_BATCH_SIZE {
                    match receiver.try_recv() {
                        Ok(job) => batch.push(job),
                        Err(_) => break,
                    }
                }
                if embedder.is_none() {
                    match SubprocessEmbedder::quiet_with_model(crate::DEFAULT_MODEL) {
                        Ok(value) => embedder = Some(Box::new(value)),
                        Err(error) => {
                            orbit_common::tracing::debug!(
                                target: "orbit.search.indexer",
                                error = %error,
                                "semantic indexing skipped because embedder initialization failed",
                            );
                            continue;
                        }
                    }
                }
                let Some(active_embedder) = embedder.as_ref() else {
                    continue;
                };
                for job in &batch {
                    if let Err(error) =
                        store.index_task(&job.task, active_embedder.as_ref(), job.force)
                    {
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

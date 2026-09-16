//! One companion per model, for the life of the host process.
//!
//! Every embed request pays for a companion that has already loaded its ONNX
//! graph and tokenizer; a freshly spawned one pays that load again (hundreds
//! of milliseconds to seconds) before answering, then pays a bounded
//! cooperative shutdown on drop. Spawning per query made that startup cost the
//! dominant term of semantic latency — twice over for a hybrid query, which
//! embeds a task branch and a doc branch.
//!
//! The pool is the one owner of live companions: it hands out an
//! `Arc<dyn Embedder>` per model alias, spawning on first use and reusing it
//! afterwards. Its query path and its background [`EmbedWorker`] share the
//! same child.
//!
//! A long-lived host holds that pool for the whole process
//! ([`EmbedderPool::process_shared`]) rather than per runtime, because such a
//! host opens a runtime per call — the MCP server resolves the addressed
//! workspace on every tool call — and a per-runtime pool would load the model
//! again for each one. A short-lived command process takes a private pool
//! ([`EmbedderPool::command_process`]) and drops it, with its companions, at
//! exit.
//!
//! [`EmbedWorker`]: crate::EmbedWorker
//!
//! ## Companion stderr
//!
//! Stderr is a spawn-time choice, so it belongs to the pool rather than to a
//! call site: a shared child cannot be loud for one caller and quiet for
//! another. A command process inherits it, because someone invoked the
//! semantic subsystem directly and needs the failure detail. A long-lived host
//! suppresses it, because the same companions serve best-effort background
//! indexing whose noise must not surface as host output.
//!
//! ## Serialization
//!
//! [`SubprocessEmbedder`] already serializes RPCs on its own child, so
//! concurrent queries against one model queue behind each other instead of
//! fanning out across companions. That is the intended trade: a queued RPC
//! against a warm companion costs far less than a parallel model load.
//! `embedder` holds the pool lock across a first spawn for the same reason —
//! two callers racing for the same model must not start two companions.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use orbit_common::OrbitError;

use crate::embedder::Embedder;
use crate::subprocess::{CompanionStderr, SubprocessEmbedder};

type SpawnFn = Box<dyn Fn(&str) -> Result<Arc<dyn Embedder>, OrbitError> + Send + Sync>;

/// Per-model cache of live embedders, keyed by model alias.
pub struct EmbedderPool {
    spawn: SpawnFn,
    cached: Mutex<BTreeMap<String, Arc<dyn Embedder>>>,
}

/// The long-lived host's pool. Process-wide, because such a host builds more
/// than one runtime over its life and all of them embed with the same models.
static PROCESS_SHARED: OnceLock<Arc<EmbedderPool>> = OnceLock::new();

impl EmbedderPool {
    /// Pool for a process that runs one command and exits. Companion stderr is
    /// inherited so a direct `orbit search` / `orbit semantic index` shows
    /// actionable companion failures.
    pub fn command_process() -> Self {
        Self::with_stderr(CompanionStderr::Inherit)
    }

    /// The process-wide pool for a host that outlives a single command (MCP
    /// serve, dashboard). Every runtime such a host opens shares it, so the
    /// model is loaded once for the life of the process rather than once per
    /// call-scoped runtime.
    ///
    /// Companion stderr is suppressed here because these companions also serve
    /// best-effort background indexing, whose noise must not read as host
    /// output.
    pub fn process_shared() -> Arc<Self> {
        Arc::clone(
            PROCESS_SHARED.get_or_init(|| Arc::new(Self::with_stderr(CompanionStderr::Suppress))),
        )
    }

    fn with_stderr(stderr: CompanionStderr) -> Self {
        Self::with_spawner(move |model| {
            SubprocessEmbedder::with_model_and_stderr(model, stderr)
                .map(|embedder| Arc::new(embedder) as Arc<dyn Embedder>)
        })
    }

    fn with_spawner(
        spawn: impl Fn(&str) -> Result<Arc<dyn Embedder>, OrbitError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            spawn: Box::new(spawn),
            cached: Mutex::new(BTreeMap::new()),
        }
    }

    /// Pool that hands out caller-supplied embedders. Tests observe reuse
    /// through the spawner without an installed companion.
    #[cfg(test)]
    pub(crate) fn for_test(
        spawn: impl Fn(&str) -> Result<Arc<dyn Embedder>, OrbitError> + Send + Sync + 'static,
    ) -> Self {
        Self::with_spawner(spawn)
    }

    /// The live embedder for `model`, spawning one only if this pool has none.
    ///
    /// `model` must be a canonical alias from the model catalog — it is the
    /// cache key, and two spellings of one model would otherwise each hold a
    /// companion.
    pub fn embedder(&self, model: &str) -> Result<Arc<dyn Embedder>, OrbitError> {
        let mut cached = self.cached.lock().map_err(|error| {
            OrbitError::Execution(format!("embedder pool mutex poisoned: {error}"))
        })?;
        if let Some(existing) = cached.get(model) {
            return Ok(Arc::clone(existing));
        }
        let embedder = (self.spawn)(model)?;
        cached.insert(model.to_string(), Arc::clone(&embedder));
        Ok(embedder)
    }

    /// Drop every cached companion; the next request spawns a fresh one.
    ///
    /// Installing or uninstalling replaces the binary a cached companion was
    /// started from, so the pool must forget it rather than keep answering
    /// from the process the user just replaced. Poisoning is not a reason to
    /// keep stale children, so a poisoned lock is drained too.
    pub fn clear(&self) {
        let drained = match self.cached.lock() {
            Ok(mut cached) => std::mem::take(&mut *cached),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        };
        // Shutdown waits run here, outside the lock, so a concurrent query is
        // not blocked behind another caller's teardown.
        drop(drained);
    }
}

impl std::fmt::Debug for EmbedderPool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let models = match self.cached.lock() {
            Ok(cached) => cached.keys().cloned().collect::<Vec<_>>(),
            Err(_) => vec!["<poisoned>".to_string()],
        };
        formatter
            .debug_struct("EmbedderPool")
            .field("cached_models", &models)
            .finish()
    }
}

//! Plugin pages served by this dashboard (plugin-pages design
//! `docs/design/plugin-pages/2_design.md` §2–§3): the one document a page's
//! frame loads, its frame policy, and the bounds on the tool calls it makes.
//!
//! The page is untrusted code. Its document is written from the bytes the
//! plugin load hashed, with nothing in a page file parsed, escaped or
//! rewritten: the load already refused a file that would end its element
//! early or markup that holds its own `script` or `link` element. The policy
//! admits exactly the scripts written here, each by the SHA-256 of its UTF-8
//! text after HTML input preprocessing, so nothing else the page smuggles in
//! can run.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use axum::body::Bytes;
use base64::Engine as _;
use orbit_common::OrbitError;
use orbit_core::app::plugin::{PageCall, PageSource};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Orbit's bridge script, version 1: compiled into the binary and written
/// into a page that names `kit: 1`; never served as a file.
pub(crate) const KIT_V1_SCRIPT: &str = include_str!("../assets/plugin-kit/v1/kit.js");

/// Orbit's page stylesheet, version 1, written beside [`KIT_V1_SCRIPT`].
pub(crate) const KIT_V1_STYLE: &str = include_str!("../assets/plugin-kit/v1/kit.css");

/// Executions one page may have running at once, across identical-call
/// sharing; a call over it is refused with `busy`.
pub(crate) const PAGE_CALL_CONCURRENCY: usize = 4;

/// The frame policy of design §2 before its `script-src` hashes.
const POLICY_HEAD: &str = "sandbox allow-scripts; default-src 'none'; script-src";

/// The frame policy of design §2 after its `script-src` hashes.
const POLICY_TAIL: &str = "; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; \
     connect-src 'none'; form-action 'none'; base-uri 'none'; frame-ancestors 'self'";

/// One page as its frame receives it.
pub(crate) struct PageDocument {
    /// The HTML document.
    pub(crate) body: Bytes,
    /// The `Content-Security-Policy` value, with one hash per script written.
    pub(crate) policy: String,
}

/// Identity of a call for in-flight sharing: the workspace runtime, the page
/// and the canonical tool name and input it runs.
#[derive(Clone, PartialEq, Eq, Hash)]
struct CallKey {
    runtime: usize,
    namespace: String,
    page: String,
    tool: String,
    input: String,
}

/// The shared outcome of one execution; every caller sharing it reads it.
type CallResult = Result<Arc<Value>, Arc<OrbitError>>;

/// The last document written for each `(namespace, page)`, with the bundle
/// digest it was written from.
type Documents = HashMap<(String, String), (String, Arc<PageDocument>)>;

/// Why a page call did not get an execution.
pub(crate) enum CallRefused {
    /// The page already has [`PAGE_CALL_CONCURRENCY`] executions running.
    Busy,
}

#[derive(Default)]
struct Calls {
    /// Executions in flight, each with the receiver its sharers wait on.
    flights: HashMap<CallKey, tokio::sync::watch::Receiver<Option<CallResult>>>,
    /// Executions in flight per `(runtime, namespace, page)`.
    running: HashMap<(usize, String, String), usize>,
}

/// Per-server page documents and page-call executions.
#[derive(Default)]
pub(crate) struct PageServing {
    /// The last document written for each `(namespace, page)`, with the
    /// bundle digest it was written from. A new digest replaces it, so this
    /// holds at most one document per declared page.
    documents: Mutex<Documents>,
    calls: Mutex<Calls>,
}

impl PageServing {
    /// The document for `source`, written once per bundle digest and served
    /// from memory after that.
    pub(crate) fn document(&self, source: &PageSource) -> Arc<PageDocument> {
        let key = (source.namespace().to_string(), source.id().to_string());
        let mut documents = self
            .documents
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some((digest, document)) = documents.get(&key)
            && digest == source.digest()
        {
            return Arc::clone(document);
        }
        let document = Arc::new(write_document(source));
        documents.insert(key, (source.digest().to_string(), Arc::clone(&document)));
        document
    }

    /// Run a checked call, sharing an identical call already in flight for
    /// the same runtime and page. A new execution over the page's bound is
    /// refused. The execution runs on its own task, so a caller that goes
    /// away does not strand the callers sharing it.
    pub(crate) async fn call(
        self: &Arc<Self>,
        runtime: usize,
        namespace: &str,
        page: &str,
        call: PageCall,
    ) -> Result<CallResult, CallRefused> {
        let mut canonical_input = call.input().clone();
        canonical_input.sort_all_objects();
        let key = CallKey {
            runtime,
            namespace: namespace.to_string(),
            page: page.to_string(),
            tool: call.tool().to_string(),
            input: serde_json::to_string(&canonical_input).unwrap_or_default(),
        };
        let bound = (runtime, key.namespace.clone(), key.page.clone());
        let mut receiver = {
            let mut calls = self.calls.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(receiver) = calls.flights.get(&key) {
                // A span per decision, so a trace shows which calls shared.
                drop(tracing::debug_span!(target: "orbit.web.plugin_page", "page_call_shared"));
                receiver.clone()
            } else {
                let running = calls.running.entry(bound.clone()).or_insert(0);
                if *running >= PAGE_CALL_CONCURRENCY {
                    return Err(CallRefused::Busy);
                }
                *running += 1;
                drop(tracing::debug_span!(target: "orbit.web.plugin_page", "page_call_started"));
                let (sender, receiver) = tokio::sync::watch::channel(None);
                calls.flights.insert(key.clone(), receiver.clone());
                let serving = Arc::clone(self);
                tokio::spawn(async move {
                    let result = match tokio::task::spawn_blocking(move || call.run()).await {
                        Ok(Ok(value)) => Ok(Arc::new(value)),
                        Ok(Err(error)) => Err(Arc::new(error)),
                        Err(join) => Err(Arc::new(OrbitError::Execution(format!(
                            "plugin page call panicked: {join}"
                        )))),
                    };
                    serving.finish(&key, &bound);
                    // Every sharer holds a receiver; none left is not an error.
                    let _ = sender.send(Some(result));
                });
                receiver
            }
        };
        let shared = match receiver.wait_for(Option::is_some).await {
            Ok(result) => result.clone(),
            Err(_) => None,
        };
        Ok(shared.unwrap_or_else(|| {
            Err(Arc::new(OrbitError::Execution(
                "plugin page call ended without a result".to_string(),
            )))
        }))
    }

    fn finish(&self, key: &CallKey, bound: &(usize, String, String)) {
        let mut calls = self.calls.lock().unwrap_or_else(PoisonError::into_inner);
        calls.flights.remove(key);
        if let Some(running) = calls.running.get_mut(bound) {
            *running = running.saturating_sub(1);
            if *running == 0 {
                calls.running.remove(bound);
            }
        }
    }
}

/// The kit a page's manifest names, as `(stylesheet, script)`. Validation
/// refuses a version this Orbit does not know, so an unknown one here writes
/// no kit rather than guessing.
fn kit(version: Option<u32>) -> Option<(&'static str, &'static str)> {
    match version {
        Some(1) => Some((KIT_V1_STYLE, KIT_V1_SCRIPT)),
        _ => None,
    }
}

/// Write one page as a document: the kit's stylesheet and each listed style
/// in `style` elements, the bytes of `index.html`, then the kit's script and
/// each listed script in `script` elements. A script named `*.mjs` is a
/// module script.
pub(crate) fn write_document(source: &PageSource) -> PageDocument {
    let kit = kit(source.kit());
    let mut body: Vec<u8> = Vec::with_capacity(
        source.markup().len()
            + source.styles().map(|(_, bytes)| bytes.len()).sum::<usize>()
            + source
                .scripts()
                .map(|(_, bytes)| bytes.len())
                .sum::<usize>()
            + kit.map_or(0, |(style, script)| style.len() + script.len())
            + 512,
    );
    body.extend_from_slice(b"<!doctype html>\n<meta charset=\"utf-8\">\n");
    let styles = kit
        .map(|(style, _)| style.as_bytes())
        .into_iter()
        .chain(source.styles().map(|(_, bytes)| bytes));
    for style in styles {
        body.extend_from_slice(b"<style>");
        body.extend_from_slice(style);
        body.extend_from_slice(b"</style>\n");
    }
    body.extend_from_slice(source.markup());
    body.push(b'\n');

    let mut hashes = Vec::new();
    let scripts = kit
        .map(|(_, script)| ("kit.js", script.as_bytes()))
        .into_iter()
        .chain(source.scripts());
    for (name, script) in scripts {
        body.extend_from_slice(if name.ends_with(".mjs") {
            b"<script type=\"module\">".as_slice()
        } else {
            b"<script>".as_slice()
        });
        body.extend_from_slice(script);
        body.extend_from_slice(b"</script>\n");
        hashes.push(script_hash(script));
    }
    PageDocument {
        body: Bytes::from(body),
        policy: frame_policy(&hashes),
    }
}

/// `'sha256-<base64>'` of one script element's HTML-preprocessed UTF-8 text.
pub(crate) fn script_hash(content: &[u8]) -> String {
    let text = String::from_utf8_lossy(content);
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let digest = Sha256::digest(normalized.as_bytes());
    format!(
        "'sha256-{}'",
        base64::engine::general_purpose::STANDARD.encode(digest)
    )
}

/// The frame policy with `hashes` as the only script sources. A page with no
/// script admits none.
fn frame_policy(hashes: &[String]) -> String {
    let sources = if hashes.is_empty() {
        " 'none'".to_string()
    } else {
        hashes.iter().fold(String::new(), |mut sources, hash| {
            sources.push(' ');
            sources.push_str(hash);
            sources
        })
    };
    format!("{POLICY_HEAD}{sources}{POLICY_TAIL}")
}

//! Plugin pages served by this dashboard (plugin-pages design
//! `docs/design/plugin-pages/2_design.md` §2–§3): the one document a page's
//! frame loads, its frame policy, and the bounds on the tool calls it makes.
//! Another host's page is written here too, from the parts its host's
//! parts route returns, once they pass the same refusals a local page's
//! files get (§5).
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
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::http::HeaderMap;
use base64::Engine as _;
use orbit_common::OrbitError;
use orbit_core::app::plugin::{PageCall, PageSource};
use orbit_types::plugin::{
    KNOWN_PAGE_KITS, MAX_PAGE_BYTES, PAGE_MARKUP_REFUSED, PAGE_SCRIPT_EXTENSIONS,
    PAGE_SCRIPT_REFUSED, PAGE_STYLE_EXTENSION, PAGE_STYLE_REFUSED, find_page_refused,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Orbit's bridge script, version 1: compiled into the binary and written
/// into a page that names `kit: 1`; never served as a file.
pub(crate) const KIT_V1_SCRIPT: &str = include_str!("../assets/plugin-kit/v1/kit.js");

/// Orbit's page stylesheet, version 1, written beside [`KIT_V1_SCRIPT`].
pub(crate) const KIT_V1_STYLE: &str = include_str!("../assets/plugin-kit/v1/kit.css");

/// Executions one page may have running at once, across identical-call
/// sharing; a call over it is refused with `busy`.
pub(crate) const PAGE_CALL_CONCURRENCY: usize = 4;

/// How long a confirmation the dashboard obtained for an action stays
/// usable: long enough for the operator to read the dialog.
pub(crate) const ACTION_CONFIRMATION_TTL: Duration = Duration::from_secs(300);

/// Unused confirmations this server holds at once; a request over it is
/// refused with `busy` until one is used or expires.
pub(crate) const ACTION_CONFIRMATION_LIMIT: usize = 32;

/// The most bytes of a remote parts response this dashboard reads: the page
/// bound in base64, plus room for names and the JSON around them.
pub(crate) const REMOTE_PARTS_LIMIT_BYTES: usize =
    (MAX_PAGE_BYTES as usize).div_ceil(3) * 4 + 256 * 1024;

/// The frame policy of design §2 before its `script-src` hashes.
const POLICY_HEAD: &str = "sandbox allow-scripts; default-src 'none'; script-src";

/// The frame policy of design §2 after its `script-src` hashes.
const POLICY_TAIL: &str = "; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; \
     connect-src 'none'; frame-src 'none'; object-src 'none'; form-action 'none'; \
     base-uri 'none'; frame-ancestors 'self'";

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

/// Bundle digest and kit version a cached document was written from.
/// Either one changing rebuilds it: the digest covers the page files, and
/// the kit version selects the stylesheet, script and policy hashes Orbit
/// writes beside them.
#[derive(PartialEq, Eq)]
struct DocumentStamp {
    digest: String,
    kit: Option<u32>,
}

/// The last document written for each `(namespace, page)`, with the bundle
/// digest and kit version it was written from.
type Documents = HashMap<(String, String), (DocumentStamp, Arc<PageDocument>)>;

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

/// What one action confirmation was issued for. It is good for one request
/// naming exactly this runtime, plugin, page, tool and input.
#[derive(PartialEq)]
pub(crate) struct ActionBinding {
    pub(crate) runtime: usize,
    pub(crate) namespace: String,
    pub(crate) page: String,
    pub(crate) tool: String,
    pub(crate) input: Value,
}

struct Confirmation {
    binding: ActionBinding,
    issued: Instant,
}

/// Why an action confirmation was not issued.
pub(crate) enum ConfirmationRefused {
    /// [`ACTION_CONFIRMATION_LIMIT`] confirmations are outstanding.
    Busy,
    /// The system random source failed.
    Unavailable(String),
}

/// Per-server page documents and page-call executions.
#[derive(Default)]
pub(crate) struct PageServing {
    /// The last document written for each `(namespace, page)`, with the
    /// bundle digest and kit version it was written from. Either one
    /// changing replaces it, so this holds at most one document per
    /// declared page.
    documents: Mutex<Documents>,
    calls: Mutex<Calls>,
    /// Unused one-time action confirmations, by their random token.
    confirmations: Mutex<HashMap<String, Confirmation>>,
}

impl PageServing {
    /// The document for `source`, written once per bundle digest and kit
    /// version and served from memory after that. A change to either input
    /// replaces the cached document.
    pub(crate) fn document(&self, source: &PageSource) -> Arc<PageDocument> {
        let key = (source.namespace().to_string(), source.id().to_string());
        let stamp = DocumentStamp {
            digest: source.digest().to_string(),
            kit: source.kit(),
        };
        let mut documents = self
            .documents
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some((cached, document)) = documents.get(&key)
            && cached == &stamp
        {
            return Arc::clone(document);
        }
        let document = Arc::new(write_document(&PageParts::of(source)));
        documents.insert(key, (stamp, Arc::clone(&document)));
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
        let key = CallKey {
            runtime,
            namespace: namespace.to_string(),
            page: page.to_string(),
            tool: call.tool().to_string(),
            input: serde_json::to_string(call.input()).unwrap_or_default(),
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

    /// Issue a one-time confirmation for `binding`: a random token the
    /// dashboard sends back with the action once the operator confirmed it.
    pub(crate) fn issue_confirmation(
        &self,
        binding: ActionBinding,
    ) -> Result<String, ConfirmationRefused> {
        let token = orbit_common::security::token::random_hex(32)
            .map_err(|error| ConfirmationRefused::Unavailable(error.to_string()))?;
        let mut confirmations = self
            .confirmations
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        confirmations.retain(|_, held| held.issued.elapsed() < ACTION_CONFIRMATION_TTL);
        if confirmations.len() >= ACTION_CONFIRMATION_LIMIT {
            return Err(ConfirmationRefused::Busy);
        }
        confirmations.insert(
            token.clone(),
            Confirmation {
                binding,
                issued: Instant::now(),
            },
        );
        Ok(token)
    }

    /// Spend `token` for `binding`. The token is gone after this call
    /// whatever it answers, so it is never good twice. `Err` names why it
    /// does not confirm this request.
    pub(crate) fn redeem_confirmation(
        &self,
        token: Option<&str>,
        binding: &ActionBinding,
    ) -> Result<(), String> {
        let Some(token) = token.filter(|token| !token.is_empty()) else {
            return Err(
                "an action runs only with the confirmation the dashboard obtains when \
                        the operator confirms it"
                    .to_string(),
            );
        };
        let held = self
            .confirmations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(token);
        match held {
            None => Err("the action's confirmation is unknown or already used".to_string()),
            Some(held) if held.issued.elapsed() >= ACTION_CONFIRMATION_TTL => {
                Err("the action's confirmation expired; confirm the action again".to_string())
            }
            Some(held) if held.binding != *binding => Err(
                "the action's confirmation was issued for another plugin, page, tool or input"
                    .to_string(),
            ),
            Some(_) => Ok(()),
        }
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

/// A page's parts as the document writer reads them: from a local page's
/// load, or from another host's parts route after [`check_remote_parts`].
pub(crate) struct PageParts<'a> {
    kit: Option<u32>,
    markup: &'a [u8],
    scripts: Vec<(&'a str, &'a [u8])>,
    styles: Vec<(&'a str, &'a [u8])>,
}

impl<'a> PageParts<'a> {
    /// The parts of an active local page, as its plugin load hashed them.
    pub(crate) fn of(source: &'a PageSource) -> Self {
        Self {
            kit: source.kit(),
            markup: source.markup(),
            scripts: source.scripts().collect(),
            styles: source.styles().collect(),
        }
    }
}

#[derive(Deserialize)]
struct WireParts {
    encoding: String,
    kit: Option<u32>,
    markup: String,
    #[serde(default)]
    scripts: Vec<WireFile>,
    #[serde(default)]
    styles: Vec<WireFile>,
}

#[derive(Deserialize)]
struct WireFile {
    name: String,
    content: String,
}

/// Another host's page parts, decoded and checked; nothing here is trusted
/// beyond what [`check_remote_parts`] verified.
pub(crate) struct RemoteParts {
    kit: Option<u32>,
    markup: Vec<u8>,
    scripts: Vec<(String, Vec<u8>)>,
    styles: Vec<(String, Vec<u8>)>,
}

impl RemoteParts {
    pub(crate) fn parts(&self) -> PageParts<'_> {
        PageParts {
            kit: self.kit,
            markup: &self.markup,
            scripts: borrow_files(&self.scripts),
            styles: borrow_files(&self.styles),
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

/// `GET /api/plugins/<ns>/pages/<page>/parts`: an active page's parts as
/// JSON, each file's bytes base64-encoded, scripts and styles in manifest
/// order (plugin-pages design §2, §5).
pub(crate) fn parts_json(source: &PageSource) -> Value {
    let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    let files = |files: &mut dyn Iterator<Item = (&str, &[u8])>| {
        files
            .map(|(name, bytes)| json!({"name": name, "content": encode(bytes)}))
            .collect::<Vec<_>>()
    };
    json!({
        "encoding": "base64",
        "kit": source.kit(),
        "markup": encode(source.markup()),
        "scripts": files(&mut source.scripts()),
        "styles": files(&mut source.styles()),
    })
}

fn borrow_files(files: &[(String, Vec<u8>)]) -> Vec<(&str, &[u8])> {
    files
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect()
}

/// Decode another host's parts response and apply the refusals a local
/// page's files get at load (plugin-pages design §1): a kit version this
/// dashboard does not know, `<script` or `<link` in the markup, `</script`
/// or `<!--` in a script, `</style` in a style, a script name not ending in
/// `.js` or `.mjs`, a style name not ending in `.css`, and a page over the
/// size bound. `Err` is the diagnostic shown in the page's place.
pub(crate) fn check_remote_parts(body: &[u8]) -> Result<RemoteParts, String> {
    let wire: WireParts = serde_json::from_slice(body)
        .map_err(|error| format!("its page parts are not valid JSON: {error}"))?;
    if wire.encoding != "base64" {
        return Err(format!(
            "its page parts use encoding '{}'; this dashboard reads only base64",
            wire.encoding
        ));
    }
    if let Some(kit) = wire.kit
        && !KNOWN_PAGE_KITS.contains(&kit)
    {
        return Err(format!(
            "the page names kit version {kit}, which this dashboard does not know"
        ));
    }
    let decode = |label: &str, content: &str| {
        base64::engine::general_purpose::STANDARD
            .decode(content)
            .map_err(|error| format!("{label} is not valid base64: {error}"))
    };
    let markup = decode("index.html", &wire.markup)?;
    let mut total = markup.len() as u64;
    if let Some(element) = find_page_refused(&markup, PAGE_MARKUP_REFUSED) {
        return Err(format!(
            "index.html holds a `{element}` element; a page's scripts and styles are written \
             by the dashboard"
        ));
    }
    let mut scripts = Vec::with_capacity(wire.scripts.len());
    for file in wire.scripts {
        if !PAGE_SCRIPT_EXTENSIONS
            .iter()
            .any(|extension| file.name.ends_with(extension))
        {
            return Err(format!(
                "script '{}' does not end in `.js` or `.mjs`",
                file.name
            ));
        }
        let bytes = decode(&format!("script '{}'", file.name), &file.content)?;
        if let Some(found) = find_page_refused(&bytes, PAGE_SCRIPT_REFUSED) {
            return Err(format!(
                "script '{}' contains `{found}`, which would end its `script` element early",
                file.name
            ));
        }
        total = total.saturating_add(bytes.len() as u64);
        scripts.push((file.name, bytes));
    }
    let mut styles = Vec::with_capacity(wire.styles.len());
    for file in wire.styles {
        if !file.name.ends_with(PAGE_STYLE_EXTENSION) {
            return Err(format!("style '{}' does not end in `.css`", file.name));
        }
        let bytes = decode(&format!("style '{}'", file.name), &file.content)?;
        if let Some(found) = find_page_refused(&bytes, PAGE_STYLE_REFUSED) {
            return Err(format!(
                "style '{}' contains `{found}`, which would end its `style` element early",
                file.name
            ));
        }
        total = total.saturating_add(bytes.len() as u64);
        styles.push((file.name, bytes));
    }
    if total > MAX_PAGE_BYTES {
        return Err(format!(
            "the page holds {total} bytes; a page may hold at most {MAX_PAGE_BYTES} bytes \
             (2 MiB) across index.html and its listed files"
        ));
    }
    Ok(RemoteParts {
        kit: wire.kit,
        markup,
        scripts,
        styles,
    })
}

/// Write one page as a document: the kit's stylesheet and each listed style
/// in `style` elements, the bytes of `index.html`, then the kit's script and
/// each listed script in `script` elements. A script named `*.mjs` is a
/// module script. A local page and another host's page are written by this
/// one function, with this dashboard's kit and policy.
pub(crate) fn write_document(source: &PageParts<'_>) -> PageDocument {
    let kit = kit(source.kit);
    let mut body: Vec<u8> = Vec::with_capacity(
        source.markup.len()
            + source
                .styles
                .iter()
                .map(|(_, bytes)| bytes.len())
                .sum::<usize>()
            + source
                .scripts
                .iter()
                .map(|(_, bytes)| bytes.len())
                .sum::<usize>()
            + kit.map_or(0, |(style, script)| style.len() + script.len())
            + 512,
    );
    body.extend_from_slice(b"<!doctype html>\n<meta charset=\"utf-8\">\n");
    let styles = kit
        .map(|(style, _)| style.as_bytes())
        .into_iter()
        .chain(source.styles.iter().map(|(_, bytes)| *bytes));
    for style in styles {
        body.extend_from_slice(b"<style>");
        body.extend_from_slice(style);
        body.extend_from_slice(b"</style>\n");
    }
    body.extend_from_slice(source.markup);
    body.push(b'\n');

    let mut hashes = Vec::new();
    let scripts = kit
        .map(|(_, script)| ("kit.js", script.as_bytes()))
        .into_iter()
        .chain(source.scripts.iter().copied());
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

/// Require Fetch Metadata proving a same-origin dashboard iframe navigation.
///
/// WHATWG HTML's `sandbox="allow-scripts"` still permits a frame to navigate
/// itself, and CSP `navigate-to` is unimplemented across browsers. The external
/// navigation boundary therefore depends on the dashboard host's
/// `frame-src 'self'` policy framing the document. Direct top-level loads
/// (`Sec-Fetch-Dest: document`, `Sec-Fetch-Site: none`), non-navigation fetches,
/// and unsupported browsers that omit Fetch Metadata fail closed here. Cross-site
/// requests, including opaque-origin self-navigations
/// (`Sec-Fetch-Site: cross-site`), are refused earlier by the API origin guard,
/// before this handler runs.
pub(crate) fn require_dashboard_frame_navigation(headers: &HeaderMap) -> Result<(), &'static str> {
    if !single_header_eq(headers, "sec-fetch-site", "same-origin") {
        return Err(
            "plugin pages require a same-origin dashboard iframe navigation (Sec-Fetch-Site: same-origin)",
        );
    }
    if !single_header_eq(headers, "sec-fetch-dest", "iframe") {
        return Err(
            "plugin pages may load only inside a dashboard iframe (Sec-Fetch-Dest: iframe)",
        );
    }
    if !single_header_eq(headers, "sec-fetch-mode", "navigate") {
        return Err("plugin pages require an iframe navigation request (Sec-Fetch-Mode: navigate)");
    }
    Ok(())
}

fn single_header_eq(headers: &HeaderMap, name: &str, expected: &str) -> bool {
    let mut values = headers.get_all(name).iter();
    let Some(first) = values.next() else {
        return false;
    };
    values.next().is_none() && first.to_str().is_ok_and(|value| value == expected)
}

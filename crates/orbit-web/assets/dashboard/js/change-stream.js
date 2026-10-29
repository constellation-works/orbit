// The dashboard's change stream: one EventSource per host and workspace on
// `/api/events/stream`, which pushes small notices (`task`, `run`, `locks`,
// `drain`, `host.resources`, `resync`) when the workspace changes, whichever
// process changed it. Panels subscribe to the kinds they show and refetch only
// on those; polling.js slows to its safety-net poll while the stream is live.
//
// A hidden tab keeps no stream: polling.js's background watch runs instead,
// as before. A stream that cannot open (an older host, the connection cap, a
// refused forward) leaves the dashboard on its ordinary polling and is retried
// after a pause.

import { getWorkspace, isAggregateView, onWorkspaceChange, withWorkspace } from './common.js';

const STREAM_PATH = "/api/events/stream";
// Notices for one subscriber arrive together after this pause, so a burst of
// changes costs one refetch.
const BATCH_MS = 250;
// After a stream that could not open, wait this long before trying again.
const RETRY_MS = 60_000;

const subscribers = new Set();
const liveListeners = new Set();
let source = null;
let sourceUrl = null;
let live = false;
let started = false;
let retryTimer = null;
let syncQueued = false;

/// Whether the stream is connected now. While it is, the panels that
/// subscribed hear about every change, so polling can slow down.
export function changeStreamLive() {
  return live;
}

/// Call `listener(live)` whenever the stream connects or drops.
export function onChangeStreamLive(listener) {
  liveListeners.add(listener);
  return () => liveListeners.delete(listener);
}

/// Subscribe to notices of `kinds`. `handler` receives each batch as an array
/// of notices, in arrival order. `resync` and `reconnect` (the stream came back
/// after a drop, so notices may have been missed) are kinds too.
export function onChange(kinds, handler) {
  const subscriber = { kinds: new Set(kinds), handler, pending: [], timer: null };
  subscribers.add(subscriber);
  return () => {
    if (subscriber.timer !== null) clearTimeout(subscriber.timer);
    subscribers.delete(subscriber);
  };
}

function deliver(notice) {
  for (const subscriber of subscribers) {
    if (!subscriber.kinds.has(notice.kind)) continue;
    subscriber.pending.push(notice);
    if (subscriber.timer !== null) continue;
    subscriber.timer = setTimeout(() => {
      subscriber.timer = null;
      const batch = subscriber.pending;
      subscriber.pending = [];
      try {
        subscriber.handler(batch);
      } catch (error) {
        console.error(error);
      }
    }, BATCH_MS);
  }
}

function setLive(next) {
  if (live === next) return;
  live = next;
  for (const listener of liveListeners) {
    try {
      listener(live);
    } catch (error) {
      console.error(error);
    }
  }
}

function close() {
  if (retryTimer !== null) {
    clearTimeout(retryTimer);
    retryTimer = null;
  }
  if (source) {
    const closing = source;
    source = null;
    sourceUrl = null;
    closing.onopen = closing.onmessage = closing.onerror = null;
    closing.close();
  }
  for (const subscriber of subscribers) {
    if (subscriber.timer !== null) clearTimeout(subscriber.timer);
    subscriber.timer = null;
    subscriber.pending = [];
  }
  setLive(false);
}

function open() {
  const wanted = !document.hidden && !isAggregateView() && getWorkspace() && typeof EventSource === "function"
    ? withWorkspace(STREAM_PATH)
    : null;
  // A scope event that settles where the stream already points keeps it.
  if (wanted && source && sourceUrl === wanted && source.readyState !== 2) return;
  close();
  if (!wanted) return;
  const stream = new EventSource(wanted);
  source = stream;
  sourceUrl = wanted;
  // Whether this stream has connected before: a later open is a reconnect.
  let opened = false;
  stream.onopen = () => {
    if (source !== stream) return;
    if (opened) deliver({ kind: "reconnect" });
    opened = true;
    setLive(true);
  };
  stream.onmessage = (event) => {
    if (source !== stream) return;
    let notice;
    try {
      notice = JSON.parse(event.data);
    } catch (error) {
      console.error("Failed to parse change notice", error);
      return;
    }
    if (notice && typeof notice.kind === "string") deliver(notice);
  };
  stream.onerror = () => {
    if (source !== stream) return;
    setLive(false);
    // CONNECTING means the browser is already retrying the dropped stream.
    if (stream.readyState !== 2) return;
    close();
    retryTimer = setTimeout(() => {
      retryTimer = null;
      open();
    }, RETRY_MS);
  };
}

// Scope changes arrive in bursts (a host switch also resets the workspace);
// open once for where they settle.
function queueSync() {
  if (syncQueued) return;
  syncQueued = true;
  setTimeout(() => {
    syncQueued = false;
    open();
  }, 0);
}

/// Start following the selected host and workspace. Safe to call once at
/// startup; scope changes and tab visibility reopen or close the stream.
export function startChangeStream() {
  if (started) return;
  started = true;
  onWorkspaceChange(queueSync);
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) close();
    else queueSync();
  });
  queueSync();
}

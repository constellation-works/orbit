/* Orbit plugin-page kit, bridge version 1 (plugin-pages design §3).
   Orbit writes this script into a page's document when its manifest names
   `kit: 1`; it is never served as a file. The page runs in a sandboxed
   frame with a unique origin and no network, so everything it learns comes
   through the dashboard that frames it:

     page -> dashboard  {v: 1, id, call: "<tool>", input: {...}}
     page -> dashboard  {v: 1, id, act: "<tool>", input: {...}}
     dashboard -> page  {v: 1, id, ok: true, output}
                        {v: 1, id, ok: false, error: {code, message}}
     dashboard -> page  {v: 1, init: {theme, host, workspace, operator, query}}
     page -> dashboard  {v: 1, query: "<query string>"}

   Only messages from the framing window are read. */
(function () {
  "use strict";
  var VERSION = 1;
  var parentWindow = window.parent;
  var pending = new Map();
  var nextId = 1;
  var context = null;
  var readyResolve;
  var ready = new Promise(function (resolve) { readyResolve = resolve; });

  function send(message) {
    message.v = VERSION;
    // A sandboxed frame has an opaque origin and cannot name the
    // dashboard's; the dashboard checks that the message came from this
    // frame before relaying it.
    parentWindow.postMessage(message, "*");
  }

  function applyTheme(theme) {
    document.documentElement.setAttribute(
      "data-theme",
      theme === "light" ? "light" : "dark"
    );
  }

  window.addEventListener("message", function (event) {
    if (event.source !== parentWindow) return;
    var message = event.data;
    if (!message || typeof message !== "object" || message.v !== VERSION) return;
    if (message.init && typeof message.init === "object") {
      context = Object.freeze(Object.assign({}, message.init));
      applyTheme(context.theme);
      readyResolve(context);
      return;
    }
    var waiter = pending.get(message.id);
    if (!waiter) return;
    pending.delete(message.id);
    if (message.ok === true) {
      waiter.resolve(message.output);
    } else {
      var error = (message.error && typeof message.error === "object") ? message.error : {};
      var failure = new Error(String(error.message || "the call failed"));
      failure.code = String(error.code || "call_failed");
      waiter.reject(failure);
    }
  });

  var orbit = {
    kit: VERSION,
    /* Call one of this plugin's read-only tools the page declares. */
    call: function (tool, input) {
      var id = String(nextId++);
      return new Promise(function (resolve, reject) {
        pending.set(id, { resolve: resolve, reject: reject });
        send({ id: id, call: String(tool), input: input === undefined ? {} : input });
      });
    },
    /* Ask to run one of this plugin's mutating tools the page names under
       `actions`. The dashboard shows the operator a confirmation outside
       this frame; the promise rejects with code `cancelled` when they
       decline, and resolves with the tool's output once it ran. */
    act: function (tool, input) {
      var id = String(nextId++);
      return new Promise(function (resolve, reject) {
        pending.set(id, { resolve: resolve, reject: reject });
        send({ id: id, act: String(tool), input: input === undefined ? {} : input });
      });
    },
    /* Resolves with {theme, host, workspace, operator, query} once the
       dashboard has introduced itself. */
    ready: ready,
    context: function () { return context; },
    /* Ask the dashboard to set this query string on the page's hash, so the
       view can be linked; it comes back as `query` on the next load. */
    setQuery: function (query) { send({ query: String(query || "") }); }
  };
  Object.defineProperty(window, "orbit", { value: Object.freeze(orbit) });
})();

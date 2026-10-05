// Shared safe Markdown rendering for dashboard-authored content.

const SANITIZE_CONFIG = {
  USE_PROFILES: { html: true },
  FORBID_TAGS: ["style"],
  FORBID_ATTR: ["style"],
};

let markedRendererConfigured = false;

function markedRuntime() {
  const candidate = globalThis.marked;
  return candidate && typeof candidate.parse === "function" ? candidate : null;
}

function purifierRuntime() {
  const candidate = globalThis.DOMPurify;
  return candidate && candidate.isSupported !== false && typeof candidate.sanitize === "function"
    ? candidate
    : null;
}

function escapeHtml(value) {
  return String(value ?? "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

function rawHtmlText(token) {
  if (typeof token === "string") return token;
  if (token && typeof token.raw === "string") return token.raw;
  if (token && typeof token.text === "string") return token.text;
  return "";
}

function configureMarkedRenderer(marked) {
  if (markedRendererConfigured || !marked || typeof marked.use !== "function") return;
  marked.use({
    renderer: {
      html(token) {
        return escapeHtml(rawHtmlText(token));
      },
    },
  });
  markedRendererConfigured = true;
}

function renderWithMarked(methodName, source) {
  const marked = markedRuntime();
  const purifier = purifierRuntime();
  if (!marked || !purifier || typeof marked[methodName] !== "function") return null;
  configureMarkedRenderer(marked);
  const rendered = marked[methodName](String(source ?? ""), { async: false });
  if (typeof rendered !== "string") return null;
  return purifier.sanitize(rendered, SANITIZE_CONFIG);
}

export function renderMarkdown(source) {
  return renderWithMarked("parse", source);
}

export function renderMarkdownInline(source) {
  return renderWithMarked("parseInline", source);
}

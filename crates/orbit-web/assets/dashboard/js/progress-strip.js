// One presentation for task lifecycle, run stages and delivery progress.
import { el, pinnedInfoHint } from './common.js';

const STATE_WORDS = {
  done: "done", active: "in progress", current: "current", pending: "not yet",
  failed: "failed", cancelled: "cancelled", awaiting: "awaiting approval",
};

export function buildProgressStrip(steps, { className, label }) {
  if (!Array.isArray(steps) || !steps.length) return null;
  const list = el("ol", { class: `progress-strip ${className}`, "aria-label": label });
  for (const step of steps) {
    const state = step.state || "pending";
    const stateText = STATE_WORDS[state] || state;
    const fullText = [step.label, step.label.toLowerCase() === stateText ? null : stateText, step.hint].filter(Boolean).join(" · ");
    const trigger = el(step.href ? "a" : "span", {
      class: "progress-strip-hint", "aria-label": fullText,
    }, [el("span", { class: "progress-strip-label", text: step.label })]);
    if (step.href) trigger.href = step.href;
    else trigger.tabIndex = 0;
    const item = el("li", { class: "progress-strip-step" }, [
      el("span", { class: "progress-strip-bar", "aria-hidden": "true" }),
      pinnedInfoHint(fullText, { trigger }),
    ]);
    item.dataset.state = state;
    if (step.current || state === "active" || state === "awaiting") {
      item.setAttribute("aria-current", "step");
    }
    list.appendChild(item);
  }
  return list;
}

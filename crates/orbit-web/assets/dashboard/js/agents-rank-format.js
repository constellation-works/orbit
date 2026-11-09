// Shared formatting for Analytics › Agents › Rank (js/agents-rank.js) and its
// score-against-cost chart (js/agents-rank-frontier.js): numbers, tiers and the
// provider colour a model name implies. Callers import the module as
// `rankFormat` and call `rankFormat.num(…)`, so every call site greps.

import { el } from './common.js';

const TIER_ORDER = ["low", "medium", "hard", "xhard", "unassessed"];
// A rank row names a model, not a provider; the name's prefix picks the colour.
const PROVIDER_PREFIXES = [
  ["gpt", "codex"], ["codex", "codex"], ["o1", "codex"], ["o3", "codex"], ["o4", "codex"],
  ["claude", "claude"], ["gemini", "gemini"], ["grok", "grok"], ["muse", "muse"],
];

function providerOf(model) {
  const name = String(model || "").toLowerCase();
  const hit = PROVIDER_PREFIXES.find(([prefix]) => name.startsWith(prefix));
  return hit ? hit[1] : null;
}

export function providerColor(model) {
  const provider = providerOf(model);
  return provider ? `var(--ag-${provider})` : "var(--fg-mute)";
}

export function providerDot(model) {
  const dot = el("span", { class: "agents-rank-dot", "aria-hidden": "true" });
  dot.style.background = providerColor(model);
  return dot;
}

export function tierIndex(tier) {
  const index = TIER_ORDER.indexOf(tier);
  return index < 0 ? TIER_ORDER.length : index;
}

export function tierLabel(tier) {
  if (!tier) return "Tier";
  if (tier === "xhard") return "Xhard";
  return tier.charAt(0).toUpperCase() + tier.slice(1);
}

export function plural(count, noun) {
  return `${num(count, 0)} ${noun}${count === 1 ? "" : "s"}`;
}

export function num(value, digits = 2) {
  if (typeof value !== "number" || !Number.isFinite(value)) return "—";
  return value.toLocaleString(undefined, { maximumFractionDigits: digits });
}

export function dollars(value) {
  if (typeof value !== "number" || !Number.isFinite(value)) return "—";
  return `$${value.toFixed(2)}`;
}

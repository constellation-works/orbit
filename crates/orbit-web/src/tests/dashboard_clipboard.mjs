// "Click to copy" controls must only claim a copy the browser actually made.
//
// The async clipboard API is absent on a dashboard served over plain HTTP from
// a LAN address and rejects when the page is unfocused; the controls used to
// flash "copied!" regardless. These scenarios drive the shipped helpers with a
// stubbed clipboard and observe what they report and what the node shows.
import assert from "node:assert/strict";

const { copyText, copyWithFeedback, makeCopyButton } = await import("./js/common.js");

const setNavigator = (value) => Object.defineProperty(globalThis, "navigator", { value, configurable: true });

// Feedback timers run when the scenario says so, not on a wall clock.
const timers = [];
globalThis.setTimeout = (callback) => {
  timers.push(callback);
  return timers.length;
};
globalThis.clearTimeout = (id) => {
  timers[id - 1] = null;
};
const elapse = () => {
  for (const callback of timers.splice(0)) if (callback) callback();
};

const copied = [];
const workingClipboard = { clipboard: { writeText: (text) => { copied.push(text); return Promise.resolve(); } } };
const rejectingClipboard = { clipboard: { writeText: () => Promise.reject(new Error("document not focused")) } };
const noClipboard = {};

// --- copyText ---------------------------------------------------------------

setNavigator(workingClipboard);
assert.equal(await copyText("ORB-1"), true, "an accepted write reports success");
assert.deepEqual(copied, ["ORB-1"]);

setNavigator(rejectingClipboard);
assert.equal(await copyText("ORB-1"), false, "a rejected write with no fallback reports failure");

setNavigator(noClipboard);
assert.equal(await copyText("ORB-1"), false, "an insecure context with no fallback reports failure");

// With the async API unavailable, the legacy selection copy still works.
let selected = null;
const createElement = document.createElement;
document.createElement = (tag) => Object.assign(createElement(tag), { select() { selected = this.value; } });
document.execCommand = (command) => command === "copy";
assert.equal(await copyText("ORB-2"), true, "the legacy path copies when the async API is unavailable");
assert.equal(selected, "ORB-2", "the legacy path selects the text it copies");
assert.equal(document.body.children.length, 0, "the legacy path leaves no scratch element behind");
setNavigator(rejectingClipboard);
assert.equal(await copyText("ORB-3"), true, "the legacy path also covers a rejected async write");
document.execCommand = () => false;
assert.equal(await copyText("ORB-3"), false, "a refused legacy copy reports failure");
delete document.execCommand;
document.createElement = createElement;

// --- copyWithFeedback -------------------------------------------------------

const idCell = document.createElement("span");
idCell.textContent = "ORB-7";

setNavigator(workingClipboard);
await copyWithFeedback(idCell, "ORB-7");
assert.equal(idCell.textContent, "copied!", "a real copy is confirmed");
elapse();
assert.equal(idCell.textContent, "ORB-7", "the id comes back after the confirmation");

// A second click during the confirmation must not capture the confirmation
// text as the label to restore.
await copyWithFeedback(idCell, "ORB-7");
await copyWithFeedback(idCell, "ORB-7");
elapse();
assert.equal(idCell.textContent, "ORB-7", "clicking twice in a row still restores the id");
assert.equal(idCell.style.color, "", "the confirmation colour is cleared");

setNavigator(noClipboard);
await copyWithFeedback(idCell, "ORB-7");
assert.equal(idCell.textContent, "copy failed", "a failed copy is reported, not confirmed");
elapse();
assert.equal(idCell.textContent, "ORB-7", "the id comes back after the failure notice");

// --- makeCopyButton ---------------------------------------------------------
// A list row's identifier must be operable from the keyboard. A real <button>
// is what makes Enter and Space fire `click` in the browser, so the contract
// here is: it is a button, it copies its value when clicked, the click stays
// off the row that toggles on click, and the outcome is announced.

const settle = async () => { for (let i = 0; i < 4; i++) await Promise.resolve(); };

setNavigator(workingClipboard);
copied.length = 0;
const idButton = makeCopyButton("ORB-9", { class: "id mono", title: "Copy task ID" });
assert.equal(idButton.tagName, "BUTTON", "the identifier is a real button, so it is a tab stop that Enter and Space activate");
assert.equal(idButton.type, "button", "it never submits a surrounding form");
assert.equal(idButton.textContent, "ORB-9", "the visible text is the identifier, which is also its accessible name");
assert.equal(idButton.getAttribute("aria-live"), "polite", "copied!/copy failed is announced to assistive technology");
assert.ok(idButton.classList.contains("id") && idButton.classList.contains("mono"), "the row's own styling classes are kept");

let propagated = true;
idButton.dispatch("click", { stopPropagation() { propagated = false; } });
await settle();
assert.deepEqual(copied, ["ORB-9"], "activating the button copies its value");
assert.equal(propagated, false, "the click does not also toggle the row that hosts the button");
assert.equal(idButton.textContent, "copied!", "the outcome is shown on the button itself");
elapse();
assert.equal(idButton.textContent, "ORB-9");

// The copied value can differ from the label (a shortened display).
copied.length = 0;
const labelled = makeCopyButton("run-full-id-123", { text: "run-full..." });
labelled.dispatch("click");
await settle();
assert.deepEqual(copied, ["run-full-id-123"]);
assert.equal(labelled.textContent, "copied!");
elapse();
assert.equal(labelled.textContent, "run-full...");

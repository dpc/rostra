// Run with `node --test crates/rostra-web-ui/tests/media-picker.js`.
// Exercise media insertion with an inert synthetic DOM and no network access.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(
  process.env.ROSTRA_APP_JS ?? new URL("../assets/app.js", import.meta.url),
  "utf8",
);

function harness(targetSelector, initialValue = "") {
  const picker = {
    classList: {
      removed: [],
      remove(name) {
        this.removed.push(name);
      },
    },
    dataset: { target: targetSelector },
    style: {},
    open: true,
    close() {
      this.open = false;
    },
  };
  const textarea = {
    value: initialValue,
    selectionStart: initialValue.length,
    selectionEnd: initialValue.length,
    focused: false,
    inputEvents: 0,
    setSelectionRange(start, end) {
      this.selectionStart = start;
      this.selectionEnd = end;
    },
    focus() {
      assert.equal(picker.open, false, "modal closes before composer regains focus");
      this.focused = true;
    },
    dispatchEvent() {
      this.inputEvents++;
    },
    setRangeText(text, start, end) {
      this.value = this.value.slice(0, start) + text + this.value.slice(end);
      this.selectionStart = this.selectionEnd = start + text.length;
    },
  };
  const document = {
    addEventListener() {},
    getElementById(id) {
      if (id === "new-post-content") return textarea;
      return id === "media-list" || id === "post-preview-dialog" ? picker : null;
    },
    querySelector(selector) {
      if (selector === targetSelector) return textarea;
      if (selector === ".o-mediaList") return picker;
      return null;
    },
  };
  const context = vm.createContext({
    document,
    Event: class Event {},
    history: { state: null },
    navigator: {},
    setTimeout,
    clearTimeout,
    URLSearchParams,
    console,
    executionSentinel: 0,
  });
  context.window = context;
  context.addEventListener = () => {};
  vm.runInContext(source, context);

  return { context, picker, textarea, window: context };
}

test("external link insertion replaces selected text and updates draft listeners", () => {
  const app = harness("#new-post-content", "before replace after");
  app.textarea.selectionStart = 7;
  app.textarea.selectionEnd = 14;
  const syntax = "[title](https://example.com/)";
  app.window.insertExternalLink(syntax);
  assert.equal(app.textarea.value, "before " + syntax + " after");
  assert.equal(app.textarea.inputEvents, 1);
  assert.equal(app.picker.open, false);
  assert.equal(app.textarea.focused, true);
});

test("hostile target data never becomes click handler program text", () => {
  const hostile =
    "');globalThis.executionSentinel++;function insertMediaSyntax(){}//";
  const app = harness(hostile);
  const handler = "insertMediaSyntax('EVENT')";

  vm.runInContext(handler, app.context);

  assert.equal(app.context.executionSentinel, 0);
  assert.equal(app.textarea.value, "![media](rostra-media:EVENT)");
  assert.deepEqual(app.picker.classList.removed, ["-active"]);
  assert.equal(app.picker.open, false);
});

test("omitted targets preserve composer and upload insertion behavior", () => {
  for (const target of [
    "#new-post-content",
    "#inline-reply-content-42",
    "#shoutbox-input",
  ]) {
    const app = harness(target, "before ");

    app.window.insertMediaSyntax("EVENT");

    assert.equal(
      app.textarea.value,
      "before ![media](rostra-media:EVENT)",
      target,
    );
    assert.equal(app.textarea.focused, true, target);
    assert.equal(app.textarea.inputEvents, 1, target);
    assert.equal(app.picker.open, false, target);
    assert.deepEqual(app.picker.classList.removed, ["-active"], target);
  }
});

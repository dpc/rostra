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

const resizeSource = source.match(/function resizeXPost\(event\) \{[\s\S]*?\n\}/)[0];

const expandSource = source.match(/function expandExternalMedia\(event\) \{[\s\S]*?\n\}/)[0];

test("external media enhances only primary activation and loads each occurrence once", () => {
  const classes = new Set();
  const loads = [];
  const media = {
    hidden: true, dataset: { src: "https://platform.twitter.com/embed/Tweet.html?id=123" },
    hasAttribute: name => name === "data-x-post",
    set src(value) { loads.push(value); },
  };
  const card = {
    classList: { contains: value => classes.has(value), add: value => classes.add(value) },
    querySelector: () => media,
  };
  const activate = (options = {}) => {
    const event = {
      button: 0, currentTarget: { closest: () => card },
      preventDefault() { this.prevented = true; },
      stopPropagation() { this.stopped = true; },
      ...options,
    };
    vm.runInNewContext(`${expandSource}; expandExternalMedia(event)`, {
      event, window: { matchMedia: () => ({ matches: true }) },
    });
    return event;
  };
  for (const options of [
    { button: 1 }, { button: 2 }, { ctrlKey: true }, { metaKey: true },
    { shiftKey: true }, { altKey: true }, { defaultPrevented: true },
  ]) {
    assert.equal(activate(options).prevented, undefined);
    assert.equal(loads.length, 0);
  }
  assert.equal(activate({ detail: 0 }).prevented, true); // Native Enter click.
  assert.equal(media.hidden, false);
  assert.deepEqual(loads, ["https://platform.twitter.com/embed/Tweet.html?id=123&theme=dark"]);
  activate();
  assert.equal(loads.length, 1);
  assert.equal(activate({ ctrlKey: true }).prevented, undefined);
});

test("X resize messages only change their own visible marked frame's bounded height", () => {
  const sender = {};
  const other = { hidden: false, contentWindow: {}, style: {} };
  const frame = { hidden: false, contentWindow: sender, style: {} };
  const resize = (data, origin = "https://platform.twitter.com", eventSource = sender) =>
    vm.runInNewContext(`${resizeSource}; resizeXPost(event)`, {
      document: { querySelectorAll(selector) {
        assert.equal(selector, "iframe[data-x-post]");
        return [other, frame];
      } },
      event: { origin, source: eventSource, data },
    });
  const payload = height => ({ "twttr.embed": {
    method: "twttr.private.resize", params: [{ height, width: 999999 }], id: "untrusted",
  } });
  resize(payload(552.2));
  assert.equal(frame.style.height, "553px");
  assert.deepEqual(other.style, {});
  resize(JSON.stringify(payload(300)));
  assert.equal(frame.style.height, "300px");
  resize(payload("300"));
  assert.equal(frame.style.height, "300px");
  resize({ "twttr.embed": { method: "twttr.private.resize", params: { height: 300 } } });
  assert.equal(frame.style.height, "300px");
  for (const height of [null, "900px", "Infinity", "100000", -1, 0, 63, 4097, Infinity, NaN]) resize(payload(height));
  for (const data of [null, {}, "bad JSON", "x".repeat(4097),
    { "twttr.embed": { method: "navigate", params: [{ height: 900 }] } },
    { "twttr.embed": { method: "twttr.private.resize", params: [] } }]) resize(data);
  resize(payload(900), "https://evil.test");
  resize(payload(900), "https://platform.twitter.com", {});
  frame.hidden = true;
  resize(payload(900));
  assert.equal(frame.style.height, "300px");
});

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

test("external link spacing depends on text before the insertion, not the draft suffix", () => {
  const syntax = "[title](https://example.com/)";
  for (const [value, start, end, expected] of [
    ["", 0, 0, syntax],
    ["text", 4, 4, "text " + syntax],
    ["text ", 5, 5, "text " + syntax],
    ["text\n", 5, 5, "text\n" + syntax],
    ["text\t", 5, 5, "text\t" + syntax],
    ["text\u00a0", 5, 5, "text\u00a0" + syntax],
    ["text\u0085", 5, 5, "text\u0085" + syntax],
    ["text\ufeff", 5, 5, "text\ufeff " + syntax],
    ["textSUFFIX ", 4, 10, "text " + syntax + " "],
    ["text SUFFIX", 5, 11, "text " + syntax],
    ["replace", 0, 7, syntax],
    ["suffix", 0, 0, syntax + "suffix"],
  ]) {
    const app = harness("#new-post-content", value);
    app.textarea.selectionStart = start;
    app.textarea.selectionEnd = end;
    app.window.insertExternalLink(syntax);
    assert.equal(app.textarea.value, expected);
    assert.equal(app.textarea.selectionStart, expected.indexOf(syntax) + syntax.length);
    assert.equal(app.textarea.inputEvents, 1);
    assert.equal(app.picker.open, false);
    assert.equal(app.textarea.focused, true);
  }
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

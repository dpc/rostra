// Run with `node --test crates/rostra-web-ui/tests/messages.js`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

function harness() {
  let initialize;
  let factory;
  const scrolls = [];
  const existing = new Set();
  const document = {
    hidden: false,
    body: {
      scrollHeight: 1000,
      scrollTop: 900,
      clientHeight: 100,
      scrollTo: (...args) => scrolls.push(args),
      style: {},
    },
    addEventListener(name, callback) {
      if (name === "alpine:init") initialize = callback;
    },
    getElementById: (id) => existing.has(id),
  };
  vm.runInNewContext(
    readFileSync(
      process.env.ROSTRA_APP_JS ?? new URL("../assets/app.js", import.meta.url),
      "utf8",
    ),
    {
      document,
      Alpine: {
        data(name, component) {
          if (name === "directMessageHistory") factory = component;
        },
      },
      window: { addEventListener() {} },
      history: { state: null },
      navigator: {},
      setTimeout() { throw new Error("live messages must not poll"); },
      requestAnimationFrame: (callback) => callback(),
      console,
    },
  );
  initialize();
  const component = factory();
  const requests = [];
  const cursor = { dataset: { afterSequence: "0", more: "false" } };
  component.$el = {
    isConnected: true,
    dataset: { refreshUrl: "/messages/peer" },
    querySelector(selector) {
      if (selector === "#direct-message-live-cursor") return cursor;
      if (selector === "#direct-message-history") return { children: [] };
      return null;
    },
  };
  component.$nextTick = async () => {};
  component.$ajax = async (url, options) => {
    requests.push({ url, options });
    cursor.dataset.afterSequence = "7";
    component.receivedNew = true;
  };
  return { component, document, requests, scrolls, existing, cursor };
}

test("wakeups request bounded fragments without replacing composer or stealing focus", async () => {
  const h = harness();
  await h.component.init();
  assert.equal(h.requests[0].url, "/messages/peer?after_sequence=0");
  assert.deepEqual(Array.from(h.requests[0].options.targets), [
    "direct-message-history", "direct-message-live-cursor",
  ]);
  assert.equal(h.requests[0].options.focus, false);
  assert.equal(h.component.loadingNew, false);
  assert.equal(h.scrolls.length, 1);
  h.document.body.scrollTop = 0;
  await h.component.queueRefresh();
  assert.equal(h.requests[1].url, "/messages/peer?after_sequence=7");
  assert.equal(h.scrolls.length, 1, "reading older history must not jump");
  h.component.destroy();
  await h.component.queueRefresh();
  assert.equal(h.requests.length, 2);
});

test("defers hidden/older-history work and retries only on another wakeup", async () => {
  const h = harness();
  h.document.hidden = true;
  await h.component.queueRefresh();
  h.document.hidden = false;
  h.component.loadingOlder = true;
  await h.component.queueRefresh();
  assert.equal(h.requests.length, 0);
  h.component.loadingOlder = false;
  h.component.$ajax = async () => { throw new Error("offline"); };
  await h.component.queueRefresh();
  assert.equal(h.component.loadingNew, false);
  assert.equal(h.component.refreshPending, true);
  h.component.$ajax = async (url) => h.requests.push(url);
  await h.component.queueRefresh();
  assert.equal(h.requests[0], "/messages/peer?after_sequence=0");
});

test("navigation during an in-flight update stops scrolling and further requests", async () => {
  const h = harness();
  let finish;
  h.component.$ajax = () => new Promise((resolve) => { finish = resolve; });
  const pending = h.component.init();
  while (!finish) await Promise.resolve();
  h.component.destroy();
  h.component.$el.isConnected = false;
  finish();
  await pending;
  assert.equal(h.scrolls.length, 0);
  assert.equal(h.component.loadingNew, false);
});

test("empty updates do not typeset or scroll an unchanged conversation", async () => {
  const h = harness();
  h.component.$ajax = async () => {};
  await h.component.init();
  assert.equal(h.scrolls.length, 0);
});

test("overlapping history and repeated updates keep only unseen message IDs", () => {
  const h = harness();
  h.component.loadingNew = true;
  h.existing.add("message-1");
  let removed = 0;
  const children = [
    { id: "message-1", remove() { removed++; } },
    { id: "message-2", remove() { throw new Error("new message removed"); } },
  ];
  h.component.beforeOlderMerge({
    target: { id: "direct-message-history" },
    detail: { content: { children } },
  });
  assert.equal(removed, 1);
});

test("one wakeup drains a multi-page backlog without waiting for more notifications", async () => {
  const h = harness();
  let requests = 0;
  h.component.$ajax = async () => {
    requests++;
    h.cursor.dataset.afterSequence = String(requests * 100);
    h.cursor.dataset.more = requests < 3 ? "true" : "false";
  };
  await h.component.init();
  assert.equal(requests, 3);
  assert.equal(h.component.refreshPending, false);
});

test("notifications during an in-flight update coalesce into one follow-up", async () => {
  const h = harness();
  let finish;
  let requests = 0;
  h.component.$ajax = async () => {
    requests++;
    if (requests === 1) await new Promise((resolve) => { finish = resolve; });
  };
  const pending = h.component.init();
  while (!finish) await Promise.resolve();
  h.component.queueRefresh();
  h.component.queueRefresh();
  finish();
  await pending;
  assert.equal(requests, 2);
});

test("older pagination waits for the live request without a busy-wait timer", async () => {
  const h = harness();
  let finish;
  h.component.$ajax = () => new Promise((resolve) => { finish = resolve; });
  const live = h.component.init();
  while (!finish) await Promise.resolve();
  let olderRequests = 0;
  const older = h.component.loadOlder(
    { isConnected: true, href: "/messages/peer?before_time=1" },
    async () => { olderRequests++; },
  );
  assert.equal(olderRequests, 0);
  finish();
  await Promise.all([live, older]);
  assert.equal(olderRequests, 1);
});

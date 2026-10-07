// Run with `node --test crates/rostra-web-ui/tests/alpine-ajax.js`.
// Exercise the shipped bundle with inert DOM targets and no network access.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const bundle = readFileSync(
  process.env.ROSTRA_ALPINE_AJAX_BUNDLE ??
    new URL("../assets/libs/alpine-ajax@0.12.6.js", import.meta.url),
  "utf8",
);
const linkMergeGuard = readFileSync(
  process.env.ROSTRA_LINK_ROUTE_SOURCE ?? new URL("../src/routes/link.rs", import.meta.url), "utf8")
  .match(/"@ajax:merge"="([^"]+)"/)[1];
const xPostLoad = readFileSync(
  process.env.ROSTRA_CONTENT_ROUTE_SOURCE ?? new URL("../src/routes/content.rs", import.meta.url), "utf8")
  .match(/onclick="([^"]+)"/)[1];

test("X post uses existing load presentation and loads only on first activation", () => {
  const loads = [];
  const classes = new Set();
  let prevented = 0;
  const frame = {
    hidden: true,
    dataset: { src: "https://platform.twitter.com/embed/Tweet.html?id=123&dnt=true" },
    set src(value) { loads.push(value); },
  };
  const wrapper = {
    classList: { contains: name => classes.has(name), add: name => classes.add(name) },
    lastElementChild: frame,
  };
  const activate = () => vm.runInNewContext(`(function() { ${xPostLoad} }).call(wrapper)`, {
    wrapper,
    event: { preventDefault() { prevented++; }, stopPropagation() {} },
    window: { matchMedia: () => ({ matches: true }) },
  });
  assert.deepEqual(loads, []);
  assert.equal(frame.hidden, true);
  activate();
  assert.deepEqual(loads, ["https://platform.twitter.com/embed/Tweet.html?id=123&dnt=true&theme=dark"]);
  assert.equal(frame.hidden, false);
  assert.equal(prevented, 1);
  activate();
  assert.equal(loads.length, 1);
  assert.equal(prevented, 1);
});

function harness(fetch, dialogTarget = false, responseScope = "opening-one") {
  const attributes = new Map();
  const events = [];
  let initialize;
  let ajax;
  let loadingTimer;
  let loading = false;
  let merges = 0;
  const incoming = {
    dataset: responseScope ? { linkRequest: responseScope } : {},
    removeAttribute() {},
    dispatchEvent: () => true,
  };
  const target = {
    isConnected: true,
    open: true,
    dataset: { linkRequest: "opening-one" },
    replaceWith() { merges++; },
    querySelectorAll: () => [],
    setAttribute: (key, value) => attributes.set(key, value),
    removeAttribute: (key) => attributes.delete(key),
    dispatchEvent(event) {
      events.push(event);
      if (event.type === "ajax:merge") {
        vm.runInNewContext(linkMergeGuard, { $el: this, $event: event });
      }
      return !event.defaultPrevented;
    },
  };
  const element = {
    getAttribute: () => null,
    closest: () => null,
    dispatchEvent(event) {
      events.push(event);
      if (event.type === "ajax:before") {
        loadingTimer = setTimeout(() => { loading = true; }, 150);
      }
      if (event.type === "ajax:after") {
        clearTimeout(loadingTimer);
        loading = false;
      }
      return true;
    },
  };
  const Alpine = {
    addInitSelector() {},
    directive() {},
    magic(name, callback) {
      if (name === "ajax") ajax = callback(element);
    },
  };
  const document = {
    baseURI: "http://localhost/messages",
    documentElement: dialogTarget ? {} : target,
    getElementById: () => target,
    querySelectorAll: () => [],
    createRange: () => ({
      createContextualFragment: () => ({
        firstElementChild: { content: { getElementById: () => incoming } },
      }),
    }),
    addEventListener(name, callback) {
      if (name === "alpine:initializing") initialize = callback;
    },
  };
  vm.runInNewContext(bundle, {
    document,
    window: { Alpine, addEventListener() {} },
    fetch,
    URL,
    setTimeout,
    console,
    DOMException,
    CustomEvent: class {
      constructor(type, options) {
        this.type = type;
        this.detail = options.detail;
      }
      preventDefault() {
        this.defaultPrevented = true;
      }
    },
  });
  initialize();
  return {
    request: (method) => ajax("/messages/test", { method, focus: false, targets: [dialogTarget ? "dialog" : "_none"] }),
    closeDialog: () => { target.open = false; },
    merges: () => merges,
    events,
    attributes,
    isLoading: () => loading,
  };
}

test("closed dialog cancels a late response at the actual target merge event", async () => {
  let respond;
  const app = harness(() => new Promise((resolve) => { respond = resolve; }), true);
  const request = app.request("POST");
  app.closeDialog();
  respond({ ok: true, status: 200, url: "http://localhost/messages/test",
    redirected: false, text: async () => "<dialog></dialog>" });
  await request;
  const merge = app.events.find((event) => event.type === "ajax:merge");
  assert.ok(merge, "merge is dispatched on the target dialog, not requesting form");
  assert.equal(merge.defaultPrevented, true);
  assert.equal(app.merges(), 0);
  assert.ok(app.events.some((event) => event.type === "ajax:after"));
});

for (const [label, responseScope] of [["Add link", "opening-two"], ["Preview", null]]) {
  test(`closed link dialog still accepts a later ${label} opening`, async () => {
    const app = harness(async () => ({ ok: true, status: 200, url: "http://localhost/messages/test",
      redirected: false, text: async () => "<dialog></dialog>" }), true, responseScope);
    app.closeDialog();
    await app.request("POST");
    assert.equal(app.merges(), 1);
    assert.ok(!app.events.find((event) => event.type === "ajax:merge").defaultPrevented);
  });
}

test("server-rendered form errors merge through the same hypermedia target", async () => {
  const app = harness(async () => ({ ok: false, status: 422, url: "http://localhost/messages/test",
    redirected: false, text: async () => "<dialog><p role='alert'>Retry or insert manually</p></dialog>" }), true);
  await app.request("POST");
  assert.equal(app.merges(), 1);
  assert.ok(app.events.some((event) => event.type === "ajax:error"));
  assert.ok(app.events.some((event) => event.type === "ajax:after"));
});

for (const method of ["GET", "POST"]) {
  for (const failure of ["fetch", "response-body"]) {
    test(`${method} ${failure} rejection releases state and permits an explicit retry`, async () => {
      let requests = 0;
      const app = harness(async () => {
        requests++;
        if (requests === 1) {
          await new Promise((resolve) => setTimeout(resolve, 180));
          if (failure === "fetch") throw new TypeError("offline");
          return { text: async () => { throw new TypeError("connection lost"); } };
        }
        return {
          ok: true,
          status: 200,
          redirected: false,
          url: "http://localhost/messages/test",
          text: async () => "",
        };
      });
      await assert.rejects(app.request(method), TypeError);
      assert.equal(requests, 1, "never replay an uncertain POST automatically");
      assert.equal(app.isLoading(), false);
      assert.equal(app.attributes.has("aria-busy"), false);
      const error = app.events.find((event) => event.type === "ajax:error");
      assert.equal(error.detail.status, 0, "shared notifications recognize transport errors");
      assert.equal(app.events.at(-1).type, "ajax:after");
      assert.equal(app.events.some((event) => event.type === "ajax:success"), false);

      await app.request(method);
      assert.equal(requests, 2, "the failed request must not stay in the GET cache");
      assert.equal(app.isLoading(), false);
      assert.equal(app.attributes.has("aria-busy"), false);
      assert.equal(app.events.some((event) => event.type === "ajax:success"), true);
    });
  }
}

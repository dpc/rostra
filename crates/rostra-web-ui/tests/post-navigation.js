// Run with `node --test crates/rostra-web-ui/tests/post-navigation.js`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(
  process.env.ROSTRA_APP_JS ?? new URL("../assets/app.js", import.meta.url),
  "utf8",
);
const navigateSource = source.match(/function navigatePost\(event\) \{[\s\S]*?\n\}/)[0];

function activate({ target = "", collapsed = false, selection = "", href = "/profile/author/post/event", ...options } = {}) {
  const opened = [];
  const window = {
    location: "/",
    getSelection: () => ({ toString: () => selection }),
    open: (...args) => opened.push(args),
  };
  const event = {
    button: 0,
    currentTarget: { dataset: { href } },
    target: {
      closest(selector) {
        if (selector === ".m-postContext__postParent:not(.-expanded)") return collapsed;
        return selector.split(", ").includes(target);
      },
    },
    preventDefault() { this.defaultPrevented = true; },
    ...options,
  };
  vm.runInNewContext(`${navigateSource}; navigatePost(event)`, { event, window });
  return { event, window, opened };
}

test("post body primary activation navigates in place; middle activation opens once", () => {
  const primary = activate();
  assert.equal(primary.window.location, "/profile/author/post/event");
  assert.deepEqual(primary.opened, []);
  const middle = activate({ button: 1 });
  assert.equal(middle.window.location, "/");
  assert.deepEqual(middle.opened, [["/profile/author/post/event", "_blank", "noopener"]]);
  assert.equal(middle.event.defaultPrevented, true);
});

test("modified primary activation keeps the current timeline", () => {
  for (const modifier of ["ctrlKey", "metaKey", "shiftKey"]) {
    const result = activate({ [modifier]: true });
    assert.equal(result.window.location, "/");
    assert.equal(result.opened.length, 1);
  }
});

test("nested native links and controls retain primary and middle activation", () => {
  for (const target of ["a", "button", "details", "form", "textarea", "input", "select", "audio", "video", "[contenteditable]"]) {
    for (const button of [0, 1]) {
      const result = activate({ target, button });
      assert.equal(result.window.location, "/");
      assert.deepEqual(result.opened, []);
      assert.equal(result.event.defaultPrevented, undefined);
    }
  }
});

test("non-navigable, folded, selected, canceled and secondary clicks do not navigate", () => {
  for (const options of [
    { href: null }, { href: "" }, { collapsed: true },
    { selection: "selected post text" }, { defaultPrevented: true }, { button: 2 },
  ]) {
    const result = activate(options);
    assert.equal(result.window.location, "/");
    assert.deepEqual(result.opened, []);
  }
});

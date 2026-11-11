// node --test src/chat/
import { readFileSync } from "node:fs";
import { test } from "node:test";
import assert from "node:assert/strict";
import vm from "node:vm";

const context = vm.createContext({});
vm.runInContext(readFileSync(new URL("widget.js", import.meta.url), "utf8"), context);
const { showsOn, parts, merge } = context.spleeChat;
const plain = (value) => JSON.parse(JSON.stringify(value));

test("shows on every page without a list, else on listed pages and the pages under them", () => {
  assert.equal(showsOn([], "/anything"), true);
  assert.equal(showsOn(["/pricing"], "/pricing"), true);
  assert.equal(showsOn(["/pricing"], "/pricing/"), true);
  assert.equal(showsOn(["/pricing"], "/pricing/teams"), true);
  assert.equal(showsOn(["/pricing"], "/pricingx"), false);
  assert.equal(showsOn(["/pricing"], "/blog"), false);
  // The home page alone, not every page under it.
  assert.equal(showsOn(["/"], "/"), true);
  assert.equal(showsOn(["/"], "/blog"), false);
});

test("splits links out of text, leaving trailing punctuation outside them", () => {
  assert.deepEqual(plain(parts("Book a time at https://app.example/book/sam. Or email us.")), [
    { text: "Book a time at " },
    { link: "https://app.example/book/sam" },
    { text: ". Or email us." },
  ]);
  assert.deepEqual(plain(parts("no links here")), [{ text: "no links here" }]);
  assert.deepEqual(plain(parts("javascript:alert(1) and <b>")), [{ text: "javascript:alert(1) and <b>" }]);
});

test("merges new messages in order without repeating ones it has", () => {
  const have = [
    { id: 1, body: "a" },
    { id: 3, body: "c" },
  ];
  assert.deepEqual(
    plain(merge(have, [{ id: 3, body: "c" }, { id: 2, body: "b" }, { id: 4, body: "d" }], 3)).map((m) => m.id),
    [2, 3, 4],
  );
});

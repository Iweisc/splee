import { readFileSync } from "node:fs";
import { test } from "node:test";
import assert from "node:assert/strict";
import vm from "node:vm";
import { parseHTML } from "linkedom";

const source = (file) => readFileSync(new URL(`../${file}`, import.meta.url), "utf8");
const fixture = (name) => readFileSync(new URL(`fixtures/${name}`, import.meta.url), "utf8");

/** Runs the readers the way the popup injects them, against a saved page opened at `href`. */
function readHtml(html, href) {
  const { document } = parseHTML(html);
  const context = vm.createContext({ URL });
  vm.runInContext(source("selectors.js"), context);
  vm.runInContext(source("readers.js"), context);
  return JSON.parse(JSON.stringify(context.spleeRead(document, href)));
}

const read = (name, href) => readHtml(fixture(name), href);

test("a signed-in LinkedIn profile", () => {
  assert.deepEqual(read("linkedin-profile.html", "https://www.linkedin.com/in/sam-lee-4b2a1/?miniProfileUrn=x"), {
    kind: "person",
    name: "Sam Lee",
    title: "Head of Growth",
    company: "Fieldnote",
    linkedinUrl: "https://www.linkedin.com/in/sam-lee-4b2a1/",
  });
});

test("a signed-out profile falls back to the public card", () => {
  assert.deepEqual(read("linkedin-profile-signed-out.html", "https://uk.linkedin.com/in/priya-nair"), {
    kind: "person",
    name: "Priya Nair",
    title: "Operations Director",
    company: "Brightpath",
    linkedinUrl: "https://www.linkedin.com/in/priya-nair/",
  });
});

test("a LinkedIn company page", () => {
  assert.deepEqual(read("linkedin-company.html", "https://www.linkedin.com/company/fieldnote/about/"), {
    kind: "company",
    name: "Fieldnote",
    domain: "fieldnote.example",
    size: "11-50",
    sourceUrl: "https://www.linkedin.com/company/fieldnote/",
  });
});

test("a company page without its About section reads the top card", () => {
  const html = fixture("linkedin-company.html").replace(/<section class="org-about-module">[\s\S]*?<\/section>/, "");
  const company = readHtml(html, "https://www.linkedin.com/company/fieldnote/");
  assert.equal(company.domain, "fieldnote.example");
  assert.equal(company.size, "11-50");
});

test("any other website is a company, with its addresses", () => {
  assert.deepEqual(read("company-site.html", "https://www.fieldnote.example/pricing"), {
    kind: "site",
    name: "Fieldnote",
    domain: "fieldnote.example",
    emails: ["hello@fieldnote.example", "press@fieldnote.example", "ana@fieldnote.example", "team@brightpath.example"],
    sourceUrl: "https://www.fieldnote.example/pricing",
  });
});

test("a site without og:site_name is named from its title", () => {
  const html = fixture("company-site.html").replace(/<meta property="og:site_name"[^>]*>/, "");
  assert.equal(readHtml(html, "https://fieldnote.example/").name, "Fieldnote");
});

test("other LinkedIn pages and browser pages aren't read", () => {
  assert.deepEqual(read("linkedin-profile.html", "https://www.linkedin.com/feed/"), { kind: "none" });
  assert.deepEqual(read("company-site.html", "chrome://extensions/"), { kind: "none" });
});

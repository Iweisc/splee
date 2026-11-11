// Firefox has promise-based `browser`, and Chrome and Edge return promises from `chrome` in Manifest V3.
const ext = globalThis.browser ?? globalThis.chrome;
const $ = (id) => document.getElementById(id);

let settings = {};
let projects = [];
let page = { kind: "none" };

function show(id) {
  for (const s of ["sign-in", "add", "loading"]) $(s).hidden = s !== id;
  $("sign-out").hidden = id !== "add";
}

function setText(id, text) {
  $(id).textContent = text;
  $(id).hidden = !text;
}

class ApiError extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
}

async function api(path, { method = "GET", body, url = settings.url, key = settings.key } = {}) {
  let res;
  try {
    res = await fetch(`${url}/api/v1/extension${path}`, {
      method,
      headers: { Authorization: `Bearer ${key}`, ...(body ? { "Content-Type": "application/json" } : {}) },
      body: body ? JSON.stringify(body) : undefined,
    });
  } catch {
    throw new ApiError(0, `Can't reach Splee at ${url}.`);
  }
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new ApiError(res.status, data.error || `Splee answered ${res.status}.`);
  return data;
}

/** The origin of whatever was pasted, so a link to any page of Splee works too. */
function origin(raw) {
  const value = raw.trim();
  try {
    return new URL(/^https?:\/\//i.test(value) ? value : `https://${value}`).origin;
  } catch {
    return "";
  }
}

async function signIn(e) {
  e.preventDefault();
  const url = origin($("url").value);
  const key = $("key").value.trim();
  if (!url) return setText("sign-in-error", "That doesn't look like a web address.");
  // Asked for in the click, since browsers only allow it then. The API answers extensions either way, so a refusal is fine.
  ext.permissions.request({ origins: [`${url}/*`] }).catch(() => false);
  $("sign-in-button").disabled = true;
  try {
    projects = await api("/projects", { url, key });
    settings = { url, key };
    await ext.storage.local.set(settings);
    setText("sign-in-error", "");
    await start();
  } catch (err) {
    setText("sign-in-error", err.status === 401 ? "That API key doesn't work. Check it and try again." : err.message);
  } finally {
    $("sign-in-button").disabled = false;
  }
}

async function signOut() {
  await ext.storage.local.clear();
  settings = {};
  $("key").value = "";
  show("sign-in");
}

/** The tab the popup was opened on. */
async function currentTab() {
  const [tab] = await ext.tabs.query({ active: true, currentWindow: true });
  return tab;
}

async function readPage() {
  try {
    const tab = await currentTab();
    const target = { tabId: tab.id };
    await ext.scripting.executeScript({ target, files: ["selectors.js", "readers.js"] });
    const [{ result }] = await ext.scripting.executeScript({ target, func: () => globalThis.spleeRead(document, location.href) });
    return result ?? { kind: "none" };
  } catch {
    // Browser pages, extension stores and PDFs can't be read.
    return { kind: "none" };
  }
}

function fillSelect(select, options, value) {
  select.replaceChildren(...options.map(([v, label]) => new Option(label, v, false, v === value)));
}

function pickers() {
  const project = projects.find((p) => p.id === settings.projectId) ?? projects[0];
  fillSelect($("project"), projects.map((p) => [p.id, p.name]), project?.id);
  const campaigns = project?.campaigns ?? [];
  const campaign = campaigns.some((c) => c.id === settings.campaignId) ? settings.campaignId : "";
  fillSelect($("campaign"), [["", "No campaign"], ...campaigns.map((c) => [c.id, c.name])], campaign);
}

async function remember() {
  settings.projectId = $("project").value;
  settings.campaignId = $("campaign").value;
  await ext.storage.local.set({ projectId: settings.projectId, campaignId: settings.campaignId });
}

const SOURCES = { person: () => "From this LinkedIn profile", company: () => "From this LinkedIn company page", site: () => `From ${page.domain}` };

function showPage() {
  const person = page.kind === "person";
  const company = page.kind === "company" || page.kind === "site";
  $("person").hidden = !person;
  $("company").hidden = !company;
  $("unreadable").hidden = person || company;
  $("submit").hidden = !(person || company);
  setText("source", person || company ? SOURCES[page.kind]() : "");
  if (person) {
    for (const [id, value] of [["full_name", page.name], ["title", page.title], ["company_name", page.company]]) $(id).value = value || "";
  }
  if (company) {
    $("name").value = page.name || "";
    $("domain").value = page.domain || "";
    $("size").value = page.size || "";
    $("size-row").hidden = page.kind !== "company";
    const emails = page.emails ?? [];
    $("emails-row").hidden = emails.length === 0;
    fillSelect($("company_email"), [["", "None, add the company"], ...emails.map((e) => [e, e])], emails[0] ?? "");
  }
  updateForm();
}

function email() {
  return (page.kind === "person" ? $("person_email").value : $("company_email").value).trim();
}

function updateForm() {
  $("find-email-row").hidden = email() !== "";
  $("submit").textContent = page.kind === "person" || email() ? "Add lead" : "Add company";
}

const humane = (s) => (s.charAt(0).toUpperCase() + s.slice(1)).replaceAll("_", " ");

function note(text, tone = "") {
  const p = document.createElement("p");
  p.className = `note ${tone}`;
  p.textContent = text;
  return p;
}

let checking = 0;

/** Says whether they're already a lead or on the do-not-contact list, before anyone presses Add. */
async function check() {
  const project = $("project").value;
  if (!project || !["person", "company", "site"].includes(page.kind)) return $("notes").replaceChildren();
  const person = page.kind === "person";
  const query = new URLSearchParams({
    project_id: project,
    email: email(),
    linkedin_url: person ? page.linkedinUrl : "",
    domain: person ? $("company_domain").value : $("domain").value,
  });
  const mine = ++checking;
  let known;
  try {
    known = await api(`/check?${query}`);
  } catch (err) {
    if (mine === checking) $("notes").replaceChildren(note(err.message, "error"));
    return;
  }
  if (mine !== checking) return;
  const notes = [];
  if (known.suppressed) notes.push(note(`${known.suppressed} is on the do-not-contact list, so Splee won't add them.`, "warn"));
  for (const lead of known.leads) {
    const here = lead.project_id === project;
    const where = here ? "this project" : lead.project_name;
    const campaign = lead.campaign ? `, in ${lead.campaign}` : "";
    notes.push(note(`${lead.full_name || lead.email} is already a lead in ${where}${campaign}, marked ${humane(lead.status)}.`, here ? "warn" : ""));
  }
  if (known.company && !person && !email()) notes.push(note(`${known.company.name || known.company.domain} is already in this project.`, "warn"));
  $("notes").replaceChildren(...notes);
}

let checkTimer;
function checkSoon() {
  clearTimeout(checkTimer);
  checkTimer = setTimeout(check, 400);
}

let added = null;

async function submit(e) {
  e.preventDefault();
  setText("error", "");
  $("done").hidden = true;
  $("submit").disabled = true;
  const base = { project_id: $("project").value, segment_id: $("campaign").value || null };
  try {
    if (page.kind === "person") {
      const res = await api("/leads", {
        method: "POST",
        body: {
          ...base,
          full_name: $("full_name").value,
          title: $("title").value,
          company_name: $("company_name").value,
          company_domain: $("company_domain").value,
          email: email(),
          linkedin_url: page.linkedinUrl,
          find_email: !email() && $("find_email").checked,
        },
      });
      added = "leads";
      const found = res.email_search === "queued" ? " Splee is looking for their email." : "";
      done(`Added ${$("full_name").value.trim() || "them"}.${found}`, res.note);
    } else if (email()) {
      const body = { ...base, email: email(), company_name: $("name").value, company_domain: $("domain").value };
      await api("/leads", { method: "POST", body });
      added = "leads";
      done(`Added ${email()}.`);
    } else {
      const body = { ...base, domain: $("domain").value, name: $("name").value, employees: $("size").value, source_url: page.sourceUrl };
      await api("/companies", { method: "POST", body });
      added = "companies";
      done(`Added ${$("name").value.trim() || $("domain").value.trim()}.`);
    }
  } catch (err) {
    if (err.status === 401) return expired();
    setText("error", err.message);
  } finally {
    $("submit").disabled = false;
  }
}

/** Swaps the button for the result, until something on the form changes. */
function done(text, extra) {
  $("done-text").textContent = extra ? `${text} ${extra}` : text;
  $("done").hidden = false;
  $("submit").hidden = true;
  $("notes").replaceChildren();
}

function edited() {
  if ($("done").hidden) return;
  $("done").hidden = true;
  $("submit").hidden = false;
  checkSoon();
}

async function expired() {
  await signOut();
  setText("sign-in-error", "That API key stopped working. Sign in with a new one.");
}

async function start() {
  show("loading");
  [projects, page] = await Promise.all([projects.length ? projects : api("/projects"), readPage()]);
  if (projects.length === 0) {
    show("add");
    $("add").replaceChildren(note("This organization has no projects yet. Create one in Splee first."));
    return;
  }
  pickers();
  showPage();
  show("add");
  check();
}

$("sign-in").addEventListener("submit", signIn);
$("sign-out").addEventListener("click", signOut);
$("add").addEventListener("submit", submit);
$("add").addEventListener("input", edited);
$("add").addEventListener("change", edited);
$("project").addEventListener("change", async () => {
  settings.projectId = $("project").value;
  pickers();
  await remember();
  check();
});
$("campaign").addEventListener("change", remember);
$("company_email").addEventListener("change", () => {
  updateForm();
  check();
});
for (const id of ["person_email", "company_domain", "domain"]) {
  $(id).addEventListener("input", () => {
    updateForm();
    checkSoon();
  });
}
$("open").addEventListener("click", () => {
  ext.tabs.create({ url: `${settings.url}/app/p/${$("project").value}/${added}` });
});

(async () => {
  settings = await ext.storage.local.get(["url", "key", "projectId", "campaignId"]);
  if (!settings.url || !settings.key) {
    show("sign-in");
    return;
  }
  try {
    await start();
  } catch (err) {
    if (err.status === 401) return expired();
    show("sign-in");
    $("url").value = settings.url;
    setText("sign-in-error", err.message);
  }
})();

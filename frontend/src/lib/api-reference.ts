// Every v1 endpoint with an example call and response. The API docs page shows them, and lib/api-spec.ts builds
// the OpenAPI spec and llms.txt from them, so an endpoint added here shows up in all three.

export interface Endpoint {
  method: "GET" | "POST" | "PUT" | "PATCH" | "DELETE";
  path: string;
  summary: string;
  params?: [name: string, description: string][];
  body?: string;
  /** A multipart form field, for an upload. */
  form?: string;
  response: string;
  /** What the response is when it isn't JSON. */
  responseType?: "text/csv" | "application/octet-stream";
}

const LEAD = `{
  "id": "c42e…",
  "project_id": "9b1f…",
  "segment_id": "5d0a…",
  "full_name": "Sarah Chen",
  "first_name": "Sarah",
  "last_name": "Chen",
  "title": "Founder",
  "email": "sarah@northwind.studio",
  "email_status": "found",
  "company_name": "Northwind Studio",
  "company_domain": "northwind.studio",
  "linkedin_url": "https://www.linkedin.com/in/sarah-chen",
  "x_url": "https://x.com/sarahchen",
  "city": "Portland",
  "country_code": "US",
  "language": "",
  "effective_language": "en",
  "effective_language_source": "country",
  "source": "agent",
  "status": "contacted",
  "notes": "",
  "last_contacted_at": "2026-11-03T09:14:00Z",
  "created_at": "2026-11-02T16:40:00Z",
  "score": 78,
  "score_fit": 43,
  "score_intent": 35,
  "score_reasons": [
    { "text": "Hiring 2 SDRs", "kind": "intent" },
    { "text": "Read /pricing yesterday", "kind": "intent" },
    { "text": "Uses a competitor: Outreach", "kind": "against" }
  ],
  "scored_at": "2026-11-11T08:20:00Z"
}`;

const indent = (json: string, spaces: number) => json.replaceAll("\n", `\n${" ".repeat(spaces)}`);

export function endpoints(projectId: string): Endpoint[] {
  return [
    {
      method: "GET",
      path: "/projects",
      summary: "Projects in your organization.",
      response: `[
  {
    "id": "${projectId}",
    "name": "Acme",
    "domain": "acme.com",
    "status": "active",
    "created_at": "2026-11-01T12:00:00Z"
  }
]`,
    },
    {
      method: "POST",
      path: "/projects",
      summary:
        "Start a project for a business, like New project in the app. Splee researches the business from its website or description, which takes a few minutes, so poll run_id with GET /runs/{id}.",
      params: [
        ["domain", "the business's website"],
        ["description", "what it sells and to whom, when there's no website"],
      ],
      body: `{ "domain": "acme.com" }`,
      response: `{ "project_id": "${projectId}", "run_id": "9b4f…" }`,
    },
    {
      method: "GET",
      path: `/projects/${projectId}`,
      summary: "One project as the list shows it, with what Splee learned about the business in company, and its controls as PATCH below returns them.",
      response: `{
  "id": "${projectId}",
  "name": "Acme",
  "domain": "acme.com",
  "status": "active",
  "company": { "name": "Acme", "…": "…" },
  "controls": { "sending": true, "daily_email_limit": 80, "sender_name": "Maya Patel", "…": "…" }
}`,
    },
    {
      method: "GET",
      path: `/projects/${projectId}/campaigns`,
      summary:
        "Campaigns of a project with their funnel. spend_micros is in micro-dollars (1,000,000 = $1). language is what the campaign writes in: lead for each lead's own, the default, or a language code like en or de. Archived campaigns are left out unless you ask for them.",
      params: [["archived", "true to list archived campaigns too. archived_at says when each was archived, null for the rest."]],
      response: `[
  {
    "id": "5d0a…",
    "name": "Design studios",
    "description": "Independent design studios in the US",
    "status": "working",
    "language": "lead",
    "companies": 40,
    "leads": 72,
    "sent": 150,
    "contacted": 70,
    "replies": 6,
    "bounced": 1,
    "hot": 2,
    "spend_micros": 4210000,
    "created_at": "2026-11-01T12:05:00Z",
    "archived_at": null
  }
]`,
    },
    {
      method: "POST",
      path: `/projects/${projectId}/campaigns`,
      summary:
        "Create a campaign from a description of who to reach, like describing one in the app. The agent names it and sets its targeting in a run you can follow with GET /runs below. language is optional and each lead's own by default.",
      body: `{ "prompt": "Dental clinics in Leeds", "language": "lead" }`,
      response: `{ "campaign_id": "5d0a…", "run_id": "9b4f…" }`,
    },
    {
      method: "PATCH",
      path: "/campaigns/5d0a…",
      summary:
        "Change a campaign's definition, as its page does. Send only what changes. Each targeting list you send replaces that list, and the others stay. Emails already written keep their brief and language until they're rewritten. A field it doesn't take, like schedule, is a 422. Returns the campaign like GET below.",
      params: [
        ["name", "can't be empty"],
        ["description", "who the campaign is for"],
        ["targeting", "industries, roles, company_sizes, countries, exclude_countries, keywords or exclude_keywords, each a list"],
        ["pain_points, angle", "the brief its emails are written from"],
        [
          "language",
          "the language of its emails, follow-ups, texts and LinkedIn messages: lead, the default, writes to each lead in the language set on them, else their country's, else their company website's, else English. Or one of en, de, fr, es, pt, it, nl, sv, nb, da, fi, pl, cs, sk, ro, bg, hu, el, tr, ru, uk, ar, he, ja, ko, zh, zh-Hant, hi, id, ms, th, vi",
        ],
        ["track_clicks, track_opens, lead_pages", "true or false"],
        ["daily_limit", "the most first emails and follow-ups it sends a day, 1 to 5,000, on top of the project's daily limit every campaign shares. null takes it off"],
        [
          "follow_ups",
          "its own follow-ups instead of the project's: up to 5 steps, each { kind: email or call, delay_days: 1 to 30 after the step before, enabled, instructions }. [] sends none, null goes back to the project's. An imported campaign sends the steps it came with, so it's a 400",
        ],
        [
          "send_hours",
          "its own days and hours for first emails and follow-ups instead of the project's: { days: 1 (Monday) to 7, start_hour: 0 to 23, end_hour: 1 to 24 }, in the project's time zone, or each lead's when the project sends in theirs. Days off stay the project's. null goes back to the project's",
        ],
        [
          "start_at",
          "a time within the next year when a campaign that isn't sending starts by itself, as POST /campaigns/{id}/start would, like 2026-12-01T09:00:00Z. null cancels it, and so do start and archive",
        ],
      ],
      body: `{ "angle": "Get paid on time without awkward emails", "targeting": { "roles": ["Founder", "Studio manager"] }, "daily_limit": 25 }`,
      response: `{
  "id": "5d0a…",
  "language": "de",
  "targeting": { "industries": ["Design"], "countries": ["US"], "roles": ["Founder", "Studio manager"] },
  "angle": "Get paid on time without awkward emails",
  "…": "…"
}`,
    },
    {
      method: "GET",
      path: "/campaigns/5d0a…",
      summary:
        "Everything the campaign's page shows: its funnel like the list above, why it has its status, who it targets, the brief its emails are written from, its schedule (its own days and hours when own_send_hours is true, else the project's), the follow-ups it sends (its own when own_follow_ups is true, else the project's), and an imported campaign's emails. read_only says what can't be changed and why.",
      response: `{
  "id": "5d0a…",
  "project_id": "${projectId}",
  "name": "Design studios",
  "status": "paused",
  "status_reason": "Paused. Its unsent emails wait as drafts until it starts again",
  "language": "lead",
  "targeting": { "industries": ["Design"], "countries": ["US"] },
  "pain_points": "Chasing late invoices",
  "angle": "Get paid on time without awkward emails",
  "daily_limit": 25,
  "start_at": null,
  "schedule": { "sending": true, "timezone": "America/New_York", "send_days": [1, 2, 3, 4, 5], "send_start_hour": 9, "send_end_hour": 17, "own_send_hours": false, "daily_email_limit": 50 },
  "follow_ups": [{ "delay_days": 3, "enabled": true, "instructions": "" }],
  "own_follow_ups": false,
  "imported_from": null,
  "sequence": [],
  "read_only": [{ "field": "schedule", "reason": "The time zone and daily limit are the project's, so change them with PATCH /v1/projects/{id}. Give the campaign its own days and hours with send_hours" }],
  "…": "…"
}`,
    },
    {
      method: "GET",
      path: "/campaigns/5d0a…/send-state",
      summary:
        "Whether the campaign's emails go out now and, if not, the one thing holding them, checked in the order you'd fix them. state is sending, scheduled, archived, not_started, paused, project_paused, no_mailbox, awaiting_approval, nothing_queued, monthly_cap_reached, lead_cap_reached, daily_limit_reached, campaign_limit_reached, mailboxes_full or outside_hours, and sending_now is true for the first two. next_action is the call that clears it, null when it only has to wait. today counts the campaign's first emails and follow-ups against its own campaign_limit, null when it has none, project_sent counts every email against the project's shared daily limit, and next_send_at says when the next one can go out. allowed_actions lists the calls below that change something now.",
      response: `{
  "campaign_id": "5d0a…",
  "state": "outside_hours",
  "sending_now": false,
  "message": "Outside sending hours. The next emails go out Mon Nov 23, 09:00 Europe/London",
  "next_action": null,
  "today": { "sent": 0, "queued": 24, "drafts": 0, "project_sent": 0, "daily_email_limit": 30, "campaign_limit": null, "next_send_at": "2026-11-23T09:00:00Z" },
  "allowed_actions": ["stop", "archive"]
}`,
    },
    {
      method: "POST",
      path: "/campaigns/5d0a…/start",
      summary:
        "The campaign's Start button: it sends on the project's schedule, and its drafts are approved for sending. A campaign that's already sending stays as it is, so a repeat is harmless.",
      response: `{ "accepted": true }`,
    },
    {
      method: "POST",
      path: "/campaigns/5d0a…/stop",
      summary:
        "The campaign's Pause button: its unsent first emails and follow-ups go back to drafts until it starts again. Replies still come in. A campaign that isn't sending stays as it is.",
      response: `{ "accepted": true }`,
    },
    {
      method: "POST",
      path: "/campaigns/5d0a…/approve",
      summary:
        "Splee has no review step for a whole campaign, so this approves each of its first emails and follow-ups waiting as drafts, like Approve on each one. A paused campaign holds them until it starts.",
      response: `{ "accepted": true, "approved": 12 }`,
    },
    {
      method: "POST",
      path: "/campaigns/5d0a…/archive",
      summary:
        "Put away a campaign you're done with. It stops like /stop and leaves the campaign list, autopilot and the review window. Its leads, emails and results stay, and replies still come in. A repeat changes nothing. Returns the campaign like the list above.",
      response: `{ "id": "5d0a…", "status": "paused", "archived_at": "2026-11-21T09:30:00Z", "…": "…" }`,
    },
    {
      method: "POST",
      path: "/campaigns/5d0a…/unarchive",
      summary: "Bring an archived campaign back to the list, paused. It sends again once you start it. A repeat changes nothing.",
      response: `{ "id": "5d0a…", "status": "paused", "archived_at": null, "…": "…" }`,
    },
    {
      method: "POST",
      path: "/campaigns/5d0a…/duplicate",
      summary:
        "Copy a campaign as a new draft named \"<name> (copy)\": its description, targeting, brief, language, tracking, lead pages, daily limit, follow-ups, send hours, its own first email, its A/B variants that aren't paused and an imported campaign's sequence. No leads, emails or results come with it, and it sends nothing until you start it. Returns the new campaign like the list above.",
      response: `{ "id": "8c41…", "name": "Design studios (copy)", "status": "draft", "leads": 0, "…": "…" }`,
    },
    {
      method: "GET",
      path: "/campaigns/5d0a…/my-copy",
      summary:
        "The campaign's first email. mode instructions has the agent write each one from the campaign's angle, returned as instructions. mode my_copy sends your own subject and body with each lead's details filled in to 9 leads in 10, and the AI email to the other tenth, so results can compare reply rates. Follow-ups are written as before. source project means the campaign sends the project's first email, and mode, subject and body are the project's.",
      response: `{
  "campaign_id": "5d0a…",
  "project_id": "${projectId}",
  "source": "campaign",
  "mode": "my_copy",
  "subject": "{Quick|Short} question, {{first_name}}",
  "body": "Hi {{first_name}},\\n\\n{{ai: one line about their clinic}}\\n\\nDoes {{company}} still book visits by phone?",
  "instructions": "Get paid on time without awkward emails",
  "results": { "my_copy": { "sent": 212, "replies": 19 }, "ai": { "sent": 24, "replies": 1 }, "note": "Not enough data yet. Each side needs at least 30 sends before the reply rates mean much." }
}`,
    },
    {
      method: "PATCH",
      path: "/campaigns/5d0a…/my-copy",
      summary:
        "Change the campaign's own first email, or turn My copy on or off. Send only what changes. A text with a mistake is a 422 with the sentence to fix, and nothing is saved. A campaign that sends the project's first email is a 409: change the project's below, or switch its source first.",
      params: [
        ["mode", "instructions or my_copy, which needs a body"],
        ["subject", "up to 300 characters. Empty sends “Your company x Their company”"],
        ["body", "up to 3,000 characters"],
        ["variables", "{{first_name}}, {{company}}, {{job_title}}, {{company_city}}, {{sender_first_name}}, {{sender_name}}, {{sender_company}}, each with an optional fallback like {{company_city|your city}}. A lead missing one with no fallback gets the AI email"],
        ["{a|b}", "one option picked per lead, the same one every time"],
        ["{{ai: instruction}}", "up to 5 lines in the body the agent writes for each lead"],
      ],
      body: `{ "mode": "my_copy", "body": "Hi {{first_name}},\\n\\nDoes {{company}} still book visits by phone?" }`,
      response: `{ "campaign_id": "5d0a…", "source": "campaign", "mode": "my_copy", "…": "…" }`,
    },
    {
      method: "PUT",
      path: "/campaigns/5d0a…/my-copy/source",
      summary:
        "project sends the project's first email, campaign the campaign's own. Nothing is lost: switching back brings the other text back. A campaign's first own text starts as a copy of the project's.",
      body: `{ "source": "campaign" }`,
      response: `{ "campaign_id": "5d0a…", "source": "campaign", "…": "…" }`,
    },
    {
      method: "GET",
      path: "/campaigns/5d0a…/my-copy/preview",
      summary:
        "Your text as the campaign's first 3 leads get it, signed like the real email. An {{ai: …}} line shows its instruction, since the agent writes it with the email. gets is ai for a lead that gets the AI email instead, with why in note.",
      response: `{
  "mode": "my_copy",
  "emails": [{
    "lead_id": "c42e…",
    "lead_name": "Sarah Chen",
    "lead_company": "Northwind Studio",
    "gets": "my_copy",
    "subject": "Quick question, Sarah",
    "body": "Hi Sarah,\\n\\n[Written for each lead: one line about their clinic]\\n\\nDoes Northwind Studio still book visits by phone?\\n\\nAlex Morgan",
    "sender_name": "Alex Morgan",
    "note": null
  }]
}`,
    },
    {
      method: "GET",
      path: `/projects/${projectId}/my-copy`,
      summary:
        "The project's default first email, sent by every campaign in campaign_ids. PATCH it with the same fields as a campaign's. instructions are the project's extra instructions for every AI email.",
      response: `{ "project_id": "${projectId}", "mode": "instructions", "subject": "", "body": "", "instructions": "Never mention pricing.", "campaign_ids": ["5d0a…"] }`,
    },
    {
      method: "PATCH",
      path: `/projects/${projectId}`,
      summary:
        "The project's sending controls. Send only what changes, or {} to read them. sending is the project's Start and Pause sending, and turning it on without an active mailbox is a 409 that saves nothing. Any other field is a 422.",
      params: [
        ["sending", "true or false"],
        ["daily_email_limit, daily_text_limit", "1 to 5,000 emails and 1 to 1,000 texts a day"],
        ["timezone, send_days, send_start_hour, send_end_hour", "the send window, days 1 (Monday) to 7 (Sunday)"],
        ["autopilot, autopilot_budget_micros, autopilot_lead_cap, autopilot_auto_send", "autopilot on or off, its monthly Claude budget and new-lead cap, and whether its emails skip approval"],
        ["review_window_hours", "1 to 72, or null so drafts wait for a person"],
        ["monthly_cap_micros", "$1 to $1,000,000 of email spend a month, or null for no cap"],
        ["sender_name", "the name the agent signs emails with, up to 100 characters and not an address, or empty for the team"],
      ],
      body: `{ "daily_email_limit": 80, "autopilot": true, "autopilot_budget_micros": 50000000 }`,
      response: `{
  "project_id": "${projectId}",
  "sending": true,
  "daily_email_limit": 80,
  "daily_text_limit": 50,
  "timezone": "America/New_York",
  "send_days": [1, 2, 3, 4, 5],
  "send_start_hour": 9,
  "send_end_hour": 17,
  "autopilot": true,
  "autopilot_budget_micros": 50000000,
  "autopilot_lead_cap": 500,
  "autopilot_auto_send": false,
  "review_window_hours": null,
  "monthly_cap_micros": null,
  "sender_name": "Maya Patel"
}`,
    },
    {
      method: "GET",
      path: `/projects/${projectId}/monthly-cap`,
      summary:
        "The monthly cap and what the project's sent emails cost since the 1st (UTC), at each mailbox's price per email. Once spend reaches the cap, first emails and follow-ups wait until resumes_at, the 1st of next month, or a higher cap. Replies to leads still go out.",
      response: `{
  "project_id": "${projectId}",
  "monthly_cap_micros": 200000000,
  "spent_this_month_micros": 200450000,
  "reached": true,
  "resumes_at": "2026-12-01T00:00:00Z"
}`,
    },
    {
      method: "GET",
      path: "/companies?q=dental&city=Leeds&country=GB",
      summary:
        "Search the company database, the most reliable listings first. q matches words at the start of words in the name or category. Companies on your dedup lists are left out. total stops counting at 10,000, and capped says there are more.",
      params: [
        ["q", "optional words in the name or category"],
        ["country", "optional two-letter code, like GB"],
        ["city", "optional city, any case"],
        ["category", "optional exact category, like dental_clinic"],
        ["has_email, has_phone", "optional true to keep only companies that list one"],
        ["tech", "optional tool, like HubSpot, or tool category, like crm"],
        ["project_id", "optional project searching, so its dedup lists apply as well as the ones for every project"],
        ["include_hidden", "optional true to include the companies on your dedup lists"],
        ["page, per_page", "1-based page number, and 1 to 200 per page, default 50"],
      ],
      response: `{
  "items": [
    {
      "domain": "smiledental.co.uk",
      "name": "Smile Dental",
      "website": "https://smiledental.co.uk",
      "phone": "+44 113 000 0000",
      "email": "hello@smiledental.co.uk",
      "city": "Leeds",
      "country_code": "GB",
      "category": "dental_clinic",
      "…": "…"
    }
  ],
  "total": 125,
  "capped": false
}`,
    },
    {
      method: "GET",
      path: "/companies/export?q=dental&country=GB&format=csv",
      responseType: "text/csv",
      summary:
        "A company search as a file to take elsewhere, like the Companies page's Download CSV, up to 50,000 rows a page. It takes the same filters as GET /companies. The splee-rows header says how many rows the page holds, and splee-capped is true when there are more on the next page. An organization can download 20 times an hour, 3 at once.",
      params: [
        ["q, country, city, category, has_email, has_phone, tech, project_id, include_hidden", "the same filters as GET /companies"],
        ["domains", "optional comma-separated domains, only these of the search's companies"],
        ["format", "optional csv or jsonl, default csv"],
        ["page, per_page", "1-based page number, and up to 50,000 rows per page, the default"],
      ],
      response: `name,domain,website,email,phone,address,city,region,postcode,country,category,confidence,source_id,source_release
Smile Dental,smiledental.co.uk,https://smiledental.co.uk,hello@smiledental.co.uk,+44 113 496 0000,12 Park Row,Leeds,England,LS1 5HD,GB,dentist,0.94,…,…`,
    },
    {
      method: "POST",
      path: "/dedup/companies",
      summary:
        "Add up to 10,000 domains or website addresses to a named dedup list, like your CRM's accounts or a competitor's clients. The list is made when your organization has none by that name. The company search, its downloads, saved searches and lookalikes leave those companies out in the projects it covers. It only hides search results and never stops an email, unlike do-not-contact. project_ids sets those projects, null or left out on a new list means all of them. skipped counts invalid domains and ones already on the list. 422 for an empty project_ids or a project outside your organization.",
      body: `{ "name": "CRM accounts", "domains": ["northwind.studio", "https://www.globex.com/"], "project_ids": ["${projectId}"] }`,
      response: `{
  "list": { "id": "6e1b…", "name": "CRM accounts", "project_ids": ["${projectId}"], "created_by": null, "created_at": "2026-11-21T09:00:00Z", "size": 2 },
  "added": 2,
  "skipped": 0
}`,
    },
    {
      method: "GET",
      path: "/dedup/companies",
      summary: "Your organization's dedup lists, newest first, with how many domains each has. project_ids is null for a list that covers every project.",
      response: `[{ "id": "6e1b…", "name": "CRM accounts", "project_ids": ["${projectId}"], "created_by": null, "created_at": "2026-11-21T09:00:00Z", "size": 2 }]`,
    },
    {
      method: "GET",
      path: "/dedup/companies/6e1b…?limit=100&offset=0",
      summary: "A dedup list with its domains in alphabetical order.",
      params: [
        ["limit", "1 to 1,000, default 100"],
        ["offset", "how many to skip, default 0"],
      ],
      response: `{ "id": "6e1b…", "name": "CRM accounts", "project_ids": ["${projectId}"], "created_by": null, "created_at": "2026-11-21T09:00:00Z", "size": 2, "domains": ["globex.com", "northwind.studio"] }`,
    },
    {
      method: "PATCH",
      path: "/dedup/companies/6e1b…",
      summary: "Change the projects a dedup list covers. Send null for all projects.",
      body: `{ "project_ids": null }`,
      response: `{ "id": "6e1b…", "name": "CRM accounts", "project_ids": null, "created_by": null, "created_at": "2026-11-21T09:00:00Z", "size": 2 }`,
    },
    {
      method: "DELETE",
      path: "/dedup/companies/6e1b…",
      summary: "Delete a dedup list and its domains. Those companies show up in searches again right away.",
      response: `{ "ok": true }`,
    },
    {
      method: "POST",
      path: "/companies/filters",
      summary:
        "Turn a description into the search's filters, to pass to GET /companies as they are. Splee only uses categories and countries the database has and tools it can detect, and note says what it couldn't turn into a filter, like company size. 30 an hour per organization, then 429 with Retry-After.",
      body: `{ "query": "Dental clinics in Leeds that list an email and use Shopify, 10 to 50 staff" }`,
      response: `{
  "filters": {
    "q": null,
    "country": "GB",
    "city": "Leeds",
    "category": "dental_clinic",
    "has_email": true,
    "has_phone": false,
    "tech": "Shopify"
  },
  "note": "Company size isn't a filter."
}`,
    },
    {
      method: "POST",
      path: "/companies/by-domains",
      summary:
        "Look up to 1,000 domains in the company database, like your CRM's accounts. Addresses are cleaned up first, so https://www.globex.com/about finds globex.com. items keeps the order you sent, and not_found has the rest, including companies your dedup lists hide unless include_hidden is true.",
      body: `{ "domains": ["https://www.smiledental.co.uk/", "nowhere.example"], "project_id": "${projectId}" }`,
      response: `{
  "items": [
    {
      "domain": "smiledental.co.uk",
      "name": "Smile Dental",
      "website": "https://smiledental.co.uk",
      "phone": "+44 113 496 0000",
      "email": "hello@smiledental.co.uk",
      "address": "12 Park Row",
      "city": "Leeds",
      "region": "West Yorkshire",
      "postcode": "LS1 5HD",
      "country_code": "GB",
      "category": "dental_clinic",
      "tech": ["Google Workspace"]
    }
  ],
  "not_found": ["nowhere.example"]
}`,
    },
    {
      method: "GET",
      path: `/projects/${projectId}/lookalikes`,
      summary:
        "Companies in the company database like the project's customers, up to 200, best first, like the Companies page's Like your customers. The seeds are the customers from won deals plus the ones added on that page, and profile shows what they have in common. score is 0 to 100, and reasons says what earned it the most points. database_empty is true until the company database has companies.",
      params: [["seeds", "optional comma-separated domains to match instead of the saved seeds"]],
      response: `{
  "profile": {
    "seeds": [
      { "domain": "brightsmile.co.uk", "name": "Bright Smile", "won": true, "in_database": true, "known": true },
      { "domain": "parkdental.co.uk", "name": "Park Dental", "won": true, "in_database": true, "known": true },
      { "domain": "leedsortho.co.uk", "name": "Leeds Orthodontics", "won": true, "in_database": true, "known": true },
      { "domain": "corephysio.co.uk", "name": "Core Physio", "won": false, "in_database": true, "known": true }
    ],
    "parts": [
      { "key": "category", "label": "Industry", "on": true, "known": 4, "values": [{ "value": "dentist", "label": "Dentist", "seeds": 3 }] },
      { "key": "tech", "label": "Tools they use", "on": false, "known": 0, "values": [] }
    ]
  },
  "results": [
    {
      "domain": "smiledental.co.uk",
      "name": "Smile Dental",
      "website": "https://smiledental.co.uk",
      "city": "Leeds",
      "region": "England",
      "country_code": "GB",
      "category": "dentist",
      "score": 72,
      "reasons": ["Dentist, like 3 of your 4 customers"]
    }
  ],
  "database_empty": false,
  "settings": { "added": [], "removed": [], "off": [] }
}`,
    },
    {
      method: "POST",
      path: `/projects/${projectId}/lookalikes`,
      summary:
        "Add matches to a campaign, each with its score and reasons as its fit. Name the domains to add, or pass top for the best few. A domain that's no longer among the matches, like one already in the project, is skipped.",
      params: [
        ["segment_id", "the campaign"],
        ["domains", "optional domains from the matches"],
        ["top", "optional, the best this many when domains is left out, up to 200"],
        ["seeds", "optional domains to match instead of the saved seeds"],
      ],
      body: `{ "segment_id": "5d0a…", "top": 25 }`,
      response: `{ "added": 24, "skipped": 1 }`,
    },
    {
      method: "GET",
      path: "/sending-domains/suggestions?domain=acme.com",
      summary:
        "Domains to send cold email from apart from your own, like getacme.com or acmehq.co, with whether your Porkbun account can buy each and its first-year price, buyable first. available is null when the registry didn't answer in time. Connect Porkbun on the Domains page first, and buy them there.",
      response: `[
  { "domain": "getacme.com", "available": true, "price_micros": 11080000, "renewal_micros": 11080000, "premium": false },
  { "domain": "acmehq.com", "available": false, "price_micros": 11080000, "renewal_micros": 11080000, "premium": false }
]`,
    },
    {
      method: "GET",
      path: "/sending-domains/availability?domains=getacme.com,tryacme.io",
      summary: "The same check for up to 20 names of your own, separated by commas.",
      response: `[{ "domain": "getacme.com", "available": true, "price_micros": 11080000, "renewal_micros": 11080000, "premium": false }]`,
    },
    {
      method: "GET",
      path: "/sending-domains",
      summary: "The sending domains you bought on the Domains page, with their status, mail provider, DNS and how many mailboxes use each.",
      response: `[
  {
    "id": "a7c1…",
    "domain": "getacme.com",
    "status": "registered",
    "provider": "google",
    "dns_status": "set",
    "mailboxes": 2,
    "warming": 1,
    "…": "…"
  }
]`,
    },
    {
      method: "POST",
      path: "/segments/5d0a…/companies",
      summary:
        "Add companies from the search above to a campaign by domain, up to 500 at a time. Companies the project already has and ones on your do-not-contact list are skipped.",
      body: `{ "domains": ["smiledental.co.uk", "brightsmiles.co.uk"] }`,
      response: `{ "added": 2, "skipped": 0 }`,
    },
    {
      method: "POST",
      path: `/projects/${projectId}/saved-searches`,
      summary:
        "Save the search's filters under a name. Splee checks it every hour for companies that match and didn't when it was saved, like ones from a country loaded later. A search saved with a key has no owner, so only the project's chat hears about new ones, with chat_alerts.",
      params: [
        ["name", "up to 120 characters"],
        ["q, country, city, category, has_email, has_phone, tech", "the search's filters, at least one"],
        ["chat_alerts", "optional true to post new companies to the project's chat"],
        ["segment_id", "optional campaign to add up to 200 new companies to each hour, skipping known and do-not-contact ones"],
      ],
      body: `{ "name": "Dentists in Leeds", "category": "dental_clinic", "city": "Leeds", "chat_alerts": true }`,
      response: `{
  "id": "9b4e…",
  "name": "Dentists in Leeds",
  "category": "dental_clinic",
  "city": "Leeds",
  "chat_alerts": true,
  "add_to_segment_id": null,
  "too_broad": false,
  "new_matches": 0,
  "…": "…"
}`,
    },
    {
      method: "GET",
      path: `/projects/${projectId}/saved-searches`,
      summary: "The project's saved searches, newest first. too_broad means it matched over 10,000 companies, so it isn't checked.",
      response: `[{ "id": "9b4e…", "name": "Dentists in Leeds", "new_matches": 3, "new_since": "2026-11-21T09:00:00Z", "…": "…" }]`,
    },
    {
      method: "GET",
      path: "/saved-searches/9b4e…/new?since=2026-11-21T09:00:00Z",
      summary: "The companies a saved search found at or after since, newest first. Without since, the ones found since it was last opened in the app.",
      response: `{
  "since": "2026-11-21T09:00:00Z",
  "items": [{ "domain": "brightsmiles.co.uk", "name": "Bright Smiles", "found_at": "2026-11-21T09:00:00Z", "…": "…" }],
  "total": 3
}`,
    },
    {
      method: "DELETE",
      path: "/saved-searches/9b4e…",
      summary: "Delete a saved search. Splee stops checking it, and companies it added stay in their campaign.",
      response: `{ "ok": true }`,
    },
    {
      method: "POST",
      path: `/projects/${projectId}/enrich/email`,
      summary:
        "Find a person's work email from their name and company with the project's email finders and verifier, the way Splee does for a lead without one. They don't become a lead. Each call is one of your organization's 200 email searches an hour, shared with the leads Splee searches, and past that it's a 429 with Retry-After. A project with no finder or verifier connected gets a 409.",
      params: [
        ["first_name, company_domain", "required, the domain or the company's website"],
        ["last_name, linkedin_url", "optional, and some finders match on the profile"],
      ],
      body: `{ "first_name": "Sarah", "last_name": "Chen", "company_domain": "northwind.studio" }`,
      response: `{
  "status": "found",
  "email": "sarah@northwind.studio",
  "email_status": "verified",
  "source": "Hunter",
  "note": "Found by Hunter.",
  "phone": null
}`,
    },
    {
      method: "POST",
      path: `/projects/${projectId}/enrich/email/batch`,
      summary:
        "The same search for up to 500 people, in the background and within the hourly limit, so a big batch takes a few hours. status is found, guessed (a common pattern the verifier accepted), catch_all, not_found, suppressed, duplicate (a lead in the project has the address, named in note) or failed. phone is filled only when the project finds phone numbers too.",
      params: [["people", "first_name, company_domain, and optional last_name, linkedin_url and client_ref, your own id given back with the result"]],
      body: `{ "people": [{ "first_name": "Sarah", "last_name": "Chen", "company_domain": "northwind.studio", "client_ref": "crm-118" }] }`,
      response: `{ "task_id": "e7c2…", "status": "pending", "total": 1 }`,
    },
    {
      method: "GET",
      path: "/enrich/email/batch/e7c2…",
      summary: "A batch's results in the order you sent them, each pending until searched. Poll until status is completed. Batches are kept for 30 days.",
      response: `{
  "task_id": "e7c2…",
  "project_id": "${projectId}",
  "status": "completed",
  "total": 1,
  "searched": 1,
  "created_at": "2026-11-21T09:00:00Z",
  "finished_at": "2026-11-21T09:01:00Z",
  "results": [
    {
      "client_ref": "crm-118",
      "first_name": "Sarah",
      "last_name": "Chen",
      "company_domain": "northwind.studio",
      "linkedin_url": "",
      "status": "guessed",
      "email": "sarah.chen@northwind.studio",
      "email_status": "verified",
      "source": "first.last",
      "note": "Guessed from the first.last@ pattern, verified by MillionVerifier.",
      "phone": null
    }
  ]
}`,
    },
    {
      method: "POST",
      path: `/projects/${projectId}/enrich/phone`,
      summary:
        "Find the mobile of the person behind a LinkedIn profile with the project's finders that return mobiles, People Data Labs, RocketReach, Lusha and Prospeo, in the project's order. They don't become a lead. status is found, not_found, suppressed (on the do-not-contact list) or failed, and note says why when nothing was found. Each call is one of your 200 email searches an hour, and a project with none of those finders gets a 409.",
      params: [
        ["linkedin_url", "required, their profile"],
        ["email", "optional, an address they're known by, which helps the finders match"],
      ],
      body: `{ "linkedin_url": "https://www.linkedin.com/in/sarah-chen" }`,
      response: `{ "status": "found", "phone": "+14155550134", "source": "People Data Labs", "note": "" }`,
    },
    {
      method: "GET",
      path: `/local-businesses?what=dentists&where=Austin,%20TX&min_rating=4&project_id=${projectId}`,
      summary:
        "Search Google Maps for local businesses through your organization's Google Maps key, up to 60 a place. Split a big area over up to 20 places with semicolons. A place searched in the last 30 days costs no Google calls, and the rest count against your monthly cap.",
      params: [
        ["what, where", "what to look for, and a city, area or ZIP code, or several split by semicolons"],
        ["min_rating, min_reviews", "optional, only businesses rated or reviewed at least this much"],
        ["with_website", "optional true to keep only businesses with a website"],
        ["project_id", "optional, marks the businesses that project already has in known"],
      ],
      response: `{
  "items": [
    {
      "place_id": "ChIJ…",
      "name": "Smile Dental",
      "category": "Dentist",
      "address": "100 Congress Ave, Austin, TX 78701, USA",
      "phone": "+15125550100",
      "website": "https://www.smiledental.example/",
      "domain": "smiledental.example",
      "rating": 4.8,
      "review_count": 212,
      "known": null,
      "…": "…"
    }
  ],
  "found": 60,
  "calls": 3,
  "usage": { "calls": 312, "cap": 2000, "cost_cents": 1092, "…": "…" },
  "note": null
}`,
    },
    {
      method: "POST",
      path: "/segments/5d0a…/local-businesses",
      summary:
        "Add businesses from the search above to a campaign, up to 50 at a time. Splee reads each one's website for a contact address at its own domain, a person's before a shared one, and checks it with your verifier. One without a website keeps its phone for calls and texts.",
      body: `{ "place_ids": ["ChIJ…"] }`,
      response: `{
  "added": [
    { "name": "Smile Dental", "domain": "smiledental.example", "email": "maria.lopez@smiledental.example", "email_kind": "person", "contact_name": "Maria Lopez", "…": "…" }
  ],
  "skipped": [{ "name": "Known Dental", "reason": "Already in this project" }]
}`,
    },
    {
      method: "POST",
      path: `/projects/${projectId}/research/questions`,
      summary:
        "Add a question the agent answers about every company, like the Companies page's Research view. A project has up to 10. label is the column's name, the question's first words when you leave it out. With use_in_emails, the writer uses the answers.",
      params: [
        ["question", "up to 300 characters"],
        ["label", "optional, up to 40 characters"],
        ["answer_type", "optional yes_no or text, default yes_no"],
        ["use_in_emails", "optional, default false"],
      ],
      body: `{ "question": "Do they sell to hospitals?", "label": "Sells to hospitals" }`,
      response: `{
  "id": "2c9c…",
  "project_id": "${projectId}",
  "label": "Sells to hospitals",
  "question": "Do they sell to hospitals?",
  "answer_type": "yes_no",
  "use_in_emails": false,
  "position": 0,
  "created_at": "2026-11-04T10:00:00Z"
}`,
    },
    {
      method: "POST",
      path: `/projects/${projectId}/research/run`,
      summary:
        "Have the agent answer the project's questions for its companies, up to 200 per call. A company is only asked what it has no answer to yet, unless redo is true. more counts the companies past the 200, for another call. dry_run counts what it would queue without queueing it, and cost_per_company_micros is what one company cost in this project's research so far.",
      params: [
        ["question_ids", "optional, every question without it"],
        ["company_ids", "optional, every company that isn't excluded without it"],
        ["segment_id", "optional, only this campaign's companies when company_ids is left out"],
        ["redo", "optional, ask again where there's an answer"],
        ["dry_run", "optional, count without queueing"],
      ],
      body: `{ "dry_run": true }`,
      response: `{ "companies": 120, "answers": 120, "more": 0, "runs": 0, "cost_per_company_micros": 21000 }`,
    },
    {
      method: "GET",
      path: `/projects/${projectId}/research?question_id=2c9c…&answer=yes`,
      summary:
        "The project's questions and its companies' answers, best score first. answers is keyed by question id, and status is queued, answered, unknown or failed. researching counts the answers being worked on across the project.",
      params: [
        ["segment_id", "optional, one campaign's companies"],
        ["q", "optional text in the company's name or domain"],
        ["question_id, answer", "optional, only companies whose answer to that question is yes, no, answered (any), unknown, or none yet"],
        ["page, per_page", "1-based page number, and 1 to 200 per page, default 50"],
      ],
      response: `{
  "questions": [{ "id": "2c9c…", "label": "Sells to hospitals", "question": "Do they sell to hospitals?", "answer_type": "yes_no", "…": "…" }],
  "items": [
    {
      "id": "e810…",
      "domain": "northwind.studio",
      "name": "Northwind Studio",
      "score": 82,
      "score_fit": 50,
      "score_intent": 32,
      "status": "new",
      "segment_name": "Design studios",
      "answers": {
        "2c9c…": {
          "question_id": "2c9c…",
          "domain": "northwind.studio",
          "status": "answered",
          "answer": "yes",
          "evidence": "Their case studies page lists three NHS trusts.",
          "source_url": "https://northwind.studio/work",
          "updated_at": "2026-11-04T10:20:00Z"
        }
      }
    }
  ],
  "total": 1,
  "researching": 0
}`,
    },
    {
      method: "GET",
      path: `/projects/${projectId}/signals?kind=funding_filing&days=30`,
      summary:
        "Funding filings and company news at the project's companies, newest first, like the Companies page's News tab, up to 1,000. source is sec for SEC filings or feed for news. amount is what a filing says was raised and offered what it set out to raise, in dollars. leads counts the project's leads at the company.",
      params: [
        ["kind", "optional funding_filing, funding_post, launch, acquisition, expansion or leadership"],
        ["days", "optional, only signals from the last this many days"],
      ],
      response: `[
  {
    "id": "7f20…",
    "domain": "northwind.studio",
    "company_id": "e810…",
    "company_name": "Northwind Studio",
    "leads": 2,
    "kind": "funding_filing",
    "source": "sec",
    "happened_on": "2026-11-02",
    "title": "Northwind Studio Inc. Form D",
    "amount": 4200000,
    "offered": 5000000,
    "industry": "Other Technology",
    "url": "https://www.sec.gov/Archives/edgar/data/…",
    "first_seen": "2026-11-03T06:00:00Z",
    "checked_at": "2026-11-03T06:00:00Z",
    "headline": "Raised $4.2M (SEC filing, Nov 2)"
  }
]`,
    },
    {
      method: "POST",
      path: `/projects/${projectId}/watch-list`,
      summary:
        "Add past customers or champions to the project's watch list, like the Job changes tab. Splee checks their LinkedIn and, when one moves to a new company, makes them a lead there. Each needs an email or a LinkedIn link. Someone already watched gets what was missing filled in and counts as updated.",
      params: [
        ["relationship", "optional won (they bought), booked (a meeting) or hot (replied with interest), default won"],
        ["people", "a list of name, email, linkedin_url, company, company_domain and title"],
        ["csv", "or a CSV with a header row, its columns in any order"],
      ],
      body: `{ "relationship": "won", "people": [{ "name": "Dana Ruiz", "linkedin_url": "https://www.linkedin.com/in/dana-ruiz", "company": "Northwind Studio" }, { "name": "Sam Lee" }] }`,
      response: `{ "added": 1, "updated": 0, "skipped": 0, "errors": [{ "row": 2, "error": "Needs an email or a LinkedIn link" }] }`,
    },
    {
      method: "GET",
      path: `/projects/${projectId}/job-changes`,
      summary:
        "Moves seen among the people the project watches, newest first and up to 500, with the lead Splee made at each new company. connected is whether the project has LinkedIn through Unipile, which the checks need, and watching counts the people on the list.",
      response: `{
  "connected": true,
  "segment_id": "5d0a…",
  "watching": 140,
  "last_checked_at": "2026-11-04T06:00:00Z",
  "changes": [
    {
      "id": "b31d…",
      "full_name": "Dana Ruiz",
      "linkedin_url": "https://www.linkedin.com/in/dana-ruiz",
      "relationship": "won",
      "old_company": "Northwind Studio",
      "old_domain": "northwind.studio",
      "old_lead_id": "c42e…",
      "new_company": "Harbor Health",
      "new_domain": "harborhealth.com",
      "new_title": "VP Operations",
      "new_lead_id": "d09a…",
      "new_lead_email": "dana@harborhealth.com",
      "segment_id": "5d0a…",
      "segment_name": "Design studios",
      "seen_at": "2026-11-04T06:00:00Z"
    }
  ]
}`,
    },
    {
      method: "GET",
      path: `/projects/${projectId}/companies?sort=score&min_score=50`,
      summary:
        "The project's own companies, best research fit first, with their hiring roles and tools. A company's score is its best lead's, or its own while it has no leads.",
      params: [
        ["segment_id", "optional campaign id"],
        ["q", "optional text in the name or domain"],
        ["min_score", "optional, only scores of at least this, 0 to 100"],
        ["sort", "optional, score for the highest first, or newest"],
        ["page", "1-based page number"],
        ["per_page", "1 to 200, default 50"],
      ],
      response: `{
  "items": [
    {
      "id": "e810…",
      "domain": "northwind.studio",
      "name": "Northwind Studio",
      "employees": "11-50",
      "country_code": "US",
      "status": "people_found",
      "score": 78,
      "score_fit": 43,
      "score_intent": 35,
      "score_reasons": [{ "text": "Hiring 2 SDRs", "kind": "intent" }],
      "hiring": ["SDR"],
      "tech": ["HubSpot", "Stripe"]
    }
  ],
  "total": 1
}`,
    },
    {
      method: "POST",
      path: "/segments/5d0a…/find-people",
      summary:
        "Have the agent find decision makers and their emails at the campaign's new companies, like Find people in the app, or at the companies you pick by id or domain. Domains the project hasn't got are added to the campaign first, from the company database where they're in it. Excluded companies and ones on your do-not-contact list are left out.",
      params: [
        ["company_ids", "optional companies of the project to search, or leave both out for the campaign's new ones"],
        ["domains", "optional, in place of company_ids: up to 100 company domains"],
        ["titles", "optional, up to 20 job titles to look for in place of the campaign's roles, like Head of Sales"],
        ["per_company", "optional people to find at each company, 1 to 5, 2 by default"],
      ],
      body: `{ "domains": ["smiledental.co.uk"], "titles": ["Practice Manager"], "per_company": 1 }`,
      response: `{ "run_id": "9b4f…" }`,
    },
    {
      method: "GET",
      path: `/projects/${projectId}/leads?status=hot&page=1`,
      summary:
        "Leads of a project, newest first, with their campaign and the status of the last email to them. score is 0-100: score_fit (0-50) is how well the company matches your targeting and score_intent (0-50) what it did lately, with the reasons strongest first.",
      params: [
        ["status", "optional lead status: new, contacted, replied, hot, meeting_booked, …"],
        ["segment_id", "optional campaign id"],
        ["q", "optional text in the name, email, company or title"],
        ["min_score", "optional, only scores of at least this, 0 to 100"],
        ["language", "optional language code, matched against effective_language"],
        ["sort", "optional, score for the highest first, or newest"],
        ["page", "1-based page number"],
        ["per_page", "1 to 200, default 50"],
      ],
      response: `{
  "items": [
    ${indent(LEAD, 4)}
  ],
  "total": 1
}`,
    },
    {
      method: "POST",
      path: `/projects/${projectId}/leads`,
      summary:
        "Add up to 1,000 leads per request. Suppressed, duplicate and invalid emails are skipped, and errors lists the first 200 of them with their zero-based index in leads.",
      body: `{
  "leads": [
    {
      "email": "sarah@northwind.studio",
      "full_name": "Sarah Chen",
      "title": "Founder",
      "company_name": "Northwind Studio",
      "company_domain": "northwind.studio",
      "linkedin_url": "https://www.linkedin.com/in/…",
      "language": "de",
      "segment_id": "5d0a…"
    },
    {
      "email": "sarah@northwind.studio",
      "full_name": "Sarah Chen"
    }
  ]
}`,
      response: `{
  "imported": 1,
  "skipped": 1,
  "errors": [{ "row": 1, "error": "Duplicate: sarah@northwind.studio" }]
}`,
    },
    {
      method: "POST",
      path: `/projects/${projectId}/leads/upsert`,
      summary:
        "Add one lead, or update the project's lead with that email. Fields you leave out or send empty keep what the lead has, and segment_id moves it into that campaign. created says which happened. 400 for a missing or invalid email, one on your do-not-contact list, or a campaign of another project.",
      body: `{ "email": "sarah@northwind.studio", "title": "CEO", "segment_id": "5d0a…" }`,
      response: `{ "created": false, "id": "c42e…", "email": "sarah@northwind.studio", "title": "CEO", "…": "…" }`,
    },
    {
      method: "POST",
      path: "/campaigns/import",
      summary:
        "Start a new campaign from your own list of up to 30,000 leads. It returns at once and imports in the background. Each lead needs email, first_name, last_name, company_domain and job_title, and one without them is skipped, not refused. Repeats in the list, addresses or domains on your do-not-contact list and people already in the project are deduped, so the campaign holds exactly the rest. instructions becomes the campaign's outreach angle for its first emails, and language works like it does on a campaign. Follow-ups are set per project, so followup_instructions is a 422. The campaign starts as a draft: write its emails with write-emails below, then start it. Send an Idempotency-Key to retry safely.",
      body: `{
  "project_id": "${projectId}",
  "name": "Design studios, March list",
  "leads": [
    {
      "email": "sarah@northwind.studio",
      "first_name": "Sarah",
      "last_name": "Chen",
      "company_domain": "northwind.studio",
      "job_title": "Founder",
      "linkedin_url": "https://www.linkedin.com/in/…",
      "client_ref": "crm-1042"
    }
  ],
  "instructions": "Mention our case study with Figma",
  "language": "en"
}`,
      response: `{ "task_id": "71c3…", "campaign_id": "5d0a…", "status": "pending" }`,
    },
    {
      method: "GET",
      path: "/campaigns/import/71c3…",
      summary:
        "An import's progress. status is pending, then completed with result, or failed with error. Tasks are kept for 30 days.",
      response: `{
  "task_id": "71c3…",
  "status": "completed",
  "project_id": "${projectId}",
  "campaign_id": "5d0a…",
  "result": { "campaign_id": "5d0a…", "imported": 1, "skipped": 0, "deduped": 0 },
  "error": null,
  "created_at": "2026-03-02T09:14:00Z",
  "completed_at": "2026-03-02T09:14:02Z"
}`,
    },
    {
      method: "GET",
      path: "/campaigns/import/71c3…/rows?page=1",
      summary:
        "What became of each lead you sent, in order, with row its zero-based index. imported has person_id, the new lead's id. skipped has reason missing_email, invalid_email, missing_first_name, missing_last_name, missing_company_domain, invalid_company_domain or missing_job_title. deduped has duplicate_in_request, do_not_contact, already_contacted or already_in_project.",
      params: [
        ["page", "1-based page number"],
        ["per_page", "1 to 200, default 50"],
      ],
      response: `{
  "items": [
    {
      "row": 0,
      "email": "sarah@northwind.studio",
      "client_ref": "crm-1042",
      "status": "imported",
      "reason": null,
      "person_id": "c42e…"
    }
  ],
  "total": 1
}`,
    },
    {
      method: "GET",
      path: `/projects/${projectId}/sheets`,
      summary:
        "The project's Google Sheets tabs, imports first: the ones leads come in from and the ones lead events go to, with how each last went. mapping is an import's {field: column header}, and events the lead events an export writes.",
      response: `[
  {
    "id": "a7c1…",
    "project_id": "${projectId}",
    "kind": "import",
    "spreadsheet_id": "1Bx9…",
    "spreadsheet_name": "Event leads",
    "tab": "October",
    "mapping": { "email": "Email", "full_name": "Name", "company_name": "Company" },
    "segment_id": "5d0a…",
    "keep_importing": true,
    "next_import_at": "2026-11-04T11:00:00Z",
    "events": [],
    "last_sync_at": "2026-11-04T10:00:00Z",
    "rows_added": 212,
    "error": null,
    "failed_at": null,
    "created_at": "2026-10-20T09:00:00Z"
  }
]`,
    },
    {
      method: "POST",
      path: `/projects/${projectId}/sheets/a7c1…/import`,
      summary:
        "Read an import tab now and add its new rows as leads, like Import now on the Integrations page. import is null when the tab had no new rows. 400 for a tab that gets lead events.",
      response: `{
  "import": { "id": "f2e4…", "segment_id": "5d0a…", "filename": "Google Sheets: Event leads › October", "total": 14, "imported": 12, "skipped": 2, "errors": [], "…": "…" },
  "connection": { "id": "a7c1…", "kind": "import", "rows_added": 224, "last_sync_at": "2026-11-04T10:20:00Z", "…": "…" }
}`,
    },
    {
      method: "POST",
      path: "/segments/5d0a…/write-emails",
      summary:
        "Have the agent write a first email for every lead in a campaign that doesn't have one yet, like Write emails in the app. Add leads with segment_id above, then call this. In a campaign that's sending, the new emails are approved straight away. 409 when every lead already has an email. The body is optional: lead_ids writes for just those leads, and instructions guides this batch.",
      body: `{ "instructions": "Mention our Leeds office" }`,
      response: `{ "run_id": "9b4f…" }`,
    },
    {
      method: "GET",
      path: "/runs/9b4f…",
      summary:
        "A run's progress, for example one started by write-emails. status goes from queued to running, then succeeded, failed or cancelled. message is its latest progress line, error says why a run failed, and cost_usd is what Claude Code reported for it.",
      response: `{
  "id": "9b4f…",
  "kind": "write_emails",
  "status": "succeeded",
  "message": "Wrote 12 emails",
  "error": "",
  "cost_usd": 0.42,
  "created_at": "2026-11-04T10:00:00Z",
  "finished_at": "2026-11-04T10:03:12Z",
  "…": "…"
}`,
    },
    {
      method: "GET",
      path: "/tasks?status=pending",
      summary:
        "Everything still working in the background, or done, newest first: agent runs, find-email batches and campaign imports. A cancelled run counts as failed. Each has the url that follows it, so a lost run_id or task_id can be found again. kind filters runs, since every reply gets its own classify_reply run.",
      params: [
        ["status", "optional: pending, completed or failed"],
        ["type", "optional: run, email_batch or campaign_import"],
        ["kind", "optional: a run's kind, like find_people"],
        ["project_id", "optional: only this project's"],
      ],
      response: `{
  "items": [
    {
      "id": "9b4f…",
      "type": "run",
      "kind": "write_emails",
      "project_id": "c1d2…",
      "status": "pending",
      "created_at": "2026-11-04T10:00:00Z",
      "finished_at": null,
      "url": "/v1/runs/9b4f…"
    }
  ],
  "total": 1
}`,
    },
    {
      method: "GET",
      path: "/leads?email=sarah@northwind.studio",
      summary:
        "Find leads by email across all your projects, for tools that only know the address, such as a calendar booking. Newest first, up to 100. Then use the id with PATCH below.",
      params: [["email", "the address to look up, any case"]],
      response: `[
  ${indent(LEAD, 2)}
]`,
    },
    {
      method: "GET",
      path: "/leads/c42e…",
      summary: "One lead with its campaign, company and every email in both directions, oldest first.",
      response: `${LEAD.slice(0, -2)},
  "segment_name": "Design studios",
  "company": { "id": "e810…", "domain": "northwind.studio", "name": "Northwind Studio", "fit_score": 86, "…": "…" },
  "emails": [{ "id": "a19c…", "direction": "outbound", "kind": "initial", "subject": "Northwind Studio x Acme", "status": "sent", "…": "…" }]
}`,
    },
    {
      method: "PATCH",
      path: "/leads/c42e…",
      summary:
        "Set a lead's status, for example meeting_booked when your calendar books a call or not_interested from your CRM, replace its notes, the ones your team sees in the inbox, or set its language. Send any of them. Follow-ups only go to contacted leads, so any other status stops them. Returns the lead like GET.",
      params: [
        ["status", "new, queued, contacted, replied, hot, meeting_booked, not_interested, bounced, unsubscribed or do_not_contact"],
        ["notes", "up to 5,000 characters, replacing the lead's notes. An empty string clears them."],
        ["language", "a code like de or a name like German, for campaigns that write in each lead's language. An empty string goes back to their country and website."],
      ],
      body: `{ "status": "meeting_booked", "notes": "Booked for Tuesday 2pm by Calendly." }`,
      response: `{
  "id": "c42e…",
  "status": "meeting_booked",
  "notes": "Booked for Tuesday 2pm by Calendly.",
  "…": "…"
}`,
    },
    {
      method: "POST",
      path: "/leads/c42e…/mark-replied",
      summary:
        "Mark a reply as answered when you answered it from your own mail client or it needs no answer, like Done in the inbox. The thread leaves Needs reply until the lead writes again, its reminder is cleared and the agent's reply still waiting for approval is cancelled. The lead's status doesn't change. 409 for a lead who hasn't replied.",
      response: `{ "lead_id": "c42e…", "answered_through": "2026-11-04T10:12:00Z" }`,
    },
    {
      method: "GET",
      path: "/leads/c42e…/thread",
      summary:
        "The lead's emails in both directions, oldest first, like the inbox's thread. reply_cc is who a reply copies in when you don't name anyone: everyone the lead copied on their last message, minus your own mailboxes and the do-not-contact list. Download a file with GET /attachments/{id}.",
      response: `{
  "lead_id": "c42e…",
  "messages": [
    {
      "id": "71aa…",
      "direction": "inbound",
      "kind": "inbound",
      "channel": "email",
      "status": "received",
      "subject": "Re: Northwind Studio x Acme",
      "body": "Sounds interesting, can you send times for next week?",
      "from_email": "sarah@northwind.studio",
      "to_email": "alex@acme.com",
      "cc_emails": [],
      "classification": "hot",
      "attachments": [],
      "sent_at": null,
      "created_at": "2026-11-04T10:12:00Z"
    }
  ],
  "reply_cc": []
}`,
    },
    {
      method: "POST",
      path: "/leads/c42e…/reply",
      summary:
        "Answer the lead's last message, like Send in the inbox. The reply goes out from the mailbox they wrote to on its next pass, in the same thread, and an agent reply still waiting for approval is cancelled. 400 for a lead who hasn't replied yet.",
      params: [
        ["body_text", "the reply"],
        ["cc_emails", "optional, the whole list to copy in. Without it, reply_all decides"],
        ["reply_all", "optional, default true: copy in everyone the lead copied in last, as reply_cc shows"],
        ["attachment_ids", "optional ids from POST /attachments, up to 10"],
        ["send_at", "optional ISO time within 30 days to send it then instead of now"],
      ],
      body: `{ "body_text": "Happy to. Does Tuesday at 10 or Wednesday at 2 work?" }`,
      response: `{
  "id": "e6f6…",
  "direction": "outbound",
  "kind": "manual_reply",
  "channel": "email",
  "status": "scheduled",
  "subject": "Re: Northwind Studio x Acme",
  "body": "Happy to. Does Tuesday at 10 or Wednesday at 2 work?",
  "to_email": "sarah@northwind.studio",
  "cc_emails": [],
  "attachments": [],
  "…": "…"
}`,
    },
    {
      method: "POST",
      path: "/attachments",
      summary:
        "Upload a file for a reply as a multipart form with the file in its file field, then pass its id in the reply's attachment_ids. Up to 25 MB. A file no reply takes is deleted after a day, and 429 means 100 are already waiting.",
      form: "file=@pricing.pdf",
      response: `{
  "id": "56b4…",
  "message_id": null,
  "filename": "pricing.pdf",
  "content_type": "application/pdf",
  "size_bytes": 48211,
  "inline": false,
  "stored": true,
  "created_at": "2026-11-04T10:20:00Z"
}`,
    },
    {
      method: "GET",
      path: "/attachments/56b4…",
      responseType: "application/octet-stream",
      summary: "Download a file from a thread or one you uploaded, as the file itself with its name in Content-Disposition.",
      response: "The file's bytes",
    },
    {
      method: "POST",
      path: "/leads/c42e…/assign",
      summary: "Give a lead to someone on your team, or take it back with null. Assignment rules never move it afterwards. Returns the lead like GET.",
      params: [["owner", "a team member's user id or email, or null for nobody"]],
      body: `{ "owner": "maya@acme.com" }`,
      response: `{ "id": "c42e…", "owner_id": "44c7…", "owner_name": "Maya Patel", "owner_email": "maya@acme.com", "…": "…" }`,
    },
    {
      method: "GET",
      path: "/suppressions?since=2026-11-01T00:00:00Z&page=1",
      summary:
        "Your organization's do-not-contact list, oldest first, to copy unsubscribes and bounces into your CRM or other sending tools. reason is manual, customer, unsubscribe or bounce. Read every page, then next time pass the last created_at you saw as since.",
      params: [
        ["since", "optional RFC 3339 timestamp, returns entries added after it"],
        ["page", "1-based page number"],
        ["per_page", "1 to 200, default 50"],
      ],
      response: `{
  "items": [
    {
      "id": "0b5f…",
      "org_id": "9e41…",
      "value": "sarah@northwind.studio",
      "kind": "email",
      "reason": "unsubscribe",
      "created_at": "2026-11-03T15:42:00Z"
    }
  ],
  "total": 1
}`,
    },
    {
      method: "POST",
      path: "/suppressions",
      summary:
        "Add up to 1,000 emails or domains to your organization's do-not-contact list, for example from your CRM. Splee checks the list before every email, so queued emails to them are cancelled too. added counts the new ones. One value that isn't an email or a domain rejects the request.",
      body: `{ "values": ["sarah@northwind.studio", "competitor.com"] }`,
      response: `{ "added": 2 }`,
    },
    {
      method: "POST",
      path: "/suppression-lists/people",
      summary:
        "Add up to 10,000 people to a named list, like your CRM's customers, kept apart from unsubscribes and bounces. The list is made when your organization has none by that name. Splee never contacts them in the projects it covers, and queued emails and texts to them are cancelled. project_ids sets those projects, null or left out on a new list means all of them. skipped counts invalid emails and ones already on the list. 422 for an empty project_ids or a project outside your organization.",
      body: `{ "list": "CRM customers", "people": [{ "email": "sarah@northwind.studio" }], "project_ids": ["${projectId}"] }`,
      response: `{
  "list": { "id": "3c7d…", "name": "CRM customers", "kind": "people", "project_ids": ["${projectId}"], "created_by": null, "created_at": "2026-11-21T09:00:00Z", "entries": 1 },
  "added": 1,
  "skipped": 0
}`,
    },
    {
      method: "POST",
      path: "/suppression-lists/companies",
      summary: "The same for companies: everyone whose email domain or company domain is on the list is left alone in the projects it covers.",
      body: `{ "list": "Partner agencies", "domains": ["northwind.studio"] }`,
      response: `{ "list": { "id": "8a21…", "name": "Partner agencies", "kind": "companies", "project_ids": null, "created_by": null, "created_at": "2026-11-21T09:00:00Z", "entries": 1 }, "added": 1, "skipped": 0 }`,
    },
    {
      method: "GET",
      path: "/suppression-lists",
      summary: "Your organization's lists, newest first, with how many entries each has. project_ids is null for a list that covers every project.",
      response: `[{ "id": "3c7d…", "name": "CRM customers", "kind": "people", "project_ids": ["${projectId}"], "created_by": null, "created_at": "2026-11-21T09:00:00Z", "entries": 1 }]`,
    },
    {
      method: "GET",
      path: "/suppression-lists/3c7d…/entries?limit=100&offset=0",
      summary: "A list's emails or domains in alphabetical order.",
      params: [
        ["limit", "1 to 1,000, default 100"],
        ["offset", "how many to skip, default 0"],
      ],
      response: `{ "items": ["sarah@northwind.studio"], "total": 1 }`,
    },
    {
      method: "PATCH",
      path: "/suppression-lists/3c7d…",
      summary: "Change the projects a list covers. Send null for all projects.",
      body: `{ "project_ids": null }`,
      response: `{ "id": "3c7d…", "name": "CRM customers", "kind": "people", "project_ids": null, "created_by": null, "created_at": "2026-11-21T09:00:00Z", "entries": 1 }`,
    },
    {
      method: "DELETE",
      path: "/suppression-lists/3c7d…",
      summary: "Delete a list and everything on it. Splee can contact those people again from the next send.",
      response: `{ "ok": true }`,
    },
    {
      method: "GET",
      path: `/projects/${projectId}/replies?since=2026-11-01T00:00:00Z`,
      summary:
        "Replies your leads sent, with the agent's classification. Oldest first, up to 500 per request. Poll with since set to the last created_at you saw.",
      params: [["since", "optional RFC 3339 timestamp, returns replies created after it"]],
      response: `[
  {
    "id": "71aa…",
    "lead_id": "c42e…",
    "direction": "inbound",
    "subject": "Re: Northwind Studio x Acme",
    "body": "Sounds interesting, can you send times for next week?",
    "from_email": "sarah@northwind.studio",
    "classification": "hot",
    "classification_reason": "Asks to schedule a call",
    "created_at": "2026-11-04T10:12:00Z"
  }
]`,
    },
    {
      method: "GET",
      path: `/inbox?project_id=${projectId}&filter=needs_reply`,
      summary:
        "Reply threads like the Inbox page, the latest first, with the last message in either direction cut to 300 characters.",
      params: [
        ["project_id", "optional, all projects without it"],
        ["segment_id", "optional, only the threads of this campaign's leads"],
        ["filter", "all, needs_reply, hot, auto_replied or other, default all"],
        ["q", "optional text in the lead's name, email or company"],
        ["page, per_page", "1-based page number, and 1 to 200 per page, default 50"],
      ],
      response: `{
  "items": [
    {
      "lead_id": "c42e…",
      "project_id": "${projectId}",
      "campaign_id": "5d0a…",
      "project_name": "Acme",
      "lead_name": "Sarah Chen",
      "lead_email": "sarah@northwind.studio",
      "company_name": "Northwind Studio",
      "lead_status": "hot",
      "classification": "hot",
      "pending_auto_reply": false,
      "last_message": {
        "id": "71aa…",
        "direction": "inbound",
        "subject": "Re: Northwind Studio x Acme",
        "snippet": "Sounds interesting, can you send times for next week?",
        "at": "2026-11-04T10:12:00Z"
      }
    }
  ],
  "total": 1
}`,
    },
    {
      method: "GET",
      path: "/hot-leads?since=2026-11-01T00:00:00Z",
      summary:
        "Every hot lead in your organization, newest first, with the reply that made them hot. Pass since as the newest became_hot_at you've seen to get only new ones.",
      params: [
        ["since", "optional, only leads that became hot after this"],
        ["project_id", "optional, all projects without it"],
        ["page, per_page", "1-based page number, and 1 to 200 per page, default 50"],
      ],
      response: `{
  "items": [
    {
      "lead_id": "c42e…",
      "project_id": "${projectId}",
      "campaign_id": "5d0a…",
      "campaign": "Design studios",
      "name": "Sarah Chen",
      "email": "sarah@northwind.studio",
      "title": "Founder",
      "company_name": "Northwind Studio",
      "company_domain": "northwind.studio",
      "linkedin_url": "https://www.linkedin.com/in/sarah-chen",
      "status": "hot",
      "reply_id": "71aa…",
      "reply": "Sounds interesting, can you send times for next week?",
      "why_hot": "Asks to schedule a call",
      "became_hot_at": "2026-11-04T10:12:00Z"
    }
  ],
  "total": 1
}`,
    },
    {
      method: "GET",
      path: `/projects/${projectId}/inbox/changes?cursor=1762251120000000_71aa…`,
      summary:
        "Replies that came in since the cursor, oldest first, so you can poll without missing or repeating one. Pass next_cursor as cursor next time, and call again straight away while has_more is true. A reply shows up about 5 seconds after it arrives, and classification is null until Splee has read it.",
      params: [
        ["cursor", "next_cursor from the last call"],
        ["since", "on the first call instead, only replies after this. Without either, from the first reply"],
        ["limit", "1 to 500, default 100"],
        ["segment_id", "optional, only this campaign's replies"],
      ],
      response: `{
  "items": [
    {
      "id": "71aa…",
      "lead_id": "c42e…",
      "campaign_id": "5d0a…",
      "lead_name": "Sarah Chen",
      "lead_email": "sarah@northwind.studio",
      "company_name": "Northwind Studio",
      "channel": "email",
      "from_email": "sarah@northwind.studio",
      "subject": "Re: Northwind Studio x Acme",
      "body": "Sounds interesting, can you send times for next week?",
      "classification": "hot",
      "received_at": "2026-11-04T10:12:00Z"
    }
  ],
  "next_cursor": "1762251120000000_71aa…",
  "has_more": false
}`,
    },
    {
      method: "GET",
      path: "/erased-leads?cursor=1762251120000000_c42e…",
      summary:
        "Leads someone deleted or forgot in Splee, oldest first, so you can delete what you keep of each one in your CRM, sheets and automations. Only the ids you got the lead with are left, because the name and address are gone. forgotten means the address also went on your do-not-contact list. Poll it like inbox changes. A lead shows up about 5 seconds after it's erased.",
      params: [
        ["project_id", "optional, only this project"],
        ["cursor", "next_cursor from the last call"],
        ["since", "on the first call instead, only leads erased after this. Without either, from the first"],
        ["limit", "1 to 500, default 100"],
      ],
      response: `{
  "items": [{ "lead_id": "c42e…", "project_id": "${projectId}", "campaign_id": "5d0a…", "forgotten": true, "erased_at": "2026-11-04T10:12:00Z" }],
  "next_cursor": "1762251120000000_c42e…",
  "has_more": false
}`,
    },
    {
      method: "GET",
      path: `/emails?project_id=${projectId}`,
      summary:
        "Draft emails waiting for approval, oldest first, across all projects or one. Pass status to see others, like approved or sent.",
      params: [
        ["project_id, segment_id", "optional project or campaign"],
        ["status", "optional, draft by default, comma-separated for several"],
        ["page, per_page", "1-based page number, and 1 to 200 per page, default 50"],
      ],
      response: `{
  "items": [
    {
      "id": "a19c…",
      "project_id": "${projectId}",
      "segment_id": "5d0a…",
      "campaign": "Design studios",
      "lead_id": "c42e…",
      "lead_name": "Sarah Chen",
      "lead_email": "sarah@northwind.studio",
      "company_name": "Northwind Studio",
      "kind": "initial",
      "step": 0,
      "subject": "Northwind Studio x Acme",
      "body": "Hi Sarah, …",
      "status": "draft",
      "scheduled_for": null,
      "created_at": "2026-11-04T10:03:00Z"
    }
  ],
  "total": 1
}`,
    },
    {
      method: "POST",
      path: "/emails/a19c…/approve",
      summary:
        "Approve an email, like Approve in the app. It goes out from your mailboxes on the project's schedule once its campaign is sending. Approving a failed or cancelled email sends it again. 400 once it was sent.",
      response: `{ "id": "a19c…", "status": "approved", "…": "…" }`,
    },
    {
      method: "POST",
      path: "/emails/a19c…/skip",
      summary: "Cancel an email so it never goes out. Approve can still send it later.",
      response: `{ "id": "a19c…", "status": "cancelled", "…": "…" }`,
    },
    {
      method: "GET",
      path: `/projects/${projectId}/calls?status=done`,
      summary:
        "Calls to make, overdue first, then the ones made in the last 14 days, newest first, like the Calls page, for a dialer or CRM to work from. bucket is overdue, today or done. local_time is the time where the lead is, and outside_hours says why now is outside their calling hours, or null.",
      params: [
        ["status", "optional open or done, both without it"],
        ["owner", "optional, a teammate's id or email, unassigned or everyone"],
      ],
      response: `[
  {
    "id": "9497…",
    "lead_id": "c42e…",
    "segment_id": "5d0a…",
    "kind": "manual",
    "step": 0,
    "status": "done",
    "due_at": "2026-11-04T14:00:00Z",
    "called_at": "2026-11-04T14:05:00Z",
    "outcome": "connected",
    "notes": "Wants pricing for two sites",
    "lead_name": "Sarah Chen",
    "lead_title": "Founder",
    "lead_company": "Northwind Studio",
    "lead_phone": "+15035550142",
    "owner_name": "Maya Patel",
    "bucket": "done",
    "time_zone": "America/Los_Angeles",
    "local_time": "07:05",
    "outside_hours": "It's 07:05 where they are. Call between 8:00 and 20:00 their time.",
    "…": "…"
  }
]`,
    },
    {
      method: "POST",
      path: "/leads/c42e…/calls",
      summary:
        "Log a call made with the lead, from a dialer or by hand. It closes their open call step if they have one, which moves their sequence on, and the outcome updates the lead as it does in the app.",
      params: [
        ["outcome", "connected, voicemail, no_answer, wrong_number, not_interested or meeting_booked"],
        ["notes", "optional"],
      ],
      body: `{ "outcome": "connected", "notes": "Wants pricing for two sites" }`,
      response: `{ "id": "9497…", "lead_id": "c42e…", "status": "done", "outcome": "connected", "notes": "Wants pricing for two sites", "…": "…" }`,
    },
    {
      method: "GET",
      path: "/leads/c42e…/meetings",
      summary:
        "The lead's sales meetings from Fireflies, Fathom, Gong, Zoom or tl;dv, oldest first, with the agent's summary, next steps and the stage it suggests for their deal. Transcripts stay in Splee, and transcript_chars is a transcript's length.",
      response: `[
  {
    "id": "3e81…",
    "lead_id": "c42e…",
    "provider": "fireflies",
    "title": "Northwind x Acme intro",
    "started_at": "2026-11-05T16:00:00Z",
    "duration_secs": 1860,
    "attendees": [{ "name": "Sarah Chen", "email": "sarah@northwind.studio" }],
    "summary": "Sarah wants one tool for both studios and a pilot in December.",
    "next_steps": ["Send pricing for two sites", "Book a demo with their ops lead"],
    "suggested_stage": "Proposal",
    "stage_reason": "They asked for pricing.",
    "stage_status": "pending",
    "follow_up_status": "draft",
    "transcript_chars": 18422,
    "…": "…"
  }
]`,
    },
    {
      method: "GET",
      path: `/projects/${projectId}/sent-emails?since=2026-11-01T00:00:00Z`,
      summary:
        "Emails Splee sent your leads: first emails, follow-ups, auto-replies and replies from the inbox. Oldest first, up to 500 per request. Poll with since set to the last sent_at you saw, or get each one as it goes out with the Email sent webhook.",
      params: [["since", "optional RFC 3339 timestamp, returns emails sent after it"]],
      response: `[
  {
    "id": "5d03…",
    "lead_id": "c42e…",
    "direction": "outbound",
    "kind": "initial",
    "step": 0,
    "subject": "Northwind Studio x Acme",
    "body": "Hi Sarah, …",
    "from_email": "alex@acme.com",
    "to_email": "sarah@northwind.studio",
    "message_id": "9e1c…@acme.com",
    "sent_at": "2026-11-03T15:40:00Z"
  }
]`,
    },
    {
      method: "GET",
      path: `/projects/${projectId}/email-events?since=2026-11-01T00:00:00Z`,
      summary:
        "Every counted open and click, oldest first and up to 500 a call, the polling twin of the email.opened and email.clicked webhooks. Opens and clicks by scanners and link checkers are left out. url is the clicked link, null for an open. Next time pass the last at you saw as since.",
      params: [["since", "optional RFC 3339 timestamp, returns events after it"]],
      response: `[
  { "id": "0c9e…", "event": "email.opened", "email_id": "a19c…", "lead_id": "c42e…", "url": null, "at": "2026-11-04T09:20:00Z" },
  { "id": "41d7…", "event": "email.clicked", "email_id": "a19c…", "lead_id": "c42e…", "url": "https://acme.com/pricing", "at": "2026-11-04T09:21:00Z" }
]`,
    },
    {
      method: "GET",
      path: `/projects/${projectId}/stats`,
      summary:
        "Lifetime results and cost of a project and each of its campaigns. Money is in micro-dollars (1,000,000 = $1). AI cost is what Claude Code reports for the agent's runs, and email cost uses each mailbox's cost per email. Runs that research the whole project count only in totals. contacted counts leads sent at least one email and replies counts leads who wrote back, not bounces, so replies over contacted is the reply rate. steps has one entry per email of the sequence that went out, step 0 being the first email: a lead's first reply counts for the last email they got before it.",
      response: `{
  "totals": {
    "leads": 240,
    "drafts": 12,
    "scheduled": 30,
    "sent": 410,
    "contacted": 230,
    "replies": 19,
    "hot": 4,
    "meetings": 1,
    "ai_cost_micros": 8420000,
    "email_cost_micros": 410000,
    "spend_micros": 8830000,
    "cost_per_hot_micros": 2207500
  },
  "campaigns": [
    {
      "id": "5d0a…",
      "name": "Design studios",
      "status": "scaling",
      "companies": 60,
      "leads": 120,
      "sent": 230,
      "contacted": 118,
      "replies": 12,
      "bounced": 2,
      "hot": 3,
      "spend_micros": 3120000,
      "cost_per_hot_micros": 1040000,
      "…": "…"
    }
  ],
  "steps": [
    { "step": 0, "sent": 230, "replied": 13 },
    { "step": 1, "sent": 140, "replied": 5 },
    { "step": 2, "sent": 40, "replied": 1 }
  ]
}`,
    },
    {
      method: "GET",
      path: "/exports",
      summary:
        "Your organization's last 30 data exports to its own storage, newest first, with the files each wrote. trigger is daily or manual, and status is running, done or failed.",
      response: `[
  {
    "id": "6b0d…",
    "dt": "2026-11-04",
    "trigger": "daily",
    "status": "done",
    "files": [{ "table": "leads", "key": "splee/leads/dt=2026-11-04/part-0000.jsonl.gz", "rows": 4210, "bytes": 391022 }],
    "rows": 4210,
    "bytes": 391022,
    "error": null,
    "started_at": "2026-11-04T03:00:00Z",
    "finished_at": "2026-11-04T03:01:12Z"
  }
]`,
    },
    {
      method: "GET",
      path: `/projects/${projectId}/analytics`,
      summary:
        "What the Analytics page shows for a date range: totals with the same totals for the period before in previous, a series per day (per week past 90 days), and breakdowns by campaign, mailbox, step, send weekday and hour, and channel. Days and hours are in the project's time zone. Each thing counts on the day it happened, and a reply, bounce, meeting or won deal counts for the campaign, mailbox and step of the last email the lead got before it. Rates are by lead, out of contacted, except steps and the heatmap, which are out of the emails sent. claude_cost_micros leaves out runs when you filter by mailbox. won_value has one entry per currency, in cents. revenue_cents is what leads paid through Stripe, Paddle or Lemon Squeezy, less refunds, in the organization's currency, and ignored_payments counts payments in other currencies. revenue_per_meeting_cents is revenue over meetings. return_on_spend is revenue over spend, and null unless the organization's currency is USD, since spend is in dollars.",
      params: [
        ["from", "optional first day, YYYY-MM-DD. 29 days before to by default"],
        ["to", "optional last day, YYYY-MM-DD. Today by default. At most 365 days in all"],
        ["segment_id", "optional campaign id"],
        ["mailbox_id", "optional mailbox id"],
      ],
      response: `{
  "from": "2026-10-13",
  "to": "2026-11-11",
  "previous_from": "2026-09-13",
  "previous_to": "2026-10-12",
  "timezone": "Europe/London",
  "currency": "USD",
  "revenue_tracked": true,
  "ignored_payments": 0,
  "ignored_currencies": [],
  "interval": "day",
  "totals": {
    "sent": 410, "contacted": 230, "delivered": 404, "replies": 19, "reply_rate": 0.083, "hot": 4, "hot_rate": 0.017,
    "meetings": 2, "deals_won": 1, "won_value": [{ "currency": "USD", "value_cents": 1200000 }], "unsubscribes": 3,
    "bounced": 6, "bounce_rate": 0.026, "claude_cost_micros": 8420000, "email_cost_micros": 410000,
    "spend_micros": 8830000, "cost_per_hot_micros": 2207500, "cost_per_meeting_micros": 4415000,
    "revenue_cents": 1440000, "refunded_cents": 0, "payments": 3, "paying_leads": 1, "revenue_per_meeting_cents": 720000,
    "return_on_spend": 1630.8
  },
  "previous": { "…": "…" },
  "series": [{ "date": "2026-10-13", "sent": 14, "replies": 1, "hot": 0, "meetings": 0 }],
  "campaigns": [{ "segment_id": "5d0a…", "name": "Design studios", "sent": 230, "replies": 12, "reply_rate": 0.1, "…": "…" }],
  "mailboxes": [{ "mailbox_id": "8b2e…", "email": "alex@acme.com", "sent": 205, "bounce_rate": 0.02, "bouncing": false, "…": "…" }],
  "steps": [{ "step": 0, "sent": 230, "replies": 13, "reply_rate": 0.057 }],
  "heatmap": [{ "weekday": 2, "hour": 9, "sent": 40, "replies": 4 }],
  "channels": [{ "channel": "email", "sent": 410, "leads": 230, "replies": 17, "reply_rate": 0.074 }]
}`,
    },
    {
      method: "GET",
      path: "/deal-stages",
      summary: "Your pipeline's stages in board order, with how many deals each holds. kind is open, won or lost, and auto is hot or booked for the stages a hot reply and a booked meeting move a deal to.",
      response: `[
  { "id": "8115…", "name": "Interested", "kind": "open", "position": 0, "probability": 10, "auto": "hot", "deals": 4 },
  { "id": "e19a…", "name": "Proposal", "kind": "open", "position": 2, "probability": 50, "auto": null, "deals": 2 },
  { "id": "acee…", "name": "Won", "kind": "won", "position": 4, "probability": 100, "auto": null, "deals": 1 }
]`,
    },
    {
      method: "GET",
      path: `/deals?project_id=${projectId}&status=open`,
      summary: "Your organization's deals, the latest first, like the Pipeline page. status is open, won or lost. value is in the deal's currency.",
      params: [
        ["project_id", "optional, all projects without it"],
        ["status", "optional open, won or lost"],
        ["stage_id, owner_id, lead_id", "optional, one stage's, one teammate's or one lead's deals"],
        ["page, per_page", "1-based page number, and 1 to 200 per page, default 50"],
      ],
      response: `{
  "items": [
    {
      "id": "5405…",
      "project_id": "${projectId}",
      "lead_id": "c42e…",
      "lead_name": "Sarah Chen",
      "lead_email": "sarah@northwind.studio",
      "company_name": "Northwind Studio",
      "stage_id": "e19a…",
      "stage": "Proposal",
      "status": "open",
      "value": 4800.0,
      "currency": "USD",
      "close_date": "2026-12-15",
      "owner_id": null,
      "notes": "Two sites, wants a call",
      "source": "api",
      "stage_changed_at": "2026-11-05T09:00:00Z",
      "closed_at": null,
      "created_at": "2026-11-04T10:30:00Z",
      "…": "…"
    }
  ],
  "total": 1
}`,
    },
    {
      method: "POST",
      path: "/deals",
      summary:
        "Open a deal for a lead, in the stage hot replies open deals in (auto hot on GET /deal-stages) unless you name one. A lead has one open deal at a time, so a second is 409. Returns the deal like the list.",
      params: [
        ["lead_id", "the lead"],
        ["stage_id or stage", "optional, a stage's id or its name"],
        ["value, currency", "optional amount and ISO currency code"],
        ["close_date", "optional YYYY-MM-DD"],
        ["owner_id, notes", "optional"],
      ],
      body: `{ "lead_id": "c42e…", "stage": "Interested", "value": 4800, "currency": "USD", "close_date": "2026-12-15" }`,
      response: `{ "id": "5405…", "stage": "Interested", "status": "open", "value": 4800.0, "…": "…" }`,
    },
    {
      method: "POST",
      path: "/deals/5405…/move",
      summary: "Move a deal to another stage, by id or by name. Moving it to Won closes it as won with value as its final amount, and to Lost with lost_reason.",
      params: [
        ["stage_id or stage", "the stage's id or its name"],
        ["value", "optional final amount, for a won stage"],
        ["lost_reason", "optional, for a lost stage"],
      ],
      body: `{ "stage": "Won", "value": 5200 }`,
      response: `{ "id": "5405…", "stage": "Won", "status": "won", "value": 5200.0, "closed_at": "2026-11-20T15:00:00Z", "…": "…" }`,
    },
  ];
}

/** The REST hook endpoints Zapier, Make and n8n call. Keep the events in sync with rest_hooks::EVENTS in backend/src/rest_hooks.rs. */
export function hookEndpoints(projectId: string): Endpoint[] {
  return [
    {
      method: "GET",
      path: "/me",
      summary: "The organization, key and projects your API key belongs to, for a tool's connection test.",
      response: `{
  "organization": { "id": "4e7c…", "name": "Acme" },
  "api_key": { "id": "0b9d…", "name": "Zapier", "prefix": "splee_k3Rt9x" },
  "projects": [{ "id": "${projectId}", "name": "Acme" }]
}`,
    },
    {
      method: "POST",
      path: "/hooks",
      summary:
        "Subscribe a URL to one event. Leave out project_id to hear about every project. 400 for an unknown event or a URL Splee can't reach, like one on a private network.",
      body: `{ "event": "hot_lead", "target_url": "https://hooks.zapier.com/hooks/standard/…", "project_id": "${projectId}" }`,
      response: `{
  "id": "e81f…",
  "project_id": "${projectId}",
  "event": "hot_lead",
  "target_url": "https://hooks.zapier.com/hooks/standard/…",
  "disabled_at": null,
  "created_at": "2026-11-12T09:30:00Z",
  "…": "…"
}`,
    },
    {
      method: "GET",
      path: `/hooks?project_id=${projectId}`,
      summary:
        "Your organization's subscriptions, oldest first. live is false for one that stopped getting events, because its receiver answered 410 Gone or its API key was revoked. GET /hooks/e81f… returns one.",
      params: [["project_id", "optional, only the ones this project's events go to, including those for every project"]],
      response: `[
  {
    "id": "e81f…",
    "project_id": "${projectId}",
    "event": "hot_lead",
    "target_url": "https://hooks.zapier.com/hooks/standard/…",
    "error": null,
    "disabled_at": null,
    "live": true,
    "…": "…"
  }
]`,
    },
    {
      method: "DELETE",
      path: "/hooks/e81f…",
      summary: "Unsubscribe, when the zap, scenario or workflow is turned off.",
      response: `{ "ok": true }`,
    },
    {
      method: "GET",
      path: `/hooks/samples/hot_lead?project_id=${projectId}`,
      summary:
        "Up to 3 recent payloads of an event, the newest first, for a trigger's setup screen. With none yet, lead_added gives your newest leads and the others give one example.",
      params: [["project_id", "optional, only this project's"]],
      response: `[
  {
    "event": "hot_lead",
    "project": { "id": "${projectId}", "name": "Acme" },
    "lead": { "id": "c42e…", "full_name": "Sarah Chen", "email": "sarah@northwind.studio", "…": "…" },
    "reply": { "id": "71aa…", "body": "Sounds interesting, can you send times for next week?", "classification": "hot", "…": "…" },
    "url": "…"
  }
]`,
    },
  ];
}

export const HOOK_EVENTS: [event: string, description: string][] = [
  ["hot_lead", "a reply that shows interest, with the reply"],
  ["lead_replied", "any reply a person wrote, with how Splee classified it"],
  ["meeting_booked", "the lead booked a call, or your team marked them Meeting booked"],
  ["status_changed", "someone on your team changed the lead's status, with previous_status"],
  ["deal.won", "a deal closed as won, with the deal"],
  ["lead_added", "a new lead in the project, from any source, within about a minute"],
  ["lead_erased", "someone deleted or forgot a lead, with only its id and campaign_id, forgotten and erased_at"],
];

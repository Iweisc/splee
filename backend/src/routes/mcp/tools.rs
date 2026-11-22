//! The tools as tools/list describes them to the client: what each does, its arguments and whether it changes anything.

use std::sync::LazyLock;

use serde_json::{Value, json};

use super::run::{MAX_DOWNLOAD_ROWS, MAX_SIGNALS};
use crate::routes::util_input::LEAD_STATUSES;

const MY_COPY_TEXT_RULES: &str = "Changes the user's own first email or turns it on or off. Pass only what changes. Variables: \
{{first_name}}, {{company}}, {{job_title}}, {{company_city}}, {{sender_first_name}}, {{sender_name}}, {{sender_company}}, each \
with an optional fallback like {{company_city|your city}}. A lead missing one with no fallback gets the AI email. {a|b} picks one \
option per lead. Up to 5 {{ai: instruction}} lines are written for each lead. The signature goes under the text unless it ends \
with the sender's name. A text with a mistake is refused with what to fix, and nothing is saved.";
/// What a campaign's language can be set to.
static LANGUAGE_CHOICES: LazyLock<Vec<&str>> =
    LazyLock::new(|| crate::language::LANGUAGES.iter().map(|l| l.code).chain([crate::language::EACH_LEAD]).collect());

fn id_prop(description: &str) -> Value {
    json!({ "type": "string", "format": "uuid", "description": description })
}

fn page_props(props: Value) -> Value {
    let mut props = props;
    props["page"] = json!({ "type": "integer", "minimum": 1, "description": "Page number, from 1" });
    props["per_page"] = json!({ "type": "integer", "minimum": 1, "maximum": 200, "description": "Results per page, 20 by default" });
    props
}

fn score_prop() -> Value {
    json!({ "type": "integer", "minimum": 0, "maximum": 100, "description": "Optional. Only scores of at least this" })
}

fn sort_prop() -> Value {
    json!({ "type": "string", "enum": ["score", "newest"], "description": "Optional. score puts the highest first" })
}

fn tool(name: &str, title: &str, description: &str, properties: Value, required: &[&str], annotations: Value) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required },
        "annotations": annotations,
    })
}

pub(super) fn tools() -> Vec<Value> {
    let read = json!({ "readOnlyHint": true });
    let adds = json!({ "readOnlyHint": false, "destructiveHint": false });
    let control = json!({ "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true });
    let texts = json!({ "type": "array", "items": { "type": "string" } });
    let project = id_prop("A project id from list_projects");
    let campaign = id_prop("A campaign id from list_campaigns or create_campaign");
    vec![
        tool("list_projects", "List projects", "The organization's projects. A project is one business doing outreach.", json!({}), &[], read.clone()),
        tool(
            "get_project",
            "Get project",
            "A project with what Splee learned about the business (company) and its controls, as update_project_controls returns them.",
            json!({ "project_id": project }),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "create_project",
            "Create project",
            "Creates a project for a business from its website or a description, as the app's New project does. An agent researches \
             the business, which takes a few minutes, then create_campaign can start. Returns project_id and run_id.",
            json!({
                "domain": { "type": "string", "description": "The business's website, like acme.com" },
                "description": { "type": "string", "description": "What the business sells and to whom, when there's no website" },
            }),
            &[],
            adds.clone(),
        ),
        tool(
            "list_campaigns",
            "List campaigns",
            "A project's campaigns with their funnel: companies, leads, emails sent, leads contacted, replies, bounces, hot leads and \
             spend in micro-dollars. A campaign that A/B tests its email angle has ab_test: each variant's instructions and results, \
             whether a winner is clear yet in note, and the winner auto-pick kept, if any. Archived campaigns are left out unless \
             archived is true, and have archived_at set.",
            json!({
                "project_id": project,
                "archived": { "type": "boolean", "description": "Optional. True to list archived campaigns too" },
            }),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "create_campaign",
            "Create campaign",
            "Creates a campaign from a description of who to reach, like \"dental clinics in Leeds\". An agent names it and sets its \
             targeting, which takes a minute or two. Returns campaign_id and run_id.",
            json!({
                "project_id": project,
                "prompt": { "type": "string", "description": "Who the campaign targets, in plain words" },
                "language": { "type": "string", "enum": LANGUAGE_CHOICES.as_slice(), "description": "Optional. lead (the default) for each lead's own language, en or another language code" },
            }),
            &["project_id", "prompt"],
            adds.clone(),
        ),
        tool(
            "import_campaign",
            "Import campaign from a lead list",
            "Makes a new draft campaign holding exactly the leads given, for a list the user already has. Each lead needs email, \
             first_name, last_name, company_domain and job_title, and one without them is skipped. Leads already in the project, \
             repeats and do-not-contact entries are deduped. Returns task_id. Then write_emails writes its emails and \
             start_campaign sends them.",
            json!({
                "project_id": project,
                "name": { "type": "string", "description": "The campaign's name" },
                "leads": {
                    "type": "array",
                    "maxItems": crate::campaign_imports::MAX_LEADS,
                    "items": {
                        "type": "object",
                        "properties": {
                            "email": { "type": "string" },
                            "first_name": { "type": "string" },
                            "last_name": { "type": "string" },
                            "company_domain": { "type": "string" },
                            "job_title": { "type": "string" },
                            "linkedin_url": { "type": "string", "description": "Optional" },
                            "client_ref": { "type": "string", "description": "Optional. The user's own id for the lead, handed back in its row" },
                        },
                        "required": ["email", "first_name", "last_name", "company_domain", "job_title"],
                    },
                },
                "instructions": { "type": "string", "description": "Optional. The campaign's outreach angle, which its first emails follow" },
                "language": { "type": "string", "description": "Optional. lead (the default) for each lead's own language, or a language code like de" },
            }),
            &["project_id", "name", "leads"],
            adds.clone(),
        ),
        tool(
            "get_campaign_import",
            "Get campaign import status",
            "An import_campaign task: status pending, completed or failed, and once completed, result with campaign_id and how \
             many leads were imported, skipped and deduped.",
            json!({ "task_id": id_prop("A task_id from import_campaign") }),
            &["task_id"],
            read.clone(),
        ),
        tool(
            "list_campaign_import_rows",
            "List campaign import rows",
            "Each lead an import_campaign task was given, in order: status imported with person_id, the lead's id, or skipped or \
             deduped with a reason code like missing_job_title, already_contacted or do_not_contact.",
            page_props(json!({ "task_id": id_prop("A task_id from import_campaign") })),
            &["task_id"],
            read.clone(),
        ),
        tool(
            "search_companies",
            "Search company database",
            "Searches the company database (businesses with a website from Overture Maps), the most reliable listings first. q matches \
             words at the start of words in the name or category. Each company's tech lists the tools its website and DNS show it \
             uses, once Splee has scanned it, which it does for every project's companies. Companies on the organization's dedup \
             lists are left out: the lists for every project, and with project_id that project's too. total stops counting at \
             10,000, and capped says there are more.",
            page_props(json!({
                "q": { "type": "string", "description": "Words in the name or category, like \"dental\"" },
                "country": { "type": "string", "description": "Two-letter country code, like GB" },
                "city": { "type": "string", "description": "City name, like Leeds" },
                "category": { "type": "string", "description": "Exact category, like dental_clinic" },
                "has_email": { "type": "boolean", "description": "Only companies that list an email" },
                "has_phone": { "type": "boolean", "description": "Only companies that list a phone" },
                "tech": { "type": "string", "description": "Only companies that use this tool, like HubSpot, or a tool of this category: crm, \
                    marketing, email_sending, support, analytics, sales_intelligence, advertising, ecommerce, cms, payments, hosting, \
                    scheduling, email or workplace" },
                "project_id": id_prop("Optional. The project searching, so its dedup lists apply too"),
                "include_hidden": { "type": "boolean", "description": "Optional. True to show the companies on dedup lists too" },
            })),
            &[],
            read.clone(),
        ),
        tool(
            "find_companies_by_domains",
            "Find companies by domain",
            "Looks up up to 1,000 domains in the company database, like a CRM's accounts or a spreadsheet's column. Domains are cleaned \
             up first, so https://www.acme.com/about finds acme.com. items has one company per domain found, in the order given, and \
             not_found the rest, including those the organization's dedup lists hide unless include_hidden is true.",
            json!({
                "domains": { "type": "array", "items": { "type": "string" }, "description": "Domains or website addresses, up to 1,000" },
                "project_id": id_prop("Optional. The project looking, so its dedup lists apply too"),
                "include_hidden": { "type": "boolean", "description": "Optional. True to find the companies on dedup lists too" },
            }),
            &["domains"],
            read.clone(),
        ),
        tool(
            "list_sending_domains",
            "List sending domains",
            "The organization's own sending domains, like getacme.com, bought on the Domains page: status, the mailbox provider, \
             whether its DNS is set, and how many mailboxes use and warm it.",
            json!({}),
            &[],
            read.clone(),
        ),
        tool(
            "suggest_sending_domains",
            "Suggest sending domains",
            "Names to send cold email from apart from the company's own domain, like getacme.com or acmehq.co, each with whether the \
             organization's Porkbun account can buy it and its price in micro-dollars, buyable first. available is null when the \
             registry didn't answer in time. Needs Porkbun connected on the Domains page, where the user buys them.",
            json!({ "domain": { "type": "string", "description": "The company's own domain, like acme.com" } }),
            &["domain"],
            read.clone(),
        ),
        tool(
            "check_sending_domains",
            "Check sending domains",
            "Whether the organization's Porkbun account can buy each of up to 20 names and at what price, buyable first. Needs \
             Porkbun connected on the Domains page, where the user buys them.",
            json!({ "domains": { "type": "array", "items": { "type": "string" }, "maxItems": 20, "description": "Names like getacme.com" } }),
            &["domains"],
            read.clone(),
        ),
        tool(
            "fill_company_filters",
            "Fill in company search filters",
            "Turns a description like \"dental clinics in Leeds that list an email and use Shopify\" into search_companies filters: \
             q, country, city, category, has_email, has_phone and tech, using only categories and countries the database has and \
             tools Splee detects. note says what it couldn't turn into a filter, like company size. Pass filters to search_companies \
             as they are. 30 an hour per organization.",
            json!({ "query": { "type": "string", "description": "What companies to find, in a sentence, up to 500 characters" } }),
            &["query"],
            read.clone(),
        ),
        tool(
            "add_companies",
            "Add companies to campaign",
            "Adds companies from search_companies to a campaign by domain, up to 500 at a time. Companies the project already has and \
             ones on the do-not-contact list are skipped. Then call find_people.",
            json!({
                "campaign_id": campaign,
                "domains": { "type": "array", "items": { "type": "string" }, "description": "Domains from search_companies" },
            }),
            &["campaign_id", "domains"],
            adds.clone(),
        ),
        tool(
            "list_saved_searches",
            "List saved company searches",
            "A project's saved company database searches with their filters. Splee checks each about hourly for companies that \
             match and didn't before. new_matches counts the ones found since the search was last opened in the app, and \
             too_broad says it matched over 10,000 companies, so it isn't checked.",
            json!({ "project_id": project }),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "save_search",
            "Save company search",
            "Saves search_companies filters under a name, so Splee checks them about hourly and reports companies that match \
             later, like ones from a country loaded into the database. What matches now isn't reported. With chat_alerts, the \
             project's chat hears about new matches. With campaign_id, up to 200 new matches a check are added to that campaign \
             the way add_companies adds them. Up to 50 a project.",
            json!({
                "project_id": project,
                "name": { "type": "string", "description": "Up to 120 characters, like \"Dentists in Leeds\"" },
                "q": { "type": "string", "description": "Words in the name or category" },
                "country": { "type": "string", "description": "Two-letter country code, like GB" },
                "city": { "type": "string" },
                "category": { "type": "string", "description": "Exact category, like dental_clinic" },
                "has_email": { "type": "boolean" },
                "has_phone": { "type": "boolean" },
                "tech": { "type": "string", "description": "A tool or tool category, as search_companies takes it" },
                "chat_alerts": { "type": "boolean", "description": "Post new matches to the project's chat" },
                "campaign_id": id_prop("Optional. A campaign of the project to add new matches to"),
            }),
            &["project_id", "name"],
            adds.clone(),
        ),
        tool(
            "delete_saved_search",
            "Delete saved company search",
            "Deletes a saved search. Splee stops checking it, and companies it added to a campaign stay there.",
            json!({ "search_id": id_prop("A search id from list_saved_searches") }),
            &["search_id"],
            json!({ "readOnlyHint": false, "destructiveHint": true, "idempotentHint": true }),
        ),
        tool(
            "list_company_dedup_lists",
            "List company dedup lists",
            "The organization's dedup lists, newest first, each with its size in domains and project_ids, null when it covers \
             every project.",
            json!({}),
            &[],
            read.clone(),
        ),
        tool(
            "create_company_dedup_list",
            "Add companies to a dedup list",
            "Adds up to 10,000 domains or website addresses to the organization's dedup list of that name, making it when there's \
             none. Searches in the projects it covers leave those companies out: search_companies, download_companies, saved \
             searches and find_lookalikes. It only hides search results and never stops an email or adding a company to a campaign. \
             skipped counts invalid domains and ones already on it.",
            json!({
                "name": { "type": "string", "description": "The list's name, like \"CRM accounts\"" },
                "domains": { "type": "array", "items": { "type": "string" }, "maxItems": 10_000, "description": "Domains like acme.com" },
                "project_ids": {
                    "type": ["array", "null"],
                    "items": { "type": "string", "format": "uuid" },
                    "description": "Optional. The projects it covers, null for all. Left out, a new list covers all and an existing one keeps its own",
                },
            }),
            &["name", "domains"],
            adds.clone(),
        ),
        tool(
            "get_company_dedup_list",
            "Get a company dedup list",
            "A dedup list with its domains in alphabetical order, 100 at a time unless limit says up to 1,000.",
            json!({
                "list_id": id_prop("A list id from list_company_dedup_lists"),
                "limit": { "type": "integer", "minimum": 1, "maximum": 1000, "description": "Optional. Domains to return, 100 by default" },
                "offset": { "type": "integer", "minimum": 0, "description": "Optional. Domains to skip" },
            }),
            &["list_id"],
            read.clone(),
        ),
        tool(
            "set_company_dedup_list_projects",
            "Set a dedup list's projects",
            "Changes the projects whose searches a dedup list hides companies from.",
            json!({
                "list_id": id_prop("A list id from list_company_dedup_lists"),
                "project_ids": {
                    "type": ["array", "null"],
                    "items": { "type": "string", "format": "uuid" },
                    "description": "Project ids, or null for all",
                },
            }),
            &["list_id", "project_ids"],
            control.clone(),
        ),
        tool(
            "delete_company_dedup_list",
            "Delete a company dedup list",
            "Deletes a dedup list and its domains. Its companies show up in searches again right away.",
            json!({ "list_id": id_prop("A list id from list_company_dedup_lists") }),
            &["list_id"],
            json!({ "readOnlyHint": false, "destructiveHint": true, "idempotentHint": true }),
        ),
        tool(
            "list_new_matches",
            "List new matches of a saved search",
            "The companies a saved search found after it was saved, newest first, each with found_at. Without since, the ones \
             found since it was last opened in the app. Reading them here doesn't change that.",
            page_props(json!({
                "search_id": id_prop("A search id from list_saved_searches"),
                "since": { "type": "string", "format": "date-time", "description": "Optional. Only companies found at or after this" },
            })),
            &["search_id"],
            read.clone(),
        ),
        tool(
            "find_email",
            "Find a person's email",
            "Finds a person's work email from their name and company domain with the project's email finders and verifier, \
             like Splee does for a lead without one, and doesn't add them as a lead. status is found, guessed (a common pattern \
             the verifier accepted), catch_all, not_found, suppressed (on the do-not-contact list), duplicate (a lead in the \
             project has it, named in note) or failed, and email_status verified, or found when no verifier confirmed it. phone \
             is filled only when the project finds phone numbers too. Each call is one of the organization's 200 email searches \
             an hour.",
            json!({
                "project_id": project,
                "first_name": { "type": "string" },
                "last_name": { "type": "string" },
                "company_domain": { "type": "string", "description": "The company's domain or website, like acme.com" },
                "linkedin_url": { "type": "string", "description": "Optional. Their LinkedIn profile, which some finders match on" },
            }),
            &["project_id", "first_name", "company_domain"],
            adds.clone(),
        ),
        tool(
            "find_phone",
            "Find a person's mobile",
            "Finds the mobile number of the person behind a LinkedIn profile with the project's email finders that return \
             mobiles (People Data Labs, RocketReach, Lusha and Prospeo, in the project's order), and doesn't add them as a \
             lead. status is found, with phone in E.164 and source the finder, not_found, suppressed (on the do-not-contact \
             list) or failed, and note says why when nothing was found. Each call is one of the organization's 200 email \
             searches an hour.",
            json!({
                "project_id": project,
                "linkedin_url": { "type": "string", "description": "Their LinkedIn profile, like https://www.linkedin.com/in/jane-doe" },
                "email": { "type": "string", "description": "Optional. An address they're known by, which helps the finders match" },
            }),
            &["project_id", "linkedin_url"],
            adds.clone(),
        ),
        tool(
            "find_emails",
            "Find emails for a list of people",
            "Queues up to 500 people for find_email's search, in the background and within the organization's 200 email \
             searches an hour. Returns task_id for get_email_batch.",
            json!({
                "project_id": project,
                "people": {
                    "type": "array",
                    "maxItems": 500,
                    "items": {
                        "type": "object",
                        "properties": {
                            "first_name": { "type": "string" },
                            "last_name": { "type": "string" },
                            "company_domain": { "type": "string" },
                            "linkedin_url": { "type": "string" },
                            "client_ref": { "type": "string", "description": "Optional. Your own id for the person, given back with their result" },
                        },
                        "required": ["first_name", "company_domain"],
                    },
                },
            }),
            &["project_id", "people"],
            adds.clone(),
        ),
        tool(
            "get_email_batch",
            "Get email batch results",
            "A find_emails batch: status pending or completed, how many of total are searched, and each person's result in the \
             order sent, pending until searched. Results are kept for 30 days.",
            json!({ "task_id": id_prop("A task_id from find_emails") }),
            &["task_id"],
            read.clone(),
        ),
        tool(
            "download_companies",
            "Download company search",
            "The companies a search_companies search finds, in its order, as CSV: name, domain, website, email, phone, address, city, \
             region, postcode, country, category, Overture's confidence, source_id and source_release. Returns the first rows (20 \
             unless per_page says up to 200), more when there are others, and download_url, the whole file from the API. Cells that \
             start like a formula start with an apostrophe. Counts against the organization's 20 downloads an hour, so search first.",
            json!({
                "q": { "type": "string", "description": "Words in the name or category, like \"dental\"" },
                "country": { "type": "string", "description": "Two-letter country code, like GB" },
                "city": { "type": "string", "description": "City name, like Leeds" },
                "category": { "type": "string", "description": "Exact category, like dental_clinic" },
                "has_email": { "type": "boolean", "description": "Only companies that list an email" },
                "has_phone": { "type": "boolean", "description": "Only companies that list a phone" },
                "tech": { "type": "string", "description": "Only companies that use this tool or a tool of this category, like search_companies" },
                "project_id": id_prop("Optional. The project searching, so its dedup lists apply too"),
                "include_hidden": { "type": "boolean", "description": "Optional. True to include the companies on dedup lists" },
                "domains": { "type": "array", "items": { "type": "string" }, "description": "Optional. Only these of the search's companies, up to 500" },
                "per_page": { "type": "integer", "minimum": 1, "maximum": MAX_DOWNLOAD_ROWS, "description": "Rows in the answer, 20 by default" },
            }),
            &[],
            read.clone(),
        ),
        tool(
            "find_lookalikes",
            "Find companies like your customers",
            "Ranks the company database by how much each company has in common with the project's customers: by default the \
             companies of its won deals, plus or minus the ones picked on the Companies page. profile is what at least a third of \
             them share (industry, tools, description words, hiring for the roles you sell to, country and region) and \
             whether each part is on. Each result has a 0-100 score and the reasons that counted most. Seeds, companies already in \
             the project and do-not-contact domains are left out. total counts all of the up to 200 matches.",
            json!({
                "project_id": project,
                "seeds": { "type": "array", "items": { "type": "string" }, "description": "Optional. Customer domains to match instead of the project's" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 200, "description": "Results to return, 25 by default" },
            }),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "add_lookalikes",
            "Add lookalike companies to campaign",
            "Adds companies from find_lookalikes to a campaign, with their score and reasons as why they fit. Pass domains, or top \
             for the best that many. Pass the same seeds as find_lookalikes if you gave it some. Companies no longer among the \
             matches are skipped. Then call find_people.",
            json!({
                "project_id": project,
                "campaign_id": campaign,
                "domains": { "type": "array", "items": { "type": "string" }, "description": "Domains from find_lookalikes" },
                "top": { "type": "integer", "minimum": 1, "maximum": 200, "description": "Instead of domains, add the best this many" },
                "seeds": { "type": "array", "items": { "type": "string" }, "description": "Optional. The seeds find_lookalikes used" },
            }),
            &["project_id", "campaign_id"],
            adds.clone(),
        ),
        tool(
            "search_local_businesses",
            "Search local businesses",
            "Searches Google Maps through the organization's own Google Maps key, connected under Integrations, for local businesses \
             like dentists, gyms or restaurants in a city or ZIP code: up to 60 a place, with category, address, phone, website, \
             rating and review count. where can list up to 20 places split by semicolons, and each business shows once. Each place \
             costs up to 3 calls against the organization's monthly cap, and a place searched in the last 30 days is free. With \
             project_id, known says whether the project already has a business as a company or a lead.",
            json!({
                "what": { "type": "string", "description": "The kind of business, like \"dentists\"" },
                "where": { "type": "string", "description": "A city, area or ZIP code, like \"Austin, TX\", or several split by semicolons" },
                "min_rating": { "type": "number", "minimum": 0, "maximum": 5, "description": "Optional. Only businesses rated at least this" },
                "min_reviews": { "type": "integer", "minimum": 0, "description": "Optional. Only businesses with at least this many reviews" },
                "with_website": { "type": "boolean", "description": "Optional. Only businesses with a website" },
                "project_id": id_prop("Optional. Marks the businesses this project already has"),
            }),
            &["what", "where"],
            read.clone(),
        ),
        tool(
            "add_local_businesses",
            "Add local businesses to campaign",
            "Adds businesses from search_local_businesses to a campaign by place_id, up to 50 at a time. Each with a website becomes \
             a company, and each one a lead: Splee reads its website for a contact address at its own domain, a person's before a \
             shared one like info@. One without a website keeps its phone for calls and texts. Returns what each got, and the ones \
             skipped with why. Then call write_emails.",
            json!({
                "campaign_id": campaign,
                "place_ids": { "type": "array", "items": { "type": "string" }, "description": "place_id values from search_local_businesses" },
            }),
            &["campaign_id", "place_ids"],
            adds.clone(),
        ),
        tool(
            "find_people",
            "Find people",
            "Starts an agent run that finds decision makers and their emails at the campaign's new companies, or only at company_ids or \
             domains, in the campaign's roles or the titles given. Returns run_id. Poll get_run until it's done, then list_leads.",
            json!({
                "campaign_id": campaign,
                "company_ids": { "type": "array", "items": { "type": "string", "format": "uuid" }, "description": "Optional. Only these companies" },
                "domains": {
                    "type": "array", "items": { "type": "string" },
                    "description": "Optional, in place of company_ids. Up to 100 company domains, added to the campaign when the project hasn't got them"
                },
                "titles": { "type": "array", "items": { "type": "string" }, "description": "Optional. Up to 20 job titles to look for instead of the campaign's roles" },
                "per_company": { "type": "integer", "minimum": 1, "maximum": 5, "description": "Optional. People to find at each company, 2 by default" },
            }),
            &["campaign_id"],
            adds.clone(),
        ),
        tool(
            "write_emails",
            "Write emails",
            "Starts an agent run that writes a personal first email for every lead in the campaign that has none. The emails wait as \
             drafts for approval. Returns run_id. Poll get_run until it's done, then list_emails_to_approve.",
            json!({
                "campaign_id": campaign,
                "instructions": { "type": "string", "description": "Optional. Extra guidance for this batch, like \"mention our Leeds office\"" },
            }),
            &["campaign_id"],
            adds.clone(),
        ),
        tool(
            "get_analytics",
            "Get analytics",
            "What's working in a project over a date range, 30 days by default and at most 365: totals with reply, hot and \
             bounce rates by lead, meetings, deals won, unsubscribes and spend split into claude_cost_micros and email_cost_micros \
             (micro-dollars), revenue_cents from the payments of leads through Stripe, Paddle or Lemon Squeezy in the organization's \
             currency (payments in others are counted in ignored_payments) with revenue_per_meeting_cents and return_on_spend (null \
             unless that currency is USD, since spend is in dollars), the same totals for the period before in previous, a daily series (weekly past 90 days), and breakdowns by campaign (with its leads' revenue), mailbox \
             (bouncing flags over 5% bounced), sequence step, A/B test angle, weekday and hour of send in the project's time zone, \
             channel, and the language the emails were written in. Replies, bounces and meetings count for the last email the lead got before them. Totals, campaigns, \
             angles, mailboxes and steps have opened, open_rate, clicked and click_rate, by email out of those sent in the range \
             that tracked opens or clicks, scanners left out. A null rate means none tracked it. Open rates are estimates, since \
             Apple Mail opens every email itself and some people block the pixel.",
            json!({
                "project_id": project,
                "from": { "type": "string", "format": "date", "description": "Optional. First day, as YYYY-MM-DD in the project's time zone" },
                "to": { "type": "string", "format": "date", "description": "Optional. Last day, as YYYY-MM-DD. Today by default" },
                "campaign_id": id_prop("Optional. Only this campaign"),
                "mailbox_id": id_prop("Optional. Only emails from this mailbox"),
            }),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "get_run",
            "Get run status",
            "An agent run's status: queued, running, succeeded, failed or cancelled, with its latest progress line in message and \
             why it failed in error.",
            json!({ "run_id": id_prop("A run_id from create_campaign, find_people or write_emails") }),
            &["run_id"],
            read.clone(),
        ),
        tool(
            "list_tasks",
            "List background tasks",
            "The organization's background work, newest first: agent runs, find_emails batches and import_campaign imports, each \
             with status pending, completed or failed and the url that follows it. For finding a run_id or task_id again.",
            page_props(json!({
                "status": { "type": "string", "enum": ["pending", "completed", "failed"], "description": "Optional" },
                "type": { "type": "string", "enum": ["run", "email_batch", "campaign_import"], "description": "Optional" },
                "kind": { "type": "string", "description": "Optional. Only runs of this kind, like find_people or write_emails" },
                "project_id": id_prop("Optional. Only this project's"),
            })),
            &[],
            read.clone(),
        ),
        tool(
            "list_leads",
            "List leads",
            "A project's leads, newest first, or the likeliest buyers first with sort \"score\". Each has a 0-100 score, its fit \
             and intent parts (0-50 each) and the reasons behind it, the language set on it, if any, and effective_language, the \
             one a campaign set to each lead's language writes to them in.",
            page_props(json!({
                "project_id": project,
                "campaign_id": id_prop("Optional. Only this campaign's leads"),
                "status": { "type": "string", "enum": LEAD_STATUSES, "description": "Optional. Only leads with this status" },
                "q": { "type": "string", "description": "Optional. Matches name, email, company or title" },
                "language": { "type": "string", "description": "Optional. Only leads whose effective_language is this code, like de" },
                "min_score": score_prop(),
                "sort": sort_prop(),
                "owner": owner_prop(),
            })),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "list_companies",
            "List companies",
            "A project's own companies, best fit first, or the likeliest buyers first with sort \"score\". A company's score is its \
             best lead's, or its own while it has no leads. search_companies searches the company database instead.",
            page_props(json!({
                "project_id": project,
                "campaign_id": id_prop("Optional. Only this campaign's companies"),
                "q": { "type": "string", "description": "Optional. Matches name or domain" },
                "min_score": score_prop(),
                "sort": sort_prop(),
            })),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "list_emails_to_approve",
            "List emails to approve",
            "Draft emails waiting for approval, oldest first, with the full subject and body. Show them to the user before approving.",
            page_props(json!({
                "project_id": id_prop("Optional. Only this project"),
                "campaign_id": id_prop("Optional. Only this campaign"),
            })),
            &[],
            read.clone(),
        ),
        tool(
            "approve_email",
            "Approve email",
            "Approves a draft so it's sent from the user's mailboxes on the project's schedule, once the campaign is sending. Only \
             approve emails the user has agreed to send.",
            json!({ "email_id": id_prop("An id from list_emails_to_approve") }),
            &["email_id"],
            json!({ "readOnlyHint": false }),
        ),
        tool(
            "skip_email",
            "Skip email",
            "Cancels a draft so it's never sent. approve_email can still send it later.",
            json!({ "email_id": id_prop("An id from list_emails_to_approve") }),
            &["email_id"],
            adds.clone(),
        ),
        tool(
            "list_replies",
            "List replies",
            "Reply threads from the inbox, the latest first, with the lead, how the reply was classified and the last message cut to \
             300 characters.",
            page_props(json!({
                "project_id": id_prop("Optional. Only this project"),
                "campaign_id": id_prop("Optional. Only this campaign's leads"),
                "filter": { "type": "string", "enum": ["all", "needs_reply", "hot", "auto_replied", "other"], "description": "all by default" },
                "q": { "type": "string", "description": "Optional. Matches the lead's name, email or company" },
                "owner": owner_prop(),
            })),
            &[],
            read.clone(),
        ),
        tool(
            "get_thread",
            "Get thread",
            "A lead's emails both ways, oldest first, with who each one copied in and the names, types and sizes of its files. \
             reply_cc is who reply_to_lead copies in by default.",
            json!({ "lead_id": id_prop("A lead id from list_replies or list_leads") }),
            &["lead_id"],
            read.clone(),
        ),
        tool(
            "reply_to_lead",
            "Reply to lead",
            "Sends a reply to the lead's last email from the mailbox they wrote to. It copies in everyone the lead copied, unless \
             cc_emails names who to copy in instead. Only reply when the user asked to, with the text they agreed to.",
            json!({
                "lead_id": id_prop("A lead id from list_replies or list_leads"),
                "body_text": { "type": "string", "description": "The reply, as plain text" },
                "cc_emails": {
                    "type": "array",
                    "items": { "type": "string" },
                    "maxItems": 20,
                    "description": "Optional. Everyone to copy in, in place of the lead's CC list. An empty list copies in nobody",
                },
                "reply_all": { "type": "boolean", "description": "Optional. false copies in nobody when cc_emails isn't given. true by default" },
            }),
            &["lead_id", "body_text"],
            json!({ "readOnlyHint": false }),
        ),
        tool(
            "mark_replied",
            "Mark replied",
            "Takes a reply the user answered outside Splee, or that needs no answer, out of Needs reply, and cancels the agent's reply \
             draft still waiting for it. The thread comes back when the lead writes again. The lead's status doesn't change.",
            json!({ "lead_id": id_prop("A lead id from list_replies or list_leads") }),
            &["lead_id"],
            json!({ "readOnlyHint": false }),
        ),
        tool(
            "list_deals",
            "List deals",
            "The pipeline's stages in order, and its deals, the latest first, with the lead, stage, value in the currency's main unit \
             and owner. A hot reply opens a deal and a booked meeting moves it to the booked stage by itself.",
            page_props(json!({
                "project_id": id_prop("Optional. Only this project"),
                "status": { "type": "string", "enum": ["open", "won", "lost"], "description": "Optional. Only deals with this status" },
                "lead_id": id_prop("Optional. Only this lead's deals"),
            })),
            &[],
            read.clone(),
        ),
        tool(
            "create_deal",
            "Create deal",
            "Opens a deal for a lead, in the first stage unless you name one. A lead can have only one open deal.",
            json!({
                "lead_id": id_prop("A lead id from list_leads or list_replies"),
                "stage": { "type": "string", "description": "Optional. A stage name from list_deals" },
                "value": { "type": "number", "minimum": 0, "description": "Optional. In the currency's main unit, like 12000" },
                "currency": { "type": "string", "description": "Optional. A three-letter code. The organization's currency by default" },
                "close_date": { "type": "string", "format": "date", "description": "Optional. When it should close, as YYYY-MM-DD" },
                "notes": { "type": "string", "description": "Optional" },
            }),
            &["lead_id"],
            adds.clone(),
        ),
        tool(
            "move_deal",
            "Move deal",
            "Moves a deal to another stage, forward or back. Moving it to a won stage closes it as won, a lost stage as lost, and an \
             open stage reopens it.",
            json!({
                "deal_id": id_prop("A deal id from list_deals"),
                "stage": { "type": "string", "description": "A stage name from list_deals" },
                "value": { "type": "number", "minimum": 0, "description": "Optional. The final value, when it's won" },
                "lost_reason": { "type": "string", "description": "Optional. Why it was lost" },
            }),
            &["deal_id", "stage"],
            adds.clone(),
        ),
        tool(
            "set_lead_language",
            "Set lead language",
            "Sets the language a lead is written to in, for campaigns that write in each lead's own language. Empty goes back to \
             picking it from their country, then their company's website, then English. Returns effective_language, the one now \
             used, and effective_language_source: set, country, site or default.",
            json!({
                "lead_id": id_prop("A lead id from list_leads or list_replies"),
                "language": { "type": "string", "description": "A code like de, fr or es, or its English name, or \"\" to clear it" },
            }),
            &["lead_id", "language"],
            adds.clone(),
        ),
        tool(
            "set_campaign_language",
            "Set campaign language",
            "Sets the language a campaign writes its emails, follow-ups, texts and LinkedIn messages in: lead (the default) to write to \
             each lead in their own language, en for English, or another language code. Emails already written keep theirs until they're rewritten. \
             Drafted replies always answer in the language the lead wrote in.",
            json!({
                "campaign_id": campaign,
                "language": { "type": "string", "enum": LANGUAGE_CHOICES.as_slice(), "description": "en, another language code, or lead" },
            }),
            &["campaign_id", "language"],
            adds.clone(),
        ),
        tool(
            "get_campaign",
            "Get campaign",
            "A campaign's whole definition as its page shows it: status and status_reason, targeting, the brief its emails are \
             written from (pain_points, angle, and the A/B test's angles in ab_test), language, tracking, the project's schedule and \
             follow-ups it sends on, an imported campaign's sequence, and its funnel. read_only lists what can't be changed here and why.",
            json!({ "campaign_id": campaign }),
            &["campaign_id"],
            read.clone(),
        ),
        tool(
            "get_campaign_send_state",
            "Is a campaign sending",
            "Whether a campaign's emails go out now and, if not, the one thing holding them: state (sending, scheduled, archived, \
             not_started, paused, project_paused, no_mailbox, awaiting_approval, nothing_queued, monthly_cap_reached, \
             lead_cap_reached, daily_limit_reached, campaign_limit_reached, mailboxes_full or outside_hours), message, next_action \
             (the call that clears it, null when it only waits), today's counts with next_send_at, and the allowed_actions. Ask the \
             user before a next_action that starts sending.",
            json!({ "campaign_id": campaign }),
            &["campaign_id"],
            read.clone(),
        ),
        tool(
            "update_campaign",
            "Update campaign",
            "Changes a campaign's definition as its page does and returns it as get_campaign does. Pass only what changes. Each \
             targeting list sent replaces that list, and the others stay. Emails already written keep their brief and language \
             until they're rewritten. The schedule and follow-ups are the project's, so they're refused here.",
            json!({
                "campaign_id": campaign,
                "name": { "type": "string" },
                "description": { "type": "string", "description": "Who the campaign is for, in a sentence or two" },
                "targeting": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "industries": texts, "roles": texts, "company_sizes": texts, "countries": texts, "exclude_countries": texts,
                        "keywords": texts, "exclude_keywords": texts,
                    },
                },
                "pain_points": { "type": "string", "description": "The problems the emails speak to" },
                "angle": { "type": "string", "description": "How the emails pitch the offer" },
                "language": { "type": "string", "enum": LANGUAGE_CHOICES.as_slice(), "description": "en, another language code, or lead" },
                "track_clicks": { "type": "boolean" },
                "track_opens": { "type": "boolean" },
                "lead_pages": { "type": "boolean", "description": "A personal page for each lead, written with the first email" },
                "daily_limit": {
                    "type": ["integer", "null"], "minimum": 1, "maximum": 5000,
                    "description": "The most first emails and follow-ups it sends a day, on top of the project's limit. null takes it off"
                },
            }),
            &["campaign_id"],
            control.clone(),
        ),
        tool(
            "get_my_copy",
            "Get campaign first email",
            "A campaign's first email setup. mode instructions has the agent write every first email (from the campaign's angle, \
             in instructions). mode my_copy sends the user's own subject and body with each lead's details filled in, to 9 leads in \
             10, and the AI email to the other tenth so the two can be compared, with how each did in results. Follow-ups are \
             written as before. source project means the campaign sends the project's first email, campaign its own.",
            json!({ "campaign_id": campaign }),
            &["campaign_id"],
            read.clone(),
        ),
        tool(
            "update_my_copy",
            "Update campaign first email",
            MY_COPY_TEXT_RULES,
            json!({
                "campaign_id": campaign,
                "mode": { "type": "string", "enum": ["instructions", "my_copy"], "description": "Optional. my_copy sends the user's text" },
                "subject": { "type": "string", "description": "Optional. Empty sends \"<your company> x <their company>\"" },
                "body": { "type": "string", "description": "Optional. Needed to turn my_copy on" },
            }),
            &["campaign_id"],
            control.clone(),
        ),
        tool(
            "set_my_copy_source",
            "Set campaign first email source",
            "project makes the campaign send the project's first email, campaign gives it its own. Nothing is lost: switching back \
             brings the other text back. A campaign's first own text starts as a copy of the project's.",
            json!({ "campaign_id": campaign, "source": { "type": "string", "enum": ["project", "campaign"] } }),
            &["campaign_id", "source"],
            control.clone(),
        ),
        tool(
            "preview_my_copy",
            "Preview campaign first email",
            "The user's text as the campaign's first 3 leads get it: variables and {a|b} filled in, and each {{ai: …}} line shown \
             as its instruction, since the agent writes those with the email. A lead that gets the AI email instead says why in note.",
            json!({ "campaign_id": campaign }),
            &["campaign_id"],
            read.clone(),
        ),
        tool(
            "get_project_my_copy",
            "Get project first email",
            "The project's default first email, which every campaign in campaign_ids sends: mode, subject and body as in \
             get_my_copy, and the project's extra instructions for every AI email.",
            json!({ "project_id": project }),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "update_project_my_copy",
            "Update project first email",
            MY_COPY_TEXT_RULES,
            json!({
                "project_id": project,
                "mode": { "type": "string", "enum": ["instructions", "my_copy"], "description": "Optional. my_copy sends the user's text" },
                "subject": { "type": "string", "description": "Optional. Empty sends \"<your company> x <their company>\"" },
                "body": { "type": "string", "description": "Optional. Needed to turn my_copy on" },
            }),
            &["project_id"],
            control.clone(),
        ),
        tool(
            "start_campaign",
            "Start campaign",
            "The campaign's Start button: it sends on the project's schedule, and its drafts are approved for sending. A campaign \
             that's already sending is left as it is. Sending also needs the project's sending on, see update_project_controls.",
            json!({ "campaign_id": campaign }),
            &["campaign_id"],
            control.clone(),
        ),
        tool(
            "stop_campaign",
            "Stop campaign",
            "The campaign's Pause button: its unsent first emails and follow-ups go back to drafts until it starts again. Replies \
             still come in, and answers to them stay queued. A campaign that isn't sending is left as it is.",
            json!({ "campaign_id": campaign }),
            &["campaign_id"],
            control.clone(),
        ),
        tool(
            "approve_campaign",
            "Approve campaign drafts",
            "Approves every first email and follow-up waiting as a draft in the campaign, like approve_email on each, and says how \
             many in approved. In a sending campaign they go out on the project's schedule. A paused one holds them until it starts.",
            json!({ "campaign_id": campaign }),
            &["campaign_id"],
            control.clone(),
        ),
        tool(
            "archive_campaign",
            "Archive campaign",
            "Puts away a campaign you're done with: it stops sending like stop_campaign and drops out of list_campaigns. Its leads, \
             emails and stats stay, and replies still come in. Archiving an archived campaign changes nothing. Returns the campaign.",
            json!({ "campaign_id": campaign }),
            &["campaign_id"],
            control.clone(),
        ),
        tool(
            "unarchive_campaign",
            "Unarchive campaign",
            "Brings an archived campaign back to list_campaigns, paused. It only sends once start_campaign starts it. Returns the campaign.",
            json!({ "campaign_id": campaign }),
            &["campaign_id"],
            control.clone(),
        ),
        tool(
            "update_project_controls",
            "Update project controls",
            "Changes a project's sending controls and returns them all. Pass only what changes, or nothing to read them. sending \
             turns the whole project's sending on or off, and on needs an active mailbox. A field it doesn't take is refused.",
            json!({
                "project_id": project,
                "sending": { "type": "boolean", "description": "Optional. The project's Start and Pause sending" },
                "daily_email_limit": { "type": "integer", "minimum": 1, "maximum": 5000, "description": "Optional. Emails a day" },
                "daily_text_limit": { "type": "integer", "minimum": 1, "maximum": 1000, "description": "Optional. Texts a day" },
                "timezone": { "type": "string", "description": "Optional. The send window's time zone, like Europe/London" },
                "send_days": { "type": "array", "items": { "type": "integer", "minimum": 1, "maximum": 7 }, "description": "Optional. 1 is Monday, 7 Sunday" },
                "send_start_hour": { "type": "integer", "minimum": 0, "maximum": 23, "description": "Optional" },
                "send_end_hour": { "type": "integer", "minimum": 1, "maximum": 24, "description": "Optional. After send_start_hour" },
                "autopilot": { "type": "boolean", "description": "Optional. Keeps sending campaigns stocked with written emails" },
                "autopilot_budget_micros": { "type": "integer", "minimum": 0, "description": "Optional. Claude spend a month in micro-dollars" },
                "autopilot_lead_cap": { "type": "integer", "minimum": 0, "maximum": 100000, "description": "Optional. New leads emailed a month" },
                "autopilot_auto_send": { "type": "boolean", "description": "Optional. Autopilot's emails go out without approval" },
                "review_window_hours": { "type": ["integer", "null"], "minimum": 1, "maximum": 72,
                                         "description": "Optional. Drafts nobody reviewed go out after this many hours. null keeps them waiting" },
                "monthly_cap_micros": { "type": ["integer", "null"], "minimum": 1000000, "maximum": 1_000_000_000_000_i64,
                                        "description": "Optional. Email spend a month in micro-dollars. null for no cap" },
                "match_provider": { "type": "boolean",
                                    "description": "Optional. First emails to Gmail or Outlook leads go from a mailbox on the same provider" },
                "sender_name": { "type": "string", "maxLength": 100,
                                 "description": "Optional. The name the agent signs emails with, not an address. Empty for the team" },
            }),
            &["project_id"],
            control,
        ),
        tool(
            "get_monthly_cap",
            "Get monthly cap",
            "The project's cap on email spend a calendar month (UTC), what its sent emails cost since the 1st, and whether first emails \
             and follow-ups are waiting for next month because it was reached. Replies always go out.",
            json!({ "project_id": project }),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "list_hot_leads",
            "List hot leads",
            "Every lead across the organization whose reply was hot, newest first: name, email, title, company, domain, LinkedIn, \
             their campaign, the reply that made them hot and why_hot, and became_hot_at. Pass since as the newest became_hot_at \
             you've seen to get only new ones.",
            page_props(json!({
                "since": { "type": "string", "format": "date-time", "description": "Optional. Only leads that became hot after this" },
                "project_id": id_prop("Optional. Only this project's leads"),
            })),
            &[],
            read.clone(),
        ),
        tool(
            "get_inbox_changes",
            "Get new replies",
            "A project's replies that came in since cursor, oldest first, each once. Pass next_cursor as cursor on the next call. \
             On the first call pass since or nothing for every reply. has_more means call again now. A reply shows up about 5 \
             seconds after it arrives, and classification is null until the classifier has read it.",
            json!({
                "project_id": project,
                "campaign_id": id_prop("Optional. Only this campaign's replies"),
                "cursor": { "type": "string", "description": "Optional. next_cursor from the last call" },
                "since": { "type": "string", "format": "date-time", "description": "Optional. On the first call, only replies after this" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 500, "description": "Optional. At most this many, 100 by default" },
            }),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "list_erased_leads",
            "List erased leads",
            "Leads someone deleted or forgot in Splee since cursor, oldest first, each once, so the user can delete what they keep of \
             each one in their CRM or sheets. Each has only lead_id, project_id, campaign_id, forgotten and erased_at, since the \
             person's name and address are gone. Pass next_cursor as cursor on the next call. has_more means call again now.",
            json!({
                "project_id": id_prop("Optional. Only this project"),
                "cursor": { "type": "string", "description": "Optional. next_cursor from the last call" },
                "since": { "type": "string", "format": "date-time", "description": "Optional. On the first call, only leads erased after this" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 500, "description": "Optional. At most this many, 100 by default" },
            }),
            &[],
            read.clone(),
        ),
        tool(
            "assign_lead",
            "Assign lead",
            "Gives a lead to someone on the team, who then gets its hot-reply alerts, or takes it back with owner null. The project's \
             assignment rule never moves a lead that has an owner.",
            json!({
                "lead_id": id_prop("A lead id from list_leads or list_replies"),
                "owner": { "type": ["string", "null"], "description": "A team member's email or user id, or null for nobody" },
            }),
            &["lead_id", "owner"],
            adds.clone(),
        ),
        tool(
            "list_call_tasks",
            "List call tasks",
            "A project's calls to make, overdue first, then the calls made in the last 14 days. bucket is overdue, today or done. \
             local_time is the time where the lead is, and outside_hours says why not to call now: calls go out between 8:00 and 20:00 \
             their time.",
            json!({
                "project_id": id_prop("A project id from list_projects"),
                "status": { "type": "string", "enum": ["open", "done"], "description": "Optional. Both by default" },
                "owner": owner_prop(),
            }),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "log_call",
            "Log call",
            "Records a call made with a lead and what happened. It closes the lead's open call task, so their sequence goes on. \
             meeting_booked books the lead, not_interested stops their sequence, and wrong_number clears their phone number.",
            json!({
                "lead_id": id_prop("A lead id from list_call_tasks or list_leads"),
                "outcome": { "type": "string", "enum": ["connected", "voicemail", "no_answer", "wrong_number", "not_interested", "meeting_booked"] },
                "notes": { "type": "string", "description": "Optional. What was said" },
            }),
            &["lead_id", "outcome"],
            adds.clone(),
        ),
        tool(
            "list_meetings",
            "List meetings",
            "A lead's sales meetings from Fireflies, Fathom, Gong, Zoom or tl;dv, oldest first: the title, when, who from their side, the \
             agent's summary and next steps, and the deal stage it suggested. stage_status is pending until someone accepts or \
             dismisses that stage in Splee. follow_up_status is the status of the follow-up email drafted after it, draft while it \
             waits for approval.",
            json!({ "lead_id": id_prop("A lead id from list_leads or list_replies") }),
            &["lead_id"],
            read.clone(),
        ),
        tool(
            "list_sheet_connections",
            "List Google Sheets",
            "A project's Google Sheets tabs: kind import is a tab leads come from, read every 15 minutes when keep_importing is on, \
             and kind export a tab that gets a row for each hot lead, booked meeting or won deal in events. rows_added counts the \
             leads or rows Splee added so far, and error says why the last read or write failed.",
            json!({ "project_id": id_prop("A project id from list_projects") }),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "import_from_sheet",
            "Import from Google Sheet",
            "Reads an import tab now and adds the rows it hasn't seen as leads in its campaign. Rows are told apart by email, or by \
             their content when they have none. Suppressed and duplicate addresses are skipped, with the reason in errors.",
            json!({
                "project_id": id_prop("A project id from list_projects"),
                "connection_id": id_prop("An import tab's id from list_sheet_connections"),
            }),
            &["project_id", "connection_id"],
            adds.clone(),
        ),
        tool(
            "list_job_changes",
            "List job changes",
            "People who bought, booked a meeting or replied hot and then moved to a new company, newest first, from their LinkedIn \
             profiles. relationship is won, booked or hot. new_lead_id is the lead Splee made at the new company, whose email the \
             finders look up. watching counts the people on the watch list, and connected says whether LinkedIn is connected through \
             Unipile, which the checks need.",
            json!({ "project_id": id_prop("A project id from list_projects") }),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "add_research_question",
            "Add a research question",
            "Adds a question the agent answers about every company in the project, like \"Do they sell to hospitals?\" or \"Which \
             CRM do they use?\". It shows as a column on the Companies page. answer_type yes_no gets yes or no, text a short answer. \
             With use_in_emails, the email writer can use the answers. Up to 10 a project. Then call research_companies.",
            json!({
                "project_id": project,
                "question": { "type": "string", "maxLength": 300, "description": "The question, about one company" },
                "label": { "type": "string", "maxLength": 40, "description": "Optional. A short column name. The question's first words by default" },
                "answer_type": { "type": "string", "enum": ["yes_no", "text"], "description": "yes_no by default" },
                "use_in_emails": { "type": "boolean", "description": "Let the email writer use the answers. False by default" },
            }),
            &["project_id", "question"],
            adds.clone(),
        ),
        tool(
            "research_companies",
            "Research companies",
            "Queues the agent to answer the project's research questions for its companies from their websites and the web, up to \
             200 companies a call, best score first. Each company is one Claude call, costed like any run. By default every \
             question, every company that isn't excluded, and only answers not found yet. companies says how many were queued and \
             more how many are left for another call. Answers take a few minutes, then show in list_research.",
            json!({
                "project_id": project,
                "question_ids": { "type": "array", "items": { "type": "string", "format": "uuid" }, "description": "Optional. Only these questions" },
                "company_ids": { "type": "array", "items": { "type": "string", "format": "uuid" }, "description": "Optional. Only these companies" },
                "campaign_id": id_prop("Optional. Only this campaign's companies"),
                "redo": { "type": "boolean", "description": "Ask again where an answer was already found" },
                "dry_run": { "type": "boolean", "description": "Only count what would be queued" },
            }),
            &["project_id"],
            adds.clone(),
        ),
        tool(
            "list_research",
            "List research answers",
            "The project's research questions and each company's answers by question id, each with its question's label, best score first. An answer's status is \
             answered, unknown (nothing the agent read said), queued (being researched) or failed. Pass question_id with answer to \
             filter: yes, no, answered, unknown, or none for not researched yet.",
            page_props(json!({
                "project_id": project,
                "campaign_id": id_prop("Optional. Only this campaign's companies"),
                "q": { "type": "string", "description": "Optional. Search names and domains" },
                "question_id": id_prop("Optional. With answer, filter by this question"),
                "answer": { "type": "string", "enum": ["yes", "no", "answered", "unknown", "none"], "description": "Optional. The answer to filter by" },
            })),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "list_company_signals",
            "List company funding and news",
            "Funding and news at the project's companies, newest first: Form D filings from SEC EDGAR (kind funding_filing, with \
             amount sold and offered in US dollars) and posts on each company's own blog or newsroom feed about funding, a launch, an \
             acquisition, an expansion or a new leader. headline is the line the score and the email writer use, like \"Raised $12M \
             (SEC filing, Oct 3)\". Signals count for 90 days. company_id is set when the company is in the project, and leads counts \
             its leads.",
            json!({
                "project_id": id_prop("A project id from list_projects"),
                "kind": { "type": "string", "enum": crate::signals::KINDS, "description": "Optional. Only this kind" },
                "days": { "type": "integer", "minimum": 1, "description": "Optional. Only signals from the last this many days" },
                "limit": { "type": "integer", "minimum": 1, "maximum": MAX_SIGNALS, "description": "At most this many, 25 by default" },
            }),
            &["project_id"],
            read,
        ),
        tool(
            "add_to_watch_list",
            "Add to job change watch list",
            "Adds past customers or champions to a project's watch list, so Splee notices when they move to a new company. Each \
             person needs an email or a LinkedIn URL, and the checks need the URL. Someone already on the list isn't added twice.",
            json!({
                "project_id": id_prop("A project id from list_projects"),
                "relationship": { "type": "string", "enum": ["won", "booked", "hot"],
                                  "description": "What they did with you: won (bought), booked (a meeting) or hot (replied with interest). Defaults to won" },
                "people": { "type": "array", "items": { "type": "object", "properties": {
                    "name": { "type": "string" }, "email": { "type": "string" }, "linkedin_url": { "type": "string" },
                    "company": { "type": "string" }, "company_domain": { "type": "string" }, "title": { "type": "string" },
                } } },
            }),
            &["project_id", "people"],
            adds,
        ),
    ]
}

fn owner_prop() -> Value {
    json!({ "type": "string", "description": "Optional. Only leads of this team member, by email or user id, or \"unassigned\"" })
}

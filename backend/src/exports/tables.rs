//! What each exported table holds, one JSON object per row, built by Postgres so the keys come out in this order. SPEC.md
//! documents every key. Changing a key's name, type or meaning is a new SCHEMA_VERSION, and adding a key is not.
//!
//! Every query takes the organization as $1 and reaches its rows through projects.org_id, so another organization's rows
//! can't come out. Tables with bodies take $2, whether to include them. The columns are listed by hand, so nothing secret
//! (unsubscribe tokens, open tokens, encrypted keys, password hashes) is ever in a file.

pub const SCHEMA_VERSION: i32 = 1;

pub struct Table {
    pub name: &'static str,
    /// For the settings page.
    pub label: &'static str,
    /// Whether the query takes $2, include_bodies.
    pub bodies: bool,
    pub sql: &'static str,
}

pub const TABLES: [Table; 9] = [
    Table {
        name: "leads",
        label: "Leads",
        bodies: false,
        sql: "select json_build_object('id', l.id, 'project_id', l.project_id, 'campaign_id', l.segment_id, 'company_id', l.company_id,
                'full_name', l.full_name, 'first_name', l.first_name, 'last_name', l.last_name, 'title', l.title, 'email', l.email,
                'email_status', l.email_status, 'phone', l.phone, 'linkedin_url', l.linkedin_url, 'x_url', l.x_url, 'company_name', l.company_name,
                'company_domain', l.company_domain, 'city', l.city, 'country_code', l.country_code, 'language', l.language, 'source', l.source,
                'status', l.status, 'status_changed_at', l.status_changed_at, 'owner_id', l.owner_id, 'score', l.score, 'notes', l.notes,
                'last_contacted_at', l.last_contacted_at, 'meeting_at', l.meeting_at, 'created_at', l.created_at)::text
              from leads l join projects p on p.id = l.project_id where p.org_id = $1",
    },
    Table {
        name: "companies",
        label: "Companies",
        bodies: false,
        sql: "select json_build_object('id', c.id, 'project_id', c.project_id, 'campaign_id', c.segment_id, 'domain', c.domain, 'name', c.name,
                'description', c.description, 'city', c.city, 'country_code', c.country_code, 'address', c.address, 'phone', c.phone,
                'employees', c.employees, 'fit_score', c.fit_score, 'fit_reason', c.fit_reason, 'score', c.score, 'status', c.status,
                'source_url', c.source_url, 'created_at', c.created_at)::text
              from companies c join projects p on p.id = c.project_id where p.org_id = $1",
    },
    Table {
        name: "campaigns",
        label: "Campaigns",
        bodies: false,
        sql: "select json_build_object('id', s.id, 'project_id', s.project_id, 'name', s.name, 'description', s.description, 'status', s.status,
                'angle', s.angle, 'pain_points', s.pain_points, 'language', s.language, 'track_opens', s.track_opens, 'track_clicks', s.track_clicks,
                'created_at', s.created_at)::text
              from segments s join projects p on p.id = s.project_id where p.org_id = $1",
    },
    Table {
        name: "emails",
        label: "Sent emails",
        bodies: true,
        // Opens and clicks leave out the ones tracking.rs found were a scanner or a bot, as the app counts them.
        sql: "select json_build_object('id', m.id, 'project_id', m.project_id, 'lead_id', m.lead_id, 'campaign_id', m.segment_id,
                'mailbox_id', m.mailbox_id, 'channel', m.channel, 'kind', m.kind, 'step', m.step, 'variant_id', m.variant_id,
                'from_email', m.from_email, 'to_email', m.to_email, 'subject', m.subject, 'body', case when $2 then m.body end,
                'language', m.language, 'status', m.status, 'error', m.error, 'sent_at', m.sent_at,
                'opens', (select count(*) from tracking_hits h where h.email_id = m.id and h.kind = 'open' and h.filter_reason is null),
                'clicks', (select count(*) from tracking_hits h where h.email_id = m.id and h.kind = 'click' and h.filter_reason is null),
                'replies', (select count(*) from email_messages r where r.lead_id = m.lead_id and r.direction = 'inbound' and r.in_reply_to = m.message_id),
                'created_at', m.created_at)::text
              from email_messages m join projects p on p.id = m.project_id
              where p.org_id = $1 and m.direction = 'outbound' and m.status in ('sending', 'sent', 'failed')",
    },
    Table {
        name: "replies",
        label: "Replies",
        bodies: true,
        sql: "select json_build_object('id', m.id, 'project_id', m.project_id, 'lead_id', m.lead_id, 'campaign_id', m.segment_id,
                'mailbox_id', m.mailbox_id, 'channel', m.channel,
                'email_id', (select o.id from email_messages o where o.lead_id = m.lead_id and o.direction = 'outbound' and o.message_id = m.in_reply_to limit 1),
                'from_email', m.from_email, 'to_email', m.to_email, 'subject', m.subject, 'body', case when $2 then m.body end,
                'classification', m.classification, 'language', m.language, 'received_at', m.created_at, 'handled_at', m.handled_at)::text
              from email_messages m join projects p on p.id = m.project_id where p.org_id = $1 and m.direction = 'inbound'",
    },
    Table {
        name: "deals",
        label: "Deals",
        bodies: false,
        sql: "select json_build_object('id', d.id, 'project_id', d.project_id, 'lead_id', d.lead_id, 'stage_id', d.stage_id, 'stage', s.name,
                'status', d.status, 'value_cents', d.value_cents, 'currency', d.currency, 'close_date', d.close_date, 'owner_id', d.owner_id,
                'source', d.source, 'lost_reason', d.lost_reason, 'notes', d.notes, 'stage_changed_at', d.stage_changed_at, 'closed_at', d.closed_at,
                'created_at', d.created_at)::text
              from deals d join projects p on p.id = d.project_id join deal_stages s on s.id = d.stage_id where p.org_id = $1",
    },
    Table {
        name: "company_signals",
        label: "Funding and news",
        bodies: false,
        sql: "select json_build_object('id', g.id, 'project_id', g.project_id, 'domain', g.domain, 'kind', g.kind, 'source', g.source,
                'happened_on', g.happened_on, 'title', g.title, 'amount', g.amount, 'offered', g.offered, 'industry', g.industry, 'url', g.url,
                'first_seen', g.first_seen)::text
              from company_signals g join projects p on p.id = g.project_id where p.org_id = $1",
    },
    Table {
        name: "job_openings",
        label: "Job openings",
        bodies: false,
        sql: "select json_build_object('id', j.id, 'project_id', j.project_id, 'domain', j.domain, 'provider', j.provider, 'title', j.title,
                'role', j.role, 'department', j.department, 'location', j.location, 'url', j.url, 'excerpt', j.excerpt, 'posted_at', j.posted_at,
                'first_seen', j.first_seen, 'last_seen', j.last_seen, 'closed_at', j.closed_at)::text
              from job_openings j join projects p on p.id = j.project_id where p.org_id = $1",
    },
    Table {
        name: "scores",
        label: "Scores",
        bodies: false,
        sql: "select json_build_object('kind', 'lead', 'id', l.id, 'project_id', l.project_id, 'score', l.score, 'fit', l.score_fit,
                'intent', l.score_intent, 'reasons', l.score_reasons, 'scored_at', l.scored_at)::text
              from leads l join projects p on p.id = l.project_id where p.org_id = $1
              union all
              select json_build_object('kind', 'company', 'id', c.id, 'project_id', c.project_id, 'score', c.score, 'fit', c.score_fit,
                'intent', c.score_intent, 'reasons', c.score_reasons, 'scored_at', c.scored_at)::text
              from companies c join projects p on p.id = c.project_id where p.org_id = $1",
    },
];

pub fn find(name: &str) -> Option<&'static Table> {
    TABLES.iter().find(|t| t.name == name)
}

'use strict';

// The calls every trigger, action and search shares.

/** Accepts the address people open Splee at, or the base URL the Integrations page shows, with or without a trailing slash. */
const apiBase = (raw) => {
  const url = String(raw || '').trim().replace(/\/+$/, '');
  return /\/api\/v1$/.test(url) ? url : `${url}/api/v1`;
};

const url = (bundle, path) => apiBase(bundle.authData.baseUrl) + path;

/** Zapier dedupes trigger items on id, and a lead event has none of its own, so it's made from what the event is about. */
const withId = (payload) => ({
  id: [payload.event, payload.lead && payload.lead.id, payload.deal ? payload.deal.id : payload.reply ? payload.reply.id : payload.previous_status]
    .filter(Boolean)
    .join(':'),
  ...payload,
});

/** Zapier sends a field left empty as "" or leaves it out. The API takes neither for an id, so those are dropped. */
const filled = (fields) => Object.fromEntries(Object.entries(fields).filter(([, v]) => v !== undefined && v !== null && v !== ''));

const includeApiKey = (request, z, bundle) => {
  if (bundle.authData && bundle.authData.apiKey) {
    request.headers = { ...request.headers, Authorization: `Bearer ${bundle.authData.apiKey}` };
  }
  return request;
};

/** Splee answers errors with { "error": "..." }, which says more than Zapier's generic message for the status. */
const explainErrors = (response, z) => {
  if (response.status >= 400 && !(response.request && response.request.skipThrowForStatus)) {
    let message;
    try {
      message = (response.data || JSON.parse(response.content)).error;
    } catch {
      message = undefined;
    }
    if (response.status === 401) message = 'Splee refused the API key. It may have been revoked. Create a new one on the API keys page.';
    throw new z.errors.Error(message || `Splee answered ${response.status}`, 'SpleeError', response.status);
  }
  return response;
};

/** The project's lead with this email, or an error a person can act on. */
const findLead = async (z, bundle, email, projectId) => {
  const response = await z.request({ url: url(bundle, '/leads'), params: filled({ email, project_id: projectId }) });
  const lead = response.data[0];
  if (!lead) throw new z.errors.Error(`No lead in that project has the email ${email}`, 'NotFound', 404);
  return lead;
};

// Keep in sync with LEAD_STATUSES in backend/src/routes/util_input.rs.
const STATUSES = {
  new: 'New',
  queued: 'Queued',
  contacted: 'Contacted',
  replied: 'Replied',
  hot: 'Hot',
  meeting_booked: 'Meeting booked',
  not_interested: 'Not interested',
  bounced: 'Bounced',
  unsubscribed: 'Unsubscribed',
  do_not_contact: 'Do not contact',
};

const projectField = (required) => ({
  key: 'project_id',
  label: 'Project',
  dynamic: 'project.id.name',
  required,
  helpText: required ? undefined : 'Leave it empty for every project.',
});

module.exports = { apiBase, url, withId, filled, includeApiKey, explainErrors, findLead, STATUSES, projectField };

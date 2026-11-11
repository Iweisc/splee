'use strict';

// Runs without network: z.request is a fake that answers from a table and keeps every request it got.
// From this folder: npm install, then npm test (or node --test).

const { test } = require('node:test');
const assert = require('node:assert/strict');

const App = require('../index');
const { apiBase, explainErrors, includeApiKey, withId } = require('../lib/splee');

const BASE = 'https://splee.example.com';
const PROJECT = '9b1f6c2e-0000-4000-8000-000000000001';

class ZapierError extends Error {
  constructor(message, type, status) {
    super(message);
    Object.assign(this, { type, status });
  }
}

/** A z whose request answers `routes["METHOD /path"]`: the data, or a function of the request giving { status, data }. */
const fakeZ = (routes) => {
  const sent = [];
  const z = {
    errors: { Error: ZapierError },
    request: async (req) => {
      const method = req.method || 'GET';
      sent.push({ ...req, method });
      const path = req.url.replace(`${BASE}/api/v1`, '');
      const answer = routes[`${method} ${path}`];
      if (answer === undefined) throw new Error(`no fake for ${method} ${path}`);
      const { status = 200, data } = typeof answer === 'function' ? answer(req) : { data: answer };
      const response = { status, data, request: req, throwForStatus: () => { throw new ZapierError(`status ${status}`); } };
      if (status >= 400 && !req.skipThrowForStatus) explainErrors({ ...response, content: JSON.stringify(data) }, z);
      return response;
    },
  };
  return { z, sent };
};

const bundle = (inputData = {}, extra = {}) => ({ authData: { baseUrl: BASE, apiKey: 'splee_k' }, inputData, ...extra });

test('the app definition passes Zapier schema validation', () => {
  // What `zapier validate` runs, without the CLI.
  const schema = require('zapier-platform-core/src/tools/schema');
  const errors = schema.validateApp(schema.compileApp({ ...App }));
  assert.deepEqual(errors.map((e) => e.stack), []);
});

test('the base URL can be the Splee address or the API base URL', () => {
  for (const raw of [BASE, `${BASE}/`, `${BASE}/api/v1`, ` ${BASE}/api/v1/ `]) assert.equal(apiBase(raw), `${BASE}/api/v1`);
});

test('the connection test reads the organization from /me', async () => {
  const { z, sent } = fakeZ({ 'GET /me': { organization: { id: 'o1', name: 'Acme' }, projects: [] } });
  const me = await App.authentication.test(z, bundle());
  assert.equal(me.organization.name, 'Acme');
  assert.equal(sent[0].url, `${BASE}/api/v1/me`);
  assert.equal(App.authentication.connectionLabel, '{{organization.name}}');
});

test('every request carries the key as a bearer token', () => {
  const req = includeApiKey({ url: 'x', headers: { Accept: 'application/json' } }, null, bundle());
  assert.deepEqual(req.headers, { Accept: 'application/json', Authorization: 'Bearer splee_k' });
});

test("errors say what Splee said, and a refused key says what to do", () => {
  const { z } = fakeZ({});
  assert.throws(() => explainErrors({ status: 400, data: { error: 'Invalid email: nope' }, request: {} }, z), /Invalid email: nope/);
  assert.throws(() => explainErrors({ status: 401, data: { error: 'Not signed in' }, request: {} }, z), /revoked/);
  assert.equal(explainErrors({ status: 404, data: {}, request: { skipThrowForStatus: true } }, z).status, 404);
});

const EVENTS = {
  new_hot_reply: 'hot_lead',
  lead_replied: 'lead_replied',
  meeting_booked: 'meeting_booked',
  lead_status_changed: 'status_changed',
  deal_won: 'deal.won',
  new_lead: 'lead_added',
};

test('each trigger subscribes to its own event, with the project when one is picked', async () => {
  for (const [key, event] of Object.entries(EVENTS)) {
    const trigger = App.triggers[key].operation;
    assert.equal(trigger.type, 'hook', key);
    const { z, sent } = fakeZ({ 'POST /hooks': (req) => ({ data: { id: 'sub1', ...req.body } }) });
    const made = await trigger.performSubscribe(z, bundle({ project_id: PROJECT }, { targetUrl: 'https://hooks.zapier.com/1' }));
    assert.deepEqual(sent[0].body, { event, target_url: 'https://hooks.zapier.com/1', project_id: PROJECT });
    assert.equal(made.id, 'sub1');
    await trigger.performSubscribe(z, bundle({ project_id: '' }, { targetUrl: 'https://hooks.zapier.com/2' }));
    assert.deepEqual(sent[1].body, { event, target_url: 'https://hooks.zapier.com/2' }, 'every project');
    assert.equal(trigger.sample.event, event);
    assert.ok(trigger.sample.id, `${key} sample has an id`);
  }
});

test('unsubscribing removes the subscription, and one Splee already turned off is fine', async () => {
  const trigger = App.triggers.new_hot_reply.operation;
  const { z, sent } = fakeZ({ 'DELETE /hooks/sub1': { ok: true }, 'DELETE /hooks/gone': () => ({ status: 404, data: { error: 'Not found' } }) });
  assert.deepEqual(await trigger.performUnsubscribe(z, bundle({}, { subscribeData: { id: 'sub1' } })), { ok: true });
  assert.deepEqual(await trigger.performUnsubscribe(z, bundle({}, { subscribeData: { id: 'gone' } })), { ok: true });
  assert.equal(sent[0].method, 'DELETE');
});

test('a posted event becomes one item with an id, and samples come from Splee', async () => {
  const payload = { event: 'hot_lead', lead: { id: 'l1', email: 'sarah@northwind.example' }, reply: { id: 'r1' } };
  const trigger = App.triggers.new_hot_reply.operation;
  assert.deepEqual(trigger.perform(null, bundle({}, { cleanedRequest: payload })), [{ id: 'hot_lead:l1:r1', ...payload }]);
  const { z, sent } = fakeZ({ 'GET /hooks/samples/deal.won': [{ event: 'deal.won', lead: { id: 'l2' }, deal: { id: 'd1' } }] });
  const listed = await App.triggers.deal_won.operation.performList(z, bundle({ project_id: PROJECT }));
  assert.deepEqual(listed.map((i) => i.id), ['deal.won:l2:d1']);
  assert.deepEqual(sent[0].params, { project_id: PROJECT });
  assert.equal(withId({ event: 'status_changed', lead: { id: 'l3' }, previous_status: 'new' }).id, 'status_changed:l3:new');
});

test('the dropdowns list projects and the chosen project\'s campaigns', async () => {
  const { z } = fakeZ({ 'GET /projects': [{ id: PROJECT, name: 'Acme' }], [`GET /projects/${PROJECT}/campaigns`]: [{ id: 'c1', name: 'Dentists' }] });
  assert.deepEqual(await App.triggers.project.operation.perform(z, bundle()), [{ id: PROJECT, name: 'Acme' }]);
  assert.deepEqual(await App.triggers.campaign.operation.perform(z, bundle({ project_id: PROJECT })), [{ id: 'c1', name: 'Dentists' }]);
  assert.deepEqual(await App.triggers.campaign.operation.perform(z, bundle()), []);
});

test('add or update lead sends only the filled fields', async () => {
  const { z, sent } = fakeZ({ [`POST /projects/${PROJECT}/leads/upsert`]: (req) => ({ data: { created: true, id: 'l1', ...req.body } }) });
  const input = { project_id: PROJECT, segment_id: '', email: 'sarah@northwind.example', full_name: 'Sarah Chen', title: '', phone: undefined };
  const lead = await App.creates.upsert_lead.operation.perform(z, bundle(input));
  assert.deepEqual(sent[0].body, { email: 'sarah@northwind.example', full_name: 'Sarah Chen' });
  assert.equal(lead.created, true);
});

test("Splee's validation errors reach the zap's history", async () => {
  const { z } = fakeZ({ [`POST /projects/${PROJECT}/leads/upsert`]: () => ({ status: 400, data: { error: 'Suppressed: stop@north.example' } }) });
  await assert.rejects(App.creates.upsert_lead.operation.perform(z, bundle({ project_id: PROJECT, email: 'stop@north.example' })), /Suppressed: stop@north.example/);
});

test('do not contact adds one value', async () => {
  const { z, sent } = fakeZ({ 'POST /suppressions': { added: 1 } });
  assert.deepEqual(await App.creates.add_to_do_not_contact.operation.perform(z, bundle({ value: ' northwind.example ' })), { value: 'northwind.example', added: true });
  assert.deepEqual(sent[0].body, { values: ['northwind.example'] });
});

test('status and owner find the lead in the project by email first', async () => {
  const routes = {
    'GET /leads': (req) => ({ data: req.params.email === 'sarah@northwind.example' ? [{ id: 'l1', project_id: PROJECT }] : [] }),
    'PATCH /leads/l1': (req) => ({ data: { id: 'l1', ...req.body } }),
    'POST /leads/l1/assign': (req) => ({ data: { id: 'l1', owner_email: req.body.owner } }),
  };
  const { z, sent } = fakeZ(routes);
  const set = await App.creates.set_lead_status.operation.perform(z, bundle({ project_id: PROJECT, email: 'sarah@northwind.example', status: 'meeting_booked' }));
  assert.equal(set.status, 'meeting_booked');
  assert.deepEqual(sent[0].params, { email: 'sarah@northwind.example', project_id: PROJECT });
  const owned = await App.creates.assign_lead_owner.operation.perform(z, bundle({ project_id: PROJECT, email: 'sarah@northwind.example', owner: 'alex@acme.example' }));
  assert.equal(owned.owner_email, 'alex@acme.example');
  await App.creates.assign_lead_owner.operation.perform(z, bundle({ project_id: PROJECT, email: 'sarah@northwind.example', owner: '' }));
  assert.deepEqual(sent.at(-1).body, { owner: null }, 'empty unassigns');
  await assert.rejects(
    App.creates.set_lead_status.operation.perform(z, bundle({ project_id: PROJECT, email: 'who@else.example', status: 'hot' })),
    /No lead in that project has the email who@else.example/,
  );
  assert.ok(App.creates.set_lead_status.operation.inputFields.find((f) => f.key === 'status').choices.meeting_booked);
});

test('find lead searches by email, in one project or all', async () => {
  const { z, sent } = fakeZ({ 'GET /leads': [{ id: 'l1' }] });
  assert.deepEqual(await App.searches.find_lead.operation.perform(z, bundle({ email: 'sarah@northwind.example' })), [{ id: 'l1' }]);
  assert.deepEqual(sent[0].params, { email: 'sarah@northwind.example' });
});

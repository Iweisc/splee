'use strict';

const { url, withId, filled, projectField } = require('../lib/splee');

const LEAD = {
  id: 'c42e7a10-0000-4000-8000-000000000002',
  full_name: 'Sarah Chen',
  email: 'sarah@northwind.example',
  title: 'Founder',
  status: 'hot',
  company_name: 'Northwind Studio',
  company_domain: 'northwind.example',
  linkedin_url: 'https://www.linkedin.com/in/sarah-chen',
  x_url: '',
  meeting_at: null,
};

const PROJECT = { id: '9b1f6c2e-0000-4000-8000-000000000001', name: 'Acme' };

/** A REST hook trigger for one Splee event. Keep the events in sync with rest_hooks::EVENTS in backend/src/rest_hooks.rs. */
const hookTrigger = ({ key, event, label, description, sample }) => ({
  key,
  noun: 'Lead',
  display: { label, description },
  operation: {
    type: 'hook',
    inputFields: [projectField(false)],
    performSubscribe: async (z, bundle) => {
      const body = { event, target_url: bundle.targetUrl, ...filled({ project_id: bundle.inputData.project_id }) };
      return (await z.request({ url: url(bundle, '/hooks'), method: 'POST', body })).data;
    },
    performUnsubscribe: async (z, bundle) => {
      // Gone already is fine: Splee turns a subscription off by itself when a post gets 410.
      const response = await z.request({ url: url(bundle, `/hooks/${bundle.subscribeData.id}`), method: 'DELETE', skipThrowForStatus: true });
      if (response.status >= 400 && response.status !== 404) response.throwForStatus();
      return response.status === 404 ? { ok: true } : response.data;
    },
    perform: (z, bundle) => [withId(bundle.cleanedRequest)],
    performList: async (z, bundle) => {
      const response = await z.request({ url: url(bundle, `/hooks/samples/${encodeURIComponent(event)}`), params: filled({ project_id: bundle.inputData.project_id }) });
      return response.data.map(withId);
    },
    sample: withId({ event, project: PROJECT, lead: LEAD, reply: null, url: 'https://splee.example.com/app/p/9b1f…/inbox?lead=c42e…', ...sample }),
  },
});

const REPLY = {
  id: '71aa3d4b-0000-4000-8000-000000000003',
  subject: 'Re: Northwind Studio x Acme',
  body: 'Sounds interesting, can you send times for next week?',
  classification: 'hot',
  reason: 'Asks to schedule a call',
};

module.exports = [
  hookTrigger({
    key: 'new_hot_reply',
    event: 'hot_lead',
    label: 'New Hot Reply',
    description: 'Triggers when a lead replies with interest, like asking for a call.',
    sample: { reply: REPLY },
  }),
  hookTrigger({
    key: 'lead_replied',
    event: 'lead_replied',
    label: 'Lead Replied',
    description: 'Triggers on every reply a person writes, with how Splee classified it.',
    sample: { reply: { ...REPLY, classification: 'question' }, lead: { ...LEAD, status: 'replied' } },
  }),
  hookTrigger({
    key: 'meeting_booked',
    event: 'meeting_booked',
    label: 'Meeting Booked',
    description: 'Triggers when a lead books a call, or your team marks them Meeting booked.',
    sample: { lead: { ...LEAD, status: 'meeting_booked', meeting_at: '2026-11-17T15:00:00Z' } },
  }),
  hookTrigger({
    key: 'lead_status_changed',
    event: 'status_changed',
    label: 'Lead Status Changed',
    description: "Triggers when someone on your team changes a lead's status in Splee.",
    sample: { lead: { ...LEAD, status: 'contacted' }, previous_status: 'new' },
  }),
  hookTrigger({
    key: 'deal_won',
    event: 'deal.won',
    label: 'Deal Won',
    description: 'Triggers when a deal closes as won, with its final value.',
    sample: {
      deal: {
        id: '5e2d8f61-0000-4000-8000-000000000004',
        stage: 'Won',
        previous_stage: 'Proposal',
        status: 'won',
        value: 12000,
        currency: 'USD',
        close_date: '2026-11-20',
        owner: 'Alex Kim',
        lost_reason: null,
        url: 'https://splee.example.com/app/pipeline?deal=5e2d…',
      },
    },
  }),
  hookTrigger({
    key: 'new_lead',
    event: 'lead_added',
    label: 'New Lead',
    description: 'Triggers when a lead is added to Splee from any source, within about a minute.',
    sample: { lead: { ...LEAD, status: 'new' } },
  }),
];

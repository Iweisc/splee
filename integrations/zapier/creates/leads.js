'use strict';

const { url, filled, findLead, STATUSES, projectField } = require('../lib/splee');

const LEAD_FIELDS = ['full_name', 'first_name', 'last_name', 'title', 'company_name', 'company_domain', 'linkedin_url', 'city', 'phone'];

const emailField = { key: 'email', label: 'Email', required: true, type: 'string' };

const LEAD_SAMPLE = { id: 'c42e7a10-0000-4000-8000-000000000002', email: 'sarah@northwind.example', full_name: 'Sarah Chen', status: 'new' };

module.exports = [
  {
    key: 'upsert_lead',
    noun: 'Lead',
    display: {
      label: 'Add or Update Lead',
      description: "Adds a lead to a project, or updates the one with that email. Empty fields keep what the lead has.",
    },
    operation: {
      inputFields: [
        projectField(true),
        { key: 'segment_id', label: 'Campaign', dynamic: 'campaign.id.name', required: false, helpText: 'Puts the lead in this campaign.' },
        emailField,
        { key: 'full_name', label: 'Full name' },
        { key: 'first_name', label: 'First name' },
        { key: 'last_name', label: 'Last name' },
        { key: 'title', label: 'Job title' },
        { key: 'company_name', label: 'Company' },
        { key: 'company_domain', label: 'Company website', helpText: 'Like northwind.example.' },
        { key: 'linkedin_url', label: 'LinkedIn URL' },
        { key: 'city', label: 'City' },
        { key: 'phone', label: 'Phone', helpText: 'With the country code, like +44 20 7946 0000.' },
      ],
      perform: async (z, bundle) => {
        const input = bundle.inputData;
        const body = filled({ email: input.email, segment_id: input.segment_id, ...Object.fromEntries(LEAD_FIELDS.map((f) => [f, input[f]])) });
        return (await z.request({ url: url(bundle, `/projects/${input.project_id}/leads/upsert`), method: 'POST', body })).data;
      },
      sample: { created: true, ...LEAD_SAMPLE },
    },
  },
  {
    key: 'add_to_do_not_contact',
    noun: 'Do-Not-Contact Entry',
    display: { label: 'Add to Do-Not-Contact List', description: 'Stops Splee from ever emailing or texting this address, domain or phone number.' },
    operation: {
      inputFields: [
        { key: 'value', label: 'Email, domain or phone', required: true, helpText: 'A phone number needs its country code, like +44 20 7946 0000.' },
      ],
      perform: async (z, bundle) => {
        const value = String(bundle.inputData.value).trim();
        const response = await z.request({ url: url(bundle, '/suppressions'), method: 'POST', body: { values: [value] } });
        return { value, added: response.data.added === 1 };
      },
      sample: { value: 'sarah@northwind.example', added: true },
    },
  },
  {
    key: 'set_lead_status',
    noun: 'Lead',
    display: { label: 'Set Lead Status', description: "Changes a lead's status, like Meeting booked. Any status but Contacted stops its follow-ups." },
    operation: {
      inputFields: [projectField(true), emailField, { key: 'status', label: 'Status', required: true, choices: STATUSES }],
      perform: async (z, bundle) => {
        const { project_id, email, status } = bundle.inputData;
        const lead = await findLead(z, bundle, email, project_id);
        return (await z.request({ url: url(bundle, `/leads/${lead.id}`), method: 'PATCH', body: { status } })).data;
      },
      sample: { ...LEAD_SAMPLE, status: 'meeting_booked' },
    },
  },
  {
    key: 'assign_lead_owner',
    noun: 'Lead',
    display: { label: 'Assign Lead Owner', description: 'Gives a lead to someone on your team, by their email. Leave it empty to unassign.' },
    operation: {
      inputFields: [projectField(true), emailField, { key: 'owner', label: "Owner's email", required: false, helpText: 'Someone on your Splee team.' }],
      perform: async (z, bundle) => {
        const { project_id, email, owner } = bundle.inputData;
        const lead = await findLead(z, bundle, email, project_id);
        return (await z.request({ url: url(bundle, `/leads/${lead.id}/assign`), method: 'POST', body: { owner: owner || null } })).data;
      },
      sample: { ...LEAD_SAMPLE, owner_id: '1d6a…', owner_email: 'alex@acme.example' },
    },
  },
];

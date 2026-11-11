'use strict';

const { url, filled, projectField } = require('../lib/splee');

module.exports = {
  key: 'find_lead',
  noun: 'Lead',
  display: { label: 'Find Lead', description: 'Finds a lead by email, the newest first when the person is a lead in several projects.' },
  operation: {
    inputFields: [{ key: 'email', label: 'Email', required: true }, projectField(false)],
    perform: async (z, bundle) => {
      const params = filled({ email: bundle.inputData.email, project_id: bundle.inputData.project_id });
      return (await z.request({ url: url(bundle, '/leads'), params })).data;
    },
    sample: { id: 'c42e7a10-0000-4000-8000-000000000002', email: 'sarah@northwind.example', full_name: 'Sarah Chen', status: 'hot' },
  },
};

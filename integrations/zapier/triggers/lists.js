'use strict';

const { url } = require('../lib/splee');

// Hidden triggers that fill the Project and Campaign dropdowns.
module.exports = [
  {
    key: 'project',
    noun: 'Project',
    display: { label: 'Project', description: 'Lists your projects.', hidden: true },
    operation: {
      perform: async (z, bundle) => (await z.request({ url: url(bundle, '/projects') })).data,
      sample: { id: '9b1f6c2e-0000-4000-8000-000000000001', name: 'Acme' },
    },
  },
  {
    key: 'campaign',
    noun: 'Campaign',
    display: { label: 'Campaign', description: "Lists a project's campaigns.", hidden: true },
    operation: {
      perform: async (z, bundle) => {
        if (!bundle.inputData.project_id) return [];
        return (await z.request({ url: url(bundle, `/projects/${bundle.inputData.project_id}/campaigns`) })).data;
      },
      sample: { id: '5d0a2b9c-0000-4000-8000-000000000005', name: 'Design studios' },
    },
  },
];

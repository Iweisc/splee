'use strict';

const { url } = require('./lib/splee');

module.exports = {
  type: 'custom',
  fields: [
    {
      key: 'baseUrl',
      label: 'Splee address',
      required: true,
      type: 'string',
      helpText: 'Where you open Splee, like https://splee.example.com. The Integrations page shows the full base URL too.',
    },
    { key: 'apiKey', label: 'API key', required: true, type: 'password', helpText: 'Create one on the API keys page in Splee.' },
  ],
  test: async (z, bundle) => (await z.request({ url: url(bundle, '/me') })).data,
  connectionLabel: '{{organization.name}}',
};

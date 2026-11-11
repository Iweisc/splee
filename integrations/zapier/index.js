'use strict';

const authentication = require('./authentication');
const { includeApiKey, explainErrors } = require('./lib/splee');
const events = require('./triggers/events');
const lists = require('./triggers/lists');
const creates = require('./creates/leads');
const findLead = require('./searches/find_lead');

const byKey = (items) => Object.fromEntries(items.map((item) => [item.key, item]));

module.exports = {
  version: require('./package.json').version,
  platformVersion: require('zapier-platform-core').version,
  authentication,
  beforeRequest: [includeApiKey],
  afterResponse: [explainErrors],
  triggers: byKey([...events, ...lists]),
  creates: byKey(creates),
  searches: byKey([findLead]),
};

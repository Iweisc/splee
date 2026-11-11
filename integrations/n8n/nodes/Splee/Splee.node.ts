import type { IDataObject, IExecuteFunctions, ILoadOptionsFunctions, INodeExecutionData, INodeType, INodeTypeDescription } from 'n8n-workflow';
import { NodeConnectionTypes, NodeOperationError } from 'n8n-workflow';

import { STATUSES, filled, getProjects, spleeRequest } from './GenericFunctions';

const LEAD_FIELDS = ['full_name', 'first_name', 'last_name', 'title', 'company_name', 'company_domain', 'linkedin_url', 'city', 'phone'];

const on = (operation: string[]) => ({ show: { resource: ['lead'], operation } });

export class Splee implements INodeType {
	description: INodeTypeDescription = {
		displayName: 'Splee',
		name: 'splee',
		icon: 'file:splee.svg',
		group: ['transform'],
		version: 1,
		subtitle: '={{$parameter["operation"] + ": " + $parameter["resource"]}}',
		description: 'Add and update leads in Splee, and keep its do-not-contact list',
		defaults: { name: 'Splee' },
		inputs: [NodeConnectionTypes.Main],
		outputs: [NodeConnectionTypes.Main],
		credentials: [{ name: 'spleeApi', required: true }],
		properties: [
			{
				displayName: 'Resource',
				name: 'resource',
				type: 'options',
				noDataExpression: true,
				options: [
					{ name: 'Lead', value: 'lead' },
					{ name: 'Do-Not-Contact List', value: 'doNotContact' },
				],
				default: 'lead',
			},
			{
				displayName: 'Operation',
				name: 'operation',
				type: 'options',
				noDataExpression: true,
				displayOptions: { show: { resource: ['lead'] } },
				options: [
					{ name: 'Add or Update', value: 'upsert', action: 'Add or update a lead', description: 'Add a lead, or update the one with that email' },
					{ name: 'Assign Owner', value: 'assign', action: "Assign a lead's owner", description: 'Give a lead to someone on your team' },
					{ name: 'Find', value: 'find', action: 'Find a lead by email' },
					{ name: 'Set Status', value: 'setStatus', action: "Set a lead's status" },
				],
				default: 'upsert',
			},
			{
				displayName: 'Operation',
				name: 'operation',
				type: 'options',
				noDataExpression: true,
				displayOptions: { show: { resource: ['doNotContact'] } },
				options: [{ name: 'Add', value: 'add', action: 'Add to the do-not-contact list' }],
				default: 'add',
			},
			{
				displayName: 'Project Name or ID',
				name: 'projectId',
				type: 'options',
				typeOptions: { loadOptionsMethod: 'getProjects' },
				default: '',
				required: true,
				displayOptions: on(['upsert', 'assign', 'setStatus']),
				description: 'Choose from the list, or specify an ID using an <a href="https://docs.n8n.io/code/expressions/">expression</a>',
			},
			{
				displayName: 'Project Name or ID',
				name: 'projectId',
				type: 'options',
				typeOptions: { loadOptionsMethod: 'getProjects' },
				default: '',
				displayOptions: on(['find']),
				description: 'Leave it empty to search every project. Choose from the list, or specify an ID using an <a href="https://docs.n8n.io/code/expressions/">expression</a>.',
			},
			{ displayName: 'Email', name: 'email', type: 'string', placeholder: 'name@email.com', default: '', required: true, displayOptions: on(['upsert', 'assign', 'find', 'setStatus']) },
			{ displayName: 'Status', name: 'status', type: 'options', options: STATUSES, default: 'meeting_booked', required: true, displayOptions: on(['setStatus']), description: 'Any status but Contacted stops its follow-ups' },
			{ displayName: "Owner's Email", name: 'owner', type: 'string', placeholder: 'name@email.com', default: '', displayOptions: on(['assign']), description: 'Someone on your Splee team. Leave it empty to unassign.' },
			{
				displayName: 'Campaign Name or ID',
				name: 'segmentId',
				type: 'options',
				typeOptions: { loadOptionsMethod: 'getCampaigns', loadOptionsDependsOn: ['projectId'] },
				default: '',
				displayOptions: on(['upsert']),
				description: 'Puts the lead in this campaign. Choose from the list, or specify an ID using an <a href="https://docs.n8n.io/code/expressions/">expression</a>.',
			},
			{
				displayName: 'Lead Fields',
				name: 'fields',
				type: 'collection',
				placeholder: 'Add Field',
				default: {},
				displayOptions: on(['upsert']),
				description: 'Fields you leave out keep what the lead has',
				options: [
					{ displayName: 'City', name: 'city', type: 'string', default: '' },
					{ displayName: 'Company', name: 'company_name', type: 'string', default: '' },
					{ displayName: 'Company Website', name: 'company_domain', type: 'string', default: '', placeholder: 'northwind.example' },
					{ displayName: 'First Name', name: 'first_name', type: 'string', default: '' },
					{ displayName: 'Full Name', name: 'full_name', type: 'string', default: '' },
					{ displayName: 'Job Title', name: 'title', type: 'string', default: '' },
					{ displayName: 'Last Name', name: 'last_name', type: 'string', default: '' },
					{ displayName: 'LinkedIn URL', name: 'linkedin_url', type: 'string', default: '' },
					{ displayName: 'Phone', name: 'phone', type: 'string', default: '', description: 'With the country code, like +44 20 7946 0000' },
				],
			},
			{
				displayName: 'Email, Domain or Phone',
				name: 'value',
				type: 'string',
				default: '',
				required: true,
				displayOptions: { show: { resource: ['doNotContact'] } },
				description: 'A phone number needs its country code, like +44 20 7946 0000',
			},
		],
	};

	methods = {
		loadOptions: {
			getProjects,
			async getCampaigns(this: ILoadOptionsFunctions) {
				const projectId = this.getCurrentNodeParameter('projectId') as string;
				if (!projectId) return [];
				const campaigns: { id: string; name: string }[] = await spleeRequest.call(this, 'GET', `/projects/${projectId}/campaigns`);
				return campaigns.map((c) => ({ name: c.name, value: c.id }));
			},
		},
	};

	async execute(this: IExecuteFunctions): Promise<INodeExecutionData[][]> {
		const items = this.getInputData();
		const out: INodeExecutionData[] = [];
		for (let i = 0; i < items.length; i++) {
			try {
				const result = await run.call(this, i);
				for (const json of Array.isArray(result) ? result : [result]) out.push({ json, pairedItem: { item: i } });
			} catch (error) {
				if (!this.continueOnFail()) throw error;
				out.push({ json: { error: (error as Error).message }, pairedItem: { item: i } });
			}
		}
		return [out];
	}
}

async function run(this: IExecuteFunctions, i: number): Promise<IDataObject | IDataObject[]> {
	const resource = this.getNodeParameter('resource', i) as string;
	const operation = this.getNodeParameter('operation', i) as string;
	if (resource === 'doNotContact') {
		const value = (this.getNodeParameter('value', i) as string).trim();
		const added = await spleeRequest.call(this, 'POST', '/suppressions', { values: [value] });
		return { value, added: added.added === 1 };
	}
	const email = (this.getNodeParameter('email', i) as string).trim();
	const projectId = this.getNodeParameter('projectId', i) as string;
	if (operation === 'find') return spleeRequest.call(this, 'GET', '/leads', undefined, filled({ email, project_id: projectId }));
	if (operation === 'upsert') {
		const fields = this.getNodeParameter('fields', i) as IDataObject;
		const segmentId = this.getNodeParameter('segmentId', i) as string;
		const body = filled({ email, segment_id: segmentId, ...Object.fromEntries(LEAD_FIELDS.map((f) => [f, fields[f]])) });
		return spleeRequest.call(this, 'POST', `/projects/${projectId}/leads/upsert`, body);
	}
	const leads: IDataObject[] = await spleeRequest.call(this, 'GET', '/leads', undefined, { email, project_id: projectId });
	if (!leads.length) throw new NodeOperationError(this.getNode(), `No lead in that project has the email ${email}`, { itemIndex: i });
	const leadId = leads[0].id as string;
	if (operation === 'setStatus') return spleeRequest.call(this, 'PATCH', `/leads/${leadId}`, { status: this.getNodeParameter('status', i) as string });
	const owner = (this.getNodeParameter('owner', i) as string).trim();
	return spleeRequest.call(this, 'POST', `/leads/${leadId}/assign`, { owner: owner || null });
}

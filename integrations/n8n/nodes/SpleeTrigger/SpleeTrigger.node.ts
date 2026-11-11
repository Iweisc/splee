import type { IHookFunctions, INodeType, INodeTypeDescription, IWebhookFunctions, IWebhookResponseData } from 'n8n-workflow';
import { NodeConnectionTypes } from 'n8n-workflow';

import { EVENTS, filled, getProjects, spleeRequest } from '../Splee/GenericFunctions';

export class SpleeTrigger implements INodeType {
	description: INodeTypeDescription = {
		displayName: 'Splee Trigger',
		name: 'spleeTrigger',
		icon: 'file:splee.svg',
		group: ['trigger'],
		version: 1,
		subtitle: '={{$parameter["event"]}}',
		description: 'Starts the workflow when a lead replies, books, changes status, wins a deal or is added',
		defaults: { name: 'Splee Trigger' },
		inputs: [],
		outputs: [NodeConnectionTypes.Main],
		credentials: [{ name: 'spleeApi', required: true }],
		webhooks: [{ name: 'default', httpMethod: 'POST', responseMode: 'onReceived', path: 'webhook' }],
		properties: [
			{ displayName: 'Event', name: 'event', type: 'options', options: EVENTS, default: 'hot_lead', required: true },
			{
				displayName: 'Project Name or ID',
				name: 'projectId',
				type: 'options',
				typeOptions: { loadOptionsMethod: 'getProjects' },
				default: '',
				description: 'Leave it empty for every project. Choose from the list, or specify an ID using an <a href="https://docs.n8n.io/code/expressions/">expression</a>.',
			},
		],
	};

	methods = { loadOptions: { getProjects } };

	// Splee keeps the subscription until it's deleted here, or until a post to it gets 410 Gone.
	webhookMethods = {
		default: {
			async checkExists(this: IHookFunctions): Promise<boolean> {
				return this.getWorkflowStaticData('node').subscriptionId !== undefined;
			},
			async create(this: IHookFunctions): Promise<boolean> {
				const body = filled({
					event: this.getNodeParameter('event') as string,
					target_url: this.getNodeWebhookUrl('default') as string,
					project_id: this.getNodeParameter('projectId') as string,
				});
				const subscription = await spleeRequest.call(this, 'POST', '/hooks', body);
				this.getWorkflowStaticData('node').subscriptionId = subscription.id;
				return true;
			},
			async delete(this: IHookFunctions): Promise<boolean> {
				const data = this.getWorkflowStaticData('node');
				if (data.subscriptionId !== undefined) {
					try {
						await spleeRequest.call(this, 'DELETE', `/hooks/${data.subscriptionId}`);
					} catch (error) {
						// Already gone in Splee is what this wanted anyway.
						if ((error as { httpCode?: string }).httpCode !== '404') return false;
					}
					delete data.subscriptionId;
				}
				return true;
			},
		},
	};

	async webhook(this: IWebhookFunctions): Promise<IWebhookResponseData> {
		return { workflowData: [this.helpers.returnJsonArray(this.getBodyData())] };
	}
}

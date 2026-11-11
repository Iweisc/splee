import type { IDataObject, IExecuteFunctions, IHookFunctions, IHttpRequestMethods, ILoadOptionsFunctions, IWebhookFunctions } from 'n8n-workflow';

type Context = IExecuteFunctions | IHookFunctions | ILoadOptionsFunctions | IWebhookFunctions;

/** Accepts the address people open Splee at, or the base URL the Integrations page shows, with or without a trailing slash. */
export function apiBase(raw: string): string {
	const url = String(raw ?? '').trim().replace(/\/+$/, '');
	return /\/api\/v1$/.test(url) ? url : `${url}/api/v1`;
}

/** Drops fields left empty, which the API doesn't take for an id. */
export function filled(fields: IDataObject): IDataObject {
	return Object.fromEntries(Object.entries(fields).filter(([, v]) => v !== undefined && v !== null && v !== ''));
}

export async function spleeRequest(this: Context, method: IHttpRequestMethods, path: string, body?: IDataObject, qs?: IDataObject): Promise<any> {
	const credentials = await this.getCredentials('spleeApi');
	return this.helpers.httpRequestWithAuthentication.call(this, 'spleeApi', {
		method,
		url: `${apiBase(credentials.baseUrl as string)}${path}`,
		body,
		qs,
		json: true,
	});
}

// Keep in sync with rest_hooks::EVENTS in backend/src/rest_hooks.rs.
export const EVENTS = [
	{ name: 'New Hot Reply', value: 'hot_lead', description: 'A lead replied with interest, like asking for a call' },
	{ name: 'Lead Replied', value: 'lead_replied', description: 'Any reply a person wrote, with how Splee classified it' },
	{ name: 'Meeting Booked', value: 'meeting_booked', description: 'A lead booked a call, or your team marked them Meeting booked' },
	{ name: 'Lead Status Changed', value: 'status_changed', description: "Someone on your team changed a lead's status" },
	{ name: 'Deal Won', value: 'deal.won', description: 'A deal closed as won, with its final value' },
	{ name: 'New Lead Added', value: 'lead_added', description: 'A lead was added from any source, within about a minute' },
];

// Keep in sync with LEAD_STATUSES in backend/src/routes/util_input.rs.
export const STATUSES = [
	['new', 'New'],
	['queued', 'Queued'],
	['contacted', 'Contacted'],
	['replied', 'Replied'],
	['hot', 'Hot'],
	['meeting_booked', 'Meeting Booked'],
	['not_interested', 'Not Interested'],
	['bounced', 'Bounced'],
	['unsubscribed', 'Unsubscribed'],
	['do_not_contact', 'Do Not Contact'],
].map(([value, name]) => ({ name, value }));

export async function getProjects(this: ILoadOptionsFunctions) {
	const projects: { id: string; name: string }[] = await spleeRequest.call(this, 'GET', '/projects');
	return projects.map((p) => ({ name: p.name, value: p.id }));
}

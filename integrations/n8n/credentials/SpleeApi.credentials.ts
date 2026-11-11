import type { IAuthenticateGeneric, ICredentialTestRequest, ICredentialType, INodeProperties } from 'n8n-workflow';

export class SpleeApi implements ICredentialType {
	name = 'spleeApi';

	displayName = 'Splee API';

	documentationUrl = 'https://github.com/Iweisc/splee/tree/main/integrations/n8n';

	properties: INodeProperties[] = [
		{
			displayName: 'Base URL',
			name: 'baseUrl',
			type: 'string',
			default: '',
			placeholder: 'https://splee.example.com/api/v1',
			description: 'The base URL on the Integrations page in Splee. The address you open Splee at works too.',
			required: true,
		},
		{
			displayName: 'API Key',
			name: 'apiKey',
			type: 'string',
			typeOptions: { password: true },
			default: '',
			description: 'Create one on the API keys page in Splee',
			required: true,
		},
	];

	authenticate: IAuthenticateGeneric = {
		type: 'generic',
		properties: { headers: { Authorization: '=Bearer {{$credentials.apiKey}}' } },
	};

	// Keep the expression in sync with apiBase in nodes/Splee/GenericFunctions.ts.
	test: ICredentialTestRequest = {
		request: {
			baseURL: '={{$credentials.baseUrl.trim().replace(/\\/+$/, "").replace(/\\/api\\/v1$/, "")}}/api/v1',
			url: '/me',
		},
	};
}

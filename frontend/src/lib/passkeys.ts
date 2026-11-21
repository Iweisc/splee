/**
 * Passkeys (WebAuthn) for the Account page and sign-in. The backend's options carry base64url strings where the browser
 * wants bytes, and it takes the browser's answer back as base64url. Done by hand because PublicKeyCredential's own JSON
 * helpers are missing from browsers that still run passkeys fine.
 */

/** Options for navigator.credentials.create() from POST /auth/passkeys/options. */
export interface PasskeyCreationOptions {
  challenge: string;
  rp: { id: string; name: string };
  user: { id: string; name: string; displayName: string };
  pubKeyCredParams: { type: "public-key"; alg: number }[];
  authenticatorSelection: AuthenticatorSelectionCriteria;
  attestation: AttestationConveyancePreference;
  timeout: number;
  excludeCredentials: { type: "public-key"; id: string; transports: AuthenticatorTransport[] }[];
}

/** Options for navigator.credentials.get() from POST /auth/passkeys/sign-in/options. */
export interface PasskeyRequestOptions {
  challenge: string;
  rpId: string;
  userVerification: UserVerificationRequirement;
  timeout: number;
}

export const passkeysSupported = () => typeof window !== "undefined" && typeof window.PublicKeyCredential === "function";

/** Whether the browser offers passkeys in the email field's autofill. */
export async function passkeyAutofill(): Promise<boolean> {
  if (!passkeysSupported() || typeof PublicKeyCredential.isConditionalMediationAvailable !== "function") return false;
  return PublicKeyCredential.isConditionalMediationAvailable();
}

function bytes(base64url: string): Uint8Array<ArrayBuffer> {
  const binary = atob(base64url.replace(/-/g, "+").replace(/_/g, "/"));
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
  return out;
}

function base64url(buffer: ArrayBuffer): string {
  let binary = "";
  for (const b of new Uint8Array(buffer)) binary += String.fromCharCode(b);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** The browser's answer was cancelled, timed out or aborted, which needs no error of its own. */
export function passkeyCancelled(e: unknown): boolean {
  return e instanceof DOMException && (e.name === "NotAllowedError" || e.name === "AbortError");
}

/** Makes a passkey and returns the body for POST /auth/passkeys. */
export async function createPasskey(options: PasskeyCreationOptions, name: string) {
  const credential = (await navigator.credentials.create({
    publicKey: {
      ...options,
      challenge: bytes(options.challenge),
      user: { ...options.user, id: bytes(options.user.id) },
      excludeCredentials: options.excludeCredentials.map((c) => ({ ...c, id: bytes(c.id) })),
    },
  })) as PublicKeyCredential | null;
  if (!credential) throw new DOMException("No passkey was made", "NotAllowedError");
  const response = credential.response as AuthenticatorAttestationResponse;
  const publicKey = response.getPublicKey();
  if (!publicKey) throw new Error("This browser made a kind of passkey Splee can't use. Try another browser.");
  return {
    name,
    credential_id: base64url(credential.rawId),
    client_data_json: base64url(response.clientDataJSON),
    authenticator_data: base64url(response.getAuthenticatorData()),
    public_key: base64url(publicKey),
    alg: response.getPublicKeyAlgorithm(),
    transports: response.getTransports(),
  };
}

/**
 * Asks for a passkey and returns the body for POST /auth/passkeys/sign-in. With `autofill`, the browser offers its passkeys
 * in the email field's suggestions until the signal aborts, instead of opening its own prompt.
 */
export async function getPasskey(options: PasskeyRequestOptions, autofill?: AbortSignal) {
  const request: CredentialRequestOptions = { publicKey: { ...options, challenge: bytes(options.challenge) } };
  if (autofill) {
    request.mediation = "conditional";
    request.signal = autofill;
  }
  const credential = (await navigator.credentials.get(request)) as PublicKeyCredential | null;
  if (!credential) throw new DOMException("No passkey was picked", "NotAllowedError");
  const response = credential.response as AuthenticatorAssertionResponse;
  return {
    credential_id: base64url(credential.rawId),
    client_data_json: base64url(response.clientDataJSON),
    authenticator_data: base64url(response.authenticatorData),
    signature: base64url(response.signature),
    user_handle: response.userHandle ? base64url(response.userHandle) : null,
  };
}

/** A name for a new passkey from the device it's made on. iPads say they're Macs, but only they have touch. */
export function deviceName(): string {
  const ua = navigator.userAgent;
  if (/iPhone/.test(ua)) return "iPhone";
  if (/iPad/.test(ua) || (/Macintosh/.test(ua) && navigator.maxTouchPoints > 1)) return "iPad";
  if (/Android/.test(ua)) return "Android";
  if (/CrOS/.test(ua)) return "Chromebook";
  if (/Macintosh/.test(ua)) return "Mac";
  if (/Windows/.test(ua)) return "Windows PC";
  if (/Linux/.test(ua)) return "Linux PC";
  return "Passkey";
}

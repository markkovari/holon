// Client for the passkey:app HTTP API, plus the browser half of a WebAuthn
// ceremony. The only real work here is base64url <-> ArrayBuffer: the API speaks
// base64url (JSON), `navigator.credentials` speaks buffers.

export function b64uToBytes(s: string): Uint8Array {
  const pad = s.replace(/-/g, "+").replace(/_/g, "/");
  const raw = atob(pad + "=".repeat((4 - (pad.length % 4)) % 4));
  return Uint8Array.from(raw, (c) => c.charCodeAt(0));
}

export function bytesToB64u(b: ArrayBuffer | Uint8Array): string {
  const bytes = b instanceof Uint8Array ? b : new Uint8Array(b);
  let s = "";
  for (const byte of bytes) s += String.fromCharCode(byte);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

let token: string | null = localStorage.getItem("passkey.token");
export const session = {
  get token() {
    return token;
  },
  set(t: string | null) {
    token = t;
    if (t) localStorage.setItem("passkey.token", t);
    else localStorage.removeItem("passkey.token");
  },
};

async function call<T>(path: string, method: "GET" | "POST", body?: unknown): Promise<{ ok: boolean; status: number; data: T }> {
  const r = await fetch(`/api${path}`, {
    method,
    headers: {
      ...(body ? { "content-type": "application/json" } : {}),
      ...(token ? { authorization: `bearer ${token}` } : {}),
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  const data = (await r.json().catch(() => ({}))) as T;
  return { ok: r.ok, status: r.status, data };
}

export const api = {
  get: <T = any>(p: string) => call<T>(p, "GET"),
  post: <T = any>(p: string, body?: unknown) => call<T>(p, "POST", body ?? {}),
};

// ---- types ----
export interface Credential {
  id: string;
  aaguid: string;
  alg: number;
  sign_count: number;
  created: number;
  last_used: number | null;
  user_verified: boolean;
  backed_up: boolean;
  backup_eligible: boolean;
  attestation_format: string;
}
export interface Me {
  username: string;
  credentials: Credential[];
}
export interface SessionResponse {
  token: string;
  username: string;
  expires: number;
  error?: string;
  detail?: string;
}

// ---- the two ceremonies ----

/// Register a new passkey. The private key is generated inside the authenticator
/// and never leaves it; all we ever see is a public key and a signature.
export async function createPasskey(username: string): Promise<SessionResponse> {
  const { ok, data: opts } = await api.post<any>("/register/begin", { username });
  if (!ok) throw new Error(opts.error ?? "could not start registration");

  const cred = (await navigator.credentials.create({
    publicKey: {
      ...opts,
      challenge: b64uToBytes(opts.challenge),
      user: { ...opts.user, id: b64uToBytes(opts.user.id) },
      excludeCredentials: (opts.excludeCredentials ?? []).map((c: any) => ({ ...c, id: b64uToBytes(c.id) })),
    },
  })) as PublicKeyCredential | null;
  if (!cred) throw new Error("the authenticator returned nothing");
  const att = cred.response as AuthenticatorAttestationResponse;

  const { ok: done, data } = await api.post<SessionResponse>("/register/finish", {
    username,
    id: cred.id,
    client_data_json: bytesToB64u(att.clientDataJSON),
    attestation_object: bytesToB64u(att.attestationObject),
  });
  if (!done) throw new Error(describe(data));
  return data;
}

/// Sign in. Omit the username to let the authenticator offer whichever passkey it
/// holds for this site (a "discoverable" credential).
export async function signInWithPasskey(username?: string): Promise<SessionResponse> {
  const { ok, data: opts } = await api.post<any>("/login/begin", username ? { username } : {});
  if (!ok) throw new Error(opts.error ?? "could not start sign-in");

  const cred = (await navigator.credentials.get({
    publicKey: {
      ...opts,
      challenge: b64uToBytes(opts.challenge),
      allowCredentials: (opts.allowCredentials ?? []).map((c: any) => ({ ...c, id: b64uToBytes(c.id) })),
    },
  })) as PublicKeyCredential | null;
  if (!cred) throw new Error("the authenticator returned nothing");
  const a = cred.response as AuthenticatorAssertionResponse;

  const { ok: done, data } = await api.post<SessionResponse>("/login/finish", {
    id: cred.id,
    client_data_json: bytesToB64u(a.clientDataJSON),
    authenticator_data: bytesToB64u(a.authenticatorData),
    signature: bytesToB64u(a.signature),
  });
  if (!done) throw new Error(describe(data));
  return data;
}

/// Turn the server's verification failure into something a human can act on.
/// These are the checks that make a passkey unphishable, so name them.
function describe(d: SessionResponse): string {
  const reasons: Record<string, string> = {
    origin_mismatch: "wrong origin — this page is not the site the passkey was made for",
    rp_id_mismatch: "that passkey belongs to a different site",
    challenge_mismatch: "stale challenge — start again",
    bad_signature: "the signature did not verify",
    counter_regressed: "the authenticator's counter went backwards (possible clone)",
    user_not_verified: "this site requires a biometric or PIN check",
    user_not_present: "the authenticator was not touched",
    unsupported_algorithm: "that authenticator uses an algorithm this server does not verify",
  };
  const key = d.error ?? "";
  return reasons[key] ?? (`${key}${d.detail ? `: ${d.detail}` : ""}` || "ceremony failed");
}

export const supported = typeof window !== "undefined" && !!window.PublicKeyCredential;

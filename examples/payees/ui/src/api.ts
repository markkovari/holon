// Thin client for the payees:app HTTP API. The token lives in localStorage so a
// reload keeps you signed in.
let token: string | null = localStorage.getItem("payees-tok");

export function setToken(t: string | null) {
  token = t;
  if (t) localStorage.setItem("payees-tok", t);
  else localStorage.removeItem("payees-tok");
}
export const hasToken = () => !!token;

export async function api<T = any>(
  path: string,
  method = "GET",
  body?: unknown,
): Promise<{ ok: boolean; status: number; data: T }> {
  const headers: Record<string, string> = { "content-type": "application/json" };
  if (token) headers.authorization = `Bearer ${token}`;
  const r = await fetch(`/api${path}`, {
    method,
    headers,
    body: body ? JSON.stringify(body) : undefined,
  });
  const data = (await r.json().catch(() => ({}))) as T;
  return { ok: r.ok, status: r.status, data };
}

// ---- types ----
export interface Me { subject: string; roles: string[] }
export interface Payee { id: string; name: string; iban: string; formatted: string; country: string }
export interface Verify {
  valid: boolean; error?: string;
  country?: string; check_digits?: string; bban?: string; formatted?: string; length?: number;
}

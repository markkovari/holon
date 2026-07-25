// Thin client for the transit:app HTTP API. The token lives in localStorage so a
// reload keeps you signed in.
let token: string | null = localStorage.getItem("transit-tok");

export function setToken(t: string | null) {
  token = t;
  if (t) localStorage.setItem("transit-tok", t);
  else localStorage.removeItem("transit-tok");
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

// Fetch an authed text resource (e.g. the QR SVG) as a string.
export async function getText(path: string): Promise<string> {
  const headers: Record<string, string> = {};
  if (token) headers.authorization = `Bearer ${token}`;
  const r = await fetch(`/api${path}`, { headers });
  return r.ok ? r.text() : "";
}

// ---- types ----
export interface Me { subject: string; roles: string[]; is_validator: boolean }
export interface Fare { key: string; name: string; kind: string; minutes: number; price: number }
export interface Ticket {
  id: string; fare: string; fare_name: string; kind: string; minutes: number; price: number;
  purchased: number; activated: number; uses: number;
  status: "valid" | "active" | "used" | "expired"; valid_until: number | null; remaining_min: number | null;
}
export interface ValidateResult {
  result: "accept" | "reject"; reason: string; kind: string; fare_name?: string;
  valid_until: number | null; remaining_min: number | null; at: number;
}

export const money = (cents: number) => `$${(cents / 100).toFixed(2)}`;
export const clock = (secs: number | null) =>
  secs ? new Date(secs * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }) : "";

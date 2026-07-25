// Thin client for the booked:app HTTP API. The token lives in localStorage so a
// reload keeps you signed in.
let token: string | null = localStorage.getItem("booked-tok");

export function setToken(t: string | null) {
  token = t;
  if (t) localStorage.setItem("booked-tok", t);
  else localStorage.removeItem("booked-tok");
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

// Fetch an authed file (e.g. an .ics) and trigger a browser download.
export async function download(path: string, filename: string) {
  const headers: Record<string, string> = {};
  if (token) headers.authorization = `Bearer ${token}`;
  const r = await fetch(`/api${path}`, { headers });
  if (!r.ok) return;
  const url = URL.createObjectURL(await r.blob());
  const a = document.createElement("a");
  a.href = url; a.download = filename; a.click();
  URL.revokeObjectURL(url);
}

// ---- types ----
export interface Me { subject: string; roles: string[]; email: string; is_owner: boolean }
export interface Resource { id: string; key: string; name: string; owner: string; slot: number; tz: string }
export interface Window { weekday: number; start: number; end: number }
export interface Slot { start: number; end: number; label: string }
export interface Booking {
  id: string; resource: string; resource_name: string; user: string; email: string;
  day: string; start: number; end: number; note: string; created: number;
}
export interface Confirmation { subject: string; text: string }
export interface BookResult { booked: Booking[]; conflicts: string[]; confirmation: Confirmation | null }

export const DOW = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
export const hhmm = (m: number) => `${String(Math.floor(m / 60)).padStart(2, "0")}:${String(m % 60).padStart(2, "0")}`;
export const today = () => new Date().toISOString().slice(0, 10);
export function weekdayOf(iso: string) { const d = new Date(iso + "T00:00:00"); return (d.getDay() + 6) % 7; } // Mon=0

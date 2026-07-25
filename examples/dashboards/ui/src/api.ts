// Thin client for the dashboards:app HTTP API. The token lives in localStorage
// so a reload keeps you signed in.
let token: string | null = localStorage.getItem("dashboards-tok");

export function setToken(t: string | null) {
  token = t;
  if (t) localStorage.setItem("dashboards-tok", t);
  else localStorage.removeItem("dashboards-tok");
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

// Fetch an authed text resource (the server-rendered chart SVG) as a string.
export async function getText(path: string): Promise<string> {
  const headers: Record<string, string> = {};
  if (token) headers.authorization = `Bearer ${token}`;
  const r = await fetch(`/api${path}`, { headers });
  return r.ok ? r.text() : "";
}

// ---- types ----
export type Kind = "bar" | "line" | "donut" | "sparkline";
export interface Me { subject: string; roles: string[] }
export interface Dashboard { id: string; name: string; owner: string }
export interface Point { label: string; value: number; color?: string }
export interface Panel { id: string; dashboard: string; title: string; kind: Kind; data: Point[] }
export interface DashboardDetail { dashboard: Dashboard; panels: Panel[] }

export const KINDS: Kind[] = ["bar", "line", "donut", "sparkline"];

// Parse a "label value" textarea into points (value = the last number on the line).
export function parsePoints(text: string): Point[] {
  return text
    .split("\n")
    .map((l) => l.trim())
    .filter(Boolean)
    .map((l) => {
      const m = l.match(/^(.*?)\s+(-?\d+(?:\.\d+)?)$/);
      return m ? { label: m[1].trim(), value: Number(m[2]) } : null;
    })
    .filter((p): p is Point => !!p);
}

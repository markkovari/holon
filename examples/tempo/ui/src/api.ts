// Thin client for the tempo:app HTTP API. The token lives in localStorage so a
// reload keeps you signed in.
let token: string | null = localStorage.getItem("tempo-tok");

export function setToken(t: string | null) {
  token = t;
  if (t) localStorage.setItem("tempo-tok", t);
  else localStorage.removeItem("tempo-tok");
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

// Fetch an authed file (e.g. the report PDF) and trigger a browser download.
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
export interface Me { subject: string; roles: string[]; email: string; can_see_all: boolean }
export interface Project { id: string; key: string; name: string; my_role?: string }
export interface Category { id: string; name: string }
export interface Entry {
  id: string; project: string; project_name: string; category: string;
  category_name: string; minutes: number; day: string; note: string; email: string;
  start?: number; // minutes from midnight; -1 / absent = unscheduled
}
export interface Report {
  from: string; to: string; scope: string; can_see_all: boolean; total_minutes: number;
  by_project: { project: string; name: string; minutes: number }[];
  by_category: { key: string; minutes: number }[];
  by_day: { day: string; minutes: number }[];
  by_user?: { key: string; minutes: number }[];
}
export interface Timer { project: string; category: string; day: string; started: number }

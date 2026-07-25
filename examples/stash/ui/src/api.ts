// Thin client for the stash:app HTTP API. The token lives in localStorage so a
// reload keeps you signed in.
let token: string | null = localStorage.getItem("stash-tok");

export function setToken(t: string | null) {
  token = t;
  if (t) localStorage.setItem("stash-tok", t);
  else localStorage.removeItem("stash-tok");
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

// Fetch an authed file (the export ZIP) and trigger a browser download.
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
export interface Me { subject: string; roles: string[] }
export interface Note { id: string; title: string; body: string; created: number }

// Thin client for the gate:app HTTP API. No auth — a gateway keys by a
// client-supplied API key passed in each request.
export async function api<T = any>(
  path: string,
  body?: unknown,
): Promise<{ ok: boolean; status: number; data: T }> {
  const r = await fetch(`/api${path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: body ? JSON.stringify(body) : undefined,
  });
  const data = (await r.json().catch(() => ({}))) as T;
  return { ok: r.ok, status: r.status, data };
}

export async function apiGet<T = any>(path: string): Promise<T> {
  const r = await fetch(`/api${path}`);
  return (await r.json().catch(() => ({}))) as T;
}

// ---- types ----
export interface Decision { allowed: boolean; retry_after_ms: number; remaining: number; algo: string; key: string }
export interface BatchSubmit { batch: string; index: number; size: number; flushed: boolean; result?: string | null }
export interface BatchState {
  id: string; key: string; items: string[]; size: number; flushed: boolean;
  results: string[] | null; created_ms: number; age_ms: number; max_size: number; max_age_ms: number;
}

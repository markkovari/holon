// Thin client for the mesh:app HTTP API. No auth — a playground keys circuits by
// a client-supplied name.
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
export type State = "closed" | "open" | "half-open";

export interface Attempt {
  n: number;
  ok: boolean;
  status: number;
  ms: number;
  state: State;
  error: string | null;
}

// POST /api/call
export interface CallResult {
  ok: boolean;
  // True when an OPEN circuit refused the request — the upstream was never called.
  shed: boolean;
  state: State;
  status?: number;
  attempts: Attempt[];
  total_ms: number;
  retry_after_ms?: number;
  error?: string | null;
  detail?: string;
  upstream_body?: string | null;
}

// GET /api/circuit/{key}
export interface CircuitView {
  key: string;
  circuit: { state: State; failures: number; successes: number; changed_ms: number; probes: number };
  stats: { attempts: number; ok: number; failed: number; shed: number; trips: number };
  would_admit: boolean;
  retry_after_ms: number;
  open_for_ms: number;
}

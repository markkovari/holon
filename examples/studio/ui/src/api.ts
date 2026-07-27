// Client for the studio:app HTTP API.
//
// Everything the canvas needs to know about a component comes from
// `wit:reflect` via /api/components — the browser never parses wasm, and it
// never guesses which imports are composable.

export interface IfaceRef {
  raw: string;
  namespace: string;
  pkg: string;
  name: string;
  version: string;
}

export interface Surface {
  name: string;
  exports: IfaceRef[];
  /// Imports another component could satisfy — these get handles.
  imports: IfaceRef[];
  /// Imports only a host can satisfy — shown, but deliberately unwireable.
  host_imports: IfaceRef[];
  size_bytes: number;
  sha256: string;
  nested_instances: number;
}

export interface PaletteEntry {
  id: string;
  surface: Surface;
  uploaded: number;
}

export interface PlanStep {
  order: number;
  socket: string;
  plugs: string[];
  output: string;
  /// Interfaces wac will wire whether or not you drew them.
  also_satisfies: string[];
}

export interface Plan {
  steps: PlanStep[];
  unsatisfied: { node: string; iface: string; name: string }[];
  host_needs: IfaceRef[];
  cyclic: boolean;
  instance_count: number;
  over_instance_limit: boolean;
  depth: { id: string; depth: number }[];
  roots: string[];
  problems: { kind: string; detail: string }[];
  buildable: boolean;
}

export interface GraphEdge {
  plug: string;
  socket: string;
  iface: string;
}

const json = async <T>(r: Response): Promise<T> => (await r.json().catch(() => ({}))) as T;

export async function palette(): Promise<PaletteEntry[]> {
  const r = await fetch("/api/components");
  return (await json<{ components: PaletteEntry[] }>(r)).components ?? [];
}

/// Reflect a .wasm the user dropped in. The server refuses anything that isn't a
/// component, so a bad drop can't become a palette entry.
export async function upload(file: File): Promise<{ ok: boolean; id?: string; error?: string }> {
  const id = file.name.replace(/\.wasm$/, "").replace(/_/g, "-");
  const r = await fetch(`/api/components?id=${encodeURIComponent(id)}`, {
    method: "POST",
    headers: { "content-type": "application/wasm" },
    body: await file.arrayBuffer(),
  });
  const body = await json<{ id?: string; error?: string }>(r);
  return r.ok ? { ok: true, id: body.id ?? id } : { ok: false, error: body.error };
}

export async function plan(nodes: string[], edges: GraphEdge[]): Promise<Plan> {
  const r = await fetch("/api/plan", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ nodes, edges }),
  });
  return json<Plan>(r);
}

/// The authoritative connection check: which interfaces `wac` would actually
/// wire between these two components. Names matching is not enough — the types
/// have to fit, and this is wac's own SubtypeChecker answering.
export async function satisfies(socket: string, plug: string): Promise<string[]> {
  const r = await fetch("/api/satisfies", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ socket, plug }),
  });
  return (await json<{ interfaces?: string[] }>(r)).interfaces ?? [];
}

export type Form = "plug" | "wac" | "workload";

export async function emit(
  nodes: string[],
  edges: GraphEdge[],
  form: Form,
  meta: Record<string, unknown> = {},
): Promise<string> {
  const r = await fetch("/api/emit", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ nodes, edges, form, meta }),
  });
  return r.ok ? await r.text() : `# ${(await json<{ error?: string }>(r)).error ?? "emit failed"}`;
}

/// Compose for real. Returns the component bytes, or the reason it can't be built.
export async function compose(
  nodes: string[],
  edges: GraphEdge[],
  root: string,
): Promise<{ ok: true; blob: Blob; name: string; hostImports: string[] } | { ok: false; error: string }> {
  const r = await fetch("/api/compose", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ nodes, edges, root }),
  });
  if (!r.ok) {
    const e = await json<{ error?: string; detail?: string }>(r);
    return { ok: false, error: [e.error, e.detail].filter(Boolean).join(": ") || "compose failed" };
  }
  const hostImports = (r.headers.get("x-studio-host-imports") ?? "").split(",").filter(Boolean);
  return {
    ok: true,
    blob: await r.blob(),
    name: `${root.replace(/-/g, "_")}.composed.wasm`,
    hostImports,
  };
}

export const shortSize = (n: number) => (n >= 1024 * 1024 ? `${(n / 1048576).toFixed(1)}M` : `${Math.round(n / 1024)}K`);

/// `records:store/store@0.1.0` -> `records:store/store` for display; the version
/// is shown separately so a mismatch is visible rather than buried mid-string.
export const shortIface = (r: IfaceRef) =>
  r.namespace ? `${r.namespace}:${r.pkg}/${r.name}` : r.raw;

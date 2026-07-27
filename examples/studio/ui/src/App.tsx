import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  addEdge,
  Background,
  Controls,
  MiniMap,
  ReactFlow,
  ReactFlowProvider,
  useEdgesState,
  useNodesState,
  useReactFlow,
  type Connection,
  type Edge,
  type IsValidConnection,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import {
  AlertTriangle,
  Boxes,
  Check,
  Download,
  LayoutGrid,
  Package,
  Search,
  Upload,
  X,
} from "lucide-react";
import ComponentNode, { type ComponentFlowNode } from "./ComponentNode";
import {
  compose,
  emit,
  palette as fetchPalette,
  plan as fetchPlan,
  satisfies,
  shortSize,
  upload,
  type Form,
  type PaletteEntry,
  type Plan,
} from "./api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";

const nodeTypes = { component: ComponentNode };

export default function App() {
  return (
    <ReactFlowProvider>
      <Studio />
    </ReactFlowProvider>
  );
}

function Studio() {
  const { fitView } = useReactFlow();
  const [palette, setPalette] = useState<PaletteEntry[]>([]);
  const [filter, setFilter] = useState("");
  const [nodes, setNodes, onNodesChange] = useNodesState<ComponentFlowNode>([]);
  const [edges, setEdges, onEdgesChange] = useEdgesState<Edge>([]);
  const [plan, setPlan] = useState<Plan | null>(null);
  const [tab, setTab] = useState<"plan" | Form>("plan");
  const [text, setText] = useState("");
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);

  const reload = useCallback(async () => setPalette(await fetchPalette()), []);
  useEffect(() => {
    reload();
  }, [reload]);

  const nodeIds = useMemo(() => nodes.map((n) => n.id), [nodes]);
  const graphEdges = useMemo(
    () =>
      edges.map((e) => ({
        plug: e.source,
        socket: e.target,
        iface: e.sourceHandle ?? "",
      })),
    [edges],
  );

  // The plan is the server's opinion, recomputed on every edit. Nothing in the
  // browser decides what is buildable.
  const timer = useRef<number>();
  useEffect(() => {
    if (nodeIds.length === 0) {
      setPlan(null);
      return;
    }
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(async () => {
      setPlan(await fetchPlan(nodeIds, graphEdges));
    }, 120);
  }, [nodeIds, graphEdges]);

  // Re-emit whichever text tab is open.
  useEffect(() => {
    if (tab === "plan" || nodeIds.length === 0) return;
    let live = true;
    const root = plan?.roots[0] ?? nodeIds[0];
    emit(nodeIds, graphEdges, tab, { name: root, namespace: root }).then((t) => live && setText(t));
    return () => {
      live = false;
    };
  }, [tab, nodeIds, graphEdges, plan?.roots]);

  const gapsByNode = useMemo(() => {
    const m = new Map<string, Set<string>>();
    for (const g of plan?.unsatisfied ?? []) {
      if (!m.has(g.node)) m.set(g.node, new Set());
      m.get(g.node)!.add(g.iface);
    }
    return m;
  }, [plan]);

  // Push gap state into the nodes so each import handle can show it.
  useEffect(() => {
    setNodes((ns) =>
      ns.map((n) => ({
        ...n,
        data: { ...n.data, gaps: gapsByNode.get(n.id) ?? new Set<string>() },
      })),
    );
  }, [gapsByNode, setNodes]);

  const removeNode = useCallback(
    (id: string) => {
      setNodes((ns) => ns.filter((n) => n.id !== id));
      setEdges((es) => es.filter((e) => e.source !== id && e.target !== id));
    },
    [setNodes, setEdges],
  );

  const addComponent = useCallback(
    (entry: PaletteEntry) => {
      setNodes((ns) => {
        // One canvas node per component: `wac plug` gives each socket its own
        // instance anyway, and a shared node with two outgoing edges is how a
        // diamond is expressed.
        if (ns.some((n) => n.id === entry.id)) {
          setNote(`${entry.id} is already on the canvas`);
          return ns;
        }
        const col = ns.length % 4;
        const row = Math.floor(ns.length / 4);
        return [
          ...ns,
          {
            id: entry.id,
            type: "component" as const,
            position: { x: 40 + col * 300, y: 40 + row * 240 },
            data: {
              componentId: entry.id,
              surface: entry.surface,
              gaps: new Set<string>(),
              onRemove: removeNode,
            },
          },
        ];
      });
      window.setTimeout(() => fitView({ maxZoom: 1, padding: 0.15 }), 30);
    },
    [setNodes, removeNode, fitView],
  );

  /// Geometry first: a source handle and a target handle can only meet if they
  /// name the SAME interface. This runs while dragging, so an impossible edge
  /// never even snaps.
  const isValidConnection: IsValidConnection = useCallback(
    (c) => !!c.sourceHandle && c.sourceHandle === c.targetHandle && c.source !== c.target,
    [],
  );

  /// Then the real check: ask the server whether wac's SubtypeChecker agrees.
  /// Same interface name with an incompatible type is exactly the case a UI that
  /// only matched strings would wire up and ship broken.
  const onConnect = useCallback(
    async (c: Connection) => {
      const iface = c.sourceHandle ?? "";
      const fits = await satisfies(c.target, c.source);
      if (!fits.includes(iface)) {
        setNote(`wac refuses ${c.source} → ${c.target}: the types for ${iface} don't fit`);
        return;
      }
      setEdges((es) =>
        addEdge(
          // Emerald, matching the export handles it comes from — the default grey
          // hairline is invisible against the canvas.
          { ...c, animated: true, style: { strokeWidth: 2.5, stroke: "#10b981" } },
          es,
        ),
      );
      setNote("");
    },
    [setEdges],
  );

  /// Columns straight from the plan's topological depth — no layout library.
  /// With no edges every node is depth 0, so fall back to a grid rather than one
  /// unusable column.
  const arrange = useCallback(() => {
    if (!plan) return;
    const byDepth = new Map<number, string[]>();
    for (const { id, depth } of plan.depth) {
      if (!byDepth.has(depth)) byDepth.set(depth, []);
      byDepth.get(depth)!.push(id);
    }
    const flat = byDepth.size <= 1;
    setNodes((ns) =>
      ns.map((n, i) => {
        if (flat) {
          return { ...n, position: { x: 40 + (i % 3) * 300, y: 40 + Math.floor(i / 3) * 240 } };
        }
        const d = plan.depth.find((x) => x.id === n.id)?.depth ?? 0;
        const row = (byDepth.get(d) ?? []).indexOf(n.id);
        return { ...n, position: { x: 40 + d * 340, y: 40 + row * 230 } };
      }),
    );
    // Bring the whole graph back into view; a wired graph is wider than the pane.
    window.setTimeout(() => fitView({ maxZoom: 1, padding: 0.15, duration: 300 }), 30);
  }, [plan, setNodes, fitView]);

  const doCompose = useCallback(async () => {
    const root = plan?.roots[0];
    if (!root) {
      setNote("no root: something is plugged into every component");
      return;
    }
    setBusy(true);
    const result = await compose(nodeIds, graphEdges, root);
    setBusy(false);
    if (!result.ok) {
      setNote(result.error);
      return;
    }
    const url = URL.createObjectURL(result.blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = result.name;
    a.click();
    URL.revokeObjectURL(url);
    setNote(
      `composed ${result.name} (${shortSize(result.blob.size)}) — still imports ${result.hostImports.length} host interfaces`,
    );
  }, [plan, nodeIds, graphEdges]);

  const onDrop = useCallback(
    async (e: React.DragEvent) => {
      e.preventDefault();
      const files = [...e.dataTransfer.files].filter((f) => f.name.endsWith(".wasm"));
      if (files.length === 0) return;
      setBusy(true);
      for (const f of files) {
        const r = await upload(f);
        setNote(r.ok ? `reflected ${r.id}` : `${f.name}: ${r.error}`);
      }
      setBusy(false);
      reload();
    },
    [reload],
  );

  const shown = palette.filter((p) => p.id.includes(filter.trim().toLowerCase()));
  const groups: [string, PaletteEntry[]][] = [
    ["apps (serve HTTP)", shown.filter((p) => p.surface.exports.some((e) => e.name === "incoming-handler"))],
    ["capabilities", shown.filter((p) => !p.surface.exports.some((e) => e.name === "incoming-handler") && p.surface.host_imports.length > 0)],
    ["pure compute", shown.filter((p) => p.surface.host_imports.length === 0 && !p.surface.exports.some((e) => e.name === "incoming-handler"))],
  ];

  return (
    <div className="flex h-[100dvh] flex-col" onDragOver={(e) => e.preventDefault()} onDrop={onDrop}>
      <header className="flex flex-wrap items-center gap-2 border-b bg-card px-4 py-2">
        <Boxes className="size-5 text-primary" />
        <span className="font-semibold">studio</span>
        <span className="hidden text-xs text-muted-foreground sm:inline">
          · wire components, get the wac or wasmCloud form
        </span>
        <div className="flex-1" />
        {note && <span className="max-w-lg truncate text-xs text-muted-foreground">{note}</span>}
        <Button size="sm" variant="outline" onClick={arrange} disabled={!plan}>
          <LayoutGrid className="size-4" /> Arrange
        </Button>
        <Button size="sm" onClick={doCompose} disabled={busy || !plan?.buildable || !plan?.roots.length}>
          <Download className="size-4" /> Compose
        </Button>
      </header>

      <div className="grid flex-1 grid-cols-[220px_1fr_400px] overflow-hidden">
        {/* ---- palette ---- */}
        <aside className="flex flex-col overflow-hidden border-r">
          <div className="flex items-center gap-2 border-b px-2 py-2">
            <Search className="size-3.5 text-muted-foreground" />
            <Input
              className="h-7 text-xs"
              placeholder={`${palette.length} components`}
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
            />
          </div>
          <div className="flex-1 overflow-y-auto p-2">
            {palette.length === 0 && (
              <p className="px-1 text-xs text-muted-foreground">
                Empty. Run <code>just seed-studio</code>, or drop a <code>.wasm</code> anywhere here.
              </p>
            )}
            {groups.map(([label, items]) =>
              items.length === 0 ? null : (
                <div key={label} className="mb-3">
                  <div className="px-1 pb-1 text-[10px] uppercase tracking-wide text-muted-foreground">
                    {label} · {items.length}
                  </div>
                  {items.map((p) => (
                    <button
                      key={p.id}
                      onClick={() => addComponent(p)}
                      className="flex w-full items-center gap-1 rounded px-1 py-1 text-left text-xs hover:bg-accent"
                      title={`${p.surface.exports.length} exports · ${p.surface.imports.length} composable imports · ${p.surface.host_imports.length} host`}
                    >
                      <Package className="size-3 shrink-0 text-muted-foreground" />
                      <span className="truncate">{p.id}</span>
                      <span className="ml-auto shrink-0 text-[10px] text-muted-foreground">
                        {shortSize(p.surface.size_bytes)}
                      </span>
                    </button>
                  ))}
                </div>
              ),
            )}
          </div>
          <div className="border-t px-2 py-2 text-[10px] text-muted-foreground">
            <Upload className="mr-1 inline size-3" />
            drop a .wasm to reflect it
          </div>
        </aside>

        {/* ---- canvas ---- */}
        <main className="relative">
          <ReactFlow
            nodes={nodes}
            edges={edges}
            onNodesChange={onNodesChange}
            onEdgesChange={onEdgesChange}
            onConnect={onConnect}
            isValidConnection={isValidConnection}
            nodeTypes={nodeTypes}
            colorMode="dark"
            fitView
            // Without a maxZoom, a two-node graph fills the screen at 3x.
            fitViewOptions={{ maxZoom: 1, padding: 0.15 }}
            minZoom={0.2}
            proOptions={{ hideAttribution: false }}
          >
            <Background gap={16} />
            <Controls showInteractive={false} />
            <MiniMap pannable zoomable className="!bg-card" />
          </ReactFlow>
          {nodes.length === 0 && (
            <div className="pointer-events-none absolute inset-0 flex items-center justify-center">
              <p className="text-sm text-muted-foreground">
                Click components on the left to place them, then drag an export handle onto a
                matching import.
              </p>
            </div>
          )}
        </main>

        {/* ---- inspector ---- */}
        <aside className="flex flex-col overflow-hidden border-l">
          <div className="flex border-b text-xs">
            {(["plan", "plug", "wac", "workload"] as const).map((t) => (
              <button
                key={t}
                onClick={() => setTab(t)}
                className={`flex-1 px-2 py-2 ${tab === t ? "border-b-2 border-primary font-medium" : "text-muted-foreground"}`}
              >
                {t === "plug" ? "wac plug" : t === "wac" ? ".wac" : t === "workload" ? "workload" : "plan"}
              </button>
            ))}
          </div>
          <div className="flex-1 overflow-y-auto">
            {tab === "plan" ? (
              <PlanPanel plan={plan} />
            ) : (
              <div className="grid gap-2 p-2">
                <div className="flex items-center gap-2">
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => navigator.clipboard?.writeText(text)}
                  >
                    Copy
                  </Button>
                  <span className="text-[10px] text-muted-foreground">
                    {tab === "plug"
                      ? "each socket gets its OWN instance of every plug"
                      : tab === "wac"
                        ? "one instance per let — shared by every consumer"
                        : "components in one workload are linked in-process; only host interfaces appear"}
                  </span>
                </div>
                <pre className="overflow-x-auto whitespace-pre rounded bg-muted p-2 text-[10px] leading-relaxed">
                  {text || "…"}
                </pre>
              </div>
            )}
          </div>
        </aside>
      </div>
    </div>
  );
}

function PlanPanel({ plan }: { plan: Plan | null }) {
  if (!plan) {
    return <p className="p-3 text-xs text-muted-foreground">Place a component to see the plan.</p>;
  }
  return (
    <div className="grid gap-3 p-3 text-xs">
      <div className="flex flex-wrap items-center gap-2">
        {plan.buildable ? (
          <Badge className="bg-green-600">
            <Check className="mr-1 size-3" /> buildable
          </Badge>
        ) : (
          <Badge className="bg-red-600">
            <X className="mr-1 size-3" /> not buildable
          </Badge>
        )}
        <Badge className={plan.over_instance_limit ? "bg-red-600" : "bg-slate-600"}>
          ~{plan.instance_count} instances
        </Badge>
        {plan.roots.length > 0 && <span className="text-muted-foreground">root: {plan.roots.join(", ")}</span>}
      </div>

      {plan.problems.length > 0 && (
        <div className="grid gap-1">
          {plan.problems.map((p, i) => (
            <div key={i} className="flex gap-2 rounded border border-amber-600/40 bg-amber-600/10 p-2">
              <AlertTriangle className="mt-0.5 size-3.5 shrink-0 text-amber-500" />
              <div>
                <div className="font-mono text-[10px] text-amber-500">{p.kind}</div>
                <div className="text-muted-foreground">{p.detail}</div>
              </div>
            </div>
          ))}
        </div>
      )}

      <section>
        <h3 className="pb-1 font-medium">Build order</h3>
        {plan.steps.length === 0 ? (
          <p className="text-muted-foreground">Nothing to build — no edges yet.</p>
        ) : (
          <ol className="grid gap-2">
            {plan.steps.map((s) => (
              <li key={s.order} className="rounded border p-2">
                <div className="font-mono text-[11px]">
                  {s.order}. {s.socket} ← {s.plugs.join(", ")}
                </div>
                <div className="text-[10px] text-muted-foreground">→ {s.output}</div>
                {s.also_satisfies.length > 0 && (
                  <div className="pt-1 text-[10px] text-amber-600">
                    wac will also wire (you didn't draw these): {s.also_satisfies.join(", ")}
                  </div>
                )}
              </li>
            ))}
          </ol>
        )}
      </section>

      <section>
        <h3 className="pb-1 font-medium">
          Unsatisfied <span className="text-muted-foreground">({plan.unsatisfied.length})</span>
        </h3>
        {plan.unsatisfied.length === 0 ? (
          <p className="text-muted-foreground">Every composable import is wired.</p>
        ) : (
          <ul className="grid gap-0.5">
            {plan.unsatisfied.map((g, i) => (
              <li key={i} className="font-mono text-[10px]">
                <span className="text-amber-600">{g.node}</span> needs {g.iface}
              </li>
            ))}
          </ul>
        )}
      </section>

      <section>
        <h3 className="pb-1 font-medium">
          Host capabilities <span className="text-muted-foreground">({plan.host_needs.length})</span>
        </h3>
        <p className="pb-1 text-[10px] text-muted-foreground">
          A runtime provides these — composing never removes them. In a workload they become
          hostInterfaces, one entry per interface.
        </p>
        <ul className="flex flex-wrap gap-1">
          {plan.host_needs.map((h) => (
            <li key={h.raw} className="rounded bg-muted px-1 py-0.5 font-mono text-[10px]" title={h.raw}>
              {h.namespace}:{h.pkg}/{h.name}
            </li>
          ))}
        </ul>
      </section>
    </div>
  );
}

// A run as nodes and edges: run → round → attempt → capability.
//
// The flat list this replaces could tell you branch 3 scored higher than branch
// 7. It could not tell you they were the SAME round — which is the difference
// between a fan-out and a for-loop, and the whole reason the loop spawns branches
// at all. Rounds are columns because a round is a generation (ADR-0078), and a
// generation is the unit that gets thrown away and retried.
//
// ## Layout is arithmetic
//
// `@xyflow/react` ships no layout engine — its dependencies are zustand,
// classcat and @xyflow/system, and React Flow's own docs point you at dagre for
// this. It does not need one here: a run IS a tree of known depth, so the column
// is the depth and the row is the index, exactly as `examples/studio/ui` does it.
// A layout library asked to lay out a grid returns the same grid, slower, behind
// an async pass that `useNodesInitialized` then has to coordinate.
//
// No cap and no virtualisation. `branches × rounds` is bounded by two numbers a
// person typed on a command line, unlike the event log, which is bounded by
// nothing and is capped server-side for exactly that reason.

import { useCallback, useMemo } from "react";
import {
  Background,
  Controls,
  type Edge,
  type Node,
  Position,
  ReactFlow,
  ReactFlowProvider,
  useReactFlow,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";

import type { Attempt, Capability, Run } from "./Runs";

const COL = 260;
const ROW = 110;

/// Emerald for what passed, red for what did not, amber for interrupted — the
/// same three tones `Outcome` uses in the list, so a colour means one thing on
/// this page.
function tone(outcome?: string): { border: string; text: string } {
  if (outcome === "merged" || outcome === "passed") return { border: "#065f46", text: "#6ee7b7" };
  if (outcome === "interrupted") return { border: "#78350f", text: "#fcd34d" };
  if (outcome) return { border: "#7f1d1d", text: "#fca5a5" };
  return { border: "#334155", text: "#94a3b8" };
}

function box(border: string) {
  return {
    background: "#0f172a",
    border: `1px solid ${border}`,
    borderRadius: 6,
    padding: "8px 10px",
    width: 200,
    fontSize: 11,
    color: "#e2e8f0",
  };
}

export type Selection = { attempt: Attempt } | null;

/// Build the whole picture in one pass.
///
/// Split out of the component so it can be reasoned about (and, when it breaks,
/// read) without React in the way. Rounds come from the attempts themselves
/// rather than from `run.branches`: an attempt exists because it was written, and
/// a graph drawn from the run's INTENDED width would invent branches that never
/// spawned.
export function build(
  run: Run | null,
  attempts: Attempt[],
  capabilities: Capability[],
  selected: string | null,
): { nodes: Node[]; edges: Edge[] } {
  const nodes: Node[] = [];
  const edges: Edge[] = [];

  const rounds = [...new Set(attempts.map((a) => a.round ?? 0))].sort((x, y) => x - y);
  const byRound = new Map(rounds.map((r) => [r, attempts.filter((a) => (a.round ?? 0) === r)]));
  // The tallest column, so the spine nodes sit against a stable centre instead of
  // drifting as later rounds get narrower.
  const tallest = Math.max(1, ...rounds.map((r) => byRound.get(r)!.length));
  const spineY = ((tallest - 1) * ROW) / 2;

  const lr = { sourcePosition: Position.Right, targetPosition: Position.Left };

  nodes.push({
    id: "run",
    position: { x: 0, y: spineY },
    ...lr,
    data: {
      label: (
        <div>
          <div style={{ color: "#64748b", fontSize: 9, textTransform: "uppercase" }}>run</div>
          <div style={{ marginTop: 2 }}>{run?.goal ?? run?.id_text ?? "—"}</div>
        </div>
      ),
    },
    style: box(tone(run?.outcome).border),
    selectable: false,
  });

  rounds.forEach((r, ri) => {
    const mine = byRound.get(r)!;
    // Two columns per round: the round marker, then its branches. Depth is the
    // column, so the chain reads left to right in the order ADR-0092 writes it.
    const roundX = COL + ri * COL * 2;
    const attemptX = roundX + COL;

    nodes.push({
      id: `round-${r}`,
      position: { x: roundX, y: spineY },
      ...lr,
      data: {
        label: (
          <div>
            <div style={{ color: "#64748b", fontSize: 9, textTransform: "uppercase" }}>
              round {r}
            </div>
            <div style={{ marginTop: 2, color: "#94a3b8" }}>
              {mine.length} branch{mine.length === 1 ? "" : "es"}
            </div>
          </div>
        ),
      },
      style: box("#334155"),
      selectable: false,
    });
    edges.push({
      id: `e-round-${r}`,
      source: ri === 0 ? "run" : `round-${rounds[ri - 1]}`,
      target: `round-${r}`,
      style: { stroke: "#334155" },
    });

    mine.forEach((a, i) => {
      const t = tone(a.outcome);
      const on = selected === a.id_text;
      nodes.push({
        id: a.id_text,
        position: { x: attemptX, y: i * ROW },
        ...lr,
        data: {
          label: (
            <div>
              <div style={{ color: t.text }}>{a.branch ?? a.id_text}</div>
              <div style={{ marginTop: 3, color: "#64748b", display: "flex", gap: 8 }}>
                <span>{a.score ?? "—"}</span>
                {a.paths?.length ? <span>{a.paths.length} file(s)</span> : null}
              </div>
            </div>
          ),
        },
        // The selected node is outlined rather than recoloured: outcome already
        // owns colour here, and a selection that changed it would say a branch
        // passed because you clicked it.
        style: { ...box(t.border), outline: on ? "2px solid #38bdf8" : "none" },
      });
      edges.push({
        id: `e-${a.id_text}`,
        source: `round-${r}`,
        target: a.id_text,
        style: { stroke: t.border },
      });
    });
  });

  // Capabilities hang off the WINNER, because that is where they come from:
  // `goalrun` derives them from the winning branch's paths, and a component from
  // a branch that failed is a directory, not a capability (ADR-0089).
  const winner = attempts.find((a) => a.branch === run?.winner) ?? null;
  if (capabilities.length) {
    const capX = COL + rounds.length * COL * 2;
    capabilities.forEach((c, i) => {
      nodes.push({
        id: `cap-${c.name}`,
        position: { x: capX, y: i * ROW },
        ...lr,
        data: {
          label: (
            <div>
              <div style={{ color: "#64748b", fontSize: 9, textTransform: "uppercase" }}>
                capability
              </div>
              <div style={{ marginTop: 2, color: "#6ee7b7" }}>{c.name}</div>
            </div>
          ),
        },
        style: box("#065f46"),
        selectable: false,
      });
      edges.push({
        id: `e-cap-${c.name}`,
        // Without a winner the capability still happened, so it hangs off the run
        // rather than vanishing from the picture.
        source: winner?.id_text ?? "run",
        target: `cap-${c.name}`,
        style: { stroke: "#065f46" },
      });
    });
  }

  return { nodes, edges };
}

function Canvas({
  run,
  attempts,
  capabilities,
  selected,
  onSelect,
}: {
  run: Run | null;
  attempts: Attempt[];
  capabilities: Capability[];
  selected: string | null;
  onSelect: (id: string | null) => void;
}) {
  const { fitView } = useReactFlow();
  const { nodes, edges } = useMemo(
    () => build(run, attempts, capabilities, selected),
    [run, attempts, capabilities, selected],
  );

  // Refit when the SHAPE changes, not on every poll: a graph that re-centres
  // itself every two seconds is a graph you cannot pan.
  const shape = nodes.length + edges.length;
  const onInit = useCallback(() => fitView({ maxZoom: 1, padding: 0.15 }), [fitView]);
  useMemo(() => {
    window.setTimeout(() => fitView({ maxZoom: 1, padding: 0.15, duration: 300 }), 30);
  }, [shape, fitView]);

  return (
    <ReactFlow
      nodes={nodes}
      edges={edges}
      onInit={onInit}
      onNodeClick={(_, n) => onSelect(n.id === selected ? null : n.id)}
      onPaneClick={() => onSelect(null)}
      nodesDraggable={false}
      nodesConnectable={false}
      proOptions={{ hideAttribution: true }}
      fitView
    >
      <Background color="#1e293b" gap={20} />
      <Controls showInteractive={false} />
    </ReactFlow>
  );
}

export function RunGraph(props: {
  run: Run | null;
  attempts: Attempt[];
  capabilities: Capability[];
  selected: string | null;
  onSelect: (id: string | null) => void;
}) {
  return (
    <div
      data-testid="run-graph"
      className="h-[420px] rounded border border-slate-800 bg-slate-950"
    >
      <ReactFlowProvider>
        <Canvas {...props} />
      </ReactFlowProvider>
    </div>
  );
}

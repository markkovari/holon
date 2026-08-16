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

import { useCallback, useEffect, useMemo } from "react";
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

/// Column pitch, node width, row pitch, and where the spine sits above the
/// branches.
///
/// Tuned so a three-round run fits the pane at roughly 1:1 rather than being
/// `fitView`-ed down to unreadable. A round marker is deliberately SMALL and sits
/// just left of its own column: that keeps every edge pointing forwards, which is
/// what lets these be plain nodes with `Position.Left`/`Right` instead of custom
/// node types with named handles.
const COL = 260;
const ROW = 92;
const NODE_W = 150;
const MARK_W = 76;
const SPINE_Y = -120;
/// Where round 0's branches start. Far enough right that its marker clears the
/// run node.
const FIRST = 260;

/// Emerald for what passed, red for what did not, amber for interrupted — the
/// same three tones `Outcome` uses in the list, so a colour means one thing on
/// this page.
function tone(outcome?: string): { border: string; text: string } {
  if (outcome === "merged" || outcome === "passed") return { border: "#065f46", text: "#6ee7b7" };
  if (outcome === "interrupted") return { border: "#78350f", text: "#fcd34d" };
  if (outcome) return { border: "#7f1d1d", text: "#fca5a5" };
  return { border: "#334155", text: "#94a3b8" };
}

function box(border: string, width = NODE_W) {
  return {
    background: "#0f172a",
    border: `1px solid ${border}`,
    borderRadius: 6,
    padding: "6px 8px",
    width,
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

  const lr = { sourcePosition: Position.Right, targetPosition: Position.Left };

  nodes.push({
    id: "run",
    position: { x: 0, y: SPINE_Y },
    ...lr,
    data: {
      label: (
        <div>
          <div style={{ color: "#64748b", fontSize: 9, textTransform: "uppercase" }}>run</div>
          <div style={{ marginTop: 2, lineHeight: 1.25 }}>{run?.goal ?? run?.id_text ?? "—"}</div>
        </div>
      ),
    },
    style: box(tone(run?.outcome).border),
    selectable: false,
  });

  rounds.forEach((r, ri) => {
    const mine = byRound.get(r)!;
    // One column per round, with the marker tucked to its left. Depth is the
    // column, so the chain reads left to right in the order ADR-0092 writes it.
    const attemptX = FIRST + ri * COL;

    nodes.push({
      id: `round-${r}`,
      position: { x: attemptX - MARK_W - 16, y: SPINE_Y },
      ...lr,
      data: {
        label: (
          <div style={{ lineHeight: 1.3 }}>
            <div style={{ color: "#64748b", fontSize: 9, textTransform: "uppercase" }}>
              round {r}
            </div>
            <div style={{ color: "#94a3b8" }}>×{mine.length}</div>
          </div>
        ),
      },
      style: box("#334155", MARK_W),
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
            <div style={{ lineHeight: 1.3 }}>
              <div style={{ color: t.text }}>{a.branch ?? a.id_text}</div>
              <div style={{ marginTop: 2, color: "#64748b", display: "flex", gap: 8 }}>
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
    const capX = FIRST + rounds.length * COL;
    capabilities.forEach((c, i) => {
      nodes.push({
        id: `cap-${c.name}`,
        position: { x: capX, y: i * ROW },
        ...lr,
        data: {
          label: (
            <div style={{ lineHeight: 1.3 }}>
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
  //
  // Also on selection, because opening the panel narrows the canvas — refitting is
  // what stops the node you just clicked from ending up behind it.
  const shape = `${nodes.length}:${edges.length}:${!!selected}`;
  const onInit = useCallback(() => fitView({ maxZoom: 1, padding: 0.15 }), [fitView]);
  useEffect(() => {
    const t = window.setTimeout(
      () => fitView({ maxZoom: 1, padding: 0.15, duration: 300 }),
      // Long enough for the container's width change to reach the ResizeObserver
      // React Flow measures with; fitting against the old width undoes the point.
      60,
    );
    return () => window.clearTimeout(t);
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

/// React Flow's Controls ship light-on-white and render as a bright rectangle on
/// this page. Restyled rather than dropped: without them a graph that has been
/// panned has no visible way back, and `fitView` is the button people actually
/// want.
const CONTROLS_CSS = `
.react-flow__controls { box-shadow: none; }
.react-flow__controls-button {
  background: #0f172a;
  border-bottom: 1px solid #1e293b;
  fill: #94a3b8;
}
.react-flow__controls-button:hover { background: #1e293b; }
.react-flow__attribution { display: none; }
`;

export function RunGraph({
  panel,
  ...props
}: {
  run: Run | null;
  attempts: Attempt[];
  capabilities: Capability[];
  selected: string | null;
  onSelect: (id: string | null) => void;
  /// The selected branch's detail, laid OVER the graph rather than beside it.
  /// Beside it, the panel took a third of a container that is 768px wide for
  /// prose reasons, and `fitView` answered by shrinking the graph to unreadable.
  /// An overlay costs the graph nothing when nothing is selected.
  panel?: React.ReactNode;
}) {
  return (
    <div
      data-testid="run-graph"
      className="relative h-[480px] rounded border border-slate-800 bg-slate-950"
    >
      <style>{CONTROLS_CSS}</style>
      {/* The canvas GIVES UP the width the panel takes rather than being covered
          by it. React Flow measures its container, so shrinking the container is
          what makes `fitView` frame the graph into the space that is actually
          visible — an overlay on a full-width canvas hides whichever node you
          just clicked, which is reliably the one you wanted to see. */}
      <div className={`absolute inset-y-0 left-0 ${panel ? "right-[19rem]" : "right-0"}`}>
        <ReactFlowProvider>
          <Canvas {...props} />
        </ReactFlowProvider>
      </div>
      {panel && (
        <div className="absolute right-2 top-2 bottom-2 w-72 overflow-y-auto">{panel}</div>
      )}
    </div>
  );
}

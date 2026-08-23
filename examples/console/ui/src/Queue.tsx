// The worklist as a graph: every goal in the column its lifecycle put it in, and
// the runs it produced hanging off it.
//
// The list this replaces could tell you a goal was `running`. It could not tell
// you which of the four runs on the runs tab was ITS run, and that join is the
// question people actually arrive with — "what happened to the thing I started".
//
// ## Columns are the state machine
//
// `queued → running → awaiting-human → done`, left to right, in exactly the order
// `goal_may` in `platform-domain` allows. That is not decoration: a goal's column
// IS the set of moves available to it, so the picture and the buttons in the panel
// cannot drift apart. `failed` and `abandoned` are terminal, so they sit in a
// column of their own past the end — a dead-letter queue you can see the size of
// without opening it (ADR-0082 made failure terminal precisely so it accumulates
// somewhere visible).
//
// Layout is arithmetic for the same reason `RunGraph` is: this is a bucketed list,
// not a graph needing a solver.
//
// ## Runs join on the spec path, not the prose
//
// `run.spec` is the goal FILE `comp-goalrun --goal` was pointed at. Matching on
// `run.goal` — the prose — looked equivalent and is not: two goals can open with
// the same sentence, and re-running one goal after editing its text would strand
// every earlier run under nothing.

import { useMemo } from "react";
import { Position, type Edge, type Node } from "@xyflow/react";

import { Flow, box, COL, NODE_W, ROW } from "./RunGraph";
import type { Run } from "./Runs";

export type Goal = {
  id: string;
  title?: string;
  state?: string;
  spec?: string;
  frozen_spec?: string;
  priority?: number;
  reason?: string;
};

/// The lifecycle, as columns. Terminal states share the last one.
const LANES: { key: string; label: string; states: string[]; border: string; text: string }[] = [
  { key: "queued", label: "queued", states: ["queued"], border: "#334155", text: "#94a3b8" },
  { key: "running", label: "running", states: ["running"], border: "#1e40af", text: "#93c5fd" },
  {
    key: "awaiting-human",
    label: "awaiting you",
    states: ["awaiting-human"],
    border: "#78350f",
    text: "#fcd34d",
  },
  { key: "done", label: "done", states: ["done"], border: "#065f46", text: "#6ee7b7" },
  {
    key: "dead",
    label: "dead-letter",
    states: ["failed", "abandoned"],
    border: "#7f1d1d",
    text: "#fca5a5",
  },
];

/// The id of the node that opens the authoring form. Not a goal id — prefixed so
/// a click can be told apart from a real one without a second piece of state.
export const NEW_NODE = "new-goal";
export const isGoalNode = (id: string) => id.startsWith("goal:");
export const isRunNode = (id: string) => id.startsWith("run:");
export const goalIdOf = (id: string) => id.slice("goal:".length);
export const runIdOf = (id: string) => id.slice("run:".length);

/// A goal's runs: the ones driven from its spec file, newest first.
export function runsOf(goal: Goal, runs: Run[]): Run[] {
  const spec = goal.frozen_spec || goal.spec;
  if (!spec) return [];
  return runs.filter((r) => r.spec === spec);
}

export function build(
  goals: Goal[],
  runs: Run[],
  selected: string | null,
): { nodes: Node[]; edges: Edge[] } {
  const nodes: Node[] = [];
  const edges: Edge[] = [];
  const lr = { sourcePosition: Position.Right, targetPosition: Position.Left };

  LANES.forEach((lane, li) => {
    const x = li * COL;
    const mine = goals.filter((g) => lane.states.includes(g.state ?? "queued"));

    nodes.push({
      id: `lane-${lane.key}`,
      position: { x, y: -110 },
      ...lr,
      data: {
        label: (
          <div style={{ lineHeight: 1.3 }}>
            <div style={{ color: "#64748b", fontSize: 9, textTransform: "uppercase" }}>
              {lane.label}
            </div>
            <div style={{ color: lane.text }}>{mine.length}</div>
          </div>
        ),
      },
      style: box(lane.border),
      selectable: false,
    });

    mine.forEach((g, i) => {
      const id = `goal:${g.id}`;
      nodes.push({
        id,
        position: { x, y: i * ROW },
        ...lr,
        data: {
          label: (
            <div style={{ lineHeight: 1.3 }}>
              <div style={{ color: lane.text }}>{g.title ?? g.id}</div>
              {g.spec && (
                <div
                  style={{
                    marginTop: 2,
                    color: "#64748b",
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                  }}
                >
                  {g.spec.split("/").pop()}
                </div>
              )}
            </div>
          ),
        },
        style: { ...box(lane.border), outline: selected === id ? "2px solid #38bdf8" : "none" },
      });
      edges.push({
        id: `e-lane-${g.id}`,
        source: `lane-${lane.key}`,
        target: id,
        style: { stroke: lane.border },
      });

      // A goal's runs sit just right of it, inside the next lane's gutter. They
      // are SMALL: the run graph is where a run is read, and a full-size node
      // here would compete with the goal it belongs to for the same glance.
      runsOf(g, runs).forEach((r, ri) => {
        const rid = `run:${r.id_text}`;
        const passed = r.outcome === "merged" || r.outcome === "passed";
        const border = r.resolved_at ? (passed ? "#065f46" : "#7f1d1d") : "#1e40af";
        nodes.push({
          id: rid,
          position: { x: x + NODE_W + 24, y: i * ROW + ri * 30 },
          ...lr,
          data: {
            label: (
              <div style={{ fontSize: 10, lineHeight: 1.2 }}>
                <div style={{ color: "#64748b", fontSize: 9, textTransform: "uppercase" }}>run</div>
                <div style={{ color: "#cbd5e1" }}>{r.outcome ?? "running…"}</div>
              </div>
            ),
          },
          style: { ...box(border, 84), outline: selected === rid ? "2px solid #38bdf8" : "none" },
        });
        edges.push({ id: `e-run-${r.id_text}`, source: id, target: rid, style: { stroke: border } });
      });
    });

    // The way in, at the bottom of the queued column — where a new goal will
    // land, rather than in a form somewhere below the fold.
    if (lane.key === "queued") {
      nodes.push({
        id: NEW_NODE,
        position: { x, y: mine.length * ROW },
        ...lr,
        data: {
          label: (
            <div style={{ color: "#94a3b8", textAlign: "center" }}>+ write a goal</div>
          ),
        },
        style: { ...box("#475569"), borderStyle: "dashed" },
      });
      edges.push({
        id: "e-new",
        source: `lane-${lane.key}`,
        target: NEW_NODE,
        style: { stroke: "#475569", strokeDasharray: "4 4" },
      });
    }
  });

  return { nodes, edges };
}

export function QueueGraph({
  goals,
  runs,
  selected,
  onSelect,
  panel,
}: {
  goals: Goal[];
  runs: Run[];
  selected: string | null;
  onSelect: (id: string | null) => void;
  panel?: React.ReactNode;
}) {
  const { nodes, edges } = useMemo(() => build(goals, runs, selected), [goals, runs, selected]);
  return (
    <Flow
      nodes={nodes}
      edges={edges}
      selected={selected}
      onSelect={onSelect}
      panel={panel}
      testid="queue-graph"
    />
  );
}

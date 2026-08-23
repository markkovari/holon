// The command box: what a person types, turned into a move on the graph.
//
// No model. This was BENCHMARKED against one — Needle 2, a 45M tool-caller built
// for exactly this — and won 24/25 to 11/25 (`bench/NEEDLE-BENCH.md`). The reason
// is the shape of the problem, not the size of the model: every state is one of
// five words and every goal title is ON SCREEN when the person types, so they are
// picking from a visible list, not naming something unseen. Matching beats
// inference when the answer is already in front of you.
//
// Read `bench/needle/cases.json` before changing any rule here — it is the
// benchmark's input and the only thing that says whether an edit helped.

export type Action =
  | { kind: "state"; state: string }
  | { kind: "focus"; title: string }
  | { kind: "run"; title: string }
  | { kind: "none" };

/// The lifecycle words, and the ways people say them.
const STATES: Record<string, string[]> = {
  queued: ["queued", "queue", "not started", "waiting to start", "todo", "backlog"],
  running: ["running", "in flight", "in progress", "active", "going", "right now"],
  "awaiting-human": ["awaiting", "await", "review", "waiting on me", "needs me", "my review", "human"],
  done: ["done", "finished", "landed", "merged", "complete"],
  failed: ["failed", "fail", "dead letter", "dead-letter", "blew up", "broken", "error"],
};

const RUN_WORDS = ["run", "what happened", "happened in", "trace"];

/// Phrases that mean an action this box deliberately does not perform. Starting a
/// goal spends money and opens a pull request (ADR-0082) — it stays a button, and
/// a typed sentence must not become one.
const DENY = ["delete", "remove", "kill", "drop", "weather", "haiku", "write me", "start "];

/// Three letters and no signal. Without this, `the` alone ties every title
/// against every query and whichever goal is first always wins.
const STOP = new Set(["the", "and", "for", "all", "one", "ones", "what", "show", "open", "get"]);

const norm = (s: string) => s.toLowerCase().replace(/[^a-z0-9 ]/g, " ");
const words = (s: string) =>
  new Set(norm(s).split(/\s+/).filter((w) => w.length > 2 && !STOP.has(w)));

/// The best title match, and how many real words it shared.
function matchGoal(q: string, titles: string[]): { title: string | null; score: number } {
  const qw = words(q);
  let title: string | null = null;
  let score = 0;
  for (const t of titles) {
    let n = 0;
    for (const w of words(t)) if (qw.has(w)) n++;
    if (n > score) [title, score] = [t, n];
  }
  return { title, score };
}

/// Longest matching phrase wins, so `dead letter queue` is not `queue`.
function matchState(q: string): string | null {
  const n = norm(q);
  let state: string | null = null;
  let len = 0;
  for (const [s, phrases] of Object.entries(STATES)) {
    for (const p of phrases) if (n.includes(p) && p.length > len) [state, len] = [s, p.length];
  }
  return state;
}

export function parse(query: string, titles: string[]): Action {
  const n = ` ${norm(query)} `;
  if (DENY.some((d) => n.includes(d))) return { kind: "none" };

  const state = matchState(query);
  const { title, score } = matchGoal(query, titles);
  const asRun = RUN_WORDS.some((r) => n.includes(r));

  // A named goal beats a state word, because a state word is one token and a
  // title is several: "open drive the queue" contains "queue" and is not a
  // filter. Two title words is the bar; one is a coincidence.
  if (title && score >= 2) return asRun ? { kind: "run", title } : { kind: "focus", title };
  if (state) return { kind: "state", state };
  if (title) return asRun ? { kind: "run", title } : { kind: "focus", title };
  return { kind: "none" };
}

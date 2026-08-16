// The run view (ADR-0092): what happened, after the terminal closed.
//
// Read-only on purpose. The value here is persistence — being able to answer "why
// did branch 3 beat branch 7" a day later — and that arrives the moment the events
// are stored.
//
// The timeline is rendered from `events`, but the run's own bounds and outcome
// come from the run NODE. A timeline that reconstructs those by scanning its own
// events gets them wrong the moment one is missing, which is exactly when you are
// looking at the page.
//
// ## Polled, not pushed
//
// Refetching the whole detail on a timer, because it is the cut that works today:
// a socket needs a `ws:socket/handler` WIT contract decided before any host code
// exists, and this endpoint is the one a socket would push the same JSON down. The
// upgrade replaces `tick` and nothing else.

import { useEffect, useState } from "react";

import { RunGraph } from "./RunGraph";

/// How often an unresolved run is refetched.
const POLL_MS = 2000;
/// How many more polls happen AFTER `resolved_at` appears.
///
/// Not zero. `trace.rs` writes the run's resolution and the last attempts and
/// events as separate statements, and it COUNTS dropped writes rather than
/// retrying them — so the tail of a run can land after the resolution does.
/// Stopping the instant a run resolves truncates the timeline exactly at the end,
/// which is the part people opened the page for.
const GRACE_POLLS = 3;

export type Run = {
  id_text: string;
  goal?: string;
  outcome?: string;
  winner?: string;
  url?: string;
  branches?: number;
  started_at?: string;
  resolved_at?: string;
};

export type Attempt = {
  id_text: string;
  branch?: string;
  round?: number;
  outcome?: string;
  score?: number;
  /// The paths this branch wrote. Not its diff — that is in the pull request,
  /// addressable and reviewable; a copy here could only disagree with it.
  paths?: string[];
  tokens?: number;
  elapsed_ms?: number;
};

export type Capability = { name: string; path?: string };

type Ev = { kind: string; attempt?: string; at?: string; data?: any };

type Detail = {
  run: Run | null;
  attempts: Attempt[];
  events: Ev[];
  eventCount?: number;
  truncated?: boolean;
  capabilities?: Capability[];
};

const api = async (path: string) => {
  const r = await fetch(path, { credentials: "same-origin" });
  const text = await r.text();
  let body: any = null;
  try {
    body = text ? JSON.parse(text) : null;
  } catch {
    body = { error: text.slice(0, 300) };
  }
  if (!r.ok) throw new Error(body?.error ?? `HTTP ${r.status}`);
  return body;
};

export function RunList({ onOpen }: { onOpen: (id: string) => void }) {
  const [runs, setRuns] = useState<Run[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api("/api/runs")
      .then((r) => setRuns(r.runs ?? []))
      .catch((e) => setError(e.message));
  }, []);

  if (error)
    return (
      <div data-testid="runs-error" className="rounded border border-red-900 bg-red-950/50 px-4 py-3 text-sm text-red-200">
        {error}
      </div>
    );
  if (runs === null) return <p className="text-slate-400">Loading…</p>;
  if (runs.length === 0)
    return (
      <p data-testid="runs-empty" className="text-slate-400">
        No runs yet. One appears here as soon as a goal is started.
      </p>
    );

  return (
    <ul data-testid="run-list" className="divide-y divide-slate-800">
      {runs.map((r) => (
        <li key={r.id_text} className="py-3">
          <button
            data-testid={`run-${r.id_text}`}
            onClick={() => onOpen(r.id_text)}
            className="flex w-full items-baseline justify-between text-left hover:opacity-80"
          >
            <span className="truncate pr-4">{r.goal ?? r.id_text}</span>
            <Outcome value={r.outcome} />
          </button>
        </li>
      ))}
    </ul>
  );
}

export function RunDetail({ id, onBack }: { id: string; onBack: () => void }) {
  const [data, setData] = useState<Detail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);

  useEffect(() => {
    setData(null);
    setSelected(null);
    let stopped = false;
    let timer = 0;
    // Polls taken since the run reported itself resolved.
    let after = 0;
    let loaded = false;

    const tick = async () => {
      try {
        const next = await api(`/api/runs/${encodeURIComponent(id)}`);
        if (stopped) return;
        loaded = true;
        setData(next);
        if (next.run?.resolved_at) {
          if (++after > GRACE_POLLS) return;
        } else {
          after = 0;
        }
      } catch (e: any) {
        if (stopped) return;
        // Only fatal on the FIRST fetch. A poll that fails against a page already
        // showing a run leaves the run on screen — a transient blip is not a
        // reason to replace what somebody is reading with an error.
        if (!loaded) {
          setError(e.message);
          return;
        }
      }
      timer = window.setTimeout(tick, POLL_MS);
    };
    tick();
    return () => {
      stopped = true;
      window.clearTimeout(timer);
    };
  }, [id]);

  if (error)
    return (
      <div data-testid="run-error" className="rounded border border-red-900 bg-red-950/50 px-4 py-3 text-sm text-red-200">
        {error}
      </div>
    );
  if (!data) return <p className="text-slate-400">Loading…</p>;

  const run = data.run;
  const rounds = new Set(data.attempts.map((a) => a.round ?? 0)).size;
  const chosen = data.attempts.find((a) => a.id_text === selected) ?? null;
  return (
    <div data-testid="run-detail" className="space-y-6">
      <button onClick={onBack} className="text-xs text-slate-400 hover:text-slate-200">
        ← all runs
      </button>

      <div>
        <h2 data-testid="run-goal" className="text-lg">
          {run?.goal ?? id}
        </h2>
        <p className="mt-1 flex gap-3 text-xs text-slate-500">
          <span data-testid="run-outcome">{run?.outcome ?? "running"}</span>
          {/* The size, stated. The graph is not capped or virtualised — it is
              bounded by two numbers a person typed — so a graph panned off-screen
              would otherwise be indistinguishable from a graph missing nodes. */}
          {!!data.attempts.length && (
            <span data-testid="run-size">
              {data.attempts.length} attempt{data.attempts.length === 1 ? "" : "s"} over {rounds}{" "}
              round{rounds === 1 ? "" : "s"}
            </span>
          )}
          {run?.winner && <span data-testid="run-winner">won by {run.winner}</span>}
          {run?.url && (
            <a className="underline" href={run.url} target="_blank" rel="noreferrer">
              pull request
            </a>
          )}
        </p>
      </div>

      {/* What the pool gained. Above the branches because it is the only part of
          a run that outlives the pull request: the app change lands and is done,
          a capability is there for every run after this one (ADR-0089). */}
      {!!data.capabilities?.length && (
        <section data-testid="capabilities" className="rounded border border-emerald-900 bg-emerald-950/30 px-4 py-3">
          <h3 className="text-xs uppercase tracking-wide text-emerald-500">New capability</h3>
          <ul className="mt-1 space-y-0.5">
            {data.capabilities.map((c) => (
              <li key={c.name} className="text-sm text-emerald-200">
                <span className="font-medium">{c.name}</span>
                <span className="ml-2 font-mono text-xs text-emerald-700">{c.path}</span>
              </li>
            ))}
          </ul>
        </section>
      )}

      <section className="space-y-2">
        <h3 className="text-sm uppercase tracking-wide text-slate-500">Branches</h3>
        {data.attempts.length === 0 ? (
          <p data-testid="no-attempts" className="text-sm text-slate-500">
            No branches recorded yet.
          </p>
        ) : (
          <RunGraph
            run={run}
            attempts={data.attempts}
            capabilities={data.capabilities ?? []}
            selected={selected}
            onSelect={setSelected}
            panel={chosen && <AttemptPanel attempt={chosen} events={data.events} />}
          />
        )}
      </section>

      <section className="space-y-2">
        <h3 className="text-sm uppercase tracking-wide text-slate-500">What happened</h3>
        {/* A truncated timeline that does not say so looks like a run that
            stopped early — the more expensive of the two mistakes. */}
        {data.truncated && (
          <p data-testid="timeline-truncated" className="text-xs text-amber-400">
            showing the first {data.events.length} of {data.eventCount} events
          </p>
        )}
        {/* Selecting a branch HIGHLIGHTS its rows rather than filtering to them.
            Filtering would destroy the interleaving, and the interleaving is the
            only thing on this page that shows two branches running concurrently
            rather than one after the other. */}
        <ol data-testid="timeline" className="space-y-1 font-mono text-xs">
          {data.events.map((e, i) => {
            const on = !!selected && e.attempt === selected;
            return (
              <li
                key={i}
                data-selected={on || undefined}
                className={`flex gap-3 rounded px-1 ${on ? "bg-sky-950/60" : ""}`}
              >
                <span className="w-40 shrink-0 text-slate-600">{e.kind}</span>
                <span className="text-slate-400">{describe(e)}</span>
              </li>
            );
          })}
        </ol>
      </section>
    </div>
  );
}

/// One branch, in full: what it cost, what it touched, and what it did.
///
/// The facts here exist nowhere else once the terminal is gone — tokens and
/// duration are how a fan-out is told from a for-loop — and they were the flat
/// list's whole content. The graph shows the shape; this shows the detail, on
/// demand, for the one branch somebody asked about.
function AttemptPanel({ attempt, events }: { attempt: Attempt; events: Ev[] }) {
  const mine = events.filter((e) => e.attempt === attempt.id_text);
  return (
    <aside
      data-testid="attempt-panel"
      // Opaque, not translucent: it sits over the graph and the edges behind it
      // would otherwise read as strikethrough on the paths.
      className="space-y-3 rounded border border-slate-700 bg-slate-950 p-3 shadow-lg shadow-black/40"
    >
      <div className="flex items-baseline justify-between">
        <span data-testid="panel-branch" className="text-sm">
          {attempt.branch ?? attempt.id_text}
        </span>
        <Outcome value={attempt.outcome} />
      </div>

      <dl className="grid grid-cols-2 gap-y-1 text-xs text-slate-500">
        <dt>round</dt>
        <dd className="text-slate-300">{attempt.round ?? "—"}</dd>
        <dt>score</dt>
        <dd className="text-slate-300">{attempt.score ?? "—"}</dd>
        <dt>tokens</dt>
        <dd className="text-slate-300">{attempt.tokens ? fmt(attempt.tokens) : "—"}</dd>
        <dt>took</dt>
        <dd className="text-slate-300">{attempt.elapsed_ms ? secs(attempt.elapsed_ms) : "—"}</dd>
      </dl>

      {!!attempt.paths?.length && (
        <div>
          <h4 className="text-xs uppercase tracking-wide text-slate-600">Wrote</h4>
          {/* Paths, not the diff. The diff is in the pull request, addressable and
              reviewable; a copy here could only disagree with it. */}
          <ul data-testid="panel-paths" className="mt-1 space-y-0.5 font-mono text-xs text-slate-500">
            {attempt.paths.map((p) => (
              <li key={p} className="truncate" title={p}>
                {p}
              </li>
            ))}
          </ul>
        </div>
      )}

      <div>
        <h4 className="text-xs uppercase tracking-wide text-slate-600">Its events</h4>
        {mine.length === 0 ? (
          <p className="mt-1 text-xs text-slate-600">None in the loaded page.</p>
        ) : (
          <ol data-testid="panel-events" className="mt-1 space-y-1 font-mono text-xs">
            {mine.map((e, i) => (
              <li key={i}>
                <span className="text-slate-600">{e.kind}</span>{" "}
                <span className="text-slate-400">{describe(e)}</span>
              </li>
            ))}
          </ol>
        )}
      </div>
    </aside>
  );
}

/// One line per event, in the vocabulary's own terms.
///
/// A `capsearch-miss` is called out rather than rendered as data: it is the graph
/// naming a capability the pool lacks, which is the most actionable row on this
/// page (ADR-0089).
function describe(e: Ev): string {
  const d = e.data ?? {};
  switch (e.kind) {
    case "run-started":
      return `seed ${d.seed}`;
    case "branch-spawned":
      return `${d.branch} (round ${d.round})`;
    case "gate-verdict":
      return `${d.passed ? "passed" : "failed"} at ${d.score}`;
    case "lesson-read":
      return `${(d.keys ?? []).length} lesson(s)`;
    case "attempt-finished":
      return `${d.outcome} at ${d.score}`;
    case "capsearch-hit":
      return `${d.hits} for “${d.query}”`;
    case "capsearch-miss":
      return `nothing for “${d.query}” — the pool is missing this`;
    case "capability-added":
      // Rendered rather than left to the JSON fallback: this kind is vocabulary
      // now (ADR-0089), and a line of raw JSON in the middle of a readable
      // timeline is the view admitting it does not know what happened.
      return `${d.name} — the pool can do this now`;
    case "run-resolved":
      return d.winner ? `${d.outcome}, won by ${d.winner}` : d.outcome;
    default:
      return JSON.stringify(d);
  }
}

/// 12400 -> "12.4k". Long runs spend six figures of tokens and a raw number
/// beside a branch name is a number nobody reads.
function fmt(n: number): string {
  return n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n);
}

function secs(ms: number): string {
  return ms >= 60_000 ? `${Math.round(ms / 60_000)}m` : `${Math.round(ms / 1000)}s`;
}

function Outcome({ value }: { value?: string }) {
  const tone =
    value === "merged" || value === "passed"
      ? "text-emerald-400"
      : value === "interrupted"
        ? "text-amber-400"
        : value
          ? "text-red-400"
          : "text-slate-500";
  return <span className={`text-xs uppercase tracking-wide ${tone}`}>{value ?? "running"}</span>;
}

// The run view (ADR-0092): what happened, after the terminal closed.
//
// Read-only on purpose. Slice two's value is persistence — being able to answer
// "why did branch 3 beat branch 7" a day later — and that arrives the moment the
// events are stored, with no socket involved. Live push is slice three.
//
// The timeline is rendered from `events`, but the run's own bounds and outcome
// come from the run NODE. A timeline that reconstructs those by scanning its own
// events gets them wrong the moment one is missing, which is exactly when you are
// looking at the page.

import { useEffect, useState } from "react";

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

type Attempt = {
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

type Capability = { name: string; path?: string };

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

  useEffect(() => {
    setData(null);
    api(`/api/runs/${encodeURIComponent(id)}`)
      .then(setData)
      .catch((e) => setError(e.message));
  }, [id]);

  if (error)
    return (
      <div data-testid="run-error" className="rounded border border-red-900 bg-red-950/50 px-4 py-3 text-sm text-red-200">
        {error}
      </div>
    );
  if (!data) return <p className="text-slate-400">Loading…</p>;

  const run = data.run;
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
        <ul data-testid="attempts" className="divide-y divide-slate-800">
          {data.attempts.map((a) => (
            <li key={a.id_text} className="py-2 text-sm">
              <div className="flex items-baseline justify-between">
                <span>{a.branch ?? a.id_text}</span>
                <span className="flex items-baseline gap-3">
                  {/* Cost and duration exist nowhere else once the terminal is
                      gone, and they are how a fan-out is told from a for-loop. */}
                  {a.tokens ? <span className="text-xs text-slate-600">{fmt(a.tokens)} tok</span> : null}
                  {a.elapsed_ms ? <span className="text-xs text-slate-600">{secs(a.elapsed_ms)}</span> : null}
                  <span className="text-slate-500">{a.score ?? "—"}</span>
                  <Outcome value={a.outcome} />
                </span>
              </div>
              {!!a.paths?.length && (
                <ul className="mt-1 space-y-0.5 font-mono text-xs text-slate-600">
                  {a.paths.slice(0, 6).map((p) => (
                    <li key={p}>{p}</li>
                  ))}
                  {a.paths.length > 6 && <li>+{a.paths.length - 6} more</li>}
                </ul>
              )}
            </li>
          ))}
        </ul>
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
        <ol data-testid="timeline" className="space-y-1 font-mono text-xs">
          {data.events.map((e, i) => (
            <li key={i} className="flex gap-3">
              <span className="w-40 shrink-0 text-slate-600">{e.kind}</span>
              <span className="text-slate-400">{describe(e)}</span>
            </li>
          ))}
        </ol>
      </section>
    </div>
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

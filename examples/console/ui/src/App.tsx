// The Holon console, slice one: sign in, read the worklist, author a goal.
//
// Authoring is the point of this slice. It is one write that exercises the whole
// stack — session cookie, the console's proxy to the platform API, and `git:forge`
// opening a pull request — so a blank screen has one plausible cause rather than
// five.
//
// There is no token in this file, and there cannot be: the session lives in an
// HttpOnly cookie the console sets, so `credentials: "same-origin"` is the whole
// of the auth story on this side. That is deliberate — the page renders
// model-written prose, and a token any script could read is the wrong thing to
// have on it.

import { useEffect, useState } from "react";
import { RunDetail, RunList, type Run } from "./Runs";
import { parse } from "./query";
import {
  QueueGraph,
  NEW_NODE,
  goalIdOf,
  isGoalNode,
  isRunNode,
  runIdOf,
  runsOf,
  type Goal,
} from "./Queue";

type Session = { authenticated?: boolean; subject?: string };
type Project = { id: string; name?: string };

const api = async (path: string, init?: RequestInit) => {
  const r = await fetch(path, { credentials: "same-origin", ...init });
  const text = await r.text();
  let body: any = null;
  try {
    body = text ? JSON.parse(text) : null;
  } catch {
    // The platform answers JSON; anything else is a proxy or a gateway talking,
    // and showing the raw text beats showing "unexpected token <".
    body = { error: text.slice(0, 300) };
  }
  if (!r.ok) throw new Error(body?.error ?? `HTTP ${r.status}`);
  return body;
};

export default function App() {
  const [session, setSession] = useState<Session | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api("/api/session")
      .then(setSession)
      .catch((e) => setError(String(e.message)));
  }, []);

  if (error) return <Shell><Notice kind="error">{error}</Notice></Shell>;
  if (!session) return <Shell><p className="text-slate-400">Loading…</p></Shell>;
  if (session.authenticated === false) return <Shell><SignIn onDone={setSession} /></Shell>;
  return <Shell><Worklist subject={session.subject} /></Shell>;
}

function Shell({ children }: { children: React.ReactNode }) {
  return (
    <div className="min-h-screen bg-slate-950 text-slate-100">
      <header className="border-b border-slate-800 px-6 py-4">
        <h1 className="text-sm font-semibold tracking-wide text-slate-300">HOLON</h1>
      </header>
      {/* 5xl rather than 3xl. The prose here is short — goal titles and event
          lines — and the run graph is the widest thing the console renders; at
          3xl `fitView` was shrinking a three-round run until its labels were
          unreadable. */}
      <main className="mx-auto max-w-5xl px-6 py-8">{children}</main>
    </div>
  );
}

function Notice({ kind, children }: { kind: "error" | "ok"; children: React.ReactNode }) {
  const tone = kind === "error" ? "border-red-900 bg-red-950/50 text-red-200" : "border-emerald-900 bg-emerald-950/50 text-emerald-200";
  return <div className={`rounded border px-4 py-3 text-sm ${tone}`}>{children}</div>;
}

function SignIn({ onDone }: { onDone: (s: Session) => void }) {
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api("/api/session", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ email, password }),
      });
      // Re-ask rather than trusting the login's answer: the cookie is what
      // actually authenticates from here, so this proves it was set.
      onDone(await api("/api/session"));
    } catch (e: any) {
      setError(e.message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <form onSubmit={submit} className="max-w-sm space-y-3">
      <h2 className="text-lg">Sign in</h2>
      <Input value={email} onChange={setEmail} placeholder="email" type="email" />
      <Input value={password} onChange={setPassword} placeholder="password" type="password" />
      {error && <Notice kind="error">{error}</Notice>}
      <button disabled={busy} className="rounded bg-slate-100 px-4 py-2 text-sm font-medium text-slate-900 disabled:opacity-50">
        {busy ? "Signing in…" : "Sign in"}
      </button>
    </form>
  );
}

function Worklist({ subject }: { subject?: string }) {
  const [projects, setProjects] = useState<Project[] | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [goals, setGoals] = useState<Goal[] | null>(null);
  const [runs, setRuns] = useState<Run[]>([]);
  const [error, setError] = useState<string | null>(null);
  /// The node the graph has selected: a goal, a run, or the authoring node.
  const [node, setNode] = useState<string | null>(null);
  /// A lifecycle state the command box narrowed the graph to. Null shows all.
  const [only, setOnly] = useState<string | null>(null);

  useEffect(() => {
    api("/api/projects")
      .then((r) => {
        const list: Project[] = r?.projects ?? r ?? [];
        setProjects(list);
        if (list.length) setSelected(list[0].id);
      })
      .catch((e) => setError(e.message));
  }, []);

  const loadGoals = (project: string) =>
    api(`/api/projects/${encodeURIComponent(project)}/goals`)
      .then((r) => setGoals(r?.goals ?? r ?? []))
      .catch((e) => setError(e.message));

  // The runs are fetched for the queue graph, which hangs each one off the goal
  // whose spec drove it. Same endpoint the runs tab reads — one list, two
  // pictures of it.
  const loadRuns = () =>
    api("/api/runs")
      .then((r) => setRuns(r?.runs ?? []))
      .catch(() => setRuns([]));

  useEffect(() => {
    if (selected) {
      loadGoals(selected);
      loadRuns();
    }
  }, [selected]);

  const [tab, setTab] = useState<"goals" | "runs">("goals");
  const [openRun, setOpenRun] = useState<string | null>(null);

  const move = async (id: string, action: string, body?: any) => {
    setError(null);
    try {
      await api(`/api/goals/${encodeURIComponent(id)}/${action}`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: body ? JSON.stringify(body) : undefined,
      });
      if (selected) await loadGoals(selected);
    } catch (e: any) {
      setError(e.message);
    }
  };

  const goal = node && isGoalNode(node) ? goals?.find((g) => g.id === goalIdOf(node)) : undefined;

  return (
    <div className="space-y-8">
      <div className="flex items-baseline justify-between">
        {/* Two views and a back button do not need a router, and reaching for one
            before the second view exists is how a dependency arrives unearned. */}
        <nav className="flex gap-4 text-sm">
          {(["goals", "runs"] as const).map((t) => (
            <button
              key={t}
              data-testid={`tab-${t}`}
              onClick={() => {
                setTab(t);
                setOpenRun(null);
              }}
              className={tab === t ? "text-slate-100 underline" : "text-slate-500 hover:text-slate-300"}
            >
              {t}
            </button>
          ))}
        </nav>
        <p className="text-xs text-slate-500">signed in as {subject ?? "—"}</p>
      </div>
      {error && <Notice kind="error">{error}</Notice>}

      {tab === "runs" &&
        (openRun ? (
          <RunDetail id={openRun} onBack={() => setOpenRun(null)} />
        ) : (
          <RunList onOpen={setOpenRun} />
        ))}

      {tab === "goals" && projects?.length === 0 && (
        <Notice kind="error">
          No projects. Create one with <code>holon project add</code> — the console does not
          create projects yet.
        </Notice>
      )}

      {tab === "goals" && selected && (
        <section className="space-y-3">
          <div className="flex items-baseline justify-between">
            <h2 className="text-lg">Worklist</h2>
            <p className="text-xs text-slate-500">
              {goals?.length ?? 0} goal(s) · click one to act on it
            </p>
          </div>
          {goals === null ? (
            <p className="text-slate-400">Loading…</p>
          ) : (
            <>
            <CommandBox
              titles={(goals ?? []).map((g) => g.title ?? g.id)}
              only={only}
              onClear={() => setOnly(null)}
              onAction={(a) => {
                if (a.kind === "state") {
                  setOnly(a.state);
                  setNode(null);
                  return true;
                }
                if (a.kind === "none") return false;
                const g = goals?.find((x) => (x.title ?? x.id) === a.title);
                if (!g) return false;
                setOnly(null);
                if (a.kind === "focus") {
                  setNode(`goal:${g.id}`);
                  return true;
                }
                const r = runsOf(g, runs)[0];
                if (!r) return false;
                setTab("runs");
                setOpenRun(r.id_text);
                return true;
              }}
            />
            <QueueGraph
              goals={only ? goals.filter((g) => (g.state ?? "queued") === only) : goals}
              runs={runs}
              selected={node}
              onSelect={setNode}
              panel={
                node === NEW_NODE ? (
                  <Panel title="Write a goal" onClose={() => setNode(null)}>
                    <AuthorGoal
                      project={selected}
                      onQueued={() => {
                        loadGoals(selected);
                        setNode(null);
                      }}
                    />
                  </Panel>
                ) : goal ? (
                  <Panel title={goal.title ?? goal.id} onClose={() => setNode(null)}>
                    <GoalPanel
                      goal={goal}
                      runs={runsOf(goal, runs)}
                      onMove={move}
                      onOpenRun={(id) => {
                        setTab("runs");
                        setOpenRun(id);
                      }}
                    />
                  </Panel>
                ) : node && isRunNode(node) ? (
                  <Panel title="Run" onClose={() => setNode(null)}>
                    <button
                      onClick={() => {
                        setTab("runs");
                        setOpenRun(runIdOf(node));
                      }}
                      className="w-full rounded bg-slate-100 px-3 py-2 text-xs font-medium text-slate-900"
                    >
                      Open the run graph
                    </button>
                  </Panel>
                ) : undefined
              }
            />
            </>
          )}
        </section>
      )}
    </div>
  );
}

/// Type a sentence, move the graph.
///
/// No model behind this, and that was measured rather than assumed:
/// `bench/NEEDLE-BENCH.md` scores this exact file at 24/25 against a 45M
/// tool-calling model's 11/25. What it will not do is act — a phrase like "start
/// X" is refused here, because starting a goal spends money and opens a pull
/// request, and that stays a button somebody presses (ADR-0082).
function CommandBox({
  titles,
  only,
  onAction,
  onClear,
}: {
  titles: string[];
  only: string | null;
  onAction: (a: ReturnType<typeof parse>) => boolean;
  onClear: () => void;
}) {
  const [q, setQ] = useState("");
  const [miss, setMiss] = useState(false);

  const submit = (e: React.FormEvent) => {
    e.preventDefault();
    const a = parse(q, titles);
    // A query that matched nothing leaves the graph ALONE and says so. Guessing
    // — showing the first goal, or clearing the filter — is worse than nothing:
    // the person cannot tell a wrong answer from a right one they misread.
    const hit = a.kind !== "none" && onAction(a);
    setMiss(!hit);
    if (hit) setQ("");
  };

  return (
    <form onSubmit={submit} className="flex items-center gap-2">
      <input
        value={q}
        onChange={(e) => {
          setQ(e.target.value);
          setMiss(false);
        }}
        data-testid="command"
        placeholder="show me the failed ones · open drive the queue · what happened in X"
        className={`flex-1 rounded border bg-slate-900 px-3 py-2 text-sm outline-none ${
          miss ? "border-red-900 focus:border-red-700" : "border-slate-800 focus:border-slate-600"
        }`}
      />
      {only && (
        <button
          type="button"
          onClick={onClear}
          className="rounded border border-slate-700 px-2 py-1 text-xs text-slate-300 hover:border-slate-500"
        >
          {only} ✕
        </button>
      )}
    </form>
  );
}

function Panel({
  title,
  onClose,
  children,
}: {
  title: string;
  onClose: () => void;
  children: React.ReactNode;
}) {
  return (
    <div className="rounded border border-slate-800 bg-slate-900/95 p-3 text-sm">
      <div className="flex items-baseline justify-between gap-2">
        <h3 className="text-sm font-medium leading-tight">{title}</h3>
        <button onClick={onClose} className="text-xs text-slate-500 hover:text-slate-300">
          ✕
        </button>
      </div>
      <div className="mt-3 space-y-3">{children}</div>
    </div>
  );
}

/// What a goal is, and what may be done to it from here.
///
/// The buttons are the transitions `goal_may` allows OUT OF this state, and
/// nothing else — a `done` button on a queued goal would be a 409 the platform
/// has to refuse, and a UI that offers refused moves teaches people to ignore it.
///
/// `start` stays a button rather than something the daemon does, on purpose: it is
/// ADR-0082's one deliberate act per goal, the moment where stopping is free, and
/// the reason the interruption rate is a number anyone can measure.
function GoalPanel({
  goal,
  runs,
  onMove,
  onOpenRun,
}: {
  goal: Goal;
  runs: Run[];
  onMove: (id: string, action: string, body?: any) => void;
  onOpenRun: (id: string) => void;
}) {
  const state = goal.state ?? "queued";
  return (
    <>
      <dl className="space-y-1 text-xs text-slate-400">
        <div className="flex justify-between gap-2">
          <dt>state</dt>
          <dd className="uppercase tracking-wide text-slate-300">{state}</dd>
        </div>
        {goal.spec && (
          <div className="flex justify-between gap-2">
            <dt>spec</dt>
            <dd className="truncate font-mono text-slate-300" title={goal.spec}>
              {goal.spec}
            </dd>
          </div>
        )}
        {goal.reason && <p className="pt-1 text-red-300">{goal.reason}</p>}
      </dl>

      {runs.length > 0 && (
        <div className="space-y-1">
          <p className="text-xs uppercase tracking-wide text-slate-500">runs</p>
          {runs.map((r) => (
            <button
              key={r.id_text}
              onClick={() => onOpenRun(r.id_text)}
              className="block w-full truncate rounded border border-slate-800 px-2 py-1 text-left text-xs text-slate-300 hover:border-slate-600"
            >
              {r.outcome ?? "running…"} · {r.id_text}
            </button>
          ))}
        </div>
      )}

      <div className="flex flex-wrap gap-2">
        {state === "queued" && (
          <Action onClick={() => onMove(goal.id, "start")}>Start</Action>
        )}
        {state === "awaiting-human" && (
          <Action onClick={() => onMove(goal.id, "done")}>Land it</Action>
        )}
        {["running", "awaiting-human"].includes(state) && (
          <Action
            tone="danger"
            onClick={() => {
              const reason = window.prompt("Why did this fail?");
              if (reason) onMove(goal.id, "fail", { reason });
            }}
          >
            Dead-letter
          </Action>
        )}
      </div>
      {state === "queued" && (
        <p className="text-xs text-slate-500">
          Starting it spends money and opens a pull request. The daemon picks up what you
          start; nothing starts itself.
        </p>
      )}
    </>
  );
}

function Action({
  children,
  onClick,
  tone = "normal",
}: {
  children: React.ReactNode;
  onClick: () => void;
  tone?: "normal" | "danger";
}) {
  const cls =
    tone === "danger"
      ? "border border-red-900 text-red-300 hover:bg-red-950/50"
      : "bg-slate-100 text-slate-900";
  return (
    <button onClick={onClick} className={`rounded px-3 py-1.5 text-xs font-medium ${cls}`}>
      {children}
    </button>
  );
}

function AuthorGoal({ project, onQueued }: { project: string; onQueued: () => void }) {
  const [title, setTitle] = useState("");
  const [spec, setSpec] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [opened, setOpened] = useState<{ url: string; spec: string } | null>(null);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    setOpened(null);
    try {
      const r = await api("/api/goals", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ project, title, spec }),
      });
      setOpened({ url: r.pullRequest.url, spec: r.spec });
      setTitle("");
      setSpec("");
      onQueued();
    } catch (e: any) {
      setError(e.message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <form onSubmit={submit} className="space-y-3">
      <p className="text-xs text-slate-400">
        The spec is committed to the repository as a pull request and queued here. Nothing
        runs until you start it.
      </p>
      <Input value={title} onChange={setTitle} placeholder="What should happen" />
      <textarea
        value={spec}
        onChange={(e) => setSpec(e.target.value)}
        rows={8}
        placeholder="The goal, in prose. This becomes the file a model reads."
        className="w-full rounded border border-slate-800 bg-slate-900 px-3 py-2 font-mono text-sm outline-none focus:border-slate-600"
      />
      {error && <Notice kind="error">{error}</Notice>}
      {opened && (
        <Notice kind="ok">
          Queued, and <a className="underline" href={opened.url} target="_blank" rel="noreferrer">
            the pull request
          </a>{" "}
          adds <code>{opened.spec}</code>.
        </Notice>
      )}
      <button
        disabled={busy || !title.trim() || !spec.trim()}
        className="rounded bg-slate-100 px-4 py-2 text-sm font-medium text-slate-900 disabled:opacity-50"
      >
        {busy ? "Proposing…" : "Propose goal"}
      </button>
    </form>
  );
}

function Input({
  value,
  onChange,
  placeholder,
  type = "text",
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder: string;
  type?: string;
}) {
  return (
    <input
      type={type}
      value={value}
      onChange={(e) => onChange(e.target.value)}
      placeholder={placeholder}
      className="w-full rounded border border-slate-800 bg-slate-900 px-3 py-2 text-sm outline-none focus:border-slate-600"
    />
  );
}

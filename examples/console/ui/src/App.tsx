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
import { RunDetail, RunList } from "./Runs";

type Session = { authenticated?: boolean; subject?: string };
type Project = { id: string; name?: string };
type Goal = { id: string; title?: string; state?: string; spec?: string; priority?: number };

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
      <main className="mx-auto max-w-3xl px-6 py-8">{children}</main>
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
  const [error, setError] = useState<string | null>(null);

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

  useEffect(() => {
    if (selected) loadGoals(selected);
  }, [selected]);

  const [tab, setTab] = useState<"goals" | "runs">("goals");
  const [openRun, setOpenRun] = useState<string | null>(null);

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
        <>
          <section className="space-y-3">
            <h2 className="text-lg">Worklist</h2>
            {goals === null && <p className="text-slate-400">Loading…</p>}
            {goals?.length === 0 && <p className="text-slate-400">Nothing queued.</p>}
            <ul className="divide-y divide-slate-800">
              {goals?.map((g) => (
                <li key={g.id} className="flex items-baseline justify-between py-2">
                  <span>{g.title ?? g.id}</span>
                  <span className="text-xs uppercase tracking-wide text-slate-500">{g.state}</span>
                </li>
              ))}
            </ul>
          </section>

          <AuthorGoal project={selected} onQueued={() => loadGoals(selected)} />
        </>
      )}
    </div>
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
    <form onSubmit={submit} className="space-y-3 border-t border-slate-800 pt-6">
      <h2 className="text-lg">Write a goal</h2>
      <p className="text-sm text-slate-400">
        The spec is committed to the repository as a pull request and queued here. Nothing
        runs until you start it.
      </p>
      <Input value={title} onChange={setTitle} placeholder="What should happen" />
      <textarea
        value={spec}
        onChange={(e) => setSpec(e.target.value)}
        rows={10}
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

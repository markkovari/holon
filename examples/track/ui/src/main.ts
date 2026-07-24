import "./style.css";
import {
  api, getToken, setToken, clearToken,
  type Principal, type Project, type Issue, type IssueDetail, type ActivityEvent,
} from "./api";

const COLS: [string, string][] = [
  ["backlog", "Backlog"], ["todo", "Todo"], ["in_progress", "In progress"], ["done", "Done"],
];
const NEXT: Record<string, [string, string][]> = {
  backlog: [["start", "→ todo"]],
  todo: [["begin", "→ in progress"], ["shelve", "→ backlog"]],
  in_progress: [["finish", "→ done"], ["stop", "→ todo"]],
  done: [["reopen", "→ todo"]],
};

const state: { me: Principal | null; project: string | null; projects: Project[]; es: EventSource | null } = {
  me: null, project: null, projects: [], es: null,
};

const esc = (s: unknown) =>
  String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]!));

const root = document.getElementById("root")!;

// ---- shells -----------------------------------------------------------------

function authView(msg = "") {
  root.innerHTML = `
    <div class="auth">
      <h1 style="margin:0">track</h1>
      <div style="color:var(--muted);font-size:.85rem">a Linear-lite tracker — register the first user as admin</div>
      <input id="email" placeholder="email" value="admin@track.io">
      <input id="password" type="password" placeholder="password" value="pw12345678">
      <div class="row"><button id="login">Log in</button><button class="ghost" id="register">Register (admin)</button></div>
      <div style="color:var(--flag);font-size:.85rem">${esc(msg)}</div>
    </div>`;
  const email = () => (document.getElementById("email") as HTMLInputElement).value;
  const pw = () => (document.getElementById("password") as HTMLInputElement).value;
  document.getElementById("register")!.onclick = async () => {
    const r = await api.register(email(), pw(), "admin");
    if (!r.ok && r.status !== 409) return authView((r.data as any).error ?? "register failed");
    doLogin(email(), pw());
  };
  document.getElementById("login")!.onclick = () => doLogin(email(), pw());
}

async function doLogin(email: string, pw: string) {
  const r = await api.login(email, pw);
  if (!r.ok) return authView((r.data as any).error ?? "login failed");
  setToken(r.data.access_token);
  boot();
}

function appView() {
  root.innerHTML = `
    <div class="top">
      <h1>track</h1><span class="dim" id="who"></span><span class="sp"></span>
      <select id="proj"></select>
      <button class="ghost" id="newProj">+ project</button>
      <button class="ghost" id="newIssue">+ issue</button>
      <button class="ghost" id="tick" title="run the background stale-issue sweep">sweep</button>
      <button class="ghost" id="logout">logout</button>
    </div>
    <div class="layout">
      <div>
        <div class="row" style="margin-bottom:.7rem"><input id="q" placeholder="search issues…" style="flex:1"><button class="ghost" id="search">search</button></div>
        <div class="board" id="board"></div>
      </div>
      <div class="feed"><h3>Activity <span class="live" id="live">● live</span></h3><div id="events"></div></div>
    </div>
    <dialog id="modal"><div class="modal" id="modalBody"></div></dialog>`;

  document.getElementById("logout")!.onclick = async () => { await api.logout(); clearToken(); authView(); };
  document.getElementById("newProj")!.onclick = onNewProject;
  document.getElementById("newIssue")!.onclick = onNewIssue;
  document.getElementById("tick")!.onclick = async () => {
    const r = await api.tick();
    alert(`swept ${r.data.swept}, flagged ${r.data.flagged}`);
    loadBoard();
  };
  document.getElementById("proj")!.addEventListener("change", (e) => {
    state.project = (e.target as HTMLSelectElement).value;
    loadBoard();
  });
  document.getElementById("search")!.onclick = doSearch;
  document.getElementById("q")!.addEventListener("keydown", (e) => { if ((e as KeyboardEvent).key === "Enter") doSearch(); });
}

// ---- boot -------------------------------------------------------------------

async function boot() {
  const me = await api.me();
  if (!me.ok) { clearToken(); return authView(); }
  state.me = me.data;
  appView();
  document.getElementById("who")!.textContent = `${me.data.subject.slice(0, 12)}… · ${me.data.roles.join(",")}`;
  await loadProjects();
  connectFeed();
}

async function loadProjects() {
  const r = await api.projects();
  state.projects = r.data.projects ?? [];
  const sel = document.getElementById("proj") as HTMLSelectElement;
  sel.innerHTML = state.projects.map((p) => `<option value="${p.id}">${esc(p.key)} — ${esc(p.name)}</option>`).join("");
  if (state.projects.length) { state.project = state.projects[0].id; sel.value = state.project; await loadBoard(); }
  else { state.project = null; document.getElementById("board")!.innerHTML = `<div style="color:var(--muted)">No projects. Create one.</div>`; }
}

async function onNewProject() {
  const key = prompt("project key (e.g. ENG)"); if (!key) return;
  const name = prompt("project name") ?? key;
  const r = await api.createProject(key, name);
  if (!r.ok) return alert((r.data as any).error);
  await loadProjects();
}

async function onNewIssue() {
  if (!state.project) return alert("create a project first");
  const title = prompt("issue title"); if (!title) return;
  const body = prompt("description (markdown ok)") ?? "";
  const label = prompt("label (optional)") ?? "";
  const r = await api.createIssue(state.project, title, body, label || undefined);
  if (!r.ok) return alert((r.data as any).error);
  loadBoard();
}

// ---- board ------------------------------------------------------------------

function card(i: Issue): string {
  return `<div class="card" data-issue="${i.id}">
    <div class="r">${esc(i.ref)}${i.flagged ? ' <span class="flag">⚑ stale</span>' : ""}</div>
    <div class="t">${esc(i.title)}</div>
    ${i.label ? `<div class="lb">${esc(i.label)}</div>` : ""}</div>`;
}

async function loadBoard() {
  if (!state.project) return;
  const r = await api.issues(state.project);
  const by: Record<string, Issue[]> = {};
  COLS.forEach(([k]) => (by[k] = []));
  (r.data.issues ?? []).forEach((i) => (by[i.status] ??= []).push(i));
  document.getElementById("board")!.innerHTML = COLS.map(([k, label]) =>
    `<div class="col ${k}"><h3><span class="dot"></span>${label} <span style="color:var(--muted)">${by[k].length}</span></h3>${by[k].map(card).join("")}</div>`,
  ).join("");
  wireCards();
}

function wireCards() {
  document.querySelectorAll<HTMLElement>("[data-issue]").forEach((el) => {
    el.onclick = () => openIssue(el.dataset.issue!);
  });
}

// ---- issue modal ------------------------------------------------------------

async function openIssue(id: string) {
  const r = await api.issue(id);
  const i: IssueDetail = r.data;
  const moves = (NEXT[i.status] ?? []).map(([ev, lbl]) => `<button class="ghost" data-move="${ev}">${lbl}</button>`).join("");
  document.getElementById("modalBody")!.innerHTML = `
    <div class="head"><div><span class="r" style="font-family:ui-monospace">${esc(i.ref)}</span> <b>${esc(i.title)}</b></div><button class="ghost" id="close">✕</button></div>
    <div class="row"><span style="color:var(--muted)">${esc(i.status)}${i.flagged ? " · ⚑ stale" : ""}</span> ${moves}<button class="ghost" id="ai">✨ summarize</button></div>
    <div class="summary hide" id="aiOut"></div>
    <div>${i.html ?? esc(i.body ?? "")}</div>
    <div id="comments">${(i.comments ?? []).map((c) => `<div class="comment"><span style="color:var(--muted)">${esc(c.author.slice(0, 10))}…</span> ${c.html}</div>`).join("")}</div>
    <div class="row"><input id="cmt" placeholder="comment (markdown)…" style="flex:1"><button id="reply">reply</button></div>`;
  const dlg = document.getElementById("modal") as HTMLDialogElement;
  dlg.showModal();
  document.getElementById("close")!.onclick = () => dlg.close();
  document.querySelectorAll<HTMLElement>("[data-move]").forEach((b) => {
    b.onclick = async () => {
      const rr = await api.move(id, b.dataset.move!);
      if (!rr.ok) return alert((rr.data as any).error);
      dlg.close(); loadBoard();
    };
  });
  document.getElementById("reply")!.onclick = async () => {
    const v = (document.getElementById("cmt") as HTMLInputElement).value.trim();
    if (!v) return;
    const rr = await api.comment(id, v);
    if (!rr.ok) return alert((rr.data as any).error);
    openIssue(id);
  };
  document.getElementById("ai")!.onclick = async () => {
    const out = document.getElementById("aiOut")!;
    out.classList.remove("hide"); out.textContent = "summarizing…";
    const rr = await api.summarize(id);
    out.textContent = rr.ok ? rr.data.summary : ((rr.data as any).error ?? "ai failed");
  };
}

// ---- search -----------------------------------------------------------------

async function doSearch() {
  const q = (document.getElementById("q") as HTMLInputElement).value.trim();
  if (!q) return loadBoard();
  const r = await api.search(q, state.project ?? "");
  const hits = r.data.hits ?? [];
  document.getElementById("board")!.innerHTML =
    `<div class="col" style="grid-column:1/-1"><h3>Search: “${esc(q)}” — ${hits.length} hits</h3>${hits.map(card).join("") || '<div style="color:var(--muted)">no matches</div>'}</div>`;
  wireCards();
}

// ---- SSE activity feed ------------------------------------------------------

function connectFeed() {
  if (state.es) state.es.close();
  const es = new EventSource("/api/stream");
  state.es = es;
  es.onmessage = (e) => {
    let ev: ActivityEvent;
    try { ev = JSON.parse(e.data); } catch { return; }
    const d = (ev.detail ?? {}) as Record<string, string>;
    const line = document.createElement("div");
    line.className = "ev";
    line.innerHTML = `<b>${esc(ev.kind)}</b> ${d.ref ? esc(d.ref) : ""} <span style="color:var(--muted)">${d.by ? esc(d.by.slice(0, 8)) + "…" : ""}</span>`;
    const feed = document.getElementById("events");
    if (!feed) return;
    feed.prepend(line);
    while (feed.children.length > 30) feed.lastChild!.remove();
    if (["issue.created", "issue.moved", "issue.flagged"].includes(ev.kind)) loadBoard();
  };
  es.onopen = () => { const l = document.getElementById("live"); if (l) l.textContent = "● live"; };
  es.onerror = () => { const l = document.getElementById("live"); if (l) l.textContent = "● reconnecting"; };
}

// ---- start ------------------------------------------------------------------

if (getToken()) boot(); else authView();

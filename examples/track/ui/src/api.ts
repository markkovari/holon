// Typed API client for the track backend. Carries the bearer token and returns
// {ok, status, data} so callers branch on ok without try/catch everywhere.

export interface Principal { subject: string; tenant: string; roles: string[] }
export interface Project { id: string; key: string; name: string; lead: string }
export interface Issue {
  id: string; ref: string; project: string; title: string; label: string;
  assignee: string; reporter: string; status: string; flagged: boolean;
  created: number; updated: number; score?: number;
}
export interface Comment { id: string; author: string; body: string; html: string; at: number }
export interface IssueDetail extends Issue { comments: Comment[]; allowed_events: string[]; html?: string; body?: string }
export interface ActivityEvent { kind: string; detail: Record<string, unknown>; at: number }

export type Status = "backlog" | "todo" | "in_progress" | "done";

let token = localStorage.getItem("track_tok") ?? "";
export const getToken = () => token;
export function setToken(t: string) { token = t; localStorage.setItem("track_tok", t); }
export function clearToken() { token = ""; localStorage.removeItem("track_tok"); }

export interface Res<T> { ok: boolean; status: number; data: T }

async function req<T = any>(method: string, path: string, body?: unknown): Promise<Res<T>> {
  const headers: Record<string, string> = {};
  if (token) headers.authorization = `Bearer ${token}`;
  if (body !== undefined) headers["content-type"] = "application/json";
  const r = await fetch(path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) });
  const data = r.status === 204 ? ({} as T) : await r.json().catch(() => ({} as T));
  return { ok: r.ok, status: r.status, data };
}

export const api = {
  register: (email: string, password: string, role?: string) =>
    req("POST", "/auth/register", { email, password, role }),
  login: (email: string, password: string) =>
    req<{ access_token: string }>("POST", "/auth/login", { email, password }),
  logout: () => req("POST", "/auth/logout"),
  me: () => req<Principal>("GET", "/auth/me"),

  projects: () => req<{ projects: Project[] }>("GET", "/api/projects"),
  createProject: (key: string, name: string) => req<Project>("POST", "/api/projects", { key, name }),
  addMember: (pid: string, subject: string, role: string) =>
    req("POST", `/api/projects/${pid}/members`, { subject, role }),

  issues: (project: string) => req<{ issues: Issue[] }>("GET", `/api/issues?project=${project}`),
  createIssue: (project: string, title: string, body: string, label?: string) =>
    req<Issue>("POST", "/api/issues", { project, title, body, label }),
  issue: (id: string) => req<IssueDetail>("GET", `/api/issues/${id}`),
  move: (id: string, event: string) => req<{ status: string }>("POST", `/api/issues/${id}/move`, { event }),
  comment: (id: string, body: string) => req("POST", `/api/issues/${id}/comments`, { body }),
  summarize: (id: string) => req<{ summary: string }>("POST", `/api/issues/${id}/summarize`),

  search: (q: string, project: string) =>
    req<{ hits: Issue[] }>("GET", `/api/search?q=${encodeURIComponent(q)}&project=${project}`),
  tick: () => req<{ swept: number; flagged: number }>("POST", "/api/tick"),
};

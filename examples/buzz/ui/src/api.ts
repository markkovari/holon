// Thin client for the buzz:app HTTP API. The HOST token lives in localStorage;
// players are anonymous (they carry a player id, not a token).
let token: string | null = localStorage.getItem("buzz-tok");

export function setToken(t: string | null) {
  token = t;
  if (t) localStorage.setItem("buzz-tok", t);
  else localStorage.removeItem("buzz-tok");
}
export const hasToken = () => !!token;

export async function api<T = any>(
  path: string,
  method = "GET",
  body?: unknown,
): Promise<{ ok: boolean; status: number; data: T }> {
  const headers: Record<string, string> = { "content-type": "application/json" };
  if (token) headers.authorization = `Bearer ${token}`;
  const r = await fetch(`/api${path}`, {
    method,
    headers,
    body: body ? JSON.stringify(body) : undefined,
  });
  const data = (await r.json().catch(() => ({}))) as T;
  return { ok: r.ok, status: r.status, data };
}

// ---- types ----
export type Phase = "lobby" | "question" | "reveal" | "final";
export interface Me { subject: string; roles: string[] }
export interface Quiz { id: string; title: string; question_count: number }
export interface HostQuestion { index: number; prompt: string; options: string[]; answer: number; time_limit: number }
export interface HostView {
  pin: string; phase: Phase; quiz_title: string; current: number; total: number;
  players: { nickname: string; score: number }[];
  leaderboard: { nickname: string; score: number }[];
  question?: HostQuestion; answered?: number; counts?: number[];
}
export interface PlayView {
  phase: Phase; nickname: string; players_count: number; my_score: number; my_rank: number;
  question?: { index: number; total: number; prompt: string; options: string[]; time_limit: number; time_left_ms: number; answered: boolean };
  reveal?: { correct_option: number; my_option: number | null; my_correct: boolean; my_points: number };
  podium?: { nickname: string; score: number }[];
}

// Kahoot-style option colors + shapes.
export const OPT = [
  { bg: "bg-red-600", shape: "▲" },
  { bg: "bg-blue-600", shape: "◆" },
  { bg: "bg-yellow-500", shape: "●" },
  { bg: "bg-green-600", shape: "■" },
  { bg: "bg-purple-600", shape: "★" },
  { bg: "bg-pink-600", shape: "✚" },
];

// Thin client for the lms:app HTTP API. The token lives in localStorage so a
// reload keeps you signed in.
let token: string | null = localStorage.getItem("lms-tok");

export function setToken(t: string | null) {
  token = t;
  if (t) localStorage.setItem("lms-tok", t);
  else localStorage.removeItem("lms-tok");
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

// Fetch an authed file (the certificate PDF) and trigger a browser download.
export async function download(path: string, filename: string) {
  const headers: Record<string, string> = {};
  if (token) headers.authorization = `Bearer ${token}`;
  const r = await fetch(`/api${path}`, { headers });
  if (!r.ok) return;
  const url = URL.createObjectURL(await r.blob());
  const a = document.createElement("a");
  a.href = url; a.download = filename; a.click();
  URL.revokeObjectURL(url);
}

// ---- types ----
export interface Me { subject: string; roles: string[]; email: string; is_instructor: boolean }
export interface Course { id: string; code: string; title: string; description: string; instructor_email: string; enrolled: boolean; is_mine: boolean; lessons: number; quizzes: number }
export interface Lesson { id: string; title: string; body: string }
export interface Question { prompt: string; options: string[]; answer?: number }
export interface Quiz { id: string; title: string; pass_mark: number; questions: Question[] }
export interface CourseDetail { course: Course; lessons: Lesson[]; quizzes: Quiz[]; enrolled: boolean; is_mine: boolean }
export interface SubmitResult { correct: number; total: number; score_pct: number; passed: boolean }
export interface ProgressRow { quiz: string; title: string; best_score: number; passed: boolean; attempted: boolean }
export interface Progress { quizzes: ProgressRow[]; passed_all: boolean; completion_pct: number; certificate_eligible: boolean }
export interface GbStudent { email: string; scores: Record<string, number>; average: number; passed_all: boolean }
export interface GbQuiz { id: string; title: string; pass_mark: number; mean: number; median: number; pass_count: number; count: number }
export interface Gradebook { students: GbStudent[]; quizzes: GbQuiz[]; enrolled: number; chart_svg: string }

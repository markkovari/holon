import { useEffect, useState } from "react";
import { GraduationCap, LogOut, Plus, Download, Check, X, BarChart3, BookOpen } from "lucide-react";
import {
  api, download, setToken, hasToken,
  type Me, type Course, type CourseDetail, type Quiz, type SubmitResult, type Progress, type Gradebook,
} from "./api";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import { Select, SelectTrigger, SelectValue, SelectContent, SelectItem } from "@/components/ui/select";

export default function App() {
  const [me, setMe] = useState<Me | null>(null);
  const [ready, setReady] = useState(false);
  useEffect(() => {
    if (!hasToken()) return setReady(true);
    api<Me>("/me").then((r) => { if (r.ok) setMe(r.data); else setToken(null); setReady(true); });
  }, []);
  if (!ready) return null;
  return me ? <Dashboard me={me} onLogout={() => { setToken(null); setMe(null); }} /> : <Login onAuthed={setMe} />;
}

function Login({ onAuthed }: { onAuthed: (m: Me) => void }) {
  const [email, setEmail] = useState("prof@acme.io");
  const [password, setPassword] = useState("pw12345678");
  const [role, setRole] = useState("instructor");
  const [msg, setMsg] = useState("Register as an instructor (creates courses; seeded a demo one) or a student (enroll, take quizzes, earn a certificate).");
  async function login() {
    const r = await api<any>("/login", "POST", { email, password });
    if (!r.ok) return setMsg(r.data.error || "login failed");
    setToken(r.data.access_token);
    const me = await api<Me>("/me");
    if (me.ok) onAuthed(me.data);
  }
  async function register() {
    const r = await api<any>("/register", "POST", { email, password, role });
    if (!r.ok && r.status !== 409) return setMsg(r.data.error || "register failed");
    login();
  }
  return (
    <div className="min-h-[100dvh] grid place-items-center p-4">
      <Card className="w-full max-w-sm">
        <CardHeader><CardTitle className="flex items-center gap-2 text-sm"><GraduationCap className="size-4" /> lms — sign in</CardTitle></CardHeader>
        <CardContent className="grid gap-3">
          <Input placeholder="email" value={email} onChange={(e) => setEmail(e.target.value)} />
          <Input type="password" placeholder="password" value={password} onChange={(e) => setPassword(e.target.value)} />
          <Select value={role} onValueChange={setRole}>
            <SelectTrigger><SelectValue /></SelectTrigger>
            <SelectContent><SelectItem value="instructor">instructor</SelectItem><SelectItem value="student">student</SelectItem></SelectContent>
          </Select>
          <div className="flex gap-2">
            <Button className="flex-1" onClick={login}>Log in</Button>
            <Button className="flex-1" variant="outline" onClick={register}>Register</Button>
          </div>
          <p className="text-xs text-muted-foreground">{msg}</p>
        </CardContent>
      </Card>
    </div>
  );
}

function Dashboard({ me, onLogout }: { me: Me; onLogout: () => void }) {
  async function logout() { await api("/logout", "POST"); onLogout(); }
  return (
    <div className="min-h-[100dvh]">
      <header className="sticky top-0 z-10 flex items-center gap-2 border-b bg-card/80 px-4 py-3 backdrop-blur">
        <GraduationCap className="size-5 text-primary" />
        <span className="font-semibold">lms</span>
        <span className="hidden text-sm text-muted-foreground sm:inline">· learning</span>
        <div className="flex-1" />
        <Badge className={me.is_instructor ? "bg-violet-600" : ""}>{me.is_instructor ? "INSTRUCTOR" : "STUDENT"}</Badge>
        <Button variant="ghost" size="icon" onClick={logout} title="Log out"><LogOut className="size-4" /></Button>
      </header>
      <main className="mx-auto max-w-3xl p-4">
        {me.is_instructor ? <InstructorApp /> : <StudentApp />}
      </main>
    </div>
  );
}

// ---- student ----------------------------------------------------------------

function StudentApp() {
  const [courses, setCourses] = useState<Course[]>([]);
  const [sel, setSel] = useState<string | null>(null);
  async function load() { setCourses((await api<{ items: Course[] }>("/courses")).data.items || []); }
  useEffect(() => { load(); }, []);

  if (sel) return <StudentCourse id={sel} onBack={() => { setSel(null); load(); }} />;
  return (
    <Card>
      <CardHeader><CardTitle>Course catalog</CardTitle></CardHeader>
      <CardContent className="grid gap-2">
        {courses.length === 0 && <p className="text-sm text-muted-foreground">No courses yet — an instructor needs to create one.</p>}
        {courses.map((c) => (
          <div key={c.id} className="flex items-center gap-3 rounded-md border px-3 py-2 text-sm">
            <div className="min-w-0 flex-1">
              <div className="font-medium"><span className="font-mono text-muted-foreground">{c.code}</span> {c.title}</div>
              <div className="text-xs text-muted-foreground">{c.lessons} lessons · {c.quizzes} quizzes · {c.instructor_email}</div>
            </div>
            {c.enrolled
              ? <Button size="sm" onClick={() => setSel(c.id)}>Open</Button>
              : <Button size="sm" variant="outline" onClick={async () => { await api(`/courses/${c.id}/enroll`, "POST"); setSel(c.id); }}>Enroll</Button>}
          </div>
        ))}
      </CardContent>
    </Card>
  );
}

function StudentCourse({ id, onBack }: { id: string; onBack: () => void }) {
  const [d, setD] = useState<CourseDetail | null>(null);
  const [prog, setProg] = useState<Progress | null>(null);
  async function load() {
    setD((await api<CourseDetail>(`/courses/${id}`)).data);
    setProg((await api<Progress>(`/courses/${id}/progress`)).data);
  }
  useEffect(() => { load(); }, [id]);
  if (!d) return null;

  return (
    <div className="grid gap-4">
      <div className="flex items-center gap-2">
        <Button variant="ghost" size="sm" onClick={onBack}>← Catalog</Button>
        <h2 className="text-lg font-semibold">{d.course.title}</h2>
      </div>

      {prog && (
        <Card>
          <CardContent className="flex items-center gap-3 pt-4">
            <div className="h-2 flex-1 overflow-hidden rounded-full bg-muted"><div className="h-full bg-primary transition-all" style={{ width: `${prog.completion_pct}%` }} /></div>
            <span className="text-sm tabular-nums text-muted-foreground">{prog.completion_pct}%</span>
            {prog.certificate_eligible && (
              <Button size="sm" onClick={() => download(`/courses/${id}/certificate.pdf`, "certificate.pdf")}><Download className="size-4" /> Certificate</Button>
            )}
          </CardContent>
        </Card>
      )}

      <Card>
        <CardHeader><CardTitle className="flex items-center gap-2 text-sm"><BookOpen className="size-4" /> Lessons</CardTitle></CardHeader>
        <CardContent className="grid gap-2">
          {d.lessons.map((l) => (
            <details key={l.id} className="rounded-md border px-3 py-2">
              <summary className="cursor-pointer text-sm font-medium">{l.title}</summary>
              <pre className="mt-2 whitespace-pre-wrap text-xs text-muted-foreground">{l.body}</pre>
            </details>
          ))}
        </CardContent>
      </Card>

      {d.quizzes.map((q) => <QuizTaker key={q.id} quiz={q} onDone={load} />)}
    </div>
  );
}

function QuizTaker({ quiz, onDone }: { quiz: Quiz; onDone: () => void }) {
  const [answers, setAnswers] = useState<Record<number, number>>({});
  const [result, setResult] = useState<SubmitResult | null>(null);
  const all = quiz.questions.every((_, i) => answers[i] !== undefined);
  async function submit() {
    const r = await api<SubmitResult>(`/quizzes/${quiz.id}/submit`, "POST", { answers: quiz.questions.map((_, i) => answers[i] ?? 0) });
    if (r.ok) { setResult(r.data); onDone(); }
  }
  return (
    <Card>
      <CardHeader><CardTitle className="flex items-center gap-2 text-sm">{quiz.title}
        {result && <Badge className={result.passed ? "bg-green-600" : "bg-red-600"}>{result.passed ? <Check className="mr-1 size-3" /> : <X className="mr-1 size-3" />}{result.score_pct}%</Badge>}
      </CardTitle></CardHeader>
      <CardContent className="grid gap-3">
        {quiz.questions.map((q, i) => (
          <div key={i} className="grid gap-1.5">
            <div className="text-sm font-medium">{i + 1}. {q.prompt}</div>
            <div className="grid gap-1">
              {q.options.map((o, j) => (
                <label key={j} className={`flex cursor-pointer items-center gap-2 rounded-md border px-2 py-1.5 text-sm ${answers[i] === j ? "border-primary bg-primary/10" : ""}`}>
                  <input type="radio" name={`${quiz.id}-${i}`} checked={answers[i] === j} onChange={() => setAnswers((a) => ({ ...a, [i]: j }))} />
                  {o}
                </label>
              ))}
            </div>
          </div>
        ))}
        <div className="flex items-center gap-2">
          <Button size="sm" onClick={submit} disabled={!all}>Submit ({quiz.pass_mark}% to pass)</Button>
          {result && <span className="text-xs text-muted-foreground">{result.correct}/{result.total} correct</span>}
        </div>
      </CardContent>
    </Card>
  );
}

// ---- instructor -------------------------------------------------------------

function InstructorApp() {
  const [courses, setCourses] = useState<Course[]>([]);
  const [sel, setSel] = useState<string | null>(null);
  const [code, setCode] = useState("");
  const [title, setTitle] = useState("");
  async function load() { setCourses((await api<{ items: Course[] }>("/courses")).data.items || []); }
  useEffect(() => { load(); }, []);
  async function create() {
    if (!code || !title) return;
    const r = await api<Course>("/courses", "POST", { code, title });
    if (r.ok) { setCode(""); setTitle(""); await load(); setSel(r.data.id); }
  }
  const mine = courses.filter((c) => c.is_mine);

  if (sel) return <InstructorCourse id={sel} onBack={() => { setSel(null); load(); }} />;
  return (
    <div className="grid gap-4">
      <Card>
        <CardHeader><CardTitle>New course</CardTitle></CardHeader>
        <CardContent className="flex flex-wrap items-end gap-2">
          <label className="grid gap-1 text-xs text-muted-foreground">Code<Input className="w-28" placeholder="WIT201" value={code} onChange={(e) => setCode(e.target.value)} /></label>
          <label className="grid gap-1 text-xs text-muted-foreground">Title<Input className="w-56" placeholder="Advanced Composition" value={title} onChange={(e) => setTitle(e.target.value)} /></label>
          <Button onClick={create}><Plus className="size-4" /> Create</Button>
        </CardContent>
      </Card>
      <Card>
        <CardHeader><CardTitle>My courses</CardTitle></CardHeader>
        <CardContent className="grid gap-2">
          {mine.map((c) => (
            <button key={c.id} onClick={() => setSel(c.id)} className="flex items-center gap-3 rounded-md border px-3 py-2 text-left text-sm hover:border-primary">
              <div className="min-w-0 flex-1"><div className="font-medium"><span className="font-mono text-muted-foreground">{c.code}</span> {c.title}</div>
                <div className="text-xs text-muted-foreground">{c.lessons} lessons · {c.quizzes} quizzes</div></div>
            </button>
          ))}
        </CardContent>
      </Card>
    </div>
  );
}

function InstructorCourse({ id, onBack }: { id: string; onBack: () => void }) {
  const [d, setD] = useState<CourseDetail | null>(null);
  const [gb, setGb] = useState<Gradebook | null>(null);
  async function load() {
    setD((await api<CourseDetail>(`/courses/${id}`)).data);
    setGb((await api<Gradebook>(`/courses/${id}/gradebook`)).data);
  }
  useEffect(() => { load(); }, [id]);
  if (!d) return null;

  return (
    <div className="grid gap-4">
      <div className="flex items-center gap-2">
        <Button variant="ghost" size="sm" onClick={onBack}>← Courses</Button>
        <h2 className="text-lg font-semibold">{d.course.title}</h2>
      </div>

      <div className="grid gap-4 sm:grid-cols-2">
        <AddLesson id={id} onDone={load} />
        <AddQuiz id={id} onDone={load} />
      </div>

      <Card>
        <CardHeader><CardTitle className="flex items-center gap-2 text-sm"><BarChart3 className="size-4" /> Gradebook · {gb?.enrolled ?? 0} enrolled</CardTitle></CardHeader>
        <CardContent className="grid gap-3">
          {gb && gb.students.length > 0 ? (
            <div className="overflow-x-auto">
              <table className="w-full text-sm">
                <thead><tr className="text-left text-xs text-muted-foreground">
                  <th className="py-1 pr-3">Student</th>
                  {gb.quizzes.map((q) => <th key={q.id} className="py-1 pr-3 text-right">{q.title}</th>)}
                  <th className="py-1 text-right">Avg</th>
                </tr></thead>
                <tbody>
                  {gb.students.map((s) => (
                    <tr key={s.email} className="border-t">
                      <td className="py-1 pr-3 truncate">{s.email}{s.passed_all && <Check className="ml-1 inline size-3 text-green-600" />}</td>
                      {gb.quizzes.map((q) => <td key={q.id} className="py-1 pr-3 text-right tabular-nums">{s.scores[q.id] ?? 0}%</td>)}
                      <td className="py-1 text-right font-semibold tabular-nums">{s.average}%</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          ) : <p className="text-sm text-muted-foreground">No submissions yet.</p>}
          {gb?.chart_svg && <div className="grid place-items-center overflow-x-auto [&_svg]:h-auto [&_svg]:max-w-full" dangerouslySetInnerHTML={{ __html: gb.chart_svg }} />}
        </CardContent>
      </Card>
    </div>
  );
}

function AddLesson({ id, onDone }: { id: string; onDone: () => void }) {
  const [title, setTitle] = useState("");
  const [body, setBody] = useState("");
  async function add() {
    if (!title) return;
    const r = await api(`/courses/${id}/lessons`, "POST", { title, body });
    if (r.ok) { setTitle(""); setBody(""); onDone(); }
  }
  return (
    <Card>
      <CardHeader><CardTitle className="text-sm">Add lesson</CardTitle></CardHeader>
      <CardContent className="grid gap-2">
        <Input placeholder="Lesson title" value={title} onChange={(e) => setTitle(e.target.value)} />
        <textarea className="min-h-20 rounded-md border bg-transparent p-2 text-sm" placeholder="Markdown body" value={body} onChange={(e) => setBody(e.target.value)} />
        <Button size="sm" onClick={add}><Plus className="size-4" /> Add lesson</Button>
      </CardContent>
    </Card>
  );
}

function AddQuiz({ id, onDone }: { id: string; onDone: () => void }) {
  const [title, setTitle] = useState("");
  const [prompt, setPrompt] = useState("");
  const [opts, setOpts] = useState(["", "", ""]);
  const [answer, setAnswer] = useState(0);
  const [err, setErr] = useState("");
  async function add() {
    const options = opts.map((o) => o.trim()).filter(Boolean);
    if (!title || !prompt || options.length < 2 || answer >= options.length) return setErr("title, prompt, ≥2 options, pick the correct one");
    const r = await api(`/courses/${id}/quizzes`, "POST", { title, pass_mark: 60, questions: [{ prompt, options, answer }] });
    if (r.ok) { setTitle(""); setPrompt(""); setOpts(["", "", ""]); setAnswer(0); setErr(""); onDone(); }
    else setErr((r.data as any).error || "failed");
  }
  return (
    <Card>
      <CardHeader><CardTitle className="text-sm">Add quiz (one question)</CardTitle></CardHeader>
      <CardContent className="grid gap-2">
        <Input placeholder="Quiz title" value={title} onChange={(e) => setTitle(e.target.value)} />
        <Input placeholder="Question prompt" value={prompt} onChange={(e) => setPrompt(e.target.value)} />
        {opts.map((o, i) => (
          <label key={i} className="flex items-center gap-2">
            <input type="radio" name="correct" checked={answer === i} onChange={() => setAnswer(i)} title="correct answer" />
            <Input placeholder={`Option ${i + 1}`} value={o} onChange={(e) => setOpts((os) => os.map((x, j) => (j === i ? e.target.value : x)))} />
          </label>
        ))}
        <Button size="sm" onClick={add}><Plus className="size-4" /> Add quiz</Button>
        {err && <p className="text-xs text-destructive">{err}</p>}
      </CardContent>
    </Card>
  );
}

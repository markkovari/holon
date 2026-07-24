import { useEffect, useMemo, useState } from "react";
import {
  ResponsiveContainer, PieChart, Pie, Cell, Tooltip, BarChart, Bar, XAxis, YAxis, CartesianGrid,
} from "recharts";
import { Clock, Play, Square, Trash2, Plus, LogOut, Users } from "lucide-react";
import { api, setToken, hasToken, type Me, type Project, type Category, type Entry, type Report, type Timer } from "./api";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "@/components/ui/tabs";
import { Select, SelectTrigger, SelectValue, SelectContent, SelectItem } from "@/components/ui/select";

const COLORS = ["#6366f1", "#22c55e", "#f97316", "#06b6d4", "#ec4899", "#eab308", "#a855f7", "#14b8a6", "#ef4444", "#3b82f6"];
const hrs = (m: number) => (m / 60).toFixed(1) + "h";
const today = () => new Date().toISOString().slice(0, 10);
type RangeKind = "week" | "month" | "year";
function rangeOf(kind: RangeKind): [string, string] {
  const now = new Date();
  if (kind === "week") { const d = new Date(now); d.setDate(d.getDate() - 6); return [d.toISOString().slice(0, 10), today()]; }
  if (kind === "year") return [`${now.getFullYear()}-01-01`, `${now.getFullYear()}-12-31`];
  return [`${now.toISOString().slice(0, 7)}-01`, today()];
}

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
  const [email, setEmail] = useState("ada@acme.io");
  const [password, setPassword] = useState("pw12345678");
  const [role, setRole] = useState("member");
  const [msg, setMsg] = useState("Pick a role to register a demo account. Admins create projects & categories.");
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
        <CardHeader><CardTitle className="flex items-center gap-2 text-sm"><Clock className="size-4" /> tempo — sign in</CardTitle></CardHeader>
        <CardContent className="grid gap-3">
          <Input placeholder="email" value={email} onChange={(e) => setEmail(e.target.value)} />
          <Input type="password" placeholder="password" value={password} onChange={(e) => setPassword(e.target.value)} />
          <Select value={role} onValueChange={setRole}>
            <SelectTrigger><SelectValue /></SelectTrigger>
            <SelectContent><SelectItem value="member">member</SelectItem><SelectItem value="admin">admin</SelectItem></SelectContent>
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
  const isAdmin = me.roles.includes("admin");
  const [projects, setProjects] = useState<Project[]>([]);
  const [cats, setCats] = useState<Category[]>([]);
  const [range, setRange] = useState<RangeKind>("month");
  const [scopeAll, setScopeAll] = useState(false);
  const [report, setReport] = useState<Report | null>(null);
  const [entries, setEntries] = useState<Entry[]>([]);
  const canSeeAll = me.can_see_all || report?.can_see_all;

  const [from, to] = rangeOf(range);
  async function refreshMeta() {
    setProjects((await api<{ items: Project[] }>("/projects")).data.items || []);
    setCats((await api<{ items: Category[] }>("/categories")).data.items || []);
  }
  async function refresh() {
    const sc = scopeAll && canSeeAll ? "all" : "me";
    setReport((await api<Report>(`/report?from=${from}&to=${to}&scope=${sc}`)).data);
    setEntries((await api<{ entries: Entry[] }>(`/entries?from=${from}&to=${to}`)).data.entries || []);
  }
  useEffect(() => { refreshMeta(); }, []);
  useEffect(() => { refresh(); /* eslint-disable-next-line */ }, [range, scopeAll]);

  async function logout() { await api("/logout", "POST"); onLogout(); }

  return (
    <div className="min-h-[100dvh]">
      <header className="sticky top-0 z-10 flex items-center gap-2 border-b bg-card/80 px-4 py-3 backdrop-blur">
        <Clock className="size-5 text-primary" />
        <span className="font-semibold">tempo</span>
        <span className="text-muted-foreground text-sm hidden sm:inline">· worktime</span>
        <div className="flex-1" />
        <Badge className="max-w-[45vw] truncate">{me.email} · {me.roles.join(",")}</Badge>
        <Button variant="ghost" size="icon" onClick={logout} title="Log out"><LogOut className="size-4" /></Button>
      </header>

      <main className="mx-auto max-w-5xl p-4">
        <Tabs defaultValue="log">
          <TabsList className="w-full sm:w-auto">
            <TabsTrigger value="log" className="flex-1 sm:flex-none">Log</TabsTrigger>
            <TabsTrigger value="calendar" className="flex-1 sm:flex-none">Calendar</TabsTrigger>
            <TabsTrigger value="reports" className="flex-1 sm:flex-none">Reports</TabsTrigger>
            {isAdmin && <TabsTrigger value="admin" className="flex-1 sm:flex-none">Admin</TabsTrigger>}
          </TabsList>

          <TabsContent value="log">
            <LogTab projects={projects} cats={cats} onChange={refresh} entries={entries} />
          </TabsContent>
          <TabsContent value="calendar">
            <CalendarTab projects={projects} cats={cats} onChange={refresh} />
          </TabsContent>
          <TabsContent value="reports">
            <ReportsTab report={report} range={range} setRange={setRange}
              scopeAll={!!(scopeAll && canSeeAll)} setScopeAll={setScopeAll} canSeeAll={!!canSeeAll} />
          </TabsContent>
          {isAdmin && (
            <TabsContent value="admin"><AdminTab projects={projects} onChange={refreshMeta} /></TabsContent>
          )}
        </Tabs>
      </main>
    </div>
  );
}

function LogTab({ projects, cats, onChange, entries }:
  { projects: Project[]; cats: Category[]; onChange: () => void; entries: Entry[] }) {
  const [proj, setProj] = useState("");
  const [cat, setCat] = useState("");
  const [mins, setMins] = useState("30");
  const [timer, setTimer] = useState<Timer | null>(null);
  const [elapsed, setElapsed] = useState(0);

  useEffect(() => { if (!proj && projects[0]) setProj(projects[0].id); }, [projects]);
  useEffect(() => { if (!cat && cats[0]) setCat(cats[0].id); }, [cats]);
  useEffect(() => { api<{ timer: Timer | null }>("/timer").then((r) => setTimer(r.data.timer)); }, []);
  useEffect(() => {
    if (!timer) return;
    const tick = () => setElapsed(Math.max(0, Math.floor(Date.now() / 1000) - timer.started));
    tick(); const h = setInterval(tick, 1000); return () => clearInterval(h);
  }, [timer]);

  async function log() {
    const m = Number(mins) || 0;
    if (!proj || !cat || m <= 0) return;
    await api("/entries", "POST", { project: proj, category: cat, minutes: m, day: today() });
    onChange();
  }
  async function toggleTimer() {
    if (timer) { await api("/timer/stop", "POST"); setTimer(null); onChange(); }
    else if (proj && cat) {
      const r = await api<Timer>("/timer/start", "POST", { project: proj, category: cat, day: today() });
      if (r.ok) setTimer(r.data);
    }
  }
  async function del(id: string) { await api(`/entries/${id}`, "DELETE"); onChange(); }
  async function edit(e: Entry) {
    const v = prompt(`Minutes for "${e.project_name} · ${e.category_name}"`, String(e.minutes));
    if (!v) return;
    await api(`/entries/${e.id}`, "PATCH", { minutes: Number(v) });
    onChange();
  }
  const clock = `${String(Math.floor(elapsed / 3600)).padStart(2, "0")}:${String(Math.floor(elapsed / 60) % 60).padStart(2, "0")}:${String(elapsed % 60).padStart(2, "0")}`;

  return (
    <div className="grid gap-4">
      <Card>
        <CardHeader><CardTitle>Log time</CardTitle></CardHeader>
        <CardContent className="grid gap-3 sm:grid-cols-[1fr_1fr_7rem_auto] sm:items-end">
          <Field label="Project">
            <Select value={proj} onValueChange={setProj}><SelectTrigger><SelectValue placeholder="project" /></SelectTrigger>
              <SelectContent>{projects.map((p) => <SelectItem key={p.id} value={p.id}>{p.name}</SelectItem>)}</SelectContent></Select>
          </Field>
          <Field label="Category">
            <Select value={cat} onValueChange={setCat}><SelectTrigger><SelectValue placeholder="category" /></SelectTrigger>
              <SelectContent>{cats.map((c) => <SelectItem key={c.id} value={c.id}>{c.name}</SelectItem>)}</SelectContent></Select>
          </Field>
          <Field label="Minutes"><Input type="number" min={1} value={mins} onChange={(e) => setMins(e.target.value)} /></Field>
          <Button onClick={log}><Plus className="size-4" /> Log</Button>
        </CardContent>
        <CardContent className="flex items-center gap-3 pt-0">
          <Button variant={timer ? "destructive" : "secondary"} onClick={toggleTimer}>
            {timer ? <><Square className="size-4" /> Stop &amp; log</> : <><Play className="size-4" /> Start timer</>}
          </Button>
          {timer && <span className="font-mono text-lg font-bold tabular-nums">{clock}</span>}
          {timer && <span className="text-sm text-muted-foreground">running…</span>}
        </CardContent>
      </Card>

      <Card>
        <CardHeader><CardTitle>Recent entries</CardTitle></CardHeader>
        <CardContent className="grid gap-1.5">
          {entries.length === 0 && <p className="text-sm text-muted-foreground">No time logged in this range yet.</p>}
          {entries.slice(0, 12).map((e) => (
            <div key={e.id} className="flex items-center gap-3 rounded-md border px-3 py-2 text-sm">
              <span className="w-16 shrink-0 text-muted-foreground tabular-nums">{e.day.slice(5)}</span>
              <span className="min-w-0 flex-1 truncate"><b>{e.project_name}</b> · {e.category_name}</span>
              <button className="tabular-nums font-medium hover:underline" onClick={() => edit(e)}>{hrs(e.minutes)}</button>
              <Button variant="ghost" size="icon" className="size-7" onClick={() => del(e.id)}><Trash2 className="size-4 text-destructive" /></Button>
            </div>
          ))}
        </CardContent>
      </Card>
    </div>
  );
}

const START_HR = 6, END_HR = 22, ROW = 48; // day grid: 6:00–22:00, px per hour
const hhmm = (m: number) => `${String(Math.floor(m / 60)).padStart(2, "0")}:${String(m % 60).padStart(2, "0")}`;
function shiftDay(iso: string, delta: number) {
  const d = new Date(iso + "T00:00:00"); d.setDate(d.getDate() + delta); return d.toISOString().slice(0, 10);
}

function CalendarTab({ projects, cats, onChange }:
  { projects: Project[]; cats: Category[]; onChange: () => void }) {
  const [date, setDate] = useState(today());
  const [items, setItems] = useState<Entry[]>([]);
  const [add, setAdd] = useState<{ at: number } | null>(null);
  const [proj, setProj] = useState(""); const [cat, setCat] = useState(""); const [mins, setMins] = useState("60");
  const projColor = (id: string) => COLORS[Math.max(0, projects.findIndex((p) => p.id === id)) % COLORS.length];

  async function load() { setItems((await api<{ entries: Entry[] }>(`/entries?from=${date}&to=${date}`)).data.entries || []); }
  useEffect(() => { load(); /* eslint-disable-next-line */ }, [date]);
  useEffect(() => { if (!proj && projects[0]) setProj(projects[0].id); }, [projects]);
  useEffect(() => { if (!cat && cats[0]) setCat(cats[0].id); }, [cats]);

  const scheduled = items.filter((e) => typeof e.start === "number" && e.start >= 0);
  const unscheduled = items.filter((e) => !(typeof e.start === "number" && e.start >= 0));

  function openAt(e: React.MouseEvent) {
    const y = e.nativeEvent.offsetY;
    const hour = Math.min(END_HR - 1, START_HR + Math.floor(y / ROW));
    setAdd({ at: hour * 60 });
  }
  async function save() {
    if (!add || !proj || !cat) return;
    await api("/entries", "POST", { project: proj, category: cat, minutes: Number(mins) || 60, day: date, start: add.at });
    setAdd(null); await load(); onChange();
  }
  async function del(e: Entry) {
    if (!confirm(`Delete ${e.project_name} · ${e.category_name} (${hrs(e.minutes)})?`)) return;
    await api(`/entries/${e.id}`, "DELETE"); await load(); onChange();
  }

  return (
    <div className="grid gap-4">
      <Card>
        <CardContent className="flex flex-wrap items-center gap-2 pt-4">
          <Button variant="outline" size="icon" onClick={() => setDate(shiftDay(date, -1))}>‹</Button>
          <Input type="date" className="w-40" value={date} onChange={(e) => setDate(e.target.value)} />
          <Button variant="outline" size="icon" onClick={() => setDate(shiftDay(date, 1))}>›</Button>
          <Button variant="ghost" onClick={() => setDate(today())}>Today</Button>
          <div className="flex-1" />
          <span className="text-sm text-muted-foreground">tap a slot to add</span>
        </CardContent>
      </Card>

      {add && (
        <Card className="border-primary">
          <CardHeader><CardTitle>Add at {hhmm(add.at)}</CardTitle></CardHeader>
          <CardContent className="flex flex-wrap items-end gap-2">
            <Select value={proj} onValueChange={setProj}><SelectTrigger className="w-36"><SelectValue placeholder="project" /></SelectTrigger>
              <SelectContent>{projects.map((p) => <SelectItem key={p.id} value={p.id}>{p.name}</SelectItem>)}</SelectContent></Select>
            <Select value={cat} onValueChange={setCat}><SelectTrigger className="w-36"><SelectValue placeholder="category" /></SelectTrigger>
              <SelectContent>{cats.map((c) => <SelectItem key={c.id} value={c.id}>{c.name}</SelectItem>)}</SelectContent></Select>
            <Input className="w-24" type="number" min={1} value={mins} onChange={(e) => setMins(e.target.value)} />
            <Button onClick={save}>Save</Button>
            <Button variant="ghost" onClick={() => setAdd(null)}>Cancel</Button>
          </CardContent>
        </Card>
      )}

      {unscheduled.length > 0 && (
        <Card>
          <CardHeader><CardTitle>Unscheduled</CardTitle></CardHeader>
          <CardContent className="flex flex-wrap gap-2">
            {unscheduled.map((e) => (
              <button key={e.id} onClick={() => del(e)} className="rounded-md px-2 py-1 text-xs font-medium text-white"
                style={{ background: projColor(e.project) }}>{e.project_name} · {e.category_name} · {hrs(e.minutes)}</button>
            ))}
          </CardContent>
        </Card>
      )}

      <Card>
        <CardContent className="pt-4">
          <div className="flex">
            <div className="w-12 shrink-0 select-none">
              {Array.from({ length: END_HR - START_HR }, (_, i) => (
                <div key={i} style={{ height: ROW }} className="-mt-2 pr-2 text-right text-xs text-muted-foreground">{START_HR + i}:00</div>
              ))}
            </div>
            <div data-testid="daygrid" className="relative flex-1 cursor-pointer rounded-md border" style={{ height: (END_HR - START_HR) * ROW }} onClick={openAt}>
              {Array.from({ length: END_HR - START_HR }, (_, i) => (
                <div key={i} style={{ top: i * ROW, height: ROW }} className="absolute inset-x-0 border-t border-border/60" />
              ))}
              {scheduled.map((e) => {
                const top = Math.max(0, ((e.start! / 60) - START_HR) * ROW);
                const h = Math.max(22, (e.minutes / 60) * ROW - 2);
                return (
                  <button key={e.id} onClick={(ev) => { ev.stopPropagation(); del(e); }}
                    className="absolute inset-x-1 overflow-hidden rounded-md px-2 py-1 text-left text-xs font-medium text-white shadow"
                    style={{ top, height: h, background: projColor(e.project) }}>
                    <div className="truncate">{e.project_name} · {e.category_name}</div>
                    <div className="opacity-80">{hhmm(e.start!)} · {hrs(e.minutes)}</div>
                  </button>
                );
              })}
            </div>
          </div>
        </CardContent>
      </Card>
    </div>
  );
}

function ReportsTab({ report, range, setRange, scopeAll, setScopeAll, canSeeAll }:
  { report: Report | null; range: RangeKind; setRange: (r: RangeKind) => void;
    scopeAll: boolean; setScopeAll: (b: boolean) => void; canSeeAll: boolean }) {
  const r = report;
  const projData = useMemo(() => (r?.by_project || []).map((p, i) => ({ name: p.name, value: p.minutes, fill: COLORS[i % COLORS.length] })), [r]);
  return (
    <div className="grid gap-4">
      <Card>
        <CardContent className="flex flex-wrap items-center gap-2 pt-4">
          <Seg options={[["week", "Week"], ["month", "Month"], ["year", "Year"]]} value={range} onChange={(v) => setRange(v as RangeKind)} />
          <div className="flex-1" />
          {canSeeAll && <Seg options={[["me", "Mine"], ["all", "Everyone"]]} value={scopeAll ? "all" : "me"} onChange={(v) => setScopeAll(v === "all")} icon={<Users className="size-3.5" />} />}
        </CardContent>
      </Card>

      <div className="grid gap-4 md:grid-cols-2">
        <Card>
          <CardHeader><CardTitle>By project {r?.scope === "all" ? "· team" : ""}</CardTitle></CardHeader>
          <CardContent className="flex items-center gap-3">
            <div className="h-40 w-40 shrink-0">
              <ResponsiveContainer>
                <PieChart>
                  <Pie data={projData.length ? projData : [{ name: "—", value: 1, fill: "hsl(var(--muted))" }]}
                    dataKey="value" nameKey="name" innerRadius={44} outerRadius={72} paddingAngle={2} stroke="none">
                    {projData.map((d, i) => <Cell key={i} fill={d.fill} />)}
                  </Pie>
                  <Tooltip formatter={(v: number) => hrs(v)} contentStyle={tip} />
                </PieChart>
              </ResponsiveContainer>
            </div>
            <div className="min-w-0 flex-1">
              <div className="text-3xl font-extrabold">{hrs(r?.total_minutes || 0)}</div>
              <div className="mb-2 text-sm text-muted-foreground">total logged</div>
              <div className="grid gap-1">
                {(r?.by_project || []).map((p, i) => (
                  <div key={p.project} className="flex items-center gap-2 text-sm">
                    <span className="size-3 shrink-0 rounded" style={{ background: COLORS[i % COLORS.length] }} />
                    <span className="min-w-0 flex-1 truncate">{p.name}</span>
                    <span className="text-muted-foreground tabular-nums">{hrs(p.minutes)}</span>
                  </div>
                ))}
                {!projData.length && <span className="text-sm text-muted-foreground">Nothing logged in this range.</span>}
              </div>
            </div>
          </CardContent>
        </Card>

        <ChartCard title="By category" data={(r?.by_category || []).map((c) => ({ label: c.key, minutes: c.minutes }))} />
      </div>

      <ChartCard title="By day" data={(r?.by_day || []).map((d) => ({ label: d.day.slice(5), minutes: d.minutes }))} vertical />
      {r?.by_user && <ChartCard title="By person (team)" data={r.by_user.map((u) => ({ label: u.key, minutes: u.minutes }))} />}
    </div>
  );
}

const tip = { background: "hsl(var(--card))", border: "1px solid hsl(var(--border))", borderRadius: 8, fontSize: 12 };

function ChartCard({ title, data, vertical }: { title: string; data: { label: string; minutes: number }[]; vertical?: boolean }) {
  return (
    <Card>
      <CardHeader><CardTitle>{title}</CardTitle></CardHeader>
      <CardContent>
        {data.length === 0 ? <p className="text-sm text-muted-foreground">Nothing here yet.</p> : (
          <div className="h-52">
            <ResponsiveContainer>
              {vertical ? (
                <BarChart data={data} margin={{ top: 4, right: 8, bottom: 0, left: -20 }}>
                  <CartesianGrid strokeDasharray="3 3" stroke="hsl(var(--border))" vertical={false} />
                  <XAxis dataKey="label" tick={{ fontSize: 11, fill: "hsl(var(--muted-foreground))" }} interval="preserveStartEnd" />
                  <YAxis tickFormatter={(v) => (v / 60).toFixed(0)} tick={{ fontSize: 11, fill: "hsl(var(--muted-foreground))" }} />
                  <Tooltip formatter={(v: number) => hrs(v)} contentStyle={tip} cursor={{ fill: "hsl(var(--muted))" }} />
                  <Bar dataKey="minutes" fill="#6366f1" radius={[4, 4, 0, 0]} />
                </BarChart>
              ) : (
                <BarChart data={data} layout="vertical" margin={{ top: 0, right: 12, bottom: 0, left: 8 }}>
                  <XAxis type="number" hide />
                  <YAxis type="category" dataKey="label" width={90} tick={{ fontSize: 11, fill: "hsl(var(--muted-foreground))" }} />
                  <Tooltip formatter={(v: number) => hrs(v)} contentStyle={tip} cursor={{ fill: "hsl(var(--muted))" }} />
                  <Bar dataKey="minutes" radius={[0, 4, 4, 0]}>
                    {data.map((_, i) => <Cell key={i} fill={COLORS[i % COLORS.length]} />)}
                  </Bar>
                </BarChart>
              )}
            </ResponsiveContainer>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

function AdminTab({ projects, onChange }: { projects: Project[]; onChange: () => void }) {
  const [pkey, setPkey] = useState(""); const [pname, setPname] = useState("");
  const [cname, setCname] = useState("");
  const [mproj, setMproj] = useState(""); const [memail, setMemail] = useState(""); const [mrole, setMrole] = useState("member");
  const [members, setMembers] = useState<{ email: string; role: string }[]>([]);
  useEffect(() => { if (!mproj && projects[0]) setMproj(projects[0].id); }, [projects]);
  useEffect(() => { if (mproj) api<{ members: any[] }>(`/projects/${mproj}/members`).then((r) => setMembers(r.data.members || [])); }, [mproj]);

  return (
    <div className="grid gap-4 md:grid-cols-2">
      <Card>
        <CardHeader><CardTitle>New project</CardTitle></CardHeader>
        <CardContent className="flex flex-wrap gap-2">
          <Input className="w-28" placeholder="KEY" value={pkey} onChange={(e) => setPkey(e.target.value)} />
          <Input className="min-w-0 flex-1" placeholder="name" value={pname} onChange={(e) => setPname(e.target.value)} />
          <Button onClick={async () => { await api("/projects", "POST", { key: pkey, name: pname }); setPkey(""); setPname(""); onChange(); }}>Add</Button>
        </CardContent>
      </Card>
      <Card>
        <CardHeader><CardTitle>New category</CardTitle></CardHeader>
        <CardContent className="flex gap-2">
          <Input className="min-w-0 flex-1" placeholder="engineering" value={cname} onChange={(e) => setCname(e.target.value)} />
          <Button onClick={async () => { await api("/categories", "POST", { name: cname }); setCname(""); onChange(); }}>Add</Button>
        </CardContent>
      </Card>
      <Card className="md:col-span-2">
        <CardHeader><CardTitle>Project membership</CardTitle></CardHeader>
        <CardContent className="grid gap-3">
          <div className="flex flex-wrap items-center gap-2">
            <Select value={mproj} onValueChange={setMproj}><SelectTrigger className="w-40"><SelectValue placeholder="project" /></SelectTrigger>
              <SelectContent>{projects.map((p) => <SelectItem key={p.id} value={p.id}>{p.name}</SelectItem>)}</SelectContent></Select>
            <Input className="min-w-0 flex-1" placeholder="user email" value={memail} onChange={(e) => setMemail(e.target.value)} />
            <Select value={mrole} onValueChange={setMrole}><SelectTrigger className="w-32"><SelectValue /></SelectTrigger>
              <SelectContent><SelectItem value="member">member</SelectItem><SelectItem value="lead">lead</SelectItem></SelectContent></Select>
            <Button onClick={async () => {
              await api(`/projects/${mproj}/members`, "POST", { email: memail, role: mrole });
              setMemail(""); const r = await api<{ members: any[] }>(`/projects/${mproj}/members`); setMembers(r.data.members || []);
            }}>Add member</Button>
          </div>
          <div className="flex flex-wrap gap-2">
            {members.map((m, i) => <Badge key={i} className={m.role === "lead" ? "bg-primary/15 text-primary" : ""}>{m.email} · {m.role}</Badge>)}
            {!members.length && <span className="text-sm text-muted-foreground">No members yet — a lead sees the whole project.</span>}
          </div>
        </CardContent>
      </Card>
    </div>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return <label className="grid gap-1 text-xs text-muted-foreground">{label}<span className="text-foreground">{children}</span></label>;
}
function Seg({ options, value, onChange, icon }:
  { options: [string, string][]; value: string; onChange: (v: string) => void; icon?: React.ReactNode }) {
  return (
    <div className="inline-flex rounded-lg bg-muted p-1">
      {options.map(([v, label]) => (
        <button key={v} onClick={() => onChange(v)}
          className={"inline-flex items-center gap-1.5 rounded-md px-3 py-1 text-sm font-medium transition-colors " +
            (value === v ? "bg-background text-foreground shadow-sm" : "text-muted-foreground")}>
          {value === v && icon}{label}
        </button>
      ))}
    </div>
  );
}

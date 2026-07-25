import { useEffect, useState } from "react";
import { LayoutDashboard, LogOut, Plus, Trash2 } from "lucide-react";
import {
  api, getText, setToken, hasToken, KINDS, parsePoints,
  type Me, type Dashboard, type Panel, type DashboardDetail, type Kind,
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
  return me ? <Dashboard onLogout={() => { setToken(null); setMe(null); }} /> : <Login onAuthed={setMe} />;
}

function Login({ onAuthed }: { onAuthed: (m: Me) => void }) {
  const [email, setEmail] = useState("you@acme.io");
  const [password, setPassword] = useState("pw12345678");
  const [msg, setMsg] = useState("Register to get a demo dashboard, then add your own panels — charts are rendered to SVG on the server.");
  async function login() {
    const r = await api<any>("/login", "POST", { email, password });
    if (!r.ok) return setMsg(r.data.error || "login failed");
    setToken(r.data.access_token);
    const me = await api<Me>("/me");
    if (me.ok) onAuthed(me.data);
  }
  async function register() {
    const r = await api<any>("/register", "POST", { email, password });
    if (!r.ok && r.status !== 409) return setMsg(r.data.error || "register failed");
    login();
  }
  return (
    <div className="min-h-[100dvh] grid place-items-center p-4">
      <Card className="w-full max-w-sm">
        <CardHeader><CardTitle className="flex items-center gap-2 text-sm"><LayoutDashboard className="size-4" /> dashboards — sign in</CardTitle></CardHeader>
        <CardContent className="grid gap-3">
          <Input placeholder="email" value={email} onChange={(e) => setEmail(e.target.value)} />
          <Input type="password" placeholder="password" value={password} onChange={(e) => setPassword(e.target.value)} />
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

function Dashboard({ onLogout }: { onLogout: () => void }) {
  const [dashboards, setDashboards] = useState<Dashboard[]>([]);
  const [sel, setSel] = useState("");
  const [detail, setDetail] = useState<DashboardDetail | null>(null);

  async function loadList() {
    const items = (await api<{ items: Dashboard[] }>("/dashboards")).data.items || [];
    setDashboards(items);
    if (!sel && items[0]) setSel(items[0].id);
  }
  async function loadDetail() {
    if (!sel) return;
    setDetail((await api<DashboardDetail>(`/dashboards/${sel}`)).data);
  }
  useEffect(() => { loadList(); }, []);
  useEffect(() => { loadDetail(); /* eslint-disable-next-line */ }, [sel]);

  async function newDashboard() {
    const name = prompt("Dashboard name");
    if (!name) return;
    const r = await api<Dashboard>("/dashboards", "POST", { name });
    if (r.ok) { await loadList(); setSel(r.data.id); }
  }
  async function logout() { await api("/logout", "POST"); onLogout(); }

  return (
    <div className="min-h-[100dvh]">
      <header className="sticky top-0 z-10 flex items-center gap-2 border-b bg-card/80 px-4 py-3 backdrop-blur">
        <LayoutDashboard className="size-5 text-primary" />
        <span className="font-semibold">dashboards</span>
        <div className="flex-1" />
        <Badge variant="secondary" className="hidden sm:inline-flex">charts rendered server-side</Badge>
        <Button variant="ghost" size="icon" onClick={logout} title="Log out"><LogOut className="size-4" /></Button>
      </header>

      <main className="mx-auto max-w-4xl p-4">
        <div className="mb-4 flex flex-wrap items-center gap-2">
          <Select value={sel} onValueChange={setSel}>
            <SelectTrigger className="w-56"><SelectValue placeholder="dashboard" /></SelectTrigger>
            <SelectContent>{dashboards.map((d) => <SelectItem key={d.id} value={d.id}>{d.name}</SelectItem>)}</SelectContent>
          </Select>
          <Button variant="outline" size="sm" onClick={newDashboard}><Plus className="size-4" /> New</Button>
        </div>

        <div className="grid gap-4 sm:grid-cols-2">
          {detail?.panels.map((p) => <PanelCard key={p.id} panel={p} onChanged={loadDetail} />)}
        </div>

        {sel && <AddPanel dashboard={sel} onAdded={loadDetail} />}
      </main>
    </div>
  );
}

function PanelCard({ panel, onChanged }: { panel: Panel; onChanged: () => void }) {
  const [svg, setSvg] = useState("");
  useEffect(() => { getText(`/panels/${panel.id}/chart.svg`).then(setSvg); }, [panel.id]);
  async function del() { await api(`/panels/${panel.id}`, "DELETE"); onChanged(); }
  return (
    <Card>
      <CardHeader className="flex-row items-center justify-between space-y-0 pb-2">
        <CardTitle className="text-sm">{panel.title || panel.kind}</CardTitle>
        <button className="text-muted-foreground hover:text-destructive" onClick={del} title="Remove panel"><Trash2 className="size-4" /></button>
      </CardHeader>
      <CardContent>
        <div className="grid place-items-center overflow-x-auto [&_svg]:h-auto [&_svg]:max-w-full" dangerouslySetInnerHTML={{ __html: svg }} />
      </CardContent>
    </Card>
  );
}

function AddPanel({ dashboard, onAdded }: { dashboard: string; onAdded: () => void }) {
  const [title, setTitle] = useState("");
  const [kind, setKind] = useState<Kind>("bar");
  const [data, setData] = useState("Web 42\nOps 28\nSales 15\nDesign 9");
  const [err, setErr] = useState("");

  async function add() {
    const points = parsePoints(data);
    if (!points.length) return setErr("Enter data as ‘label value’ lines.");
    setErr("");
    const r = await api(`/dashboards/${dashboard}/panels`, "POST", { title, kind, data: points });
    if (r.ok) { setTitle(""); onAdded(); } else setErr((r.data as any).error || "failed");
  }
  return (
    <Card className="mt-4">
      <CardHeader><CardTitle className="text-sm">Add a panel</CardTitle></CardHeader>
      <CardContent className="grid gap-3 sm:grid-cols-[1fr_11rem]">
        <div className="grid gap-3">
          <Input placeholder="Panel title" value={title} onChange={(e) => setTitle(e.target.value)} />
          <textarea
            className="min-h-24 rounded-md border bg-transparent p-2 font-mono text-sm"
            value={data} onChange={(e) => setData(e.target.value)} placeholder={"Web 42\nOps 28"}
          />
        </div>
        <div className="grid content-start gap-3">
          <Select value={kind} onValueChange={(v) => setKind(v as Kind)}>
            <SelectTrigger><SelectValue /></SelectTrigger>
            <SelectContent>{KINDS.map((k) => <SelectItem key={k} value={k}>{k}</SelectItem>)}</SelectContent>
          </Select>
          <Button onClick={add}><Plus className="size-4" /> Add panel</Button>
          {err && <p className="text-xs text-destructive">{err}</p>}
          <p className="text-xs text-muted-foreground">One “label value” per line. Rendered on the server via <code>svg:chart</code>.</p>
        </div>
      </CardContent>
    </Card>
  );
}

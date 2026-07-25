import { useEffect, useState } from "react";
import { CalendarDays, LogOut, Plus, Download, Trash2, Check, Repeat, Rss } from "lucide-react";
import {
  api, download, setToken, hasToken, DOW, hhmm, today, weekdayOf,
  type Me, type Resource, type Window, type Slot, type Booking, type BookResult,
} from "./api";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "@/components/ui/tabs";
import { Select, SelectTrigger, SelectValue, SelectContent, SelectItem } from "@/components/ui/select";

const toMin = (v: string) => { const [h, m] = v.split(":").map(Number); return (h || 0) * 60 + (m || 0); };

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
  const [email, setEmail] = useState("owner@acme.io");
  const [password, setPassword] = useState("pw12345678");
  const [role, setRole] = useState("owner");
  const [msg, setMsg] = useState("Register a demo account. An owner creates resources & weekly availability; anyone books free slots.");
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
        <CardHeader><CardTitle className="flex items-center gap-2 text-sm"><CalendarDays className="size-4" /> booked — sign in</CardTitle></CardHeader>
        <CardContent className="grid gap-3">
          <Input placeholder="email" value={email} onChange={(e) => setEmail(e.target.value)} />
          <Input type="password" placeholder="password" value={password} onChange={(e) => setPassword(e.target.value)} />
          <Select value={role} onValueChange={setRole}>
            <SelectTrigger><SelectValue /></SelectTrigger>
            <SelectContent><SelectItem value="member">member</SelectItem><SelectItem value="owner">owner</SelectItem></SelectContent>
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
  const [resources, setResources] = useState<Resource[]>([]);
  const [tick, setTick] = useState(0);
  const bump = () => setTick((t) => t + 1);
  async function loadResources() { setResources((await api<{ items: Resource[] }>("/resources")).data.items || []); }
  useEffect(() => { loadResources(); }, [tick]);
  async function logout() { await api("/logout", "POST"); onLogout(); }

  return (
    <div className="min-h-[100dvh]">
      <header className="sticky top-0 z-10 flex items-center gap-2 border-b bg-card/80 px-4 py-3 backdrop-blur">
        <CalendarDays className="size-5 text-primary" />
        <span className="font-semibold">booked</span>
        <span className="text-muted-foreground text-sm hidden sm:inline">· scheduling</span>
        <div className="flex-1" />
        <Badge className="max-w-[45vw] truncate">{me.email} · {me.roles.join(",")}</Badge>
        <Button variant="ghost" size="icon" onClick={logout} title="Log out"><LogOut className="size-4" /></Button>
      </header>

      <main className="mx-auto max-w-3xl p-4">
        <Tabs defaultValue="book">
          <TabsList className="w-full sm:w-auto">
            <TabsTrigger value="book" className="flex-1 sm:flex-none">Book</TabsTrigger>
            <TabsTrigger value="mine" className="flex-1 sm:flex-none">My bookings</TabsTrigger>
            {me.is_owner && <TabsTrigger value="manage" className="flex-1 sm:flex-none">Manage</TabsTrigger>}
          </TabsList>
          <TabsContent value="book"><BookTab resources={resources} onChange={bump} /></TabsContent>
          <TabsContent value="mine"><MineTab me={me} onChange={bump} tick={tick} /></TabsContent>
          {me.is_owner && <TabsContent value="manage"><ManageTab resources={resources} onChange={bump} /></TabsContent>}
        </Tabs>
      </main>
    </div>
  );
}

function BookTab({ resources, onChange }: { resources: Resource[]; onChange: () => void }) {
  const [res, setRes] = useState("");
  const [day, setDay] = useState(today());
  const [slots, setSlots] = useState<Slot[]>([]);
  const [repeat, setRepeat] = useState(false);
  const [weeks, setWeeks] = useState(4);
  const [result, setResult] = useState<BookResult | null>(null);
  const [note, setNote] = useState("");

  useEffect(() => { if (!res && resources[0]) setRes(resources[0].id); }, [resources]);
  async function loadSlots() {
    if (!res) return setSlots([]);
    const r = await api<{ slots: Slot[] }>(`/resources/${res}/slots?day=${day}`);
    setSlots(r.data.slots || []);
  }
  useEffect(() => { loadSlots(); setResult(null); /* eslint-disable-next-line */ }, [res, day]);

  async function book(s: Slot) {
    const body: any = { resource: res, day, start: s.start, end: s.end, note };
    if (repeat && weeks > 1) body.repeat = { freq: "weekly", count: weeks };
    const r = await api<BookResult>("/bookings", "POST", body);
    if (r.ok) { setResult(r.data); loadSlots(); onChange(); }
    else setResult({ booked: [], conflicts: [r.data as any as string], confirmation: null });
  }

  return (
    <div className="grid gap-4">
      <Card>
        <CardContent className="flex flex-wrap items-end gap-2 pt-4">
          <label className="grid gap-1 text-xs text-muted-foreground">Resource
            <Select value={res} onValueChange={setRes}><SelectTrigger className="w-48"><SelectValue placeholder="resource" /></SelectTrigger>
              <SelectContent>{resources.map((r) => <SelectItem key={r.id} value={r.id}>{r.name}</SelectItem>)}</SelectContent></Select></label>
          <label className="grid gap-1 text-xs text-muted-foreground">Day ({DOW[weekdayOf(day)]})
            <Input type="date" className="w-40" value={day} onChange={(e) => setDay(e.target.value)} /></label>
          <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={repeat} onChange={(e) => setRepeat(e.target.checked)} /><Repeat className="size-4" /> weekly</label>
          {repeat && <label className="grid gap-1 text-xs text-muted-foreground">for weeks
            <Input type="number" className="w-20" min={2} value={weeks} onChange={(e) => setWeeks(Math.max(2, Number(e.target.value) || 2))} /></label>}
        </CardContent>
      </Card>

      <Card>
        <CardHeader><CardTitle>Free slots · {day}</CardTitle></CardHeader>
        <CardContent className="flex flex-wrap gap-2">
          {slots.length === 0 && <p className="text-sm text-muted-foreground">No free slots — pick another day, or set availability in Manage.</p>}
          {slots.map((s) => (
            <Button key={s.start} variant="outline" size="sm" onClick={() => book(s)}>{s.label}</Button>
          ))}
        </CardContent>
      </Card>

      {result && (
        <Card className="border-primary">
          <CardHeader><CardTitle className="flex items-center gap-2">
            {result.booked.length ? <><Check className="size-4 text-green-600" /> Booked {result.booked.length}</> : "Not booked"}
          </CardTitle></CardHeader>
          <CardContent className="grid gap-2 text-sm">
            {result.booked.map((b) => (
              <div key={b.id} className="flex items-center gap-2">
                <span className="flex-1"><b>{b.resource_name}</b> · {b.day} · {hhmm(b.start)}–{hhmm(b.end)}</span>
                <Button variant="ghost" size="sm" onClick={() => download(`/bookings/${b.id}.ics`, `booking-${b.day}.ics`)}><Download className="size-4" /> .ics</Button>
              </div>
            ))}
            {result.confirmation && (
              <div className="rounded-md bg-muted p-3">
                <div className="font-medium">{result.confirmation.subject}</div>
                <pre className="whitespace-pre-wrap text-xs text-muted-foreground">{result.confirmation.text}</pre>
              </div>
            )}
            {result.conflicts.length > 0 && <p className="text-xs text-destructive">Skipped: {result.conflicts.join(", ")}</p>}
          </CardContent>
        </Card>
      )}
    </div>
  );
}

function MineTab({ me, onChange, tick }: { me: Me; onChange: () => void; tick: number }) {
  const [items, setItems] = useState<Booking[]>([]);
  async function load() { setItems((await api<{ items: Booking[] }>(`/bookings?from=${today()}&to=9999-99-99`)).data.items || []); }
  useEffect(() => { load(); /* eslint-disable-next-line */ }, [tick]);
  async function cancel(id: string) { await api(`/bookings/${id}`, "DELETE"); load(); onChange(); }

  return (
    <Card>
      <CardHeader><CardTitle>{me.is_owner ? "All upcoming bookings" : "My upcoming bookings"}</CardTitle></CardHeader>
      <CardContent className="grid gap-1.5">
        {items.length === 0 && <p className="text-sm text-muted-foreground">Nothing upcoming.</p>}
        {items.map((b) => (
          <div key={b.id} className="flex items-center gap-3 rounded-md border px-3 py-2 text-sm">
            <span className="w-24 shrink-0 tabular-nums text-muted-foreground">{b.day}</span>
            <span className="min-w-0 flex-1 truncate"><b>{b.resource_name}</b> · {hhmm(b.start)}–{hhmm(b.end)}{me.is_owner && b.email ? ` · ${b.email}` : ""}</span>
            <Button variant="ghost" size="icon" className="size-7" onClick={() => download(`/bookings/${b.id}.ics`, `booking-${b.day}.ics`)}><Download className="size-4" /></Button>
            <Button variant="ghost" size="icon" className="size-7" onClick={() => cancel(b.id)}><Trash2 className="size-4 text-destructive" /></Button>
          </div>
        ))}
      </CardContent>
    </Card>
  );
}

function ManageTab({ resources, onChange }: { resources: Resource[]; onChange: () => void }) {
  const [key, setKey] = useState("");
  const [name, setName] = useState("");
  const [slot, setSlot] = useState(30);
  const [sel, setSel] = useState("");
  const [windows, setWindows] = useState<Window[]>([]);

  useEffect(() => { if (!sel && resources[0]) setSel(resources[0].id); }, [resources]);
  async function loadAvail() {
    if (!sel) return;
    const w = (await api<{ windows: Window[] }>(`/resources/${sel}/availability`)).data.windows || [];
    setWindows(w);
  }
  useEffect(() => { loadAvail(); /* eslint-disable-next-line */ }, [sel]);

  async function createResource() {
    if (!key || !name) return;
    await api("/resources", "POST", { key, name, slot });
    setKey(""); setName(""); onChange();
  }

  // per-weekday single window editor (enabled + start/end)
  const rowFor = (wd: number) => windows.find((w) => w.weekday === wd);
  function setRow(wd: number, patch: Partial<Window> | null) {
    setWindows((ws) => {
      const rest = ws.filter((w) => w.weekday !== wd);
      if (!patch) return rest;
      const cur = ws.find((w) => w.weekday === wd) || { weekday: wd, start: 9 * 60, end: 17 * 60 };
      return [...rest, { ...cur, ...patch }].sort((a, b) => a.weekday - b.weekday);
    });
  }
  async function saveAvail() {
    await api(`/resources/${sel}/availability`, "POST", { windows });
    loadAvail(); onChange();
  }

  return (
    <div className="grid gap-4">
      <Card>
        <CardHeader><CardTitle>New resource</CardTitle></CardHeader>
        <CardContent className="flex flex-wrap items-end gap-2">
          <label className="grid gap-1 text-xs text-muted-foreground">Key<Input className="w-28" placeholder="room-a" value={key} onChange={(e) => setKey(e.target.value)} /></label>
          <label className="grid gap-1 text-xs text-muted-foreground">Name<Input className="w-44" placeholder="Room A" value={name} onChange={(e) => setName(e.target.value)} /></label>
          <label className="grid gap-1 text-xs text-muted-foreground">Slot (min)<Input className="w-24" type="number" min={5} step={5} value={slot} onChange={(e) => setSlot(Math.max(5, Number(e.target.value) || 30))} /></label>
          <Button onClick={createResource}><Plus className="size-4" /> Add</Button>
        </CardContent>
      </Card>

      <Card>
        <CardHeader><CardTitle>Weekly availability</CardTitle></CardHeader>
        <CardContent className="grid gap-3">
          <Select value={sel} onValueChange={setSel}><SelectTrigger className="w-48"><SelectValue placeholder="resource" /></SelectTrigger>
            <SelectContent>{resources.map((r) => <SelectItem key={r.id} value={r.id}>{r.name}</SelectItem>)}</SelectContent></Select>
          <div className="grid gap-1.5">
            {DOW.map((d, wd) => {
              const row = rowFor(wd);
              return (
                <div key={wd} className="flex items-center gap-3 text-sm">
                  <label className="flex w-24 items-center gap-2"><input type="checkbox" checked={!!row} onChange={(e) => setRow(wd, e.target.checked ? {} : null)} />{d}</label>
                  {row ? (
                    <>
                      <Input type="time" className="w-28" value={hhmm(row.start)} onChange={(e) => setRow(wd, { start: toMin(e.target.value) })} />
                      <span className="text-muted-foreground">–</span>
                      <Input type="time" className="w-28" value={hhmm(row.end)} onChange={(e) => setRow(wd, { end: toMin(e.target.value) })} />
                    </>
                  ) : <span className="text-muted-foreground">closed</span>}
                </div>
              );
            })}
          </div>
          <div className="flex items-center gap-2">
            <Button onClick={saveAvail}>Save availability</Button>
            <Button variant="outline" size="sm" onClick={() => download(`/resources/${sel}/calendar.ics`, `${sel}-calendar.ics`)}><Rss className="size-4" /> Feed .ics</Button>
          </div>
        </CardContent>
      </Card>
    </div>
  );
}

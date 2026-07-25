import { useEffect, useRef, useState } from "react";
import { Landmark, LogOut, Plus, Trash2, Check, X } from "lucide-react";
import { api, setToken, hasToken, type Me, type Payee, type Verify } from "./api";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";

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
  const [msg, setMsg] = useState("Register to get a few demo payees. Add one — the IBAN is validated (country length + mod-97 checksum) as you type.");
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
        <CardHeader><CardTitle className="flex items-center gap-2 text-sm"><Landmark className="size-4" /> payees — sign in</CardTitle></CardHeader>
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
  const [payees, setPayees] = useState<Payee[]>([]);
  async function load() { setPayees((await api<{ items: Payee[] }>("/payees")).data.items || []); }
  useEffect(() => { load(); }, []);
  async function del(id: string) { await api(`/payees/${id}`, "DELETE"); load(); }
  async function logout() { await api("/logout", "POST"); onLogout(); }

  return (
    <div className="min-h-[100dvh]">
      <header className="sticky top-0 z-10 flex items-center gap-2 border-b bg-card/80 px-4 py-3 backdrop-blur">
        <Landmark className="size-5 text-primary" />
        <span className="font-semibold">payees</span>
        <span className="hidden text-sm text-muted-foreground sm:inline">· payee book</span>
        <div className="flex-1" />
        <Button variant="ghost" size="icon" onClick={logout} title="Log out"><LogOut className="size-4" /></Button>
      </header>

      <main className="mx-auto grid max-w-2xl gap-4 p-4">
        <AddPayee onAdded={load} />
        <Card>
          <CardHeader><CardTitle>Payees ({payees.length})</CardTitle></CardHeader>
          <CardContent className="grid gap-1.5">
            {payees.length === 0 && <p className="text-sm text-muted-foreground">No payees yet.</p>}
            {payees.map((p) => (
              <div key={p.id} className="flex items-center gap-3 rounded-md border px-3 py-2 text-sm">
                <div className="min-w-0 flex-1">
                  <div className="font-medium">{p.name}</div>
                  <div className="font-mono text-xs text-muted-foreground">{p.formatted}</div>
                </div>
                <Badge variant="secondary">{p.country}</Badge>
                <Button variant="ghost" size="icon" className="size-7" onClick={() => del(p.id)}><Trash2 className="size-4 text-destructive" /></Button>
              </div>
            ))}
          </CardContent>
        </Card>
      </main>
    </div>
  );
}

function AddPayee({ onAdded }: { onAdded: () => void }) {
  const [name, setName] = useState("");
  const [iban, setIban] = useState("");
  const [v, setV] = useState<Verify | null>(null);
  const [err, setErr] = useState("");
  const timer = useRef<any>(null);

  // live validation: debounce, then ask the server (which runs iban:validate).
  useEffect(() => {
    setV(null);
    const raw = iban.trim();
    if (raw.replace(/\s/g, "").length < 5) return;
    clearTimeout(timer.current);
    timer.current = setTimeout(async () => {
      const r = await api<Verify>("/verify", "POST", { iban: raw });
      if (r.ok) setV(r.data);
    }, 350);
    return () => clearTimeout(timer.current);
  }, [iban]);

  async function add() {
    if (!v?.valid || !name.trim()) return;
    const r = await api("/payees", "POST", { name, iban });
    if (r.ok) { setName(""); setIban(""); setV(null); setErr(""); onAdded(); }
    else setErr((r.data as any).error || "failed");
  }

  return (
    <Card>
      <CardHeader><CardTitle>New payee</CardTitle></CardHeader>
      <CardContent className="grid gap-3">
        <Input placeholder="Name" value={name} onChange={(e) => setName(e.target.value)} />
        <div className="grid gap-1">
          <Input className="font-mono" placeholder="IBAN (e.g. DE89 3704 0044 0532 0130 00)" value={iban} onChange={(e) => setIban(e.target.value)} />
          {v && (
            <div className={`flex items-center gap-1.5 text-xs ${v.valid ? "text-green-600" : "text-destructive"}`}>
              {v.valid ? <Check className="size-3.5" /> : <X className="size-3.5" />}
              {v.valid ? <span>Valid · <b>{v.country}</b> · {v.formatted}</span> : <span>{v.error}</span>}
            </div>
          )}
        </div>
        <div className="flex items-center gap-2">
          <Button onClick={add} disabled={!v?.valid || !name.trim()}><Plus className="size-4" /> Add payee</Button>
          {err && <span className="text-xs text-destructive">{err}</span>}
          <span className="text-xs text-muted-foreground">Checked by the <code>iban:validate</code> component.</span>
        </div>
      </CardContent>
    </Card>
  );
}

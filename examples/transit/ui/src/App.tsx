import { useEffect, useRef, useState } from "react";
import { Ticket as TicketIcon, LogOut, Check, X, ScanLine, Camera, QrCode, CreditCard } from "lucide-react";
import {
  api, getText, setToken, hasToken, money, clock,
  type Me, type Fare, type Ticket, type ValidateResult,
} from "./api";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import { Tabs, TabsList, TabsTrigger, TabsContent } from "@/components/ui/tabs";
import { Select, SelectTrigger, SelectValue, SelectContent, SelectItem } from "@/components/ui/select";

const KIND_LABEL: Record<string, string> = { single: "Single", duration: "Timed", pass: "Pass" };
const STATUS_STYLE: Record<string, string> = {
  valid: "bg-blue-600", active: "bg-green-600", used: "bg-zinc-500", expired: "bg-red-600",
};

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
  const [email, setEmail] = useState("rider@acme.io");
  const [password, setPassword] = useState("pw12345678");
  const [role, setRole] = useState("rider");
  const [msg, setMsg] = useState("Register as a rider to buy & show tickets, or a validator to scan & validate them.");
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
        <CardHeader><CardTitle className="flex items-center gap-2 text-sm"><TicketIcon className="size-4" /> transit — sign in</CardTitle></CardHeader>
        <CardContent className="grid gap-3">
          <Input placeholder="email" value={email} onChange={(e) => setEmail(e.target.value)} />
          <Input type="password" placeholder="password" value={password} onChange={(e) => setPassword(e.target.value)} />
          <Select value={role} onValueChange={setRole}>
            <SelectTrigger><SelectValue /></SelectTrigger>
            <SelectContent><SelectItem value="rider">rider</SelectItem><SelectItem value="validator">validator</SelectItem></SelectContent>
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
        <TicketIcon className="size-5 text-primary" />
        <span className="font-semibold">transit</span>
        <span className="text-muted-foreground text-sm hidden sm:inline">· {me.is_validator ? "validator" : "tickets"}</span>
        <div className="flex-1" />
        <Badge className={me.is_validator ? "bg-amber-600" : ""}>{me.is_validator ? "VALIDATOR" : "RIDER"}</Badge>
        <Button variant="ghost" size="icon" onClick={logout} title="Log out"><LogOut className="size-4" /></Button>
      </header>
      <main className="mx-auto max-w-lg p-4">
        {me.is_validator ? <Validator /> : <Rider />}
      </main>
    </div>
  );
}

// ---- rider ------------------------------------------------------------------

function Rider() {
  const [fares, setFares] = useState<Fare[]>([]);
  const [tickets, setTickets] = useState<Ticket[]>([]);
  const [tab, setTab] = useState("buy");
  async function load() {
    setFares((await api<{ items: Fare[] }>("/fares")).data.items || []);
    setTickets((await api<{ items: Ticket[] }>("/tickets")).data.items || []);
  }
  useEffect(() => { load(); }, []);
  async function buy(fare: string) {
    await api("/tickets", "POST", { fare });
    await load();
    setTab("tickets");
  }
  return (
    <Tabs value={tab} onValueChange={setTab}>
      <TabsList className="w-full">
        <TabsTrigger value="buy" className="flex-1">Buy</TabsTrigger>
        <TabsTrigger value="tickets" className="flex-1">My tickets{tickets.length ? ` (${tickets.length})` : ""}</TabsTrigger>
      </TabsList>

      <TabsContent value="buy" className="grid gap-3">
        {fares.map((f) => (
          <Card key={f.key}>
            <CardContent className="flex items-center gap-3 pt-4">
              <CreditCard className="size-5 text-muted-foreground" />
              <div className="min-w-0 flex-1">
                <div className="font-medium">{f.name}</div>
                <div className="text-xs text-muted-foreground">{KIND_LABEL[f.kind]}{f.minutes ? ` · valid ${f.minutes >= 1440 ? f.minutes / 1440 + "d" : f.minutes + " min"} from first scan` : " · one ride"}</div>
              </div>
              <div className="text-right">
                <div className="font-bold tabular-nums">{money(f.price)}</div>
                <Button size="sm" className="mt-1" onClick={() => buy(f.key)}>Buy</Button>
              </div>
            </CardContent>
          </Card>
        ))}
      </TabsContent>

      <TabsContent value="tickets" className="grid gap-3">
        {tickets.length === 0 && <p className="text-sm text-muted-foreground">No tickets yet — buy one in the Buy tab.</p>}
        {tickets.map((t) => <TicketCard key={t.id} t={t} />)}
      </TabsContent>
    </Tabs>
  );
}

function TicketCard({ t }: { t: Ticket }) {
  const [svg, setSvg] = useState("");
  const [show, setShow] = useState(false);
  const scannable = t.status === "valid" || t.status === "active";
  async function toggle() {
    if (!show && !svg) setSvg(await getText(`/tickets/${t.id}/qr.svg`));
    setShow((s) => !s);
  }
  return (
    <Card>
      <CardContent className="pt-4">
        <div className="flex items-center gap-3">
          <div className="min-w-0 flex-1">
            <div className="font-medium">{t.fare_name}</div>
            <div className="text-xs text-muted-foreground">
              {money(t.price)}
              {t.status === "active" && t.remaining_min != null && ` · ${t.remaining_min} min left (until ${clock(t.valid_until)})`}
              {t.status === "valid" && " · not yet activated"}
            </div>
          </div>
          <Badge className={STATUS_STYLE[t.status]}>{t.status}</Badge>
          {scannable && <Button variant="outline" size="sm" onClick={toggle}><QrCode className="size-4" /> {show ? "Hide" : "Show"}</Button>}
        </div>
        {show && scannable && (
          <div className="mt-3 grid place-items-center">
            <div className="w-52 rounded-lg bg-white p-3" dangerouslySetInnerHTML={{ __html: svg }} />
            <p className="mt-2 text-xs text-muted-foreground">Show this to a validator to scan.</p>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

// ---- validator --------------------------------------------------------------

function Validator() {
  const [result, setResult] = useState<ValidateResult | null>(null);
  const [manual, setManual] = useState("");
  const [recent, setRecent] = useState<{ code: string; result: string }[]>([]);
  const last = useRef<{ code: string; t: number }>({ code: "", t: 0 });

  async function validate(code: string) {
    const c = code.trim();
    if (!c) return;
    const now = Date.now();
    if (c === last.current.code && now - last.current.t < 3000) return; // debounce repeats
    last.current = { code: c, t: now };
    const r = await api<ValidateResult>("/validate", "POST", { code: c });
    if (r.ok) { setResult(r.data); setRecent((x) => [{ code: c, result: r.data.result }, ...x].slice(0, 6)); }
  }

  const ok = result?.result === "accept";
  return (
    <div className="grid gap-3">
      <Scanner onCode={validate} />

      <Card>
        <CardContent className="flex items-end gap-2 pt-4">
          <label className="grid flex-1 gap-1 text-xs text-muted-foreground">Manual code (paste a ticket id)
            <Input placeholder="ticket id…" value={manual} onChange={(e) => setManual(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && (validate(manual), setManual(""))} /></label>
          <Button onClick={() => { validate(manual); setManual(""); }}>Validate</Button>
        </CardContent>
      </Card>

      {result && (
        <Card className={ok ? "border-green-600" : "border-red-600"}>
          <CardContent className={`grid place-items-center gap-1 py-8 text-center ${ok ? "text-green-600" : "text-red-600"}`}>
            {ok ? <Check className="size-14" /> : <X className="size-14" />}
            <div className="text-2xl font-extrabold uppercase tracking-wide">{ok ? "Accepted" : "Rejected"}</div>
            <div className="text-sm text-foreground">{result.reason}</div>
            {result.remaining_min != null && ok && <div className="text-xs text-muted-foreground">{result.remaining_min} min remaining · until {clock(result.valid_until)}</div>}
          </CardContent>
        </Card>
      )}

      {recent.length > 0 && (
        <Card>
          <CardHeader><CardTitle className="text-sm">Recent scans</CardTitle></CardHeader>
          <CardContent className="grid gap-1">
            {recent.map((r, i) => (
              <div key={i} className="flex items-center gap-2 text-xs">
                {r.result === "accept" ? <Check className="size-3.5 text-green-600" /> : <X className="size-3.5 text-red-600" />}
                <span className="truncate font-mono text-muted-foreground">{r.code}</span>
              </div>
            ))}
          </CardContent>
        </Card>
      )}
    </div>
  );
}

// Camera QR scanner via the native BarcodeDetector API (Chromium/Android/iOS
// Safari). Falls back to the manual field when the camera or detector is
// unavailable. getUserMedia needs a secure context (localhost counts).
function Scanner({ onCode }: { onCode: (code: string) => void }) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const [on, setOn] = useState(false);
  const [err, setErr] = useState("");

  useEffect(() => {
    if (!on) return;
    let stream: MediaStream | undefined;
    let raf = 0;
    let stop = false;
    const supported = "BarcodeDetector" in window;
    (async () => {
      try {
        stream = await navigator.mediaDevices.getUserMedia({ video: { facingMode: "environment" } });
        if (videoRef.current) { videoRef.current.srcObject = stream; await videoRef.current.play(); }
      } catch {
        setErr("Camera unavailable — use the manual field below."); setOn(false); return;
      }
      if (!supported) { setErr("QR detection isn't supported in this browser — use the manual field."); return; }
      const det = new (window as any).BarcodeDetector({ formats: ["qr_code"] });
      const tick = async () => {
        if (stop) return;
        try {
          const codes = await det.detect(videoRef.current);
          if (codes[0]?.rawValue) onCode(codes[0].rawValue);
        } catch { /* frame not ready */ }
        raf = requestAnimationFrame(tick);
      };
      raf = requestAnimationFrame(tick);
    })();
    return () => { stop = true; if (raf) cancelAnimationFrame(raf); stream?.getTracks().forEach((t) => t.stop()); };
  }, [on]);

  return (
    <Card>
      <CardContent className="grid gap-2 pt-4">
        <div className="relative aspect-square w-full overflow-hidden rounded-lg bg-black grid place-items-center">
          <video ref={videoRef} className="h-full w-full object-cover" muted playsInline />
          {!on && <div className="absolute inset-0 grid place-items-center text-muted-foreground"><ScanLine className="size-12 opacity-40" /></div>}
          {on && <div className="pointer-events-none absolute inset-8 rounded-lg border-2 border-primary/70" />}
        </div>
        <Button variant={on ? "destructive" : "default"} onClick={() => { setErr(""); setOn((v) => !v); }}>
          <Camera className="size-4" /> {on ? "Stop camera" : "Scan a ticket"}
        </Button>
        {err && <p className="text-xs text-amber-600">{err}</p>}
      </CardContent>
    </Card>
  );
}

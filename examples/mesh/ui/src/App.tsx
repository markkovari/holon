import { useEffect, useRef, useState } from "react";
import {
  ShieldCheck, ShieldAlert, ShieldQuestion, Zap, RotateCcw, Check, X, Ban, Timer, Server, Network,
} from "lucide-react";
import { api, apiGet, type CallResult, type CircuitView, type State } from "./api";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";

// The four ways the demo upstream can behave. The path is what mesh forwards
// through proxy:route — the upstream itself decides nothing, so every run is
// reproducible. `/dead/*` routes to a port nothing listens on.
const UPSTREAMS = [
  { id: "healthy", label: "Healthy", icon: Check, path: () => `/upstream/hit?id=demo` },
  { id: "fail", label: "500s", icon: X, path: () => `/upstream/hit?id=demo&fail=1` },
  { id: "slow", label: "Slow (300ms)", icon: Timer, path: () => `/upstream/hit?id=demo&delay=300` },
  { id: "dead", label: "Unreachable", icon: Ban, path: () => `/dead/anything` },
] as const;

const STATE_UI: Record<State, { icon: typeof ShieldCheck; cls: string; badge: string; blurb: string }> = {
  closed: {
    icon: ShieldCheck,
    cls: "text-green-600",
    badge: "bg-green-600",
    blurb: "Calls flow. Failures are counted.",
  },
  open: {
    icon: ShieldAlert,
    cls: "text-red-600",
    badge: "bg-red-600",
    blurb: "Tripped — requests are refused here. The upstream is not dialled at all.",
  },
  "half-open": {
    icon: ShieldQuestion,
    cls: "text-amber-600",
    badge: "bg-amber-600",
    blurb: "Cooldown over — a probe is allowed through to test recovery.",
  },
};

export default function App() {
  const [key, setKey] = useState("checkout");
  const [circuit, setCircuit] = useState<CircuitView | null>(null);
  const [log, setLog] = useState<CallResult[]>([]);
  const [busy, setBusy] = useState(false);

  // Policy knobs, all sent per call — the app has no stored config.
  const [attempts, setAttempts] = useState(3);
  const [baseMs, setBaseMs] = useState(50);
  const [sloMs, setSloMs] = useState(0);
  const [failureThreshold, setFailureThreshold] = useState(3);
  const [openMs, setOpenMs] = useState(4000);

  // Poll the circuit so the OPEN countdown and the automatic half-open flip are
  // visible without clicking anything.
  const keyRef = useRef(key);
  keyRef.current = key;
  useEffect(() => {
    const tick = async () => setCircuit(await apiGet<CircuitView>(`/circuit/${keyRef.current}`));
    tick();
    const t = setInterval(tick, 400);
    return () => clearInterval(t);
  }, []);

  async function call(path: string, times = 1) {
    setBusy(true);
    for (let i = 0; i < times; i++) {
      const r = await api<CallResult>("/call", {
        key, path, attempts, base_ms: baseMs, slo_ms: sloMs,
        failure_threshold: failureThreshold, open_ms: openMs,
        success_threshold: 1, half_open_probes: 1,
      });
      setLog((l) => [r.data, ...l].slice(0, 12));
      setCircuit(await apiGet<CircuitView>(`/circuit/${key}`));
    }
    setBusy(false);
  }

  async function reset() {
    await api("/reset", { key });
    setLog([]);
    setCircuit(await apiGet<CircuitView>(`/circuit/${key}`));
  }

  const state = circuit?.circuit.state ?? "closed";
  const ui = STATE_UI[state];
  const StateIcon = ui.icon;
  const s = circuit?.stats;

  return (
    <div className="min-h-[100dvh]">
      <header className="sticky top-0 z-10 flex flex-wrap items-center gap-2 border-b bg-card/80 px-4 py-3 backdrop-blur">
        <Network className="size-5 text-primary" />
        <span className="font-semibold">mesh</span>
        <span className="hidden text-sm text-muted-foreground sm:inline">· retry · circuit breaker · SLO</span>
        <div className="flex-1" />
        <label className="flex items-center gap-2 text-xs text-muted-foreground">circuit
          <Input className="w-32" value={key} onChange={(e) => setKey(e.target.value)} /></label>
        <Button variant="outline" size="sm" onClick={reset}><RotateCcw className="size-4" /> Reset</Button>
      </header>

      <main className="mx-auto grid max-w-5xl gap-4 p-4 md:grid-cols-2">
        {/* ---- the circuit ---- */}
        <Card className="md:col-span-2">
          <CardContent className="flex flex-wrap items-center gap-4 pt-6">
            <StateIcon className={`size-10 ${ui.cls}`} />
            <div className="grid gap-1">
              <div className="flex items-center gap-2">
                <Badge className={ui.badge}>{state}</Badge>
                {state === "open" && (
                  // Once the cooldown elapses the circuit is still stored as open —
                  // it flips to half-open on the next call, which spends the probe.
                  <span className="text-xs text-muted-foreground">
                    {circuit?.would_admit ? "cooldown over — next call probes" : `probe in ${circuit?.retry_after_ms ?? 0}ms`}
                  </span>
                )}
                {state === "closed" && (circuit?.circuit.failures ?? 0) > 0 && (
                  <span className="text-xs text-muted-foreground">
                    {circuit?.circuit.failures}/{failureThreshold} failures in the window
                  </span>
                )}
              </div>
              <p className="text-xs text-muted-foreground">{ui.blurb}</p>
            </div>
            <div className="flex-1" />
            <div className="grid grid-cols-5 gap-3 text-center text-xs">
              {[
                ["calls", s?.attempts ?? 0],
                ["ok", s?.ok ?? 0],
                ["failed", s?.failed ?? 0],
                ["shed", s?.shed ?? 0],
                ["trips", s?.trips ?? 0],
              ].map(([label, n]) => (
                <div key={label as string}>
                  <div className="font-mono text-lg">{n as number}</div>
                  <div className="text-muted-foreground">{label as string}</div>
                </div>
              ))}
            </div>
          </CardContent>
        </Card>

        {/* ---- drive the upstream ---- */}
        <Card>
          <CardHeader><CardTitle className="flex items-center gap-2 text-sm"><Server className="size-4" /> Upstream</CardTitle></CardHeader>
          <CardContent className="grid gap-3">
            <div className="grid grid-cols-2 gap-2">
              {UPSTREAMS.map((u) => {
                const Icon = u.icon;
                return (
                  <Button key={u.id} size="sm" variant="secondary" disabled={busy} onClick={() => call(u.path())}>
                    <Icon className="size-4" /> {u.label}
                  </Button>
                );
              })}
            </div>
            <Button size="sm" disabled={busy} onClick={() => call(UPSTREAMS[1].path(), failureThreshold + 1)}>
              <Zap className="size-4" /> Hammer it ×{failureThreshold + 1} (trip the breaker)
            </Button>
            <p className="text-xs text-muted-foreground">
              Once it trips, keep clicking: the calls come back <b>503 shed</b> and the upstream's hit
              counter stops moving — the request never leaves the host.
            </p>
          </CardContent>
        </Card>

        {/* ---- policy ---- */}
        <Card>
          <CardHeader><CardTitle className="text-sm">Policy</CardTitle></CardHeader>
          <CardContent className="grid grid-cols-2 gap-3">
            <Field label="attempts" value={attempts} onChange={setAttempts} />
            <Field label="backoff base ms" value={baseMs} onChange={setBaseMs} />
            <Field label="slo ms (0 = off)" value={sloMs} onChange={setSloMs} />
            <Field label="failures to trip" value={failureThreshold} onChange={setFailureThreshold} />
            <Field label="open for ms" value={openMs} onChange={setOpenMs} />
          </CardContent>
        </Card>

        {/* ---- what happened ---- */}
        <Card className="md:col-span-2">
          <CardHeader><CardTitle className="text-sm">Calls</CardTitle></CardHeader>
          <CardContent className="grid gap-2">
            {log.length === 0 && <p className="text-xs text-muted-foreground">Call the upstream…</p>}
            {log.map((r, i) => (
              <div key={i} className="grid gap-1 rounded-md border p-2">
                <div className="flex items-center gap-2 text-xs">
                  {r.shed ? <Ban className="size-3.5 text-red-600" /> : r.ok ? <Check className="size-3.5 text-green-600" /> : <X className="size-3.5 text-red-600" />}
                  <span className="font-medium">{r.shed ? "shed — circuit open" : r.ok ? "ok" : "failed"}</span>
                  <Badge className={STATE_UI[r.state].badge}>{r.state}</Badge>
                  <span className="text-muted-foreground">{r.total_ms}ms total</span>
                  {r.shed && <span className="text-muted-foreground">· upstream not called</span>}
                  {r.error && <span className="truncate text-red-600">· {r.error}</span>}
                </div>
                {r.attempts.map((a) => (
                  <div key={a.n} className="flex items-center gap-2 pl-5 text-xs text-muted-foreground">
                    <span className="font-mono">#{a.n}</span>
                    <span className={a.ok ? "text-green-600" : "text-red-600"}>{a.status || "—"}</span>
                    <span>{a.ms}ms</span>
                    {a.error && <span className="truncate">{a.error}</span>}
                  </div>
                ))}
              </div>
            ))}
          </CardContent>
        </Card>
      </main>

      <footer className="mx-auto max-w-5xl px-4 pb-8 text-xs text-muted-foreground">
        The breaker state machine and the backoff schedule are the stateless <code>resilience:breaker</code>{" "}
        component; the circuit itself is a per-key record in <code>records:store</code> under a revision
        compare-and-set, so concurrent callers converge on one circuit. The upstream hop is a real
        outgoing HTTP request through <code>proxy:route</code> — which is why a tripped breaker is
        observable as a request that never happened. See <code>docs/apps/MESH.md</code>.
      </footer>
    </div>
  );
}

function Field({ label, value, onChange }: { label: string; value: number; onChange: (n: number) => void }) {
  return (
    <label className="grid gap-1 text-xs text-muted-foreground">{label}
      <Input type="number" value={value} onChange={(e) => onChange(Number(e.target.value) || 0)} /></label>
  );
}

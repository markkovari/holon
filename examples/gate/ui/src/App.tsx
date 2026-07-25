import { useState } from "react";
import { Gauge, Activity, Layers, Zap, RotateCcw, Check, X, Send, Workflow } from "lucide-react";
import { api, apiGet, type Decision, type BatchSubmit, type BatchState } from "./api";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";

type LogRow = { ok: boolean; label: string };

function Field({ label, value, onChange, w = "w-20" }: { label: string; value: number; onChange: (n: number) => void; w?: string }) {
  return (
    <label className="grid gap-1 text-xs text-muted-foreground">{label}
      <Input className={w} type="number" value={value} onChange={(e) => onChange(Number(e.target.value) || 0)} /></label>
  );
}

function LogList({ rows }: { rows: LogRow[] }) {
  return (
    <div className="grid gap-1">
      {rows.length === 0 && <p className="text-xs text-muted-foreground">Fire some requests…</p>}
      {rows.map((r, i) => (
        <div key={i} className="flex items-center gap-2 text-xs">
          {r.ok ? <Check className="size-3.5 text-green-600" /> : <X className="size-3.5 text-red-600" />}
          <span className={r.ok ? "text-green-600 font-medium" : "text-red-600 font-medium"}>{r.ok ? "200" : "429"}</span>
          <span className="text-muted-foreground">{r.label}</span>
        </div>
      ))}
    </div>
  );
}

export default function App() {
  const [key, setKey] = useState("acme");
  const [resetMsg, setResetMsg] = useState("");
  async function reset() {
    await api("/reset", { key });
    setResetMsg("cleared");
    setTimeout(() => setResetMsg(""), 1200);
  }
  return (
    <div className="min-h-[100dvh]">
      <header className="sticky top-0 z-10 flex flex-wrap items-center gap-2 border-b bg-card/80 px-4 py-3 backdrop-blur">
        <Workflow className="size-5 text-primary" />
        <span className="font-semibold">gate</span>
        <span className="hidden text-sm text-muted-foreground sm:inline">· durable traffic shaping</span>
        <div className="flex-1" />
        <label className="flex items-center gap-2 text-xs text-muted-foreground">API key
          <Input className="w-32" value={key} onChange={(e) => setKey(e.target.value)} /></label>
        <Button variant="outline" size="sm" onClick={reset}><RotateCcw className="size-4" /> Reset{resetMsg && ` ✓`}</Button>
      </header>

      <main className="mx-auto grid max-w-5xl gap-4 p-4 md:grid-cols-3">
        <RatePanel apiKey={key} />
        <ThrottlePanel apiKey={key} />
        <BatchPanel apiKey={key} />
      </main>

      <footer className="mx-auto max-w-5xl px-4 pb-8 text-xs text-muted-foreground">
        Each panel is a durable, per-key <b>worker pattern</b>: state lives in <code>records:store</code> under a
        revision compare-and-set, the shaping math is <code>shaper:limit</code>. On <b>Golem Cloud</b> each key would be a
        single-threaded durable worker — the CAS becomes exact serialization, the batch flush an atomic region, and the
        throttle a scheduled drain. See <code>GATE.md</code>.
      </footer>
    </div>
  );
}

function RatePanel({ apiKey }: { apiKey: string }) {
  const [capacity, setCapacity] = useState(5);
  const [refill, setRefill] = useState(1);
  const [rows, setRows] = useState<LogRow[]>([]);
  const [tokens, setTokens] = useState(capacity);

  async function fire(n: number) {
    const out: LogRow[] = [];
    for (let i = 0; i < n; i++) {
      const r = await api<Decision>("/ratelimit", { key: apiKey, capacity, refill });
      setTokens(r.data.remaining);
      out.unshift({ ok: r.data.allowed, label: r.data.allowed ? `${r.data.remaining.toFixed(1)} tokens left` : `retry in ${r.data.retry_after_ms}ms` });
    }
    setRows((rs) => [...out, ...rs].slice(0, 12));
  }
  const pct = Math.max(0, Math.min(100, (tokens / capacity) * 100));
  return (
    <Card>
      <CardHeader><CardTitle className="flex items-center gap-2 text-sm"><Gauge className="size-4" /> Rate limit · token bucket</CardTitle></CardHeader>
      <CardContent className="grid gap-3">
        <div className="flex items-end gap-2">
          <Field label="capacity" value={capacity} onChange={setCapacity} />
          <Field label="refill/s" value={refill} onChange={setRefill} />
        </div>
        <div className="h-2 overflow-hidden rounded-full bg-muted"><div className="h-full bg-primary transition-all" style={{ width: `${pct}%` }} /></div>
        <div className="flex gap-2">
          <Button size="sm" onClick={() => fire(1)}><Send className="size-4" /> Send</Button>
          <Button size="sm" variant="secondary" onClick={() => fire(10)}><Zap className="size-4" /> Burst ×10</Button>
        </div>
        <LogList rows={rows} />
      </CardContent>
    </Card>
  );
}

function ThrottlePanel({ apiKey }: { apiKey: string }) {
  const [rate, setRate] = useState(4);
  const [burst, setBurst] = useState(2);
  const [rows, setRows] = useState<LogRow[]>([]);

  async function fire(n: number) {
    const out: LogRow[] = [];
    for (let i = 0; i < n; i++) {
      const r = await api<Decision>("/throttle", { key: apiKey, rate, burst });
      out.unshift({ ok: r.data.allowed, label: r.data.allowed ? "admitted" : `space out ${r.data.retry_after_ms}ms` });
    }
    setRows((rs) => [...out, ...rs].slice(0, 12));
  }
  return (
    <Card>
      <CardHeader><CardTitle className="flex items-center gap-2 text-sm"><Activity className="size-4" /> Throttle · GCRA</CardTitle></CardHeader>
      <CardContent className="grid gap-3">
        <div className="flex items-end gap-2">
          <Field label="rate/s" value={rate} onChange={setRate} />
          <Field label="burst" value={burst} onChange={setBurst} />
        </div>
        <p className="text-xs text-muted-foreground">Smooths to {rate}/s with a {burst}-cell burst budget.</p>
        <div className="flex gap-2">
          <Button size="sm" onClick={() => fire(1)}><Send className="size-4" /> Send</Button>
          <Button size="sm" variant="secondary" onClick={() => fire(10)}><Zap className="size-4" /> Burst ×10</Button>
        </div>
        <LogList rows={rows} />
      </CardContent>
    </Card>
  );
}

function BatchPanel({ apiKey }: { apiKey: string }) {
  const [maxSize, setMaxSize] = useState(4);
  const [maxAge, setMaxAge] = useState(8000);
  const [item, setItem] = useState("");
  const [batch, setBatch] = useState<BatchState | null>(null);

  async function refresh(id: string) { setBatch(await apiGet<BatchState>(`/batch/${id}`)); }
  async function submit(v: string) {
    if (!v) return;
    const r = await api<BatchSubmit>("/batch/submit", { key: apiKey, item: v, max_size: maxSize, max_age_ms: maxAge });
    if (r.ok) { setItem(""); refresh(r.data.batch); }
  }
  const sample = ["sku-91", "sku-42", "sku-17", "sku-63", "sku-08"];
  return (
    <Card>
      <CardHeader><CardTitle className="flex items-center gap-2 text-sm"><Layers className="size-4" /> Batch · coalesce + flush</CardTitle></CardHeader>
      <CardContent className="grid gap-3">
        <div className="flex items-end gap-2">
          <Field label="max size" value={maxSize} onChange={setMaxSize} />
          <Field label="max age ms" value={maxAge} onChange={setMaxAge} w="w-24" />
        </div>
        <div className="flex gap-2">
          <Input placeholder="item…" value={item} onChange={(e) => setItem(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && submit(item)} />
          <Button size="sm" onClick={() => submit(item)}><Send className="size-4" /></Button>
        </div>
        <Button size="sm" variant="secondary" onClick={() => submit(sample[Math.floor((batch?.size ?? 0)) % sample.length])}>
          <Zap className="size-4" /> Submit a sample
        </Button>

        {batch && (
          <div className="grid gap-2 rounded-md border p-3">
            <div className="flex items-center gap-2 text-xs">
              <span className="text-muted-foreground">batch {batch.id.slice(-6)}</span>
              <Badge className={batch.flushed ? "bg-green-600" : "bg-amber-600"}>{batch.flushed ? "flushed" : `filling ${batch.size}/${batch.max_size}`}</Badge>
            </div>
            <div className="grid gap-1">
              {batch.items.map((it, i) => (
                <div key={i} className="flex items-center gap-2 text-xs">
                  <span className="w-16 truncate font-mono">{it}</span>
                  {batch.flushed && batch.results && <span className="text-green-600">→ {batch.results[i]}</span>}
                </div>
              ))}
            </div>
            {batch.flushed && <p className="text-xs text-muted-foreground">All {batch.size} items processed in one batch.</p>}
          </div>
        )}
      </CardContent>
    </Card>
  );
}

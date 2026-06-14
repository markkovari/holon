// Shared measurement: warm up, then time N iterations with nanosecond precision,
// report ns/op (mean), ops/sec, and p50/p95/p99.

export interface Result {
  op: string;
  iters: number;
  meanNs: number;
  p50Ns: number;
  p95Ns: number;
  p99Ns: number;
  opsPerSec: number;
}

export async function measure(
  op: string,
  fn: () => unknown | Promise<unknown>,
  opts: { iters?: number; warmup?: number } = {},
): Promise<Result> {
  const iters = opts.iters ?? 2000;
  const warmup = opts.warmup ?? Math.min(200, Math.floor(iters / 10));

  for (let i = 0; i < warmup; i++) await fn();

  const samples = new Float64Array(iters);
  for (let i = 0; i < iters; i++) {
    const t0 = process.hrtime.bigint();
    await fn();
    const t1 = process.hrtime.bigint();
    samples[i] = Number(t1 - t0); // ns
  }

  const sorted = Array.from(samples).sort((a, b) => a - b);
  const mean = sorted.reduce((s, x) => s + x, 0) / iters;
  const pct = (p: number) => sorted[Math.min(iters - 1, Math.floor((p / 100) * iters))];

  return {
    op,
    iters,
    meanNs: Math.round(mean),
    p50Ns: Math.round(pct(50)),
    p95Ns: Math.round(pct(95)),
    p99Ns: Math.round(pct(99)),
    opsPerSec: Math.round(1e9 / mean),
  };
}

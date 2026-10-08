use instant_distance::{Builder, Point, Search};
use std::time::Instant;
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 { self.0 ^= self.0 << 13; self.0 ^= self.0 >> 7; self.0 ^= self.0 << 17; self.0 }
    fn f(&mut self) -> f32 { (self.next() >> 11) as f32 / (1u64 << 53) as f32 }
    fn g(&mut self) -> f32 { let (a, b) = (self.f().max(1e-9), self.f()); (-2.0 * a.ln()).sqrt() * (6.2831853 * b).cos() }
}
fn unit(v: &mut [f32]) { let n = v.iter().map(|x| x * x).sum::<f32>().sqrt(); v.iter_mut().for_each(|x| *x /= n); }
#[inline] fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut s = [0f32; 8];
    for (x, y) in a.chunks_exact(8).zip(b.chunks_exact(8)) { for i in 0..8 { s[i] += x[i] * y[i]; } }
    s.iter().sum()
}
#[inline] fn dot_i8(a: &[i8], b: &[i8]) -> i32 {
    let mut s = [0i32; 16];
    for (x, y) in a.chunks_exact(16).zip(b.chunks_exact(16)) { for i in 0..16 { s[i] += x[i] as i32 * y[i] as i32; } }
    s.iter().sum()
}
fn topk(scores: impl Iterator<Item = (usize, f32)>, k: usize) -> Vec<usize> {
    let mut best: Vec<(f32, usize)> = Vec::with_capacity(k + 1);
    for (i, s) in scores {
        if best.len() < k || s > best[best.len() - 1].0 {
            let pos = best.partition_point(|b| b.0 > s); best.insert(pos, (s, i)); best.truncate(k);
        }
    }
    best.into_iter().map(|b| b.1).collect()
}
#[derive(Clone)] struct P(Vec<f32>);
impl Point for P { fn distance(&self, o: &Self) -> f32 { 1.0 - dot(&self.0, &o.0) } }
fn stat(mut v: Vec<f64>) -> (f64, f64) { v.sort_by(|a, b| a.partial_cmp(b).unwrap()); (v[v.len() / 2], v[v.len() * 95 / 100]) }

fn main() {
    let mut r = Rng(0x9e3779b97f4a7c15);
    for &dim in &[256usize, 128] {
        let cents: Vec<Vec<f32>> = (0..10).map(|_| (0..dim).map(|_| r.g()).collect()).collect();
        for &n in &[1_000usize, 10_000, 100_000, 200_000, 1_000_000] {
            let mut data = vec![0f32; n * dim];
            for i in 0..n { let c = &cents[i % 10]; let row = &mut data[i * dim..(i + 1) * dim]; for j in 0..dim { row[j] = c[j] + 0.8 * r.g(); } unit(row); }
            let q8: Vec<i8> = data.iter().map(|x| (x * 127.0).round() as i8).collect();
            let queries: Vec<Vec<f32>> = (0..20).map(|_| { let mut v: Vec<f32> = cents[3].iter().map(|c| c + 0.8 * r.g()).collect(); unit(&mut v); v }).collect();
            // flat f32, single thread
            let mut t1 = vec![]; let mut exact = vec![];
            for q in &queries { let t = Instant::now();
                let top = topk((0..n).map(|i| (i, dot(&data[i * dim..(i + 1) * dim], q))), 10);
                t1.push(t.elapsed().as_secs_f64() * 1000.0); exact.push(top); }
            // flat f32, 8 threads
            let mut t8 = vec![];
            for q in &queries { let t = Instant::now();
                let chunk = n.div_ceil(8);
                let parts: Vec<Vec<(f32, usize)>> = std::thread::scope(|s| {
                    (0..8).map(|c| { let (data, q) = (&data, q); s.spawn(move || {
                        let (lo, hi) = (c * chunk, ((c + 1) * chunk).min(n));
                        let ids = topk((lo..hi).map(|i| (i, dot(&data[i * dim..(i + 1) * dim], q))), 10);
                        ids.into_iter().map(|i| (dot(&data[i * dim..(i + 1) * dim], q), i)).collect()
                    }) }).collect::<Vec<_>>().into_iter().map(|h| h.join().unwrap()).collect() });
                let mut all: Vec<(f32, usize)> = parts.into_iter().flatten().collect();
                all.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap()); all.truncate(10);
                t8.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            // int8 single thread, recall vs exact
            let mut ti = vec![]; let mut rec = 0.0;
            for (qi, q) in queries.iter().enumerate() {
                let qq: Vec<i8> = q.iter().map(|x| (x * 127.0).round() as i8).collect();
                let t = Instant::now();
                let top = topk((0..n).map(|i| (i, dot_i8(&q8[i * dim..(i + 1) * dim], &qq) as f32)), 10);
                ti.push(t.elapsed().as_secs_f64() * 1000.0);
                rec += top.iter().filter(|x| exact[qi].contains(x)).count() as f64 / 10.0;
            }
            let (a1, b1) = stat(t1); let (a8, b8) = stat(t8); let (ai, bi) = stat(ti);
            println!("{{\"dim\":{dim},\"n\":{n},\"f32_1thr_p50\":{a1:.3},\"f32_1thr_p95\":{b1:.3},\"f32_8thr_p50\":{a8:.3},\"f32_8thr_p95\":{b8:.3},\"i8_1thr_p50\":{ai:.3},\"i8_1thr_p95\":{bi:.3},\"i8_recall10\":{:.3},\"mem_mb_f32\":{:.0}}}", rec / 20.0, (n * dim * 4) as f64 / 1e6);
            // HNSW (instant-distance) for the bigger ones, dim 256 only
            if dim == 256 && n >= 10_000 && n <= 200_000 {
                let pts: Vec<P> = (0..n).map(|i| P(data[i * dim..(i + 1) * dim].to_vec())).collect();
                let t = Instant::now(); let h = Builder::default().ef_construction(100).build(pts.clone(), (0..n).collect::<Vec<usize>>());
                let build = t.elapsed().as_secs_f64();
                let mut th = vec![]; let mut rc = 0.0;
                for (qi, q) in queries.iter().enumerate() {
                    let mut s = Search::default(); let t = Instant::now();
                    let got: Vec<usize> = h.search(&P(q.clone()), &mut s).take(10).map(|x| *x.value).collect();
                    th.push(t.elapsed().as_secs_f64() * 1000.0);
                    rc += got.iter().filter(|x| exact[qi].contains(x)).count() as f64 / 10.0;
                }
                let (ah, bh) = stat(th);
                println!("{{\"hnsw_instant_distance\":true,\"n\":{n},\"build_s\":{build:.1},\"p50\":{ah:.3},\"p95\":{bh:.3},\"recall10\":{:.3}}}", rc / 20.0);
            }
        }
    }
}

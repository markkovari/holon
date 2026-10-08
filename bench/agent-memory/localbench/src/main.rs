// Benchmarks the REAL Store::recall_semantic / recall over synthetic clustered vectors.
use agent_runtime::store::Store;
use std::time::Instant;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 { self.0 ^= self.0 << 13; self.0 ^= self.0 >> 7; self.0 ^= self.0 << 17; self.0 }
    fn f(&mut self) -> f32 { (self.next() >> 11) as f32 / (1u64 << 53) as f32 }
    fn g(&mut self) -> f32 { let (a, b) = (self.f().max(1e-9), self.f()); (-2.0 * a.ln()).sqrt() * (6.2831853 * b).cos() }
}
fn unit(mut v: Vec<f32>) -> Vec<f32> { let n = v.iter().map(|x| x * x).sum::<f32>().sqrt(); v.iter_mut().for_each(|x| *x /= n); v }

fn main() {
    let dim = 256;
    let mut r = Rng(0x9e3779b97f4a7c15);
    let cents: Vec<Vec<f32>> = (0..10).map(|_| (0..dim).map(|_| r.g()).collect()).collect();
    println!("{{\"bench\":\"local_jsonl\",\"dim\":{dim}}}");
    for &n in &[100usize, 1_000, 5_000, 10_000, 50_000, 100_000] {
        let dir = tempfile::tempdir().unwrap();
        let st = Store::open(dir.path()).unwrap();
        // write memories + sidecar exactly as the runtime lays them out
        let mut mem = String::new(); let mut vecs = String::new();
        for i in 0..n {
            let text = format!("note {i} about topic {} with some words to look like a memory entry of moderate length", i % 10);
            mem.push_str(&format!("{{\"at\":{i},\"text\":{}}}\n", serde_json::to_string(&text).unwrap()));
            let v = unit(cents[i % 10].iter().map(|c| c + 0.8 * r.g()).collect());
            vecs.push_str(&format!("{{\"text\":{},\"vec\":{}}}\n", serde_json::to_string(&text).unwrap(), serde_json::to_string(&v).unwrap()));
        }
        let md = dir.path().join("memory"); std::fs::create_dir_all(&md).unwrap();
        let q = unit(cents[3].iter().map(|c| c + 0.8 * r.g()).collect());
        std::fs::write(md.join("a.jsonl"), &mem).unwrap();
        std::fs::write(md.join("a.vecs.jsonl"), &vecs).unwrap();
        let reps = if n >= 50_000 { 5 } else { 20 };
        let mut ts = vec![];
        for _ in 0..reps {
            let t = Instant::now();
            let h = st.recall_semantic("a", &q, 5, &|_| None);
            ts.push(t.elapsed().as_secs_f64() * 1000.0);
            assert!(!h.is_empty() || n == 0);
        }
        ts.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mut lx = vec![];
        for _ in 0..reps { let t = Instant::now(); let _ = st.recall("a", "topic note words", 5); lx.push(t.elapsed().as_secs_f64() * 1000.0); }
        lx.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!("{{\"n\":{n},\"semantic_p50_ms\":{:.2},\"semantic_p95_ms\":{:.2},\"lexical_p50_ms\":{:.2},\"sidecar_mb\":{:.1}}}",
            ts[ts.len()/2], ts[(ts.len()*95/100).min(ts.len()-1)], lx[lx.len()/2], vecs.len() as f64/1e6);
    }
}
fn walk(p: &std::path::Path, out: &mut Vec<std::path::PathBuf>) { if let Ok(rd) = std::fs::read_dir(p) { for e in rd.flatten() { let q = e.path(); if q.is_dir() { walk(&q, out) } else { out.push(q) } } } }
fn find_side(root: &std::path::Path) -> std::path::PathBuf { let _ = root; root.join("memory").join("a.vecs.jsonl") }
fn find_mem(root: &std::path::Path) -> (std::path::PathBuf, ()) { (root.join("memory").join("a.jsonl"), ()) }

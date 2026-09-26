//! Sharpness: `green-stab-v1`.
//!
//! A plain Laplacian variance scores sensor noise as detail — at ISO 6400 the
//! "sharpest" tile of a real frame was an out-of-focus white shirt. So: square
//! root first (shot noise grows with √signal, this flattens it), a 3×3
//! binomial blur, the 4-neighbour Laplacian's variance per tile of a 16×16
//! grid, and the frame's own noise floor is its median tile. The Metal kernel
//! in `tools/media-apple` computes the same numbers; the unit tests below pin
//! the CPU half.

use serde_json::{json, Value};

pub(crate) const GRID: usize = 16;

/// Per-tile Laplacian variance (x1e6) of `p`, on a `grid`×`grid` layout. The
/// last row and column of tiles take the remainder, and the one-pixel border
/// the Laplacian cannot reach is skipped — exactly as the Metal kernel does.
fn tile_variances(p: &[f32], w: usize, h: usize, grid: usize) -> Vec<f64> {
    let (tw, th) = ((w / grid).max(1), (h / grid).max(1));
    let mut acc = vec![(0f64, 0f64, 0f64); grid * grid];
    for y in 1..h.saturating_sub(1) {
        let ty = (y / th).min(grid - 1);
        for x in 1..w - 1 {
            let c = p[y * w + x];
            let l = (p[(y - 1) * w + x] + p[(y + 1) * w + x] + p[y * w + x - 1] + p[y * w + x + 1]
                - 4.0 * c) as f64;
            let t = &mut acc[ty * grid + (x / tw).min(grid - 1)];
            t.0 += l;
            t.1 += l * l;
            t.2 += 1.0;
        }
    }
    acc.iter()
        .map(|&(s, ss, n)| if n > 0.0 { (ss / n - (s / n).powi(2)) * 1e6 } else { 0.0 })
        .collect()
}

/// Separable [1 2 1]/4, leaving the border row/column as it was (the Metal
/// kernel matches this, border and all).
fn blur3(p: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut tmp = p.to_vec();
    for y in 0..h {
        for x in 1..w.saturating_sub(1) {
            let i = y * w + x;
            tmp[i] = (p[i - 1] + 2.0 * p[i] + p[i + 1]) * 0.25;
        }
    }
    let mut out = tmp.clone();
    for y in 1..h.saturating_sub(1) {
        for x in 0..w {
            let i = y * w + x;
            out[i] = (tmp[i - w] + 2.0 * tmp[i] + tmp[i + w]) * 0.25;
        }
    }
    out
}

/// `green-stab-v1` on a linear 0–1 plane, in the callback's `sharpness` shape
/// (minus `subjects`, which need Vision's faces).
pub(crate) fn green_stab_v1(plane: &[f32], w: usize, h: usize) -> Value {
    let stab: Vec<f32> = plane.iter().map(|v| v.max(0.0).sqrt()).collect();
    let tiles = tile_variances(&blur3(&stab, w, h), w, h, GRID);
    let (floor, peak) = floor_and_peak(&tiles);
    json!({
        "method": "green-stab-v1",
        "grid": GRID,
        "tiles": tiles.iter().map(|v| round(*v, 3)).collect::<Vec<_>>(),
        "floor": round(floor, 3),
        "peak": round(peak, 3),
        "focus_ratio": round(if floor > 0.0 { peak / floor } else { 0.0 }, 3),
        "subjects": [],
    })
}

/// The median tile — most of a frame is not the subject, so the middle of the
/// distribution is the noise — and the highest tile above it.
fn floor_and_peak(tiles: &[f64]) -> (f64, f64) {
    let mut sorted = tiles.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let floor = sorted[sorted.len() / 2];
    let peak = tiles.iter().map(|t| (t - floor).max(0.0)).fold(0.0, f64::max);
    (floor, peak)
}

pub(crate) fn round(v: f64, places: i32) -> f64 {
    let m = 10f64.powi(places);
    (v * m).round() / m
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny deterministic noise source — the test must not depend on a
    /// crate's RNG, and must give the same plane every run.
    fn noise(seed: &mut u64) -> f32 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        (*seed % 10_000) as f32 / 10_000.0 - 0.5
    }

    /// 256×256: a vertical edge at x=128 in the middle tile rows, sharp or
    /// blurred, on a mid-grey with shot-like noise everywhere.
    fn plane(edge: Option<usize>, noise_amp: f32) -> Vec<f32> {
        let (w, h) = (256usize, 256usize);
        let mut seed = 0x9e3779b97f4a7c15u64;
        let mut p = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let base = match edge {
                    Some(0) => {
                        if x < 128 {
                            0.2
                        } else {
                            0.8
                        }
                    }
                    // A ramp `width` pixels wide: an out-of-focus version.
                    Some(width) => {
                        let t = ((x as f32 - 128.0) / width as f32 + 0.5).clamp(0.0, 1.0);
                        0.2 + 0.6 * t
                    }
                    None => 0.5,
                };
                p[y * w + x] = (base + noise_amp * noise(&mut seed) * base.sqrt()).clamp(0.0, 1.0);
            }
        }
        p
    }

    fn peak(v: &Value) -> f64 {
        v["peak"].as_f64().unwrap()
    }

    /// A sharp edge beats a blurred one at the same noise, and a frame of
    /// pure noise — however loud — has no focus to speak of: its best tile is
    /// barely above its own floor.
    #[test]
    fn green_stab_v1_ranks_a_sharp_edge_over_a_blurred_one_and_noise_has_no_focus() {
        let sharp = green_stab_v1(&plane(Some(0), 0.02), 256, 256);
        let blurred = green_stab_v1(&plane(Some(24), 0.02), 256, 256);
        let loud_noise = green_stab_v1(&plane(None, 0.2), 256, 256);
        assert!(
            peak(&sharp) > 3.0 * peak(&blurred),
            "sharp {} vs blurred {}",
            peak(&sharp),
            peak(&blurred)
        );
        let ratio = |v: &Value| v["focus_ratio"].as_f64().unwrap();
        assert!(ratio(&loud_noise) < 1.0, "noise focus ratio {}", ratio(&loud_noise));
        assert!(
            ratio(&sharp) > 10.0 * ratio(&loud_noise),
            "sharp {} vs noise {}",
            ratio(&sharp),
            ratio(&loud_noise)
        );
        assert_eq!(sharp["tiles"].as_array().unwrap().len(), GRID * GRID);
        assert_eq!(sharp["method"], "green-stab-v1");
    }

    /// The finding that made the metric: at high ISO a bright, flat,
    /// out-of-focus area (a white shirt) has the most absolute noise in the
    /// frame, and a raw Laplacian variance calls it the sharpest thing there.
    /// After the square root the shirt is as quiet as everything else and the
    /// real edge wins.
    #[test]
    fn a_bright_noisy_flat_patch_does_not_beat_a_real_edge() {
        let (w, h) = (256usize, 256usize);
        let mut seed = 42u64;
        let mut p = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                // The shirt: top-left, bright and flat, its outline as soft as
                // anything out of focus is (a 48 px smoothstep, not a step).
                let inside = (96.0 - x.max(y) as f32) / 48.0 + 0.5;
                let shirt = inside.clamp(0.0, 1.0);
                let shirt = shirt * shirt * (3.0 - 2.0 * shirt);
                let base: f32 = if x < 120 && y < 120 {
                    0.1 + 0.8 * shirt
                } else if y >= 160 && x >= 160 {
                    if x < 208 {
                        0.08
                    } else {
                        0.2
                    } // a dim, sharp edge bottom-right
                } else {
                    0.1
                };
                p[y * w + x] = (base + 0.08 * noise(&mut seed) * base.sqrt()).clamp(0.0, 1.0);
            }
        }
        let best = |tiles: &[f64]| {
            let i = (0..tiles.len()).max_by(|a, b| tiles[*a].total_cmp(&tiles[*b])).unwrap();
            (i % GRID, i / GRID)
        };
        let raw = tile_variances(&p, w, h, GRID);
        let (rx, ry) = best(&raw);
        assert!(rx < 5 && ry < 5, "the raw metric should fall for the shirt, picked ({rx},{ry})");
        let v = green_stab_v1(&p, w, h);
        let tiles: Vec<f64> =
            v["tiles"].as_array().unwrap().iter().map(|t| t.as_f64().unwrap()).collect();
        assert_eq!(
            best(&tiles).0,
            13,
            "green-stab-v1 must pick the edge column, got {:?}",
            best(&tiles)
        );
    }

    /// The sharp tiles are where the edge is: columns 7 and 8 of 16.
    #[test]
    fn the_peak_is_in_the_tiles_under_the_edge() {
        let v = green_stab_v1(&plane(Some(0), 0.02), 256, 256);
        let tiles: Vec<f64> =
            v["tiles"].as_array().unwrap().iter().map(|t| t.as_f64().unwrap()).collect();
        let best = (0..tiles.len()).max_by(|a, b| tiles[*a].total_cmp(&tiles[*b])).unwrap();
        assert!([7, 8].contains(&(best % GRID)), "best tile column {}", best % GRID);
    }
}

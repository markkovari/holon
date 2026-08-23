//! `svg-chart` — render a chart to SVG on the server — bar, line, donut and sparkline
//!
//! A dependency-free renderer: one data series in, a standalone `<svg>` out —
//! bar, line, donut, or sparkline. Axes and labels are drawn in `currentColor`
//! (so an embedded chart follows the page's light/dark theme); data uses a
//! palette or a caller-supplied per-slice color. Pure compute, no host imports.
//!
//! ponytail: single-series only, which is the common report shape. Multi-series
//! / stacked / rich gridlines is a much bigger renderer and out of scope.

#[allow(warnings)]
mod bindings;

use std::f64::consts::PI;

use bindings::exports::svg::chart::charts::{Chart, Guest, Kind, Slice};

struct Component;

const PALETTE: &[&str] = &[
    "#6366f1", "#22c55e", "#f97316", "#06b6d4", "#ec4899", "#eab308", "#a855f7", "#14b8a6",
    "#ef4444", "#3b82f6",
];

fn color_of(s: &Slice, i: usize) -> String {
    if s.color.is_empty() {
        PALETTE[i % PALETTE.len()].to_string()
    } else {
        s.color.clone()
    }
}

/// Format a value: integers bare, else up to 2 decimals with trailing zeros trimmed.
fn fmt(v: f64) -> String {
    if v.is_finite() && v == v.round() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{:.2}", v);
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// One decimal for coordinates (compact SVG).
fn n(x: f64) -> String {
    format!("{:.1}", x)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}

fn open(w: f64, h: f64) -> String {
    // Explicit width/height give the SVG an intrinsic size (so it doesn't
    // collapse when embedded); the viewBox keeps it scalable via CSS.
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" viewBox=\"0 0 {} {}\" font-family=\"ui-sans-serif,system-ui,sans-serif\" font-size=\"11\" fill=\"currentColor\">",
        n(w), n(h), n(w), n(h)
    )
}

fn title_el(title: &str, w: f64) -> String {
    if title.is_empty() {
        String::new()
    } else {
        format!(
            "<text x=\"{}\" y=\"18\" text-anchor=\"middle\" font-size=\"13\" font-weight=\"700\">{}</text>",
            n(w / 2.0),
            esc(title)
        )
    }
}

fn max_val(data: &[Slice]) -> f64 {
    data.iter().map(|s| s.value).fold(0.0_f64, f64::max).max(1.0)
}

fn bar(c: &Chart, w: f64, h: f64) -> String {
    let (ml, mr, mt, mb) = (40.0, 12.0, if c.title.is_empty() { 14.0 } else { 30.0 }, 34.0);
    let (pw, ph) = (w - ml - mr, h - mt - mb);
    let base = mt + ph;
    let max = max_val(&c.data);
    let ncols = c.data.len().max(1) as f64;
    let bwidth = pw / ncols;
    let bar_w = bwidth * 0.62;

    let mut s = open(w, h) + &title_el(&c.title, w);
    // baseline
    s += &format!(
        "<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke=\"currentColor\" stroke-opacity=\"0.3\"/>",
        n(ml), n(base), n(ml + pw), n(base)
    );
    for (i, sl) in c.data.iter().enumerate() {
        let bh = (sl.value.max(0.0) / max) * ph;
        let x = ml + i as f64 * bwidth + (bwidth - bar_w) / 2.0;
        let y = base - bh;
        s += &format!(
            "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"2\" fill=\"{}\"/>",
            n(x),
            n(y),
            n(bar_w),
            n(bh),
            color_of(sl, i)
        );
        s += &format!(
            "<text x=\"{}\" y=\"{}\" text-anchor=\"middle\" font-size=\"10\">{}</text>",
            n(x + bar_w / 2.0),
            n(y - 3.0),
            fmt(sl.value)
        );
        s += &format!(
            "<text x=\"{}\" y=\"{}\" text-anchor=\"middle\" font-size=\"10\" fill-opacity=\"0.7\">{}</text>",
            n(x + bar_w / 2.0), n(base + 13.0), esc(&truncate(&sl.label, 7))
        );
    }
    s + "</svg>"
}

fn line(c: &Chart, w: f64, h: f64) -> String {
    let (ml, mr, mt, mb) = (40.0, 12.0, if c.title.is_empty() { 14.0 } else { 30.0 }, 34.0);
    let (pw, ph) = (w - ml - mr, h - mt - mb);
    let base = mt + ph;
    let max = max_val(&c.data);
    let ncols = c.data.len();
    let xat = |i: usize| {
        if ncols <= 1 {
            ml + pw / 2.0
        } else {
            ml + (i as f64 / (ncols - 1) as f64) * pw
        }
    };
    let yat = |v: f64| base - (v.max(0.0) / max) * ph;

    let mut s = open(w, h) + &title_el(&c.title, w);
    s += &format!(
        "<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke=\"currentColor\" stroke-opacity=\"0.3\"/>",
        n(ml), n(base), n(ml + pw), n(base)
    );
    let pts: Vec<String> = c
        .data
        .iter()
        .enumerate()
        .map(|(i, sl)| format!("{},{}", n(xat(i)), n(yat(sl.value))))
        .collect();
    if !pts.is_empty() {
        s += &format!(
            "<polyline points=\"{}\" fill=\"none\" stroke=\"{}\" stroke-width=\"2\"/>",
            pts.join(" "),
            PALETTE[0]
        );
    }
    for (i, sl) in c.data.iter().enumerate() {
        s += &format!(
            "<circle cx=\"{}\" cy=\"{}\" r=\"3\" fill=\"{}\"/>",
            n(xat(i)),
            n(yat(sl.value)),
            PALETTE[0]
        );
        s += &format!(
            "<text x=\"{}\" y=\"{}\" text-anchor=\"middle\" font-size=\"10\" fill-opacity=\"0.7\">{}</text>",
            n(xat(i)), n(base + 13.0), esc(&truncate(&sl.label, 6))
        );
    }
    s + "</svg>"
}

fn donut(c: &Chart, w: f64, h: f64) -> String {
    let mt = if c.title.is_empty() { 12.0 } else { 30.0 };
    let total: f64 = c.data.iter().map(|s| s.value.max(0.0)).sum();
    let legend_h = c.data.len() as f64 * 18.0;
    let dia = (w.min(h - mt - legend_h)).max(60.0);
    let cx = w / 2.0;
    let cy = mt + dia / 2.0;
    let outer = dia / 2.0 * 0.92;
    let inner = outer * 0.58;
    let rmid = (outer + inner) / 2.0;
    let sw = outer - inner;
    let circ = 2.0 * PI * rmid;

    let mut s = open(w, h) + &title_el(&c.title, w);
    if total > 0.0 {
        let mut acc = 0.0;
        for (i, sl) in c.data.iter().enumerate() {
            let frac = sl.value.max(0.0) / total;
            let len = frac * circ;
            s += &format!(
                "<circle cx=\"{}\" cy=\"{}\" r=\"{}\" fill=\"none\" stroke=\"{}\" stroke-width=\"{}\" stroke-dasharray=\"{} {}\" stroke-dashoffset=\"{}\" transform=\"rotate(-90 {} {})\"/>",
                n(cx), n(cy), n(rmid), color_of(sl, i), n(sw), n(len), n(circ - len), n(-acc), n(cx), n(cy)
            );
            acc += len;
        }
        s += &format!(
            "<text x=\"{}\" y=\"{}\" text-anchor=\"middle\" dominant-baseline=\"central\" font-size=\"15\" font-weight=\"700\">{}</text>",
            n(cx), n(cy), fmt(total)
        );
    }
    // legend below.
    let mut ly = mt + dia + 4.0;
    for (i, sl) in c.data.iter().enumerate() {
        s += &format!(
            "<rect x=\"12\" y=\"{}\" width=\"11\" height=\"11\" rx=\"2\" fill=\"{}\"/>",
            n(ly),
            color_of(sl, i)
        );
        s += &format!(
            "<text x=\"29\" y=\"{}\" font-size=\"11\">{} · {}</text>",
            n(ly + 9.5),
            esc(&truncate(&sl.label, 22)),
            fmt(sl.value)
        );
        ly += 18.0;
    }
    s + "</svg>"
}

fn sparkline(c: &Chart, w: f64, h: f64) -> String {
    let pad = 3.0;
    let (pw, ph) = (w - pad * 2.0, h - pad * 2.0);
    let ncols = c.data.len();
    let vals: Vec<f64> = c.data.iter().map(|s| s.value).collect();
    let (mn, mx) =
        vals.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &v| (a.min(v), b.max(v)));
    let span = (mx - mn).max(1e-9);
    let xat = |i: usize| {
        if ncols <= 1 {
            pad + pw / 2.0
        } else {
            pad + (i as f64 / (ncols - 1) as f64) * pw
        }
    };
    let yat = |v: f64| pad + ph - ((v - mn) / span) * ph;

    let mut s = open(w, h);
    let pts: Vec<String> = c
        .data
        .iter()
        .enumerate()
        .map(|(i, sl)| format!("{},{}", n(xat(i)), n(yat(sl.value))))
        .collect();
    if !pts.is_empty() {
        s += &format!(
            "<polyline points=\"{}\" fill=\"none\" stroke=\"{}\" stroke-width=\"1.5\"/>",
            pts.join(" "),
            PALETTE[0]
        );
        if let Some(last) = c.data.last() {
            s += &format!(
                "<circle cx=\"{}\" cy=\"{}\" r=\"2\" fill=\"{}\"/>",
                n(xat(ncols - 1)),
                n(yat(last.value)),
                PALETTE[0]
            );
        }
    }
    s + "</svg>"
}

impl Guest for Component {
    fn render(c: Chart) -> String {
        let (dw, dh) = match c.kind {
            Kind::Sparkline => (160.0, 40.0),
            Kind::Donut => (320.0, 170.0 + c.data.len() as f64 * 18.0),
            _ => (360.0, 220.0),
        };
        let w = if c.width == 0 { dw } else { c.width as f64 };
        let h = if c.height == 0 { dh } else { c.height as f64 };
        match c.kind {
            Kind::Bar => bar(&c, w, h),
            Kind::Line => line(&c, w, h),
            Kind::Donut => donut(&c, w, h),
            Kind::Sparkline => sparkline(&c, w, h),
        }
    }
}

bindings::export!(Component with_types_in bindings);

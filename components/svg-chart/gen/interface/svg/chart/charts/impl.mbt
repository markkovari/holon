///|
const PI : Double = 3.14159265358979323846

///|
let palette : Array[String] = [
  "#6366f1", "#22c55e", "#f97316", "#06b6d4", "#ec4899", "#eab308", "#a855f7", "#14b8a6",
  "#ef4444", "#3b82f6",
]

///|
fn color_of(s : Slice, i : Int) -> String {
  if s.color.length() == 0 {
    palette[i % palette.length()]
  } else {
    s.color
  }
}

///|
fn n(x : Double) -> String {
  let rounded = (x * 10.0 + (if x >= 0.0 { 0.5 } else { -0.5 })).to_int64()
  let whole = rounded / 10
  let frac = (rounded % 10).abs()
  if whole == 0 && rounded < 0 {
    "-0.\{frac}"
  } else {
    "\{whole}.\{frac}"
  }
}

///|
fn fmt(v : Double) -> String {
  let v_round = (v + (if v >= 0.0 { 0.5 } else { -0.5 })).to_int64().to_double()
  if v == v_round && v.abs() < 1.0e15 {
    return "\{v.to_int64()}"
  }
  let rounded = (v * 100.0 + (if v >= 0.0 { 0.5 } else { -0.5 })).to_int64()
  let whole = rounded / 100
  let frac = (rounded % 100).abs()
  let frac_str = if frac < 10 { "0\{frac}" } else { "\{frac}" }
  let mut s = if whole == 0 && rounded < 0 {
    "-0.\{frac_str}"
  } else {
    "\{whole}.\{frac_str}"
  }
  while s.length() > 0 && s[s.length() - 1] == '0' {
    s = s[0:s.length() - 1].to_owned()
  }
  if s.length() > 0 && s[s.length() - 1] == '.' {
    s = s[0:s.length() - 1].to_owned()
  }
  s
}

///|
fn esc(s : String) -> String {
  let buf = StringBuilder()
  for c in s {
    match c {
      '&' => buf.write_string("&amp;")
      '<' => buf.write_string("&lt;")
      '>' => buf.write_string("&gt;")
      _ => buf.write_char(c)
    }
  }
  buf.to_string()
}

///|
fn truncate(s : String, max : Int) -> String {
  if s.length() <= max {
    s
  } else {
    let limit = if max > 1 { max - 1 } else { 0 }
    s[0:limit].to_owned() + "…"
  }
}

///|
fn open_svg(w : Double, h : Double) -> String {
  "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"" + n(w) + "\" height=\"" + n(h) + "\" viewBox=\"0 0 " + n(w) + " " + n(h) + "\" font-family=\"ui-sans-serif,system-ui,sans-serif\" font-size=\"11\" fill=\"currentColor\">"
}

///|
fn title_el(title : String, w : Double) -> String {
  if title.length() == 0 {
    ""
  } else {
    "<text x=\"" + n(w / 2.0) + "\" y=\"18\" text-anchor=\"middle\" font-size=\"13\" font-weight=\"700\">" + esc(title) + "</text>"
  }
}

///|
fn max_val(data : Array[Slice]) -> Double {
  let mut m = 0.0
  for sl in data {
    if sl.value > m {
      m = sl.value
    }
  }
  if m < 1.0 { 1.0 } else { m }
}

///|
fn bar(c : Chart, w : Double, h : Double) -> String {
  let (ml, mr, mt, mb) = (40.0, 12.0, if c.title.length() == 0 { 14.0 } else { 30.0 }, 34.0)
  let (pw, ph) = (w - ml - mr, h - mt - mb)
  let base = mt + ph
  let max = max_val(c.data)
  let ncols = (if c.data.length() > 0 { c.data.length() } else { 1 }).to_double()
  let bwidth = pw / ncols
  let bar_w = bwidth * 0.62

  let buf = StringBuilder()
  buf.write_string(open_svg(w, h))
  buf.write_string(title_el(c.title, w))
  buf.write_string("<line x1=\"" + n(ml) + "\" y1=\"" + n(base) + "\" x2=\"" + n(ml + pw) + "\" y2=\"" + n(base) + "\" stroke=\"currentColor\" stroke-opacity=\"0.3\"/>")

  for i = 0; i < c.data.length(); i = i + 1 {
    let sl = c.data[i]
    let val = if sl.value > 0.0 { sl.value } else { 0.0 }
    let bh = (val / max) * ph
    let x = ml + i.to_double() * bwidth + (bwidth - bar_w) / 2.0
    let y = base - bh
    buf.write_string("<rect x=\"" + n(x) + "\" y=\"" + n(y) + "\" width=\"" + n(bar_w) + "\" height=\"" + n(bh) + "\" rx=\"2\" fill=\"" + color_of(sl, i) + "\"/>")
    buf.write_string("<text x=\"" + n(x + bar_w / 2.0) + "\" y=\"" + n(y - 3.0) + "\" text-anchor=\"middle\" font-size=\"10\">" + fmt(sl.value) + "</text>")
    buf.write_string("<text x=\"" + n(x + bar_w / 2.0) + "\" y=\"" + n(base + 13.0) + "\" text-anchor=\"middle\" font-size=\"10\" fill-opacity=\"0.7\">" + esc(truncate(sl.label, 7)) + "</text>")
  }
  buf.write_string("</svg>")
  buf.to_string()
}

///|
fn line(c : Chart, w : Double, h : Double) -> String {
  let (ml, mr, mt, mb) = (40.0, 12.0, if c.title.length() == 0 { 14.0 } else { 30.0 }, 34.0)
  let (pw, ph) = (w - ml - mr, h - mt - mb)
  let base = mt + ph
  let max = max_val(c.data)
  let ncols = c.data.length()
  fn xat(i : Int) -> Double {
    if ncols <= 1 {
      ml + pw / 2.0
    } else {
      ml + (i.to_double() / (ncols - 1).to_double()) * pw
    }
  }
  fn yat(v : Double) -> Double {
    let val = if v > 0.0 { v } else { 0.0 }
    base - (val / max) * ph
  }

  let buf = StringBuilder()
  buf.write_string(open_svg(w, h))
  buf.write_string(title_el(c.title, w))
  buf.write_string("<line x1=\"" + n(ml) + "\" y1=\"" + n(base) + "\" x2=\"" + n(ml + pw) + "\" y2=\"" + n(base) + "\" stroke=\"currentColor\" stroke-opacity=\"0.3\"/>")

  let pts = StringBuilder()
  for i = 0; i < ncols; i = i + 1 {
    if i > 0 {
      pts.write_string(" ")
    }
    pts.write_string(n(xat(i)) + "," + n(yat(c.data[i].value)))
  }
  if ncols > 0 {
    buf.write_string("<polyline points=\"" + pts.to_string() + "\" fill=\"none\" stroke=\"" + palette[0] + "\" stroke-width=\"2\"/>")
  }
  for i = 0; i < ncols; i = i + 1 {
    let sl = c.data[i]
    buf.write_string("<circle cx=\"" + n(xat(i)) + "\" cy=\"" + n(yat(sl.value)) + "\" r=\"3\" fill=\"" + palette[0] + "\"/>")
    buf.write_string("<text x=\"" + n(xat(i)) + "\" y=\"" + n(base + 13.0) + "\" text-anchor=\"middle\" font-size=\"10\" fill-opacity=\"0.7\">" + esc(truncate(sl.label, 6)) + "</text>")
  }
  buf.write_string("</svg>")
  buf.to_string()
}

///|
fn donut(c : Chart, w : Double, h : Double) -> String {
  let mt = if c.title.length() == 0 { 12.0 } else { 30.0 }
  let mut total = 0.0
  for sl in c.data {
    if sl.value > 0.0 {
      total += sl.value
    }
  }
  let legend_h = c.data.length().to_double() * 18.0
  let mut dia = w
  if h - mt - legend_h < dia {
    dia = h - mt - legend_h
  }
  if dia < 60.0 {
    dia = 60.0
  }
  let cx = w / 2.0
  let cy = mt + dia / 2.0
  let outer = dia / 2.0 * 0.92
  let inner = outer * 0.58
  let rmid = (outer + inner) / 2.0
  let sw = outer - inner
  let circ = 2.0 * PI * rmid

  let buf = StringBuilder()
  buf.write_string(open_svg(w, h))
  buf.write_string(title_el(c.title, w))
  if total > 0.0 {
    let mut acc = 0.0
    for i = 0; i < c.data.length(); i = i + 1 {
      let sl = c.data[i]
      let val = if sl.value > 0.0 { sl.value } else { 0.0 }
      let frac = val / total
      let len = frac * circ
      buf.write_string("<circle cx=\"" + n(cx) + "\" cy=\"" + n(cy) + "\" r=\"" + n(rmid) + "\" fill=\"none\" stroke=\"" + color_of(sl, i) + "\" stroke-width=\"" + n(sw) + "\" stroke-dasharray=\"" + n(len) + " " + n(circ - len) + "\" stroke-dashoffset=\"" + n(-acc) + "\" transform=\"rotate(-90 " + n(cx) + " " + n(cy) + ")\"/>")
      acc += len
    }
    buf.write_string("<text x=\"" + n(cx) + "\" y=\"" + n(cy) + "\" text-anchor=\"middle\" dominant-baseline=\"central\" font-size=\"15\" font-weight=\"700\">" + fmt(total) + "</text>")
  }
  let mut ly = mt + dia + 4.0
  for i = 0; i < c.data.length(); i = i + 1 {
    let sl = c.data[i]
    buf.write_string("<rect x=\"12\" y=\"" + n(ly) + "\" width=\"11\" height=\"11\" rx=\"2\" fill=\"" + color_of(sl, i) + "\"/>")
    buf.write_string("<text x=\"29\" y=\"" + n(ly + 9.5) + "\" font-size=\"11\">" + esc(truncate(sl.label, 22)) + " · " + fmt(sl.value) + "</text>")
    ly += 18.0
  }
  buf.write_string("</svg>")
  buf.to_string()
}

///|
fn sparkline(c : Chart, w : Double, h : Double) -> String {
  let pad = 3.0
  let (pw, ph) = (w - pad * 2.0, h - pad * 2.0)
  let ncols = c.data.length()
  let mut mn = 1.0e300
  let mut mx = -1.0e300
  for sl in c.data {
    if sl.value < mn {
      mn = sl.value
    }
    if sl.value > mx {
      mx = sl.value
    }
  }
  let mut span = mx - mn
  if span < 1.0e-9 {
    span = 1.0e-9
  }
  fn xat(i : Int) -> Double {
    if ncols <= 1 {
      pad + pw / 2.0
    } else {
      pad + (i.to_double() / (ncols - 1).to_double()) * pw
    }
  }
  fn yat(v : Double) -> Double {
    pad + ph - ((v - mn) / span) * ph
  }

  let buf = StringBuilder()
  buf.write_string(open_svg(w, h))
  let pts = StringBuilder()
  for i = 0; i < ncols; i = i + 1 {
    if i > 0 {
      pts.write_string(" ")
    }
    pts.write_string(n(xat(i)) + "," + n(yat(c.data[i].value)))
  }
  if ncols > 0 {
    buf.write_string("<polyline points=\"" + pts.to_string() + "\" fill=\"none\" stroke=\"" + palette[0] + "\" stroke-width=\"1.5\"/>")
    let last = c.data[ncols - 1]
    buf.write_string("<circle cx=\"" + n(xat(ncols - 1)) + "\" cy=\"" + n(yat(last.value)) + "\" r=\"2\" fill=\"" + palette[0] + "\"/>")
  }
  buf.write_string("</svg>")
  buf.to_string()
}

///|
pub fn render(c : Chart) -> String {
  let (dw, dh) = match c.kind {
    SPARKLINE => (160.0, 40.0)
    DONUT => (320.0, 170.0 + c.data.length().to_double() * 18.0)
    _ => (360.0, 220.0)
  }
  let w = if c.width == 0U { dw } else { c.width.reinterpret_as_int().to_double() }
  let h = if c.height == 0U { dh } else { c.height.reinterpret_as_int().to_double() }
  match c.kind {
    BAR => bar(c, w, h)
    LINE => line(c, w, h)
    DONUT => donut(c, w, h)
    SPARKLINE => sparkline(c, w, h)
  }
}

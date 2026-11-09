// CRDT (CvRDT) merger and state operations.

priv struct LwwState {
  v : @json.Json
  ts : UInt64
  replica : String
}

priv struct PnState {
  p : Map[String, UInt64]
  n : Map[String, UInt64]
}

priv struct OrSetState {
  adds : Map[String, Array[String]]
  removes : Array[String]
}

priv struct LwwReg {
  v : @json.Json?
  ts : UInt64
  replica : String
}

priv struct LwwMapState {
  entries : Map[String, LwwReg]
}

priv struct RgaElem {
  ch : String
  after : String
  mut del : Bool
}

priv struct RgaState {
  elems : Map[String, RgaElem]
}

priv enum CrdtState {
  Lww(LwwState)
  Pn(PnState)
  OrSet(OrSetState)
  LwwMap(LwwMapState)
  Rga(RgaState)
}

fn escape_str(s : String) -> String {
  let buf = StringBuilder()
  buf.write_char('"')
  for c in s {
    match c {
      '"' => buf.write_string("\\\"")
      '\\' => buf.write_string("\\\\")
      '\n' => buf.write_string("\\n")
      '\r' => buf.write_string("\\r")
      '\t' => buf.write_string("\\t")
      _ => {
        let code = c.to_int()
        if code < 32 {
          buf.write_string("\\u00")
          let h1 = code / 16
          let h2 = code % 16
          let hex_c = fn(n : Int) -> Char {
            if n < 10 {
              (n + 48).unsafe_to_char()
            } else {
              (n - 10 + 97).unsafe_to_char()
            }
          }
          buf.write_char(hex_c(h1))
          buf.write_char(hex_c(h2))
        } else {
          buf.write_char(c)
        }
      }
    }
  }
  buf.write_char('"')
  buf.to_string()
}

fn canon_json(j : @json.Json) -> String {
  match j {
    Null => "null"
    True => "true"
    False => "false"
    Number(n, repr~) =>
      match repr {
        Some(r) => r
        None =>
          if n.to_int64().to_double() == n {
            n.to_int64().to_string()
          } else {
            n.to_string()
          }
      }
    String(s) => escape_str(s)
    Array(arr) => {
      let buf = StringBuilder()
      buf.write_char('[')
      for i = 0; i < arr.length(); i = i + 1 {
        if i > 0 {
          buf.write_char(',')
        }
        buf.write_string(canon_json(arr[i]))
      }
      buf.write_char(']')
      buf.to_string()
    }
    Object(map) => {
      let keys : Array[String] = []
      for k in map.keys() {
        keys.push(k)
      }
      sort_strings_lex(keys)
      let buf = StringBuilder()
      buf.write_char('{')
      for i = 0; i < keys.length(); i = i + 1 {
        if i > 0 {
          buf.write_char(',')
        }
        let k = keys[i]
        buf.write_string(escape_str(k))
        buf.write_char(':')
        buf.write_string(canon_json(map[k]))
      }
      buf.write_char('}')
      buf.to_string()
    }
  }
}

fn cmp_str(a : String, b : String) -> Int {
  let len_a = a.length()
  let len_b = b.length()
  let min_len = if len_a < len_b { len_a } else { len_b }
  for i = 0; i < min_len; i = i + 1 {
    let ca = a[i].to_int()
    let cb = b[i].to_int()
    if ca != cb {
      return if ca < cb { -1 } else { 1 }
    }
  }
  if len_a != len_b {
    return if len_a < len_b { -1 } else { 1 }
  }
  0
}

fn sort_strings_lex(arr : Array[String]) -> Unit {
  let len = arr.length()
  for i = 1; i < len; i = i + 1 {
    let key = arr[i]
    let mut j = i - 1
    while j >= 0 && cmp_str(arr[j], key) > 0 {
      arr[j + 1] = arr[j]
      j = j - 1
    }
    arr[j + 1] = key
  }
}

fn pad4(n : Int) -> String {
  let s = n.to_string()
  let len = s.length()
  if len >= 4 {
    s
  } else if len == 1 {
    "000" + s
  } else if len == 2 {
    "00" + s
  } else {
    "0" + s
  }
}

fn dump(st : CrdtState) -> String {
  match st {
    Lww(lww) =>
      "{\"type\":\"lww\",\"v\":" +
      canon_json(lww.v) +
      ",\"ts\":" +
      lww.ts.to_string() +
      ",\"replica\":" +
      escape_str(lww.replica) +
      "}"
    Pn(pn) => {
      let p_keys : Array[String] = []
      for k in pn.p.keys() {
        p_keys.push(k)
      }
      sort_strings_lex(p_keys)
      let buf = StringBuilder()
      buf.write_string("{\"type\":\"pn\",\"p\":{")
      for i = 0; i < p_keys.length(); i = i + 1 {
        if i > 0 {
          buf.write_char(',')
        }
        let k = p_keys[i]
        buf.write_string(escape_str(k))
        buf.write_char(':')
        buf.write_string(pn.p[k].to_string())
      }
      buf.write_string("},\"n\":{")
      let n_keys : Array[String] = []
      for k in pn.n.keys() {
        n_keys.push(k)
      }
      sort_strings_lex(n_keys)
      for i = 0; i < n_keys.length(); i = i + 1 {
        if i > 0 {
          buf.write_char(',')
        }
        let k = n_keys[i]
        buf.write_string(escape_str(k))
        buf.write_char(':')
        buf.write_string(pn.n[k].to_string())
      }
      buf.write_string("}}")
      buf.to_string()
    }
    OrSet(orset) => {
      let add_keys : Array[String] = []
      for k in orset.adds.keys() {
        add_keys.push(k)
      }
      sort_strings_lex(add_keys)
      let buf = StringBuilder()
      buf.write_string("{\"type\":\"orset\",\"adds\":{")
      for i = 0; i < add_keys.length(); i = i + 1 {
        if i > 0 {
          buf.write_char(',')
        }
        let k = add_keys[i]
        buf.write_string(escape_str(k))
        buf.write_string(":[")
        let tags = orset.adds[k].copy()
        sort_strings_lex(tags)
        for j = 0; j < tags.length(); j = j + 1 {
          if j > 0 {
            buf.write_char(',')
          }
          buf.write_string(escape_str(tags[j]))
        }
        buf.write_char(']')
      }
      buf.write_string("},\"removes\":[")
      let rems = orset.removes.copy()
      sort_strings_lex(rems)
      for i = 0; i < rems.length(); i = i + 1 {
        if i > 0 {
          buf.write_char(',')
        }
        buf.write_string(escape_str(rems[i]))
      }
      buf.write_string("]}")
      buf.to_string()
    }
    LwwMap(lwwmap) => {
      let keys : Array[String] = []
      for k in lwwmap.entries.keys() {
        keys.push(k)
      }
      sort_strings_lex(keys)
      let buf = StringBuilder()
      buf.write_string("{\"type\":\"lwwmap\",\"entries\":{")
      for i = 0; i < keys.length(); i = i + 1 {
        if i > 0 {
          buf.write_char(',')
        }
        let k = keys[i]
        let reg = lwwmap.entries[k]
        buf.write_string(escape_str(k))
        buf.write_string(":{\"v\":")
        match reg.v {
          Some(v) => buf.write_string(canon_json(v))
          None => buf.write_string("null")
        }
        buf.write_string(",\"ts\":")
        buf.write_string(reg.ts.to_string())
        buf.write_string(",\"replica\":")
        buf.write_string(escape_str(reg.replica))
        buf.write_char('}')
      }
      buf.write_string("}}")
      buf.to_string()
    }
    Rga(rga) => {
      let ids : Array[String] = []
      for id in rga.elems.keys() {
        ids.push(id)
      }
      sort_strings_lex(ids)
      let buf = StringBuilder()
      buf.write_string("{\"type\":\"rga\",\"elems\":{")
      for i = 0; i < ids.length(); i = i + 1 {
        if i > 0 {
          buf.write_char(',')
        }
        let id = ids[i]
        let el = rga.elems[id]
        buf.write_string(escape_str(id))
        buf.write_string(":{\"ch\":")
        buf.write_string(escape_str(el.ch))
        buf.write_string(",\"after\":")
        buf.write_string(escape_str(el.after))
        buf.write_string(",\"del\":")
        buf.write_string(if el.del { "true" } else { "false" })
        buf.write_char('}')
      }
      buf.write_string("}}")
      buf.to_string()
    }
  }
}

fn parse_u64(j : @json.Json) -> UInt64? {
  match j {
    Number(n, ..) => Some(n.to_int64().reinterpret_as_uint64())
    _ => None
  }
}

fn parse_state(s : String, what : String) -> Result[CrdtState, CrdtError] {
  let root = @json.parse(s) catch {
    _ => return Err(InvalidJson("\{what}: invalid JSON"))
  }
  let map = match root {
    Object(m) => m
    _ => return Err(InvalidState("\{what}: expected JSON object"))
  }
  let type_str = match map.get("type") {
    Some(String(t)) => t
    _ => return Err(InvalidState("\{what}: missing or invalid 'type' field"))
  }
  match type_str {
    "lww" => {
      let v = match map.get("v") {
        Some(val) => val
        None => return Err(InvalidState("\{what}: lww missing 'v'"))
      }
      let ts = match map.get("ts") {
        Some(t) =>
          match parse_u64(t) {
            Some(u) => u
            None => return Err(InvalidState("\{what}: lww invalid 'ts'"))
          }
        None => return Err(InvalidState("\{what}: lww missing 'ts'"))
      }
      let replica = match map.get("replica") {
        Some(String(r)) => r
        _ => return Err(InvalidState("\{what}: lww missing 'replica'"))
      }
      Ok(Lww({ v, ts, replica }))
    }
    "pn" => {
      let p_map : Map[String, UInt64] = Map([])
      match map.get("p") {
        Some(Object(pm)) =>
          for k in pm.keys() {
            match parse_u64(pm[k]) {
              Some(val) => p_map[k] = val
              None => ()
            }
          }
        _ => ()
      }
      let n_map : Map[String, UInt64] = Map([])
      match map.get("n") {
        Some(Object(nm)) =>
          for k in nm.keys() {
            match parse_u64(nm[k]) {
              Some(val) => n_map[k] = val
              None => ()
            }
          }
        _ => ()
      }
      Ok(Pn({ p: p_map, n: n_map }))
    }
    "orset" => {
      let adds_map : Map[String, Array[String]] = Map([])
      match map.get("adds") {
        Some(Object(am)) =>
          for k in am.keys() {
            match am[k] {
              Array(tags) => {
                let tag_arr : Array[String] = []
                for t in tags {
                  match t {
                    String(ts) => tag_arr.push(ts)
                    _ => ()
                  }
                }
                adds_map[k] = tag_arr
              }
              _ => ()
            }
          }
        _ => ()
      }
      let removes_arr : Array[String] = []
      match map.get("removes") {
        Some(Array(rm)) =>
          for t in rm {
            match t {
              String(ts) => removes_arr.push(ts)
              _ => ()
            }
          }
        _ => ()
      }
      Ok(OrSet({ adds: adds_map, removes: removes_arr }))
    }
    "lwwmap" => {
      let entries : Map[String, LwwReg] = Map([])
      match map.get("entries") {
        Some(Object(em)) =>
          for k in em.keys() {
            match em[k] {
              Object(rm) => {
                let v = match rm.get("v") {
                  Some(Null) => None
                  Some(val) => Some(val)
                  None => None
                }
                let ts = match rm.get("ts") {
                  Some(t) => parse_u64(t).unwrap_or(0UL)
                  None => 0UL
                }
                let replica = match rm.get("replica") {
                  Some(String(r)) => r
                  _ => ""
                }
                entries[k] = { v, ts, replica }
              }
              _ => ()
            }
          }
        _ => ()
      }
      Ok(LwwMap({ entries, }))
    }
    "rga" => {
      let elems : Map[String, RgaElem] = Map([])
      match map.get("elems") {
        Some(Object(em)) =>
          for id in em.keys() {
            match em[id] {
              Object(elm) => {
                let ch = match elm.get("ch") {
                  Some(String(c)) => c
                  _ => ""
                }
                let after = match elm.get("after") {
                  Some(String(a)) => a
                  _ => ""
                }
                let del = match elm.get("del") {
                  Some(True) => true
                  Some(False) => false
                  _ => false
                }
                elems[id] = { ch, after, del }
              }
              _ => ()
            }
          }
        _ => ()
      }
      Ok(Rga({ elems, }))
    }
    _ => Err(InvalidState("\{what}: unrecognized CRDT type: \{type_str}"))
  }
}

fn compare_stamp(
  ts1 : UInt64,
  rep1 : String,
  has_v1 : Int,
  canon1 : String,
  ts2 : UInt64,
  rep2 : String,
  has_v2 : Int,
  canon2 : String,
) -> Int {
  if ts1 != ts2 {
    return if ts1 > ts2 { 1 } else { -1 }
  }
  let c_rep = cmp_str(rep1, rep2)
  if c_rep != 0 {
    return c_rep
  }
  if has_v1 != has_v2 {
    return if has_v1 > has_v2 { 1 } else { -1 }
  }
  let c_val = cmp_str(canon1, canon2)
  if c_val != 0 {
    return c_val
  }
  0
}

fn rga_sequence(elems : Map[String, RgaElem]) -> Array[String] {
  let children : Map[String, Array[String]] = Map([])
  for id in elems.keys() {
    let e = elems[id]
    match children.get(e.after) {
      Some(kids) => kids.push(id)
      None => children[e.after] = [id]
    }
  }
  for k in children.keys() {
    let kids = children[k]
    sort_strings_lex(kids)
  }

  let out : Array[String] = []
  let stack : Array[String] = match children.get("") {
    Some(root_kids) => {
      let rk = root_kids.copy()
      sort_strings_lex(rk)
      rk
    }
    None => []
  }

  while stack.length() > 0 {
    let id = stack.pop().unwrap()
    out.push(id)
    match children.get(id) {
      Some(kids) =>
        for i = 0; i < kids.length(); i = i + 1 {
          stack.push(kids[i])
        }
      None => ()
    }
  }
  out
}

fn rga_visible(elems : Map[String, RgaElem]) -> Array[String] {
  let seq = rga_sequence(elems)
  let visible : Array[String] = []
  for id in seq {
    match elems.get(id) {
      Some(e) => if !e.del { visible.push(id) }
      None => ()
    }
  }
  visible
}

fn rga_text(elems : Map[String, RgaElem]) -> String {
  let vis = rga_visible(elems)
  let buf = StringBuilder()
  for id in vis {
    match elems.get(id) {
      Some(e) => buf.write_string(e.ch)
      None => ()
    }
  }
  buf.to_string()
}

fn value_of(st : CrdtState) -> String {
  match st {
    Lww(lww) => canon_json(lww.v)
    Pn(pn) => {
      let mut sp : Int64 = 0L
      for k in pn.p.keys() {
        sp = sp + pn.p[k].reinterpret_as_int64()
      }
      let mut sn : Int64 = 0L
      for k in pn.n.keys() {
        sn = sn + pn.n[k].reinterpret_as_int64()
      }
      (sp - sn).to_string()
    }
    OrSet(orset) => {
      let rem_set : Map[String, Bool] = Map([])
      for r in orset.removes {
        rem_set[r] = true
      }
      let present_keys : Array[String] = []
      for el in orset.adds.keys() {
        let tags = orset.adds[el]
        let mut has_live = false
        for t in tags {
          if rem_set.get(t) is None {
            has_live = true
            break
          }
        }
        if has_live {
          present_keys.push(el)
        }
      }
      sort_strings_lex(present_keys)
      let buf = StringBuilder()
      buf.write_char('[')
      for i = 0; i < present_keys.length(); i = i + 1 {
        if i > 0 {
          buf.write_char(',')
        }
        buf.write_string(escape_str(present_keys[i]))
      }
      buf.write_char(']')
      buf.to_string()
    }
    LwwMap(lwwmap) => {
      let live_keys : Array[String] = []
      for k in lwwmap.entries.keys() {
        let reg = lwwmap.entries[k]
        if reg.v is Some(_) {
          live_keys.push(k)
        }
      }
      sort_strings_lex(live_keys)
      let buf = StringBuilder()
      buf.write_char('{')
      for i = 0; i < live_keys.length(); i = i + 1 {
        if i > 0 {
          buf.write_char(',')
        }
        let k = live_keys[i]
        buf.write_string(escape_str(k))
        buf.write_char(':')
        buf.write_string(canon_json(lwwmap.entries[k].v.unwrap()))
      }
      buf.write_char('}')
      buf.to_string()
    }
    Rga(rga) => escape_str(rga_text(rga.elems))
  }
}

fn merge_states(a : CrdtState, b : CrdtState) -> CrdtState? {
  match (a, b) {
    (Lww(la), Lww(lb)) => {
      let cmp = compare_stamp(
        lb.ts,
        lb.replica,
        1,
        canon_json(lb.v),
        la.ts,
        la.replica,
        1,
        canon_json(la.v),
      )
      if cmp > 0 {
        Some(Lww(lb))
      } else {
        Some(Lww(la))
      }
    }
    (Pn(pa), Pn(pb)) => {
      let p : Map[String, UInt64] = Map([])
      for k in pa.p.keys() {
        p[k] = pa.p[k]
      }
      for k in pb.p.keys() {
        let v = pb.p[k]
        match p.get(k) {
          Some(cur) => if v > cur { p[k] = v }
          None => p[k] = v
        }
      }
      let n : Map[String, UInt64] = Map([])
      for k in pa.n.keys() {
        n[k] = pa.n[k]
      }
      for k in pb.n.keys() {
        let v = pb.n[k]
        match n.get(k) {
          Some(cur) => if v > cur { n[k] = v }
          None => n[k] = v
        }
      }
      Some(Pn({ p, n }))
    }
    (OrSet(oa), OrSet(ob)) => {
      let adds : Map[String, Array[String]] = Map([])
      for k in oa.adds.keys() {
        adds[k] = oa.adds[k].copy()
      }
      for k in ob.adds.keys() {
        let b_tags = ob.adds[k]
        match adds.get(k) {
          Some(a_tags) => {
            let tag_set : Map[String, Bool] = Map([])
            for t in a_tags {
              tag_set[t] = true
            }
            for t in b_tags {
              if tag_set.get(t) is None {
                a_tags.push(t)
                tag_set[t] = true
              }
            }
          }
          None => adds[k] = b_tags.copy()
        }
      }
      let rem_set : Map[String, Bool] = Map([])
      let removes : Array[String] = []
      for r in oa.removes {
        if rem_set.get(r) is None {
          removes.push(r)
          rem_set[r] = true
        }
      }
      for r in ob.removes {
        if rem_set.get(r) is None {
          removes.push(r)
          rem_set[r] = true
        }
      }
      Some(OrSet({ adds, removes }))
    }
    (LwwMap(ma), LwwMap(mb)) => {
      let entries : Map[String, LwwReg] = Map([])
      for k in ma.entries.keys() {
        entries[k] = ma.entries[k]
      }
      for k in mb.entries.keys() {
        let reg_b = mb.entries[k]
        match entries.get(k) {
          Some(reg_a) => {
            let cmp = compare_stamp(
              reg_b.ts,
              reg_b.replica,
              if reg_b.v is Some(_) { 1 } else { 0 },
              match reg_b.v {
                Some(v) => canon_json(v)
                None => ""
              },
              reg_a.ts,
              reg_a.replica,
              if reg_a.v is Some(_) { 1 } else { 0 },
              match reg_a.v {
                Some(v) => canon_json(v)
                None => ""
              },
            )
            if cmp > 0 {
              entries[k] = reg_b
            }
          }
          None => entries[k] = reg_b
        }
      }
      Some(LwwMap({ entries, }))
    }
    (Rga(ra), Rga(rb)) => {
      let elems : Map[String, RgaElem] = Map([])
      for id in ra.elems.keys() {
        let e = ra.elems[id]
        elems[id] = { ch: e.ch, after: e.after, del: e.del }
      }
      for id in rb.elems.keys() {
        let eb = rb.elems[id]
        match elems.get(id) {
          Some(ea) => ea.del = ea.del || eb.del
          None => elems[id] = { ch: eb.ch, after: eb.after, del: eb.del }
        }
      }
      Some(Rga({ elems, }))
    }
    _ => None
  }
}

fn parse_val(s : String) -> Result[@json.Json, CrdtError] {
  let v = @json.parse(s) catch {
    _ => return Err(InvalidJson("value-json: invalid JSON"))
  }
  Ok(v)
}

///|
pub fn merge(a : String, b : String) -> Result[String, CrdtError] {
  let sa = match parse_state(a, "a") {
    Ok(s) => s
    Err(e) => return Err(e)
  }
  let sb = match parse_state(b, "b") {
    Ok(s) => s
    Err(e) => return Err(e)
  }
  match merge_states(sa, sb) {
    Some(res) => Ok(dump(res))
    None => Err(TypeMismatch("cannot merge different CRDT types"))
  }
}

///|
pub fn value(state : String) -> Result[String, CrdtError] {
  let st = match parse_state(state, "state") {
    Ok(s) => s
    Err(e) => return Err(e)
  }
  Ok(value_of(st))
}

///|
pub fn lww_new(
  value_json : String,
  timestamp : UInt64,
  replica : String,
) -> Result[String, CrdtError] {
  let v = match parse_val(value_json) {
    Ok(val) => val
    Err(e) => return Err(e)
  }
  Ok(dump(Lww({ v, ts: timestamp, replica })))
}

///|
pub fn lww_set(
  state : String,
  value_json : String,
  timestamp : UInt64,
  replica : String,
) -> Result[String, CrdtError] {
  let cand = match lww_new(value_json, timestamp, replica) {
    Ok(c) => c
    Err(e) => return Err(e)
  }
  merge(state, cand)
}

///|
pub fn counter_new() -> String {
  dump(Pn({ p: Map([]), n: Map([]) }))
}

///|
pub fn counter_add(
  state : String,
  replica : String,
  delta : Int64,
) -> Result[String, CrdtError] {
  let st = match parse_state(state, "state") {
    Ok(s) => s
    Err(e) => return Err(e)
  }
  match st {
    Pn(pn) => {
      let p : Map[String, UInt64] = Map([])
      for k in pn.p.keys() {
        p[k] = pn.p[k]
      }
      let n : Map[String, UInt64] = Map([])
      for k in pn.n.keys() {
        n[k] = pn.n[k]
      }
      if delta >= 0L {
        let cur = p.get(replica).unwrap_or(0UL)
        p[replica] = cur + delta.reinterpret_as_uint64()
      } else {
        let cur = n.get(replica).unwrap_or(0UL)
        n[replica] = cur + (-delta).reinterpret_as_uint64()
      }
      Ok(dump(Pn({ p, n })))
    }
    _ => Err(InvalidState("expected a pn-counter"))
  }
}

///|
pub fn orset_new() -> String {
  dump(OrSet({ adds: Map([]), removes: [] }))
}

///|
pub fn orset_add(
  state : String,
  element : String,
  tag : String,
) -> Result[String, CrdtError] {
  let st = match parse_state(state, "state") {
    Ok(s) => s
    Err(e) => return Err(e)
  }
  match st {
    OrSet(orset) => {
      let adds : Map[String, Array[String]] = Map([])
      for k in orset.adds.keys() {
        adds[k] = orset.adds[k].copy()
      }
      let removes = orset.removes.copy()
      match adds.get(element) {
        Some(tags) => {
          let mut found = false
          for t in tags {
            if t == tag {
              found = true
              break
            }
          }
          if !found {
            tags.push(tag)
          }
        }
        None => adds[element] = [tag]
      }
      Ok(dump(OrSet({ adds, removes })))
    }
    _ => Err(InvalidState("expected an orset"))
  }
}

///|
pub fn orset_remove(
  state : String,
  element : String,
) -> Result[String, CrdtError] {
  let st = match parse_state(state, "state") {
    Ok(s) => s
    Err(e) => return Err(e)
  }
  match st {
    OrSet(orset) => {
      let adds : Map[String, Array[String]] = Map([])
      for k in orset.adds.keys() {
        adds[k] = orset.adds[k].copy()
      }
      let removes = orset.removes.copy()
      match adds.get(element) {
        Some(tags) => {
          let rem_set : Map[String, Bool] = Map([])
          for r in removes {
            rem_set[r] = true
          }
          for t in tags {
            if rem_set.get(t) is None {
              removes.push(t)
              rem_set[t] = true
            }
          }
        }
        None => ()
      }
      Ok(dump(OrSet({ adds, removes })))
    }
    _ => Err(InvalidState("expected an orset"))
  }
}

fn lwwmap_put(
  state : String,
  key : String,
  cand : LwwReg,
) -> Result[String, CrdtError] {
  let st = match parse_state(state, "state") {
    Ok(s) => s
    Err(e) => return Err(e)
  }
  match st {
    LwwMap(lwwmap) => {
      let entries : Map[String, LwwReg] = Map([])
      for k in lwwmap.entries.keys() {
        entries[k] = lwwmap.entries[k]
      }
      let take = match entries.get(key) {
        Some(cur) => {
          let cmp = compare_stamp(
            cand.ts,
            cand.replica,
            if cand.v is Some(_) { 1 } else { 0 },
            match cand.v {
              Some(v) => canon_json(v)
              None => ""
            },
            cur.ts,
            cur.replica,
            if cur.v is Some(_) { 1 } else { 0 },
            match cur.v {
              Some(v) => canon_json(v)
              None => ""
            },
          )
          cmp > 0
        }
        None => true
      }
      if take {
        entries[key] = cand
      }
      Ok(dump(LwwMap({ entries, })))
    }
    _ => Err(InvalidState("expected a lwwmap"))
  }
}

///|
pub fn lwwmap_new() -> String {
  dump(LwwMap({ entries: Map([]) }))
}

///|
pub fn lwwmap_set(
  state : String,
  key : String,
  value_json : String,
  timestamp : UInt64,
  replica : String,
) -> Result[String, CrdtError] {
  let v = match parse_val(value_json) {
    Ok(val) => Some(val)
    Err(e) => return Err(e)
  }
  lwwmap_put(state, key, { v, ts: timestamp, replica })
}

///|
pub fn lwwmap_remove(
  state : String,
  key : String,
  timestamp : UInt64,
  replica : String,
) -> Result[String, CrdtError] {
  lwwmap_put(state, key, { v: None, ts: timestamp, replica })
}

///|
pub fn rga_new() -> String {
  dump(Rga({ elems: Map([]) }))
}

///|
pub fn rga_insert(
  state : String,
  index : UInt,
  text : String,
  id_base : String,
) -> Result[String, CrdtError] {
  let st = match parse_state(state, "state") {
    Ok(s) => s
    Err(e) => return Err(e)
  }
  match st {
    Rga(rga) => {
      let elems : Map[String, RgaElem] = Map([])
      for id in rga.elems.keys() {
        let e = rga.elems[id]
        elems[id] = { ch: e.ch, after: e.after, del: e.del }
      }
      let visible = rga_visible(elems)
      let idx_int = index.reinterpret_as_int()
      let idx = if idx_int > visible.length() {
        visible.length()
      } else {
        idx_int
      }
      let mut anchor = if idx == 0 { "" } else { visible[idx - 1] }
      let chars = text.to_array()
      for k = 0; k < chars.length(); k = k + 1 {
        let id = "\{id_base}:\{pad4(k)}"
        elems[id] = { ch: chars[k].to_string(), after: anchor, del: false }
        anchor = id
      }
      Ok(dump(Rga({ elems, })))
    }
    _ => Err(InvalidState("expected an rga"))
  }
}

///|
pub fn rga_delete(
  state : String,
  index : UInt,
  count : UInt,
) -> Result[String, CrdtError] {
  let st = match parse_state(state, "state") {
    Ok(s) => s
    Err(e) => return Err(e)
  }
  match st {
    Rga(rga) => {
      let elems : Map[String, RgaElem] = Map([])
      for id in rga.elems.keys() {
        let e = rga.elems[id]
        elems[id] = { ch: e.ch, after: e.after, del: e.del }
      }
      let visible = rga_visible(elems)
      let start_int = index.reinterpret_as_int()
      let start = if start_int > visible.length() {
        visible.length()
      } else {
        start_int
      }
      let count_int = count.reinterpret_as_int()
      let end = if start + count_int > visible.length() {
        visible.length()
      } else {
        start + count_int
      }
      for i = start; i < end; i = i + 1 {
        let id = visible[i]
        match elems.get(id) {
          Some(e) => e.del = true
          None => ()
        }
      }
      Ok(dump(Rga({ elems, })))
    }
    _ => Err(InvalidState("expected an rga"))
  }
}

///|
pub fn rga_insert_after(
  state : String,
  after_id : String,
  text : String,
  id_base : String,
) -> Result[String, CrdtError] {
  let st = match parse_state(state, "state") {
    Ok(s) => s
    Err(e) => return Err(e)
  }
  match st {
    Rga(rga) => {
      let elems : Map[String, RgaElem] = Map([])
      for id in rga.elems.keys() {
        let e = rga.elems[id]
        elems[id] = { ch: e.ch, after: e.after, del: e.del }
      }
      if after_id != "" && elems.get(after_id) is None {
        return Err(InvalidState("after-id not found: \{after_id}"))
      }
      let mut anchor = after_id
      let chars = text.to_array()
      for k = 0; k < chars.length(); k = k + 1 {
        let id = "\{id_base}:\{pad4(k)}"
        elems[id] = { ch: chars[k].to_string(), after: anchor, del: false }
        anchor = id
      }
      Ok(dump(Rga({ elems, })))
    }
    _ => Err(InvalidState("expected an rga"))
  }
}

///|
pub fn rga_delete_ids(
  state : String,
  ids : Array[String],
) -> Result[String, CrdtError] {
  let st = match parse_state(state, "state") {
    Ok(s) => s
    Err(e) => return Err(e)
  }
  match st {
    Rga(rga) => {
      let elems : Map[String, RgaElem] = Map([])
      for id in rga.elems.keys() {
        let e = rga.elems[id]
        elems[id] = { ch: e.ch, after: e.after, del: e.del }
      }
      for id in ids {
        match elems.get(id) {
          Some(e) => e.del = true
          None => ()
        }
      }
      Ok(dump(Rga({ elems, })))
    }
    _ => Err(InvalidState("expected an rga"))
  }
}

///|
pub fn rga_elements(state : String) -> Result[String, CrdtError] {
  let st = match parse_state(state, "state") {
    Ok(s) => s
    Err(e) => return Err(e)
  }
  match st {
    Rga(rga) => {
      let visible = rga_visible(rga.elems)
      let buf = StringBuilder()
      buf.write_char('[')
      let mut first = true
      for id in visible {
        match rga.elems.get(id) {
          Some(e) => {
            if !first {
              buf.write_char(',')
            }
            buf.write_string("{\"id\":")
            buf.write_string(escape_str(id))
            buf.write_string(",\"ch\":")
            buf.write_string(escape_str(e.ch))
            buf.write_char('}')
            first = false
          }
          None => ()
        }
      }
      buf.write_char(']')
      Ok(buf.to_string())
    }
    _ => Err(InvalidState("expected an rga"))
  }
}

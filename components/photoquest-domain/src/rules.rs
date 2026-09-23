//! Requirement checking and `auto-v1` scoring (CONTRACT.md "Requirements",
//! "Timed competitions"). Pure functions over a stored photo record: no I/O, so
//! every rule is unit-tested here and quests and competitions judge the same way.
//!
//! The one idea every check below follows: a value that is MISSING because the
//! stage that produces it did not run is not a bad value. Such a check reports
//! `ok: null` with a detail saying which stage was missing, and counts as not
//! passed — so a photographer is told "not looked at", never "bad photo".
//!
//! Where each value comes from (the callback body, stored verbatim on the photo):
//!
//! | check                        | field                                         | stage |
//! |------------------------------|-----------------------------------------------|-------|
//! | `subject` (label)            | `vision.labels[].{id, confidence}`            | Vision |
//! | `subject` (face)             | `vision.faces[]`                              | Vision |
//! | `sharpness.min_focus_ratio`  | `sharpness.focus_ratio`                       | sharpness |
//! | `sharpness.min_subject_ratio`| best `sharpness.subjects[kind=face].ratio`    | sharpness + Vision (the face boxes) |
//! | `aesthetics_min`             | `vision.aesthetics.overall`                   | Vision |
//! | `exposure.*`                 | `metadata.{fnumber, exposure_s, focal_mm, iso}` | metadata |
//! | `format`                     | the original's key / filename / content type  | always known |
//! | `captured_after_start`       | `metadata.captured_at` (camera clock, no zone, read as UTC) | metadata |

use serde_json::{json, Map, Value};

/// Every top-level requirement key `validate` accepts.
const KEYS: &[&str] =
    &["subject", "sharpness", "aesthetics_min", "exposure", "format", "captured_after_start"];
const SHARPNESS_KEYS: &[&str] = &["min_focus_ratio", "min_subject_ratio"];
const EXPOSURE_KEYS: &[&str] =
    &["max_fnumber", "max_shutter_s", "min_focal_mm", "max_focal_mm", "max_iso"];

const NEEDS_VISION: &str = "needs Vision; this photo was evaluated without it";

// ---- reading a photo record -------------------------------------------------

/// Did the Vision stage run for this photo? `backend.vision` is what the daemon
/// says; a `vision` object has to be there too, or there is nothing to read.
fn vision_ran(photo: &Map<String, Value>) -> bool {
    let flagged = photo
        .get("backend")
        .and_then(|b| b.get("vision"))
        .and_then(Value::as_bool)
        // An older record without `backend.vision`: trust the object itself.
        .unwrap_or(true);
    flagged && photo.get("vision").map(Value::is_object).unwrap_or(false)
}

fn path<'a>(photo: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    let (first, rest) = keys.split_first()?;
    let mut v = photo.get(*first)?;
    for k in rest {
        v = v.get(*k)?;
    }
    if v.is_null() {
        None
    } else {
        Some(v)
    }
}

fn num(photo: &Map<String, Value>, keys: &[&str]) -> Option<f64> {
    path(photo, keys).and_then(Value::as_f64)
}

/// The best face `ratio` among `sharpness.subjects`, if any face was measured.
fn best_face_ratio(photo: &Map<String, Value>) -> Option<f64> {
    path(photo, &["sharpness", "subjects"])?
        .as_array()?
        .iter()
        .filter(|s| s.get("kind").and_then(Value::as_str) == Some("face"))
        .filter_map(|s| s.get("ratio").and_then(Value::as_f64))
        .fold(None, |best: Option<f64>, r| Some(best.map_or(r, |b| b.max(r))))
}

/// True when the original is a Sony ARW: by the stored key's or the filename's
/// extension, or by the content type the browser reported.
pub fn is_raw(photo: &Map<String, Value>) -> bool {
    let ext = |k: &str| {
        photo
            .get(k)
            .and_then(Value::as_str)
            .and_then(|s| s.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()))
            .unwrap_or_default()
    };
    let ct =
        photo.get("content_type").and_then(Value::as_str).unwrap_or_default().to_ascii_lowercase();
    ext("key") == "arw" || ext("filename") == "arw" || ct == "image/x-sony-arw" || ct == "image/arw"
}

fn original_kind(photo: &Map<String, Value>) -> String {
    for k in ["key", "filename"] {
        if let Some((_, e)) = photo.get(k).and_then(Value::as_str).and_then(|s| s.rsplit_once('.'))
        {
            return format!(".{}", e.to_ascii_lowercase());
        }
    }
    match photo.get("content_type").and_then(Value::as_str) {
        Some(ct) if !ct.is_empty() => ct.to_string(),
        _ => "an unknown type".to_string(),
    }
}

// ---- time ---------------------------------------------------------------------

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `YYYY-MM-DDTHH:MM:SS` (a space works for the `T`; anything after the seconds
/// — fractions, a zone — is ignored) as unix seconds, the wall time read as UTC.
pub fn parse_captured_at(s: &str) -> Option<i64> {
    let b = s.trim().as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !(b[10] == b'T' || b[10] == b' ') {
        return None;
    }
    if b[13] != b':' || b[16] != b':' {
        return None;
    }
    let n = |from: usize, to: usize| -> Option<i64> {
        let part = std::str::from_utf8(&b[from..to]).ok()?;
        if !part.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    let (y, mo, d) = (n(0, 4)?, n(5, 7)?, n(8, 10)?);
    let (h, mi, sec) = (n(11, 13)?, n(14, 16)?, n(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec)
}

/// Unix seconds as `YYYY-MM-DDTHH:MM:SS` UTC — the shape `captured_at` has, so a
/// detail compares like with like.
pub fn format_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}", tod / 3600, (tod % 3600) / 60, tod % 60)
}

// ---- the verdict --------------------------------------------------------------

fn check(name: &str, ok: Option<bool>, detail: String) -> Value {
    json!({ "name": name, "ok": ok, "detail": detail })
}

/// A number formatted for a detail: at most two decimals, at least one
/// (`5.0`, `3.1`, `0.69`), whole numbers ≥ 100 bare (`6400`), and small
/// fractions as they are (`0.004`) so a shutter speed does not read as zero.
fn f(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() >= 100.0 {
        return format!("{x:.0}");
    }
    if x != 0.0 && x.abs() < 0.1 {
        return format!("{x}");
    }
    let s = format!("{x:.2}");
    let s = s.trim_end_matches('0');
    if s.ends_with('.') {
        format!("{s}0")
    } else {
        s.to_string()
    }
}

/// `value ≥ min` style: `ok` when `pass(value)`.
fn compare(
    name: &str,
    value: Option<f64>,
    missing: &str,
    pass: bool,
    ok_sym: &str,
    bad_sym: &str,
    limit: f64,
) -> Value {
    match value {
        None => check(name, None, missing.to_string()),
        Some(v) => {
            let sym = if pass { ok_sym } else { bad_sym };
            check(name, Some(pass), format!("{} {sym} {}", f(v), f(limit)))
        }
    }
}

fn subject_check(req: &Value, photo: &Map<String, Value>) -> Value {
    if !vision_ran(photo) {
        return check("subject", None, NEEDS_VISION.into());
    }
    if req.get("face").and_then(Value::as_bool) == Some(true) {
        let faces =
            path(photo, &["vision", "faces"]).and_then(Value::as_array).map(Vec::len).unwrap_or(0);
        return if faces > 0 {
            check(
                "subject",
                Some(true),
                format!("{faces} face{}", if faces == 1 { "" } else { "s" }),
            )
        } else {
            check("subject", Some(false), "no face found".into())
        };
    }
    let label = req.get("label").and_then(Value::as_str).unwrap_or_default();
    let min = req.get("min_confidence").and_then(Value::as_f64).unwrap_or(0.0);
    let found = path(photo, &["vision", "labels"])
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|l| {
            l.get("id").and_then(Value::as_str).is_some_and(|id| id.eq_ignore_ascii_case(label))
        })
        .filter_map(|l| l.get("confidence").and_then(Value::as_f64))
        .fold(None, |best: Option<f64>, c| Some(best.map_or(c, |b| b.max(c))));
    match found {
        None => check("subject", Some(false), format!("no {label} label")),
        Some(c) if c >= min => check("subject", Some(true), format!("{label} {c:.2} ≥ {min:.2}")),
        Some(c) => check("subject", Some(false), format!("{label} {c:.2} < {min:.2}")),
    }
}

/// Judge `photo` against `requirements`. `starts_at` is the quest's or
/// competition's start, for `captured_after_start`.
///
/// Every requirement present yields one check (`captured_after_start` is on
/// unless explicitly `false`), in a fixed order. `pass` is true only when every
/// check is `ok: true` — a `null` is not a pass.
pub fn verdict(requirements: &Value, photo: &Map<String, Value>, starts_at: u64) -> Value {
    let empty = Map::new();
    let req = requirements.as_object().unwrap_or(&empty);
    let mut checks = Vec::new();

    if let Some(s) = req.get("subject").filter(|v| v.is_object()) {
        checks.push(subject_check(s, photo));
    }

    if let Some(s) = req.get("sharpness").filter(|v| v.is_object()) {
        if let Some(min) = s.get("min_focus_ratio").and_then(Value::as_f64) {
            let v = num(photo, &["sharpness", "focus_ratio"]);
            checks.push(compare(
                "sharpness.min_focus_ratio",
                v,
                "needs the sharpness stage; this photo has no focus_ratio",
                v.is_some_and(|v| v >= min),
                "≥",
                "<",
                min,
            ));
        }
        if let Some(min) = s.get("min_subject_ratio").and_then(Value::as_f64) {
            let name = "sharpness.min_subject_ratio";
            if !vision_ran(photo) {
                checks.push(check(name, None, format!("{NEEDS_VISION} (faces come from Vision)")));
            } else {
                match best_face_ratio(photo) {
                    None => checks.push(check(name, Some(false), "no face to measure".into())),
                    Some(r) => checks.push(compare(name, Some(r), "", r >= min, "≥", "<", min)),
                }
            }
        }
    }

    if let Some(min) = req.get("aesthetics_min").and_then(Value::as_f64) {
        let name = "aesthetics_min";
        if !vision_ran(photo) {
            checks.push(check(name, None, NEEDS_VISION.into()));
        } else {
            let v = num(photo, &["vision", "aesthetics", "overall"]);
            checks.push(compare(
                name,
                v,
                "Vision ran but gave no aesthetics score",
                v.is_some_and(|v| v >= min),
                "≥",
                "<",
                min,
            ));
        }
    }

    if let Some(e) = req.get("exposure").filter(|v| v.is_object()) {
        // (requirement key, metadata field, is a maximum)
        let rows: [(&str, &str, bool); 5] = [
            ("max_fnumber", "fnumber", true),
            ("max_shutter_s", "exposure_s", true),
            ("min_focal_mm", "focal_mm", false),
            ("max_focal_mm", "focal_mm", true),
            ("max_iso", "iso", true),
        ];
        for (key, field, is_max) in rows {
            let Some(limit) = e.get(key).and_then(Value::as_f64) else { continue };
            let v = num(photo, &["metadata", field]);
            let pass = v.is_some_and(|v| if is_max { v <= limit } else { v >= limit });
            let (ok_sym, bad_sym) = if is_max { ("≤", ">") } else { ("≥", "<") };
            checks.push(compare(
                &format!("exposure.{key}"),
                v,
                &format!("the photo's metadata has no {field}"),
                pass,
                ok_sym,
                bad_sym,
                limit,
            ));
        }
    }

    if req.get("format").and_then(Value::as_str) == Some("raw") {
        if is_raw(photo) {
            checks.push(check("format", Some(true), "ARW original".into()));
        } else {
            checks.push(check(
                "format",
                Some(false),
                format!("original is {}, not RAW (ARW)", original_kind(photo)),
            ));
        }
    }

    if req.get("captured_after_start").and_then(Value::as_bool).unwrap_or(true) {
        let name = "captured_after_start";
        let start = format_utc(starts_at as i64);
        match path(photo, &["metadata", "captured_at"]).and_then(Value::as_str) {
            None => {
                checks.push(check(name, None, "the photo's metadata has no capture time".into()))
            }
            Some(raw) => match parse_captured_at(raw) {
                None => {
                    checks.push(check(name, None, format!("capture time {raw:?} is not readable")))
                }
                Some(at) if at >= starts_at as i64 => checks.push(check(
                    name,
                    Some(true),
                    format!("captured {} ≥ start {start}", format_utc(at)),
                )),
                Some(at) => checks.push(check(
                    name,
                    Some(false),
                    format!("captured {} < start {start}", format_utc(at)),
                )),
            },
        }
    }

    let pass = checks.iter().all(|c| c["ok"] == json!(true));
    json!({ "pass": pass, "checks": checks })
}

// ---- validation ---------------------------------------------------------------

fn unknown_keys(obj: &Map<String, Value>, allowed: &[&str], within: &str) -> Result<(), String> {
    match obj.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) if within.is_empty() => Err(format!("unknown requirement {k:?}")),
        Some(k) => Err(format!("unknown requirement {within}.{k}")),
        None => Ok(()),
    }
}

/// A number ≥ 0 (and ≤ `max` when given), or null = not checked.
fn opt_number(
    obj: &Map<String, Value>,
    key: &str,
    within: &str,
    max: Option<f64>,
) -> Result<Option<f64>, String> {
    let name = if within.is_empty() { key.to_string() } else { format!("{within}.{key}") };
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let Some(x) = v.as_f64() else { return Err(format!("{name} must be a number")) };
            if !x.is_finite() || x < 0.0 {
                return Err(format!("{name} must be ≥ 0"));
            }
            if let Some(m) = max {
                if x > m {
                    return Err(format!("{name} must be ≤ {m}"));
                }
            }
            Ok(Some(x))
        }
    }
}

/// Refuse a malformed requirements object before it is stored (`bad_requirements`).
/// `null` is accepted and means "no requirements" beyond `captured_after_start`.
pub fn validate(requirements: &Value) -> Result<(), String> {
    let req = match requirements {
        Value::Null => return Ok(()),
        Value::Object(m) => m,
        _ => return Err("requirements must be an object".into()),
    };
    unknown_keys(req, KEYS, "")?;

    match req.get("subject") {
        None | Some(Value::Null) => {}
        Some(Value::Object(s)) => {
            let face = s.get("face");
            let label = s.get("label");
            match (face, label) {
                (Some(_), Some(_)) => {
                    return Err("subject is either a label or a face, not both".into())
                }
                (Some(Value::Bool(true)), None) => unknown_keys(s, &["face"], "subject")?,
                (Some(_), None) => return Err("subject.face must be true".into()),
                (None, Some(Value::String(l))) if !l.trim().is_empty() => {
                    unknown_keys(s, &["label", "min_confidence"], "subject")?;
                    opt_number(s, "min_confidence", "subject", Some(1.0))?;
                }
                (None, Some(_)) => return Err("subject.label must be a non-empty string".into()),
                (None, None) => return Err("subject needs a label or face: true".into()),
            }
        }
        Some(_) => return Err("subject must be an object".into()),
    }

    match req.get("sharpness") {
        None | Some(Value::Null) => {}
        Some(Value::Object(s)) => {
            unknown_keys(s, SHARPNESS_KEYS, "sharpness")?;
            opt_number(s, "min_focus_ratio", "sharpness", None)?;
            opt_number(s, "min_subject_ratio", "sharpness", None)?;
        }
        Some(_) => return Err("sharpness must be an object".into()),
    }

    opt_number(req, "aesthetics_min", "", Some(1.0))?;

    match req.get("exposure") {
        None | Some(Value::Null) => {}
        Some(Value::Object(e)) => {
            unknown_keys(e, EXPOSURE_KEYS, "exposure")?;
            for k in ["max_fnumber", "max_shutter_s", "max_iso", "max_focal_mm"] {
                if opt_number(e, k, "exposure", None)? == Some(0.0) {
                    return Err(format!("exposure.{k} must be > 0"));
                }
            }
            let min = opt_number(e, "min_focal_mm", "exposure", None)?;
            let max = opt_number(e, "max_focal_mm", "exposure", None)?;
            if let (Some(lo), Some(hi)) = (min, max) {
                if lo > hi {
                    return Err("exposure.min_focal_mm is above exposure.max_focal_mm".into());
                }
            }
        }
        Some(_) => return Err("exposure must be an object".into()),
    }

    match req.get("format") {
        None | Some(Value::Null) => {}
        Some(Value::String(s)) if s == "raw" => {}
        Some(_) => return Err("format must be \"raw\" or absent".into()),
    }

    match req.get("captured_after_start") {
        None | Some(Value::Null) | Some(Value::Bool(_)) => {}
        Some(_) => return Err("captured_after_start must be true or false".into()),
    }
    Ok(())
}

// ---- auto-v1 ------------------------------------------------------------------

/// `auto-v1`, 0..=1, plus flags for parts that could not be computed.
///
/// `0.5·clamp(subject_or_focus/8) + 0.3·aesthetics + 0.2·(1 − clip_penalty)`:
/// `subject_or_focus` is the best face ratio × 10 when a face was measured, else
/// `focus_ratio`; `clip_penalty = min(1, (shadows% + highlights%)/5)`. A part
/// whose input is missing counts 0 and is flagged — never guessed.
pub fn auto_v1(photo: &Map<String, Value>) -> (f64, Vec<String>) {
    let mut flags = Vec::new();
    let clamp = |x: f64| x.clamp(0.0, 1.0);

    let subject_or_focus = match best_face_ratio(photo) {
        Some(r) => Some(r * 10.0),
        None => num(photo, &["sharpness", "focus_ratio"]),
    };
    let sharp = match subject_or_focus {
        Some(v) => clamp(v / 8.0),
        None => {
            flags.push("sharpness: no focus_ratio; counted 0".to_string());
            0.0
        }
    };

    let aesthetics = if vision_ran(photo) {
        match num(photo, &["vision", "aesthetics", "overall"]) {
            Some(a) => clamp(a),
            None => {
                flags.push("aesthetics: Vision gave no score; counted 0".to_string());
                0.0
            }
        }
    } else {
        flags.push("aesthetics: needs Vision; counted 0".to_string());
        0.0
    };

    let shadows = num(photo, &["colour", "clipped_shadows_pct"]);
    let highlights = num(photo, &["colour", "clipped_highlights_pct"]);
    let exposure = match (shadows, highlights) {
        (Some(s), Some(h)) => 1.0 - ((s + h) / 5.0).clamp(0.0, 1.0),
        _ => {
            flags.push("exposure: no clipping figures; counted 0".to_string());
            0.0
        }
    };

    (clamp(0.5 * sharp + 0.3 * aesthetics + 0.2 * exposure), flags)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap()
    }

    /// The gate's `result_for`, as stored on a photo record.
    fn sample() -> Value {
        json!({
            "owner": "u", "filename": "DSC01234.ARW", "content_type": "image/x-sony-arw",
            "key": "originals/X.arw", "state": "evaluated",
            "backend": {"sharpness": "metal", "develop": "coreimage", "vision": true},
            "sha256": "ab",
            "metadata": {"captured_at": "2026-09-23T13:11:47", "exposure_s": 0.004, "fnumber": 4.0,
                         "focal_mm": 200.0, "iso": 6400},
            "sharpness": {"focus_ratio": 6.1,
                          "subjects": [{"kind": "face", "ratio": 0.5}, {"kind": "face", "ratio": 0.7}]},
            "vision": {"labels": [{"id": "people", "confidence": 0.96}, {"id": "Grass", "confidence": 0.9}],
                       "faces": [{"quality": 0.55}], "aesthetics": {"overall": 0.688}},
            "colour": {"clipped_shadows_pct": 0.3, "clipped_highlights_pct": 0.1}
        })
    }

    fn no_vision() -> Value {
        let mut p = sample();
        p["backend"]["vision"] = json!(false);
        p["vision"] = Value::Null;
        p
    }

    fn by_name<'a>(v: &'a Value, name: &str) -> &'a Value {
        v["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .unwrap_or_else(|| panic!("no check {name}: {v}"))
    }

    const CAPTURED: u64 = 1_790_169_107; // 2026-09-23T13:11:47Z

    #[test]
    fn captured_at_is_read_as_utc() {
        assert_eq!(parse_captured_at("2026-09-23T13:11:47"), Some(CAPTURED as i64));
        assert_eq!(parse_captured_at("2026-09-23 13:11:47"), Some(CAPTURED as i64));
        assert_eq!(parse_captured_at("2026-09-23T13:11:47.25+02:00"), Some(CAPTURED as i64));
        assert_eq!(parse_captured_at("1970-01-01T00:00:00"), Some(0));
        assert_eq!(parse_captured_at("2026:09:23 13:11:47"), None);
        assert_eq!(parse_captured_at("2026-13-01T00:00:00"), None);
        assert_eq!(parse_captured_at("soon"), None);
        assert_eq!(format_utc(CAPTURED as i64), "2026-09-23T13:11:47");
        assert_eq!(format_utc(0), "1970-01-01T00:00:00");
    }

    #[test]
    fn empty_requirements_only_check_the_capture_time() {
        let v = verdict(&json!({}), &photo(sample()), CAPTURED);
        assert_eq!(v["checks"].as_array().unwrap().len(), 1, "{v}");
        assert_eq!(v["pass"], true, "captured exactly at start passes: {v}");
        let v = verdict(&json!({"captured_after_start": false}), &photo(sample()), CAPTURED + 1);
        assert_eq!(v["checks"].as_array().unwrap().len(), 0, "{v}");
        assert_eq!(v["pass"], true);
        let v = verdict(&Value::Null, &photo(sample()), CAPTURED + 1);
        assert_eq!(v["pass"], false, "captured a second before the start: {v}");
        assert_eq!(by_name(&v, "captured_after_start")["ok"], false);
        assert_eq!(
            by_name(&v, "captured_after_start")["detail"],
            "captured 2026-09-23T13:11:47 < start 2026-09-23T13:11:48"
        );
    }

    #[test]
    fn a_missing_capture_time_is_not_looked_at_not_failed() {
        let mut p = sample();
        p["metadata"].as_object_mut().unwrap().remove("captured_at");
        let v = verdict(&json!({}), &photo(p), 0);
        assert!(by_name(&v, "captured_after_start")["ok"].is_null(), "{v}");
        assert_eq!(v["pass"], false, "null counts as not passed");
        let mut p = sample();
        p["metadata"] = Value::Null;
        let v = verdict(&json!({}), &photo(p), 0);
        assert!(by_name(&v, "captured_after_start")["ok"].is_null(), "{v}");
    }

    #[test]
    fn subject_label_matches_case_insensitively_against_a_threshold() {
        let r = json!({"subject": {"label": "grass", "min_confidence": 0.5}, "captured_after_start": false});
        let v = verdict(&r, &photo(sample()), 0);
        assert_eq!(v["pass"], true, "{v}");
        assert_eq!(by_name(&v, "subject")["detail"], "grass 0.90 ≥ 0.50");
        let r = json!({"subject": {"label": "grass", "min_confidence": 0.95}, "captured_after_start": false});
        let v = verdict(&r, &photo(sample()), 0);
        assert_eq!(by_name(&v, "subject")["ok"], false);
        assert_eq!(by_name(&v, "subject")["detail"], "grass 0.90 < 0.95");
        let r = json!({"subject": {"label": "dog"}, "captured_after_start": false});
        let v = verdict(&r, &photo(sample()), 0);
        assert_eq!(by_name(&v, "subject")["ok"], false);
        assert_eq!(by_name(&v, "subject")["detail"], "no dog label");
    }

    #[test]
    fn subject_face() {
        let r = json!({"subject": {"face": true}, "captured_after_start": false});
        assert_eq!(verdict(&r, &photo(sample()), 0)["pass"], true);
        let mut p = sample();
        p["vision"]["faces"] = json!([]);
        let v = verdict(&r, &photo(p), 0);
        assert_eq!(by_name(&v, "subject")["ok"], false, "{v}");
    }

    #[test]
    fn vision_checks_are_null_without_vision() {
        let r = json!({
            "subject": {"label": "grass"}, "aesthetics_min": 0.4,
            "sharpness": {"min_focus_ratio": 5.0, "min_subject_ratio": 0.5},
            "captured_after_start": false
        });
        let v = verdict(&r, &photo(no_vision()), 0);
        assert_eq!(v["pass"], false, "{v}");
        assert!(by_name(&v, "subject")["ok"].is_null());
        assert_eq!(by_name(&v, "subject")["detail"], NEEDS_VISION);
        assert!(by_name(&v, "aesthetics_min")["ok"].is_null());
        assert!(by_name(&v, "sharpness.min_subject_ratio")["ok"].is_null());
        // focus_ratio is the sharpness stage, not Vision: still judged.
        assert_eq!(by_name(&v, "sharpness.min_focus_ratio")["ok"], true);
        // `backend.vision: false` wins even if a stale vision object is there.
        let mut p = sample();
        p["backend"]["vision"] = json!(false);
        assert!(by_name(&verdict(&r, &photo(p), 0), "subject")["ok"].is_null());
    }

    #[test]
    fn sharpness_checks() {
        let r = json!({"sharpness": {"min_focus_ratio": 5.0, "min_subject_ratio": 0.6}, "captured_after_start": false});
        let v = verdict(&r, &photo(sample()), 0);
        assert_eq!(v["pass"], true, "best face ratio 0.7 counts: {v}");
        assert_eq!(by_name(&v, "sharpness.min_focus_ratio")["detail"], "6.1 ≥ 5.0");
        let mut p = sample();
        p["sharpness"]["focus_ratio"] = json!(3.1);
        p["sharpness"]["subjects"] = json!([]);
        let v = verdict(&r, &photo(p), 0);
        assert_eq!(by_name(&v, "sharpness.min_focus_ratio")["ok"], false);
        assert_eq!(by_name(&v, "sharpness.min_focus_ratio")["detail"], "3.1 < 5.0");
        assert_eq!(by_name(&v, "sharpness.min_subject_ratio")["ok"], false);
        assert_eq!(by_name(&v, "sharpness.min_subject_ratio")["detail"], "no face to measure");
        let mut p = sample();
        p["sharpness"] = Value::Null;
        let v = verdict(&r, &photo(p), 0);
        assert!(by_name(&v, "sharpness.min_focus_ratio")["ok"].is_null(), "{v}");
    }

    #[test]
    fn aesthetics() {
        let r = json!({"aesthetics_min": 0.7, "captured_after_start": false});
        let v = verdict(&r, &photo(sample()), 0);
        assert_eq!(by_name(&v, "aesthetics_min")["ok"], false);
        assert_eq!(by_name(&v, "aesthetics_min")["detail"], "0.69 < 0.7");
        let r = json!({"aesthetics_min": 0.5, "captured_after_start": false});
        assert_eq!(verdict(&r, &photo(sample()), 0)["pass"], true);
    }

    #[test]
    fn exposure_limits() {
        let r = json!({"exposure": {"max_fnumber": 4.0, "max_shutter_s": 0.001, "min_focal_mm": 85,
                                    "max_focal_mm": null, "max_iso": 3200}, "captured_after_start": false});
        let v = verdict(&r, &photo(sample()), 0);
        assert_eq!(v["checks"].as_array().unwrap().len(), 4, "a null limit is not checked: {v}");
        assert_eq!(by_name(&v, "exposure.max_fnumber")["ok"], true, "f/4 ≤ f/4");
        assert_eq!(by_name(&v, "exposure.max_shutter_s")["ok"], false);
        assert_eq!(by_name(&v, "exposure.min_focal_mm")["ok"], true);
        assert_eq!(by_name(&v, "exposure.max_iso")["ok"], false);
        assert_eq!(by_name(&v, "exposure.max_iso")["detail"], "6400 > 3200");
        let mut p = sample();
        p["metadata"].as_object_mut().unwrap().remove("fnumber");
        let v = verdict(
            &json!({"exposure": {"max_fnumber": 1.4}, "captured_after_start": false}),
            &photo(p),
            0,
        );
        assert!(by_name(&v, "exposure.max_fnumber")["ok"].is_null(), "{v}");
        assert_eq!(
            by_name(&v, "exposure.max_fnumber")["detail"],
            "the photo's metadata has no fnumber"
        );
    }

    #[test]
    fn format_raw_reads_key_filename_or_content_type() {
        let r = json!({"format": "raw", "captured_after_start": false});
        assert_eq!(verdict(&r, &photo(sample()), 0)["pass"], true);
        let jpeg =
            json!({"filename": "a.jpg", "key": "originals/X.jpg", "content_type": "image/jpeg"});
        let v = verdict(&r, &photo(jpeg), 0);
        assert_eq!(by_name(&v, "format")["ok"], false);
        assert_eq!(by_name(&v, "format")["detail"], "original is .jpg, not RAW (ARW)");
        assert!(is_raw(&photo(json!({"filename": "X.arw"}))));
        assert!(is_raw(&photo(json!({"filename": "noext", "content_type": "image/x-sony-arw"}))));
        assert!(is_raw(&photo(json!({"key": "originals/X.ARW"}))));
        assert!(!is_raw(&photo(json!({"filename": "X.dng"}))));
    }

    #[test]
    fn the_contract_example_fails_with_every_check_reported() {
        let r = json!({"subject": {"label": "grass", "min_confidence": 0.5},
                       "sharpness": {"min_focus_ratio": 5.0}, "aesthetics_min": 0.4});
        let mut p = no_vision();
        p["sharpness"]["focus_ratio"] = json!(3.1);
        let v = verdict(&r, &photo(p), CAPTURED);
        assert_eq!(v["pass"], false);
        let names: Vec<&str> =
            v["checks"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            ["subject", "sharpness.min_focus_ratio", "aesthetics_min", "captured_after_start"]
        );
    }

    #[test]
    fn validate_accepts_the_contract_schema() {
        assert_eq!(validate(&Value::Null), Ok(()));
        assert_eq!(validate(&json!({})), Ok(()));
        assert_eq!(
            validate(&json!({
                "subject": {"label": "grass", "min_confidence": 0.5},
                "sharpness": {"min_focus_ratio": 5.0, "min_subject_ratio": 0.5},
                "aesthetics_min": 0.4,
                "exposure": {"max_fnumber": 2.8, "max_shutter_s": 0.001, "min_focal_mm": 85,
                             "max_focal_mm": null, "max_iso": 3200},
                "format": "raw", "captured_after_start": true
            })),
            Ok(())
        );
        assert_eq!(validate(&json!({"subject": {"face": true}})), Ok(()));
    }

    #[test]
    fn validate_refuses_malformed_requirements() {
        let bad = [
            json!([]),
            json!("raw"),
            json!({"colour": 1}),
            json!({"subject": {}}),
            json!({"subject": {"label": ""}}),
            json!({"subject": {"label": "a", "face": true}}),
            json!({"subject": {"face": false}}),
            json!({"subject": {"label": "a", "min_confidence": 1.5}}),
            json!({"subject": {"label": "a", "extra": 1}}),
            json!({"subject": "grass"}),
            json!({"sharpness": {"min_focus_ratio": -1}}),
            json!({"sharpness": {"min_focus_ratio": "5"}}),
            json!({"sharpness": {"focus": 5}}),
            json!({"aesthetics_min": 2}),
            json!({"exposure": {"max_fnumber": 0}}),
            json!({"exposure": {"min_focal_mm": 200, "max_focal_mm": 85}}),
            json!({"exposure": {"shutter": 1}}),
            json!({"format": "jpeg"}),
            json!({"captured_after_start": "yes"}),
        ];
        for r in bad {
            assert!(validate(&r).is_err(), "should refuse {r}");
        }
        assert_eq!(
            validate(&json!({"sharpness": {"focus": 5}})),
            Err("unknown requirement sharpness.focus".into())
        );
    }

    #[test]
    fn auto_v1_follows_the_formula() {
        // face ratio 0.7 → 7/8; aesthetics 0.688; clip (0.3+0.1)/5 = 0.08.
        let (s, flags) = auto_v1(&photo(sample()));
        let want = 0.5 * (7.0 / 8.0) + 0.3 * 0.688 + 0.2 * (1.0 - 0.08);
        assert!((s - want).abs() < 1e-9, "{s} vs {want}");
        assert!(flags.is_empty(), "{flags:?}");
        // No face: focus_ratio 6.1 → 6.1/8; clamps at 1.
        let mut p = sample();
        p["sharpness"]["subjects"] = json!([]);
        p["sharpness"]["focus_ratio"] = json!(12.0);
        p["colour"]["clipped_highlights_pct"] = json!(10.0);
        let (s, _) = auto_v1(&photo(p));
        assert!((s - (0.5 + 0.3 * 0.688)).abs() < 1e-9, "{s}");
    }

    #[test]
    fn auto_v1_flags_what_it_could_not_compute() {
        let (s, flags) = auto_v1(&photo(no_vision()));
        // The face ratio is still in `sharpness.subjects`; only aesthetics drops.
        let want = 0.5 * (7.0 / 8.0) + 0.2 * (1.0 - 0.08);
        assert!((s - want).abs() < 1e-9, "{s}");
        assert_eq!(flags.len(), 1, "{flags:?}");
        assert!(flags[0].starts_with("aesthetics"));
        let (s, flags) = auto_v1(&Map::new());
        assert_eq!(s, 0.0);
        assert_eq!(flags.len(), 3, "{flags:?}");
    }
}

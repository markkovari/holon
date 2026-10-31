use super::{repo_root, Gate};
use serde_json::Value;
use std::collections::HashMap;

enum Segment<'a> {
    Key(&'a str),
    Index(usize),
}

fn split_path_segments(path: &str) -> Vec<Segment<'_>> {
    let mut segments = Vec::new();
    for part in path.split('.') {
        if part.is_empty() {
            continue;
        }
        let mut rest = part;
        while let Some(start) = rest.find('[') {
            let key = &rest[..start];
            if !key.is_empty() {
                segments.push(Segment::Key(key));
            }
            if let Some(end) = rest.find(']') {
                if let Ok(idx) = rest[start + 1..end].parse::<usize>() {
                    segments.push(Segment::Index(idx));
                }
                rest = &rest[end + 1..];
            } else {
                break;
            }
        }
        if !rest.is_empty() {
            segments.push(Segment::Key(rest));
        }
    }
    segments
}

pub fn jsonpath_eval<'a>(val: &'a Value, path: &str) -> Option<&'a Value> {
    let trimmed = path.trim();
    let p = trimmed.strip_prefix("$.").unwrap_or(trimmed.strip_prefix('$').unwrap_or(trimmed));
    if p.is_empty() {
        return Some(val);
    }
    let mut curr = val;
    for segment in split_path_segments(p) {
        match segment {
            Segment::Key(k) => {
                curr = curr.get(k)?;
            }
            Segment::Index(i) => {
                curr = curr.get(i)?;
            }
        }
    }
    Some(curr)
}

fn sub_vars(s: &str, vars: &HashMap<String, String>) -> String {
    let mut out = s.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{{{}}}}}", k), v);
    }
    out
}

enum Section {
    None,
    Captures,
    Asserts,
}

pub fn run(gate: &Gate, scenario_rel_path: &str) {
    let full_path = repo_root().join(scenario_rel_path);
    assert!(full_path.exists(), "scenario file not found at {}", full_path.display());

    let content = std::fs::read_to_string(&full_path).unwrap_or_else(|e| {
        panic!("could not read scenario {}: {e}", full_path.display());
    });

    let mut vars: HashMap<String, String> = HashMap::new();
    vars.insert("base".to_string(), gate.base.clone());

    let mut current_method: Option<String> = None;
    let mut current_path: Option<String> = None;
    let mut current_headers: Vec<(String, String)> = Vec::new();
    let mut current_body_lines: Vec<String> = Vec::new();

    let mut resp_headers: reqwest::header::HeaderMap = reqwest::header::HeaderMap::new();
    let mut resp_bytes: Vec<u8> = Vec::new();
    let mut resp_json: Option<Value> = None;

    let mut section = Section::None;

    for (line_idx, raw_line) in content.lines().enumerate() {
        let line_no = line_idx + 1;
        let line = raw_line.trim();

        if line.starts_with('#') {
            continue;
        }

        if line == "[Captures]" {
            section = Section::Captures;
            continue;
        }
        if line == "[Asserts]" {
            section = Section::Asserts;
            continue;
        }

        // Section handling
        match section {
            Section::Captures => {
                if line.is_empty() {
                    section = Section::None;
                    continue;
                }
                if let Some((var_name, rest)) = line.split_once(':') {
                    let var_name = var_name.trim();
                    let rest = rest.trim();
                    if let Some(path) =
                        rest.strip_prefix("jsonpath").map(|s| s.trim().trim_matches('"'))
                    {
                        let json = resp_json.as_ref().unwrap_or_else(|| {
                            panic!(
                                "[{}:{line_no}] cannot capture jsonpath '{path}': response is not valid JSON\nBody: {}",
                                scenario_rel_path,
                                String::from_utf8_lossy(&resp_bytes)
                            );
                        });
                        let val = jsonpath_eval(json, path).unwrap_or_else(|| {
                            panic!(
                                "[{}:{line_no}] jsonpath '{path}' not found in response\nBody: {json}",
                                scenario_rel_path
                            );
                        });
                        let val_str = match val {
                            Value::String(s) => s.clone(),
                            Value::Number(n) => n.to_string(),
                            Value::Bool(b) => b.to_string(),
                            other => other.to_string(),
                        };
                        vars.insert(var_name.to_string(), val_str);
                    }
                }
                continue;
            }
            Section::Asserts => {
                if line.is_empty() {
                    section = Section::None;
                    continue;
                }
                // strip trailing comment like `; # "%PDF-1"`
                let clean_line =
                    if let Some((code, _)) = line.split_once('#') { code.trim() } else { line };
                let clean_line = clean_line.trim_end_matches(';').trim();

                if clean_line.starts_with("header ") {
                    // header "Content-Type" == "application/pdf"
                    let rest = clean_line.strip_prefix("header").unwrap().trim();
                    if let Some((hname, expected)) = rest.split_once("==") {
                        let hname = hname.trim().trim_matches('"');
                        let expected = sub_vars(expected.trim().trim_matches('"'), &vars);
                        let actual =
                            resp_headers.get(hname).and_then(|v| v.to_str().ok()).unwrap_or("");
                        assert!(
                            actual.contains(&expected),
                            "[{}:{line_no}] header '{}' expected '{}', got '{}'",
                            scenario_rel_path,
                            hname,
                            expected,
                            actual
                        );
                    }
                } else if clean_line.starts_with("bytes count >") {
                    let min_count: usize = clean_line
                        .strip_prefix("bytes count >")
                        .unwrap()
                        .trim()
                        .parse()
                        .expect("bytes count > N");
                    assert!(
                        resp_bytes.len() > min_count,
                        "[{}:{line_no}] expected bytes count > {}, got {}",
                        scenario_rel_path,
                        min_count,
                        resp_bytes.len()
                    );
                } else if clean_line.starts_with("bytes startsWith hex,") {
                    let hex_str = clean_line.strip_prefix("bytes startsWith hex,").unwrap().trim();
                    let expected_bytes = (0..hex_str.len())
                        .step_by(2)
                        .map(|i| u8::from_str_radix(&hex_str[i..i + 2], 16).expect("valid hex"))
                        .collect::<Vec<u8>>();
                    assert!(
                        resp_bytes.starts_with(&expected_bytes),
                        "[{}:{line_no}] response bytes do not start with expected magic bytes {hex_str}",
                        scenario_rel_path
                    );
                } else if clean_line.starts_with("jsonpath ") {
                    let rest = clean_line.strip_prefix("jsonpath").unwrap().trim();
                    let json = resp_json.as_ref().unwrap_or_else(|| {
                        panic!(
                            "[{}:{line_no}] cannot assert jsonpath: response is not valid JSON\nBody: {}",
                            scenario_rel_path,
                            String::from_utf8_lossy(&resp_bytes)
                        );
                    });

                    if let Some((path_str, expected_count)) = rest.split_once("count ==") {
                        let path = path_str.trim().trim_matches('"');
                        let expected: usize = expected_count.trim().parse().expect("count integer");
                        let val = jsonpath_eval(json, path).unwrap_or_else(|| {
                            panic!(
                                "[{}:{line_no}] jsonpath '{path}' not found: {json}",
                                scenario_rel_path
                            );
                        });
                        let actual = match val {
                            Value::Array(a) => a.len(),
                            Value::Object(o) => o.len(),
                            _ => panic!("[{}:{line_no}] jsonpath '{path}' is neither array nor object: {val}", scenario_rel_path),
                        };
                        assert_eq!(
                            actual, expected,
                            "[{}:{line_no}] jsonpath '{path}' expected count {}, got {}",
                            scenario_rel_path, expected, actual
                        );
                    } else if let Some((path_str, expected_sub)) = rest.split_once("startsWith") {
                        let path = path_str.trim().trim_matches('"');
                        let expected = sub_vars(expected_sub.trim().trim_matches('"'), &vars);
                        let val = jsonpath_eval(json, path).unwrap_or_else(|| {
                            panic!(
                                "[{}:{line_no}] jsonpath '{path}' not found: {json}",
                                scenario_rel_path
                            );
                        });
                        let s = val.as_str().unwrap_or_else(|| {
                            panic!(
                                "[{}:{line_no}] jsonpath '{path}' not a string: {val}",
                                scenario_rel_path
                            );
                        });
                        assert!(
                            s.starts_with(&expected),
                            "[{}:{line_no}] jsonpath '{path}' ('{s}') does not start with '{}'",
                            scenario_rel_path,
                            expected
                        );
                    } else if let Some((path_str, expected_item)) = rest.split_once("contains") {
                        let path = path_str.trim().trim_matches('"');
                        let expected = sub_vars(expected_item.trim().trim_matches('"'), &vars);
                        let val = jsonpath_eval(json, path).unwrap_or_else(|| {
                            panic!(
                                "[{}:{line_no}] jsonpath '{path}' not found: {json}",
                                scenario_rel_path
                            );
                        });
                        let found = match val {
                            Value::Array(a) => a.iter().any(|item| match item {
                                Value::String(s) => s == &expected,
                                Value::Number(n) => n.to_string() == expected,
                                Value::Bool(b) => b.to_string() == expected,
                                _ => false,
                            }),
                            Value::String(s) => s.contains(&expected),
                            _ => panic!(
                                "[{}:{line_no}] contains on unsupported json value: {val}",
                                scenario_rel_path
                            ),
                        };
                        assert!(
                            found,
                            "[{}:{line_no}] jsonpath '{path}' ({val}) does not contain '{expected}'",
                            scenario_rel_path
                        );
                    } else if let Some((path_str, expected_val)) = rest.split_once("==") {
                        let path = path_str.trim().trim_matches('"');
                        let expected_str = sub_vars(expected_val.trim(), &vars);
                        let val = jsonpath_eval(json, path).unwrap_or_else(|| {
                            panic!(
                                "[{}:{line_no}] jsonpath '{path}' not found: {json}",
                                scenario_rel_path
                            );
                        });

                        if expected_str.starts_with('"') && expected_str.ends_with('"') {
                            let expected = &expected_str[1..expected_str.len() - 1];
                            let actual = val.as_str().unwrap_or_else(|| {
                                panic!(
                                    "[{}:{line_no}] expected string '{expected}', got {val}",
                                    scenario_rel_path
                                );
                            });
                            assert_eq!(
                                actual, expected,
                                "[{}:{line_no}] jsonpath '{path}' string mismatch",
                                scenario_rel_path
                            );
                        } else if expected_str == "true" || expected_str == "false" {
                            let expected = expected_str == "true";
                            let actual = val.as_bool().unwrap_or_else(|| {
                                panic!(
                                    "[{}:{line_no}] expected bool {expected}, got {val}",
                                    scenario_rel_path
                                );
                            });
                            assert_eq!(
                                actual, expected,
                                "[{}:{line_no}] jsonpath '{path}' bool mismatch",
                                scenario_rel_path
                            );
                        } else if let Ok(expected_num) = expected_str.parse::<f64>() {
                            let actual = val.as_f64().unwrap_or_else(|| {
                                panic!(
                                    "[{}:{line_no}] expected number {expected_num}, got {val}",
                                    scenario_rel_path
                                );
                            });
                            assert!(
                                (actual - expected_num).abs() < 0.0001,
                                "[{}:{line_no}] jsonpath '{path}' expected {expected_num}, got {actual}",
                                scenario_rel_path
                            );
                        } else {
                            panic!(
                                "[{}:{line_no}] unhandled assertion value '{expected_str}'",
                                scenario_rel_path
                            );
                        }
                    }
                }
                continue;
            }
            Section::None => {}
        }

        // Request execution when encountering HTTP status
        if let Some(status_str) = line.strip_prefix("HTTP ") {
            let expected_status: u16 = status_str.trim().parse().expect("HTTP status integer");
            let method = current_method.take().unwrap_or_else(|| {
                panic!("[{}:{line_no}] HTTP without preceding request method", scenario_rel_path);
            });
            let path_with_vars = current_path.take().unwrap();
            let resolved_path = sub_vars(&path_with_vars, &vars);

            let url =
                if resolved_path.starts_with("http://") || resolved_path.starts_with("https://") {
                    resolved_path
                } else {
                    format!("{}{}", gate.base, resolved_path)
                };

            let req_method =
                reqwest::Method::from_bytes(method.as_bytes()).expect("valid HTTP method");
            let mut req = gate.client.request(req_method, &url);

            for (k, v) in current_headers.drain(..) {
                let resolved_v = sub_vars(&v, &vars);
                req = req.header(k, resolved_v);
            }

            if !current_body_lines.is_empty() {
                let body_raw = current_body_lines.join("\n");
                current_body_lines.clear();
                let resolved_body = sub_vars(&body_raw, &vars);
                req = req.header("Content-Type", "application/json").body(resolved_body);
            }

            let resp = req.send().unwrap_or_else(|e| {
                panic!("[{}:{line_no}] {method} {url}: transport error: {e}", scenario_rel_path);
            });

            let resp_status = resp.status().as_u16();
            resp_headers = resp.headers().clone();
            resp_bytes = resp.bytes().map(|b| b.to_vec()).unwrap_or_default();
            resp_json = serde_json::from_slice(&resp_bytes).ok();

            assert_eq!(
                resp_status,
                expected_status,
                "[{}:{line_no}] {method} {url} expected HTTP {expected_status}, got {resp_status}\nResponse: {}",
                scenario_rel_path,
                String::from_utf8_lossy(&resp_bytes)
            );
            continue;
        }

        // Method line: GET, POST, PUT, DELETE, PATCH
        let mut parts = line.split_whitespace();
        if let Some(first) = parts.next() {
            if ["GET", "POST", "PUT", "DELETE", "PATCH"].contains(&first) {
                if let Some(target) = parts.next() {
                    current_method = Some(first.to_string());
                    current_path = Some(target.to_string());
                    current_headers.clear();
                    current_body_lines.clear();
                    section = Section::None;
                    continue;
                }
            }
        }

        // Headers
        if current_method.is_some()
            && current_body_lines.is_empty()
            && line.contains(':')
            && !line.starts_with('{')
        {
            if let Some((hk, hv)) = line.split_once(':') {
                current_headers.push((hk.trim().to_string(), hv.trim().to_string()));
                continue;
            }
        }

        // Body lines
        if current_method.is_some() && (line.starts_with('{') || !current_body_lines.is_empty()) {
            current_body_lines.push(raw_line.to_string());
            continue;
        }
    }
}

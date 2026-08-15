//! The goal runner's client for `contract:registry`, and the generation boundary
//! it drives.
//!
//! ## What happens at a boundary, in order
//!
//! 1. **Ratify.** A part that passed its own gate against an amendment it granted
//!    has demonstrated it can implement what it agreed to, so that version becomes
//!    canonical. Until then the other parts keep building on the last ratified one
//!    — an amendment is a promotion, and only a passing gate promotes (ADR-0084).
//! 2. **Report what is outstanding.** Requests nobody has answered stay pending
//!    and the run continues on the current contract. That is the liveness rule
//!    made concrete: an unanswered question costs a generation, never a deadlock.
//! 3. **Read the contract back.** Whatever is canonical now is what the next round
//!    builds against.
//!
//! ## Answering
//!
//! A pending request is answered by ONE cheap model call carrying the asked part's
//! goal, what it has built, the contract as it stands, and the question. The reply
//! is parsed strictly, and **an unparseable reply answers nothing** — the request
//! stays pending and is retried at the next boundary. Inventing a verdict from a
//! reply nobody could read would put a denial the model never made into the
//! record, and a denial is the one answer that cannot be taken back.
//!
//! An `Answerer` is optional. Without one, `boundary` reports what is outstanding
//! and the run continues on the current contract — which is a supported way to run
//! and the shape a deployment with no provider gets.
//!
//! Unlike `memory.rs`, a failure here is **not** always survivable. A contract that
//! cannot be read is a run whose parts have nothing to agree on, and carrying on
//! would produce two halves that each pass and cannot compose. So `current` and
//! `composable` propagate their errors; only `ratify` is best-effort, because an
//! unratified amendment costs a generation rather than correctness.

use std::time::Duration;

use serde_json::Value;

use crate::generation::PartOutcome;

#[derive(Clone)]
pub struct Registry {
    pub url: String,
    pub host: String,
    pub timeout: Duration,
}

/// A version of the interface the parts build against.
#[derive(Clone, Debug, PartialEq)]
pub struct Version {
    pub number: u32,
    pub body: String,
    pub canonical: bool,
    /// The part that must ratify it, or empty for the human's first contract.
    pub owner: String,
}

/// One thing a part asked another for.
#[derive(Clone, Debug, PartialEq)]
pub struct Ask {
    pub id: String,
    pub from_part: String,
    pub to_part: String,
    pub subject: String,
    pub body: String,
    pub at_version: u32,
}

fn client(timeout: Duration) -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder().timeout(timeout).build().expect("http client")
}

fn enc(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            ' ' => "+".to_string(),
            other => other.to_string().bytes().map(|b| format!("%{b:02X}")).collect::<String>(),
        })
        .collect()
}

/// A registry answer, or what it refused with. The probe reports a refusal as a
/// 200 carrying `{"error":…}`, so the transport being fine says nothing about the
/// call having worked.
fn unwrap_answer(v: Value) -> Result<Value, String> {
    if let Some(kind) = v["error"].as_str() {
        return Err(format!("{kind}: {}", v["detail"].as_str().unwrap_or_default()));
    }
    Ok(v)
}

impl Registry {
    fn call(&self, method: reqwest::Method, path: &str, body: String) -> Result<Value, String> {
        let r = client(self.timeout)
            .request(method, format!("{}{path}", self.url))
            .header("host", &self.host)
            .body(body)
            .send()
            .map_err(|e| format!("{e}"))?;
        let text = r.text().unwrap_or_default();
        let v: Value =
            serde_json::from_str(&text).map_err(|e| format!("unreadable answer ({e}): {text}"))?;
        unwrap_answer(v)
    }

    /// The human's contract, once, at the start of a run.
    pub fn publish(&self, body: &str) -> Result<u32, String> {
        let v = self.call(reqwest::Method::POST, "/publish", body.to_string())?;
        v["version"].as_u64().map(|n| n as u32).ok_or_else(|| format!("no version in {v}"))
    }

    /// What the parts build against: the latest RATIFIED version.
    pub fn current(&self) -> Result<Version, String> {
        let v = self.call(reqwest::Method::GET, "/current", String::new())?;
        Ok(Version {
            number: v["version"].as_u64().unwrap_or(0) as u32,
            body: v["body"].as_str().unwrap_or_default().to_string(),
            canonical: v["canonical"].as_bool().unwrap_or(false),
            owner: v["owner"].as_str().unwrap_or_default().to_string(),
        })
    }

    /// The amendment this part granted and has not yet demonstrated, if any.
    pub fn proposed(&self, part: &str) -> Result<Option<Version>, String> {
        let v = self.call(
            reqwest::Method::GET,
            &format!("/proposed?part={}", enc(part)),
            String::new(),
        )?;
        if v["found"] == Value::Bool(false) {
            return Ok(None);
        }
        Ok(Some(Version {
            number: v["version"].as_u64().unwrap_or(0) as u32,
            body: v["body"].as_str().unwrap_or_default().to_string(),
            canonical: v["canonical"].as_bool().unwrap_or(false),
            owner: v["owner"].as_str().unwrap_or_default().to_string(),
        }))
    }

    pub fn get(&self, version: u32) -> Result<Option<Version>, String> {
        let v = self.call(reqwest::Method::GET, &format!("/get?version={version}"), String::new())?;
        if v["found"] == Value::Bool(false) {
            return Ok(None);
        }
        Ok(Some(Version {
            number: v["version"].as_u64().unwrap_or(0) as u32,
            body: v["body"].as_str().unwrap_or_default().to_string(),
            canonical: v["canonical"].as_bool().unwrap_or(false),
            owner: v["owner"].as_str().unwrap_or_default().to_string(),
        }))
    }

    pub fn ask(&self, a: &Ask) -> Result<String, String> {
        let v = self.call(
            reqwest::Method::POST,
            &format!(
                "/ask?from={}&to={}&subject={}&at={}",
                enc(&a.from_part),
                enc(&a.to_part),
                enc(&a.subject),
                a.at_version
            ),
            a.body.clone(),
        )?;
        v["id"].as_str().map(str::to_string).ok_or_else(|| format!("no id in {v}"))
    }

    pub fn pending(&self, part: &str) -> Result<Vec<Ask>, String> {
        let v =
            self.call(reqwest::Method::GET, &format!("/pending?part={}", enc(part)), String::new())?;
        Ok(v["requests"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|r| Ask {
                id: r["id"].as_str().unwrap_or_default().to_string(),
                from_part: r["from_part"].as_str().unwrap_or_default().to_string(),
                to_part: r["to_part"].as_str().unwrap_or_default().to_string(),
                subject: r["subject"].as_str().unwrap_or_default().to_string(),
                body: r["body"].as_str().unwrap_or_default().to_string(),
                at_version: r["at_version"].as_u64().unwrap_or(0) as u32,
            })
            .collect())
    }

    /// `verdict` is `granted` | `denied` | `counter`; the returned version is 0 for
    /// anything but a grant, because only a grant moves the contract.
    pub fn answer(&self, id: &str, verdict: &str, body: &str) -> Result<u32, String> {
        let v = self.call(
            reqwest::Method::POST,
            &format!("/answer?id={}&verdict={}", enc(id), enc(verdict)),
            body.to_string(),
        )?;
        Ok(v["version"].as_u64().unwrap_or(0) as u32)
    }

    pub fn ratify(&self, version: u32, part: &str, gate_score: u64) -> Result<(), String> {
        self.call(
            reqwest::Method::POST,
            &format!("/ratify?version={version}&part={}&score={gate_score}", enc(part)),
            String::new(),
        )
        .map(|_| ())
    }

    pub fn built_against(&self, candidate: &str, part: &str, version: u32) -> Result<(), String> {
        self.call(
            reqwest::Method::POST,
            &format!(
                "/built-against?candidate={}&part={}&version={version}",
                enc(candidate),
                enc(part)
            ),
            String::new(),
        )
        .map(|_| ())
    }

    /// Empty means composable. Otherwise one line per disagreement.
    pub fn composable(&self, candidates: &[String]) -> Result<Vec<String>, String> {
        let v = self.call(
            reqwest::Method::GET,
            &format!("/composable?candidates={}", enc(&candidates.join(","))),
            String::new(),
        )?;
        Ok(v["problems"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|p| p.as_str().map(str::to_string))
            .collect())
    }

    /// The generation boundary: ratify what has been demonstrated, count what is
    /// still outstanding, and hand back what the next round builds against.
    ///
    /// Errors are reported into `log` rather than returned, except the final read:
    /// a boundary that cannot say what the contract is has nothing to hand the next
    /// round, and continuing on a guess is how two halves that each pass fail
    /// together.
    pub fn boundary(
        &self,
        outcomes: &[PartOutcome],
        answerer: Option<&Answerer>,
        log: &mut Vec<String>,
    ) -> Result<Vec<(String, String, u32)>, String> {
        for (version, part, score) in ratifiable(outcomes) {
            match self.ratify(version, &part, score) {
                Ok(()) => log.push(format!("{part} ratified contract v{version}")),
                // Best-effort on purpose: an unratified amendment costs a
                // generation, not correctness — the other parts simply keep
                // building against the last canonical version.
                Err(e) => log.push(format!("{part} could not ratify v{version}: {e}")),
            }
        }
        let contract = self.current()?;
        for o in outcomes {
            let asks = match self.pending(&o.part) {
                Ok(a) => a,
                Err(e) => {
                    log.push(format!("could not read {}'s requests: {e}", o.part));
                    continue;
                }
            };
            if asks.is_empty() {
                continue;
            }
            let Some(answerer) = answerer else {
                log.push(format!(
                    "{} has {} unanswered request(s): {} — no answerer is configured, so the run \
                     continues on the current contract",
                    o.part,
                    asks.len(),
                    asks.iter().map(|a| a.subject.clone()).collect::<Vec<_>>().join("; ")
                ));
                continue;
            };
            let state = state_of(o);
            for ask in &asks {
                let asked = Asked { part: &o.part, goal: "", state: &state };
                match answerer
                    .reply_to(&prompt(ask, &asked, &contract.body))
                    .and_then(|reply| parse_reply(&reply, &contract.body))
                    .and_then(|(verdict, body)| {
                        self.answer(&ask.id, &verdict, &body).map(|v| (verdict, v))
                    }) {
                    Ok((verdict, 0)) => {
                        log.push(format!("{} {verdict} {:?}", o.part, ask.subject))
                    }
                    Ok((verdict, version)) => log.push(format!(
                        "{} {verdict} {:?} → contract v{version} proposed, canonical once {} \
                         passes its gate against it",
                        o.part, ask.subject, o.part
                    )),
                    // Unanswered, deliberately: it stays pending and is retried at
                    // the next boundary. A verdict invented from a reply nobody
                    // could read is a denial the model never made.
                    Err(e) => log.push(format!(
                        "{} could not answer {:?} ({e}) — left pending",
                        o.part, ask.subject
                    )),
                }
            }
        }
        // Read it back AFTER answering: a grant made in this pass has to reach the
        // next round, or the part that granted it never gets the chance to
        // demonstrate it.
        let canonical = self.current()?;
        let mut next = Vec::new();
        for o in outcomes {
            // The owner of an undemonstrated proposal builds against ITS OWN
            // proposal; everyone else stays on the last ratified version. Without
            // this a granted amendment can never be ratified, because ratification
            // means "I passed my gate against it" and nothing would ever hand it to
            // the part that has to pass.
            match self.proposed(&o.part) {
                Ok(Some(p)) if p.number > canonical.number => {
                    log.push(format!(
                        "{} builds against its own proposal v{} until its gate passes against it",
                        o.part, p.number
                    ));
                    next.push((o.part.clone(), p.body, p.number));
                }
                Ok(_) => next.push((o.part.clone(), canonical.body.clone(), canonical.number)),
                Err(e) => {
                    log.push(format!("could not read {}'s proposal ({e}) — using the canonical one", o.part));
                    next.push((o.part.clone(), canonical.body.clone(), canonical.number));
                }
            }
        }
        Ok(next)
    }
}

// ---------------------------------------------------------------- answering

/// The model that answers a request on a part's behalf.
///
/// A separate struct from `Registry` because it is a different deployment
/// decision: the registry is where the run's agreements live, and this is whichever
/// provider the deployment is willing to spend a small call on.
#[derive(Clone)]
pub struct Answerer {
    pub url: String,
    pub host: String,
    pub timeout: Duration,
}

/// What the answering part knows that the asker does not.
pub struct Asked<'a> {
    /// The part being asked — it is answering as this.
    pub part: &'a str,
    /// What that part was told to build.
    pub goal: &'a str,
    /// Where it has got to: its best score, and what is still failing. Not its
    /// code — the question is about an interface, and a diff would spend the
    /// budget on tokens that cannot change the answer.
    pub state: &'a str,
}

/// The one strict format the reply has to be in.
///
/// Strict on purpose. A free-form answer would have to be interpreted, and an
/// interpretation of "well, maybe" that resolves to `granted` amends the contract
/// every other part builds against.
pub fn prompt(ask: &Ask, asked: &Asked, contract: &str) -> String {
    format!(
        "You are the {part} half of one application. Another part has asked you to \
change the shared interface. You own this surface, so the decision is yours.\n\n\
WHAT YOU ARE BUILDING:\n{goal}\n\n\
WHERE YOU HAVE GOT TO:\n{state}\n\n\
THE INTERFACE AS IT STANDS (contract v{version}):\n{contract}\n\n\
{from} ASKS: {subject}\n{body}\n\n\
Answer in exactly this format and nothing else:\n\n\
VERDICT: granted|denied|counter\n\
---\n\
<for granted: the COMPLETE amended interface, in the same format as above, \
including everything that has not changed>\n\
<for denied: why, in one or two sentences — they will read it and may ask again>\n\
<for counter: the alternative and why it is better for both of you>\n\n\
Grant only what you can actually implement. If it costs you something the asker \
cannot see — a full scan, a migration, a round trip — counter with what is cheap \
and say what it would have cost.",
        part = asked.part,
        goal = asked.goal,
        state = asked.state,
        version = ask.at_version,
        contract = contract,
        from = ask.from_part,
        subject = ask.subject,
        body = ask.body,
    )
}

/// Pull a verdict and a body out of a reply, or refuse to.
///
/// `current` is used for one check that is worth its cost: if the contract in force
/// is valid JSON, an amendment must be too. A model that answers `granted` with a
/// paragraph of prose has not amended anything, and storing that paragraph as the
/// interface would break every part at once.
pub fn parse_reply(reply: &str, current: &str) -> Result<(String, String), String> {
    let text = reply.trim();
    let mut lines = text.lines();
    let head = lines
        .find(|l| !l.trim().is_empty())
        .ok_or_else(|| "the model answered nothing".to_string())?;
    let verdict = head
        .trim()
        .strip_prefix("VERDICT:")
        .ok_or_else(|| format!("no VERDICT line: {:?}", head.trim()))?
        .trim()
        .to_lowercase();
    if !matches!(verdict.as_str(), "granted" | "denied" | "counter") {
        return Err(format!("{verdict:?} is not a verdict"));
    }
    // Everything after the separator. A model that omits it has usually also
    // omitted the body, which the next check catches.
    let body = match text.split_once("---") {
        Some((_, rest)) => rest.trim().to_string(),
        None => String::new(),
    };
    if body.is_empty() {
        return Err(match verdict.as_str() {
            "granted" => "granted with no amended interface".into(),
            _ => "refused with no reason — the asker will only ask again".to_string(),
        });
    }
    if verdict == "granted"
        && serde_json::from_str::<Value>(current).is_ok()
        && serde_json::from_str::<Value>(&body).is_err()
    {
        return Err("granted, but the amendment is not the interface — the contract in force \
                    is JSON and this is not"
            .into());
    }
    Ok((verdict, body))
}

impl Answerer {
    /// One call, one answer. The reply is returned raw; parsing is separate so the
    /// strictness is testable without a provider.
    pub fn reply_to(&self, prompt: &str) -> Result<String, String> {
        let r = client(self.timeout)
            .post(format!("{}/chat", self.url))
            .header("host", &self.host)
            .body(prompt.to_string())
            .send()
            .map_err(|e| format!("{e}"))?;
        let text = r.text().unwrap_or_default();
        let v: Value =
            serde_json::from_str(&text).map_err(|e| format!("unreadable answer ({e}): {text}"))?;
        let v = unwrap_answer(v)?;
        v["text"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("no text in the model's answer: {v}"))
    }
}

/// Where a part has got to, in a sentence the model can use. Not its code: the
/// question is about an interface, and a diff spends budget on tokens that cannot
/// change the answer.
pub fn state_of(o: &PartOutcome) -> String {
    match &o.best {
        None => format!("{} has produced nothing yet.", o.part),
        Some(b) => format!(
            "{} is at score {} after {} generation(s){}.",
            o.part,
            b.score,
            o.rounds.len(),
            if b.accepted { ", and its gate passes" } else { ", and its gate does not pass yet" }
        ),
    }
}

/// Which amendments a part has earned the right to make canonical.
///
/// A part ratifies a version when it PASSED its own gate against it. Two things
/// this deliberately does not do: ratify on behalf of a part that did not accept
/// (an amendment nobody implemented is not demonstrated), and ratify a version a
/// part did not build against (passing against v3 says nothing about v4).
///
/// Split out as a pure function because it is the decision, and the HTTP around it
/// is not.
pub fn ratifiable(outcomes: &[PartOutcome]) -> Vec<(u32, String, u64)> {
    outcomes
        .iter()
        .filter(|o| o.accepted && o.built_against > 0)
        .filter_map(|o| {
            o.best.as_ref().map(|b| (o.built_against, o.part.clone(), b.score))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation::{Entry, PartOutcome, Round};
    use serde_json::json;

    fn outcome(part: &str, accepted: bool, version: u32, score: u64) -> PartOutcome {
        PartOutcome {
            part: part.into(),
            rounds: vec![Round { entries: vec![], best: None }],
            best: Some(Entry {
                branch: "branch-0".into(),
                accepted,
                score,
                digest: "d".into(),
                spent_tokens: 0,
                attempts: 1,
                files: json!([]),
                failures: json!([]),
                note: String::new(),
                elapsed_ms: 0,
                stopped: "accepted".into(),
            }),
            accepted,
            spent_tokens: 0,
            built_against: version,
        }
    }

    #[test]
    fn only_a_part_that_passed_may_ratify_and_only_its_own_version() {
        let outcomes = vec![
            outcome("backend", true, 2, 1000),
            outcome("frontend", false, 2, 400),
        ];
        let out = ratifiable(&outcomes);
        assert_eq!(out, vec![(2, "backend".to_string(), 1000)]);
    }

    #[test]
    fn a_part_that_built_against_nothing_ratifies_nothing() {
        // version 0 is "no contract recorded" — ratifying it would make a version
        // that does not exist canonical.
        assert!(ratifiable(&[outcome("backend", true, 0, 1000)]).is_empty());
    }

    #[test]
    fn a_refusal_is_an_error_even_though_the_transport_was_fine() {
        // The probe answers a refusal with a 200, so the shape has to be read
        // rather than the status code.
        let refused = json!({ "error": "refused", "detail": "v2 is owned by \"backend\"" });
        let e = unwrap_answer(refused).expect_err("a refusal must not read as success");
        assert!(e.contains("refused"), "{e}");
        assert!(e.contains("owned by"), "the reason has to survive: {e}");
        assert!(unwrap_answer(json!({ "version": 3 })).is_ok());
    }

    #[test]
    fn a_reply_is_read_strictly_or_not_at_all() {
        let json_contract = r#"{"routes":[]}"#;
        let (v, b) = parse_reply(
            "VERDICT: granted\n---\n{\"routes\":[\"/api/search\"]}",
            json_contract,
        )
        .expect("a well-formed grant");
        assert_eq!(v, "granted");
        assert!(b.contains("/api/search"));

        let (v, b) = parse_reply(
            "VERDICT: counter\n---\nuse has_more; total pages costs a COUNT on every query",
            json_contract,
        )
        .expect("a counter is prose, even when the contract is JSON");
        assert_eq!(v, "counter");
        assert!(b.starts_with("use has_more"));

        // The reply the strictness exists for: a model that says something
        // plausible and amends nothing.
        assert!(parse_reply("Sure, that seems reasonable to me!", json_contract).is_err());
        assert!(parse_reply("VERDICT: maybe\n---\nwell", json_contract).is_err());
        assert!(parse_reply("VERDICT: granted\n---\n", json_contract).is_err());
        assert!(parse_reply("VERDICT: denied", json_contract).is_err(), "a denial needs a reason");
        assert!(parse_reply("", json_contract).is_err());
    }

    #[test]
    fn a_grant_that_is_prose_is_not_an_amendment() {
        // The contract in force is JSON, so an amendment must be too — storing a
        // paragraph as the interface would break every part at once.
        let e = parse_reply(
            "VERDICT: granted\n---\nYes, I will add total_pages to the response.",
            r#"{"routes":[]}"#,
        )
        .expect_err("prose is not an interface");
        assert!(e.contains("not the interface"), "{e}");
        // But a contract that is not JSON in the first place imposes no such rule:
        // the format is the goal's business, not the registry's.
        assert!(parse_reply(
            "VERDICT: granted\n---\nGET /api/search -> { hits, total_pages }",
            "GET /api/search -> { hits }",
        )
        .is_ok());
    }

    #[test]
    fn the_prompt_carries_what_only_the_asked_part_knows() {
        let ask = Ask {
            id: "9f3a".into(),
            from_part: "frontend".into(),
            to_part: "backend".into(),
            subject: "SearchResult needs total_pages".into(),
            body: "I cannot paginate from next_cursor alone".into(),
            at_version: 3,
        };
        let asked = Asked {
            part: "backend",
            goal: "serve /api/search over the corpus",
            state: "backend is at score 700 after 2 generation(s), and its gate does not pass yet.",
        };
        let p = prompt(&ask, &asked, r#"{"routes":["/api/search"]}"#);
        assert!(p.contains("You are the backend half"), "{p}");
        assert!(p.contains("serve /api/search over the corpus"), "the goal is in it");
        assert!(p.contains("score 700"), "and where it has got to");
        assert!(p.contains("contract v3"), "and which version is being asked about");
        assert!(p.contains("frontend ASKS: SearchResult needs total_pages"), "{p}");
        assert!(p.contains("VERDICT: granted|denied|counter"), "the format is stated");
        assert!(p.contains("counter with what is cheap"), "and the reason counters exist");
    }

    #[test]
    fn what_the_model_is_told_about_a_part_is_a_sentence_not_a_diff() {
        let none = PartOutcome {
            part: "frontend".into(),
            rounds: vec![],
            best: None,
            accepted: false,
            spent_tokens: 0,
            built_against: 0,
        };
        assert_eq!(state_of(&none), "frontend has produced nothing yet.");
        let some = outcome("backend", true, 2, 1000);
        let s = state_of(&some);
        assert!(s.contains("score 1000"), "{s}");
        assert!(s.contains("its gate passes"), "{s}");
    }

    #[test]
    fn a_subject_survives_being_put_in_a_query_string() {
        assert_eq!(enc("SearchResult needs total_pages"), "SearchResult+needs+total_pages");
        assert_eq!(enc("GET /api/search?q="), "GET+%2Fapi%2Fsearch%3Fq%3D");
    }
}

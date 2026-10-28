//! Food photos read by Jev, which only answers typed questions: a dish not in the person's pool
//! joins it, with its photo as the reference for next time.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::agent::{Cause, Host};
use crate::jev::{self, Jev};
use crate::model::Image;
use crate::spec::validate_topic;

pub const VARIETIES: &[&str] = &[
    "salad",
    "soup or stew",
    "pasta",
    "rice or grain dish",
    "meat dish",
    "fish or seafood dish",
    "vegetable dish",
    "sandwich or burger",
    "pizza",
    "eggs or breakfast",
    "bread or pastry",
    "fruit",
    "dessert or sweets",
    "snack",
    "drink",
    "other",
];

/// Jev's score is the probability-weighted band number, so a fraction lands between two bands.
const BANDS: &[(&str, f64)] = &[
    ("a drink or a bite, under 150 kcal", 75.0),
    ("a small portion, 150 to 300 kcal", 225.0),
    ("a light meal, 300 to 450 kcal", 375.0),
    ("an ordinary meal, 450 to 600 kcal", 525.0),
    ("a large meal, 600 to 800 kcal", 700.0),
    ("a very large meal, 800 to 1000 kcal", 900.0),
    ("a huge plate, 1000 to 1300 kcal", 1150.0),
    ("an enormous plate, over 1300 kcal", 1500.0),
];

const NONE: &str = "none of these";
const MAX_CANDIDATES: usize = 12;
const FOOD_AT: f64 = 0.5;
const MATCH_AT: f64 = 0.5;
/// Jev's cost grows with the pixels, and a plate is clear at this size.
pub const QUERY_PX: u32 = 800;
const REFERENCE_PX: u32 = 400;
const DAY: u64 = 86_400;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Item {
    pub name: String,
    pub variety: String,
    pub kcal: u32,
    pub seen: u32,
    pub estimated: bool,
    pub photo: Option<String>,
    pub first: u64,
    pub last: u64,
}

pub fn namespace(user: &str) -> Result<String, String> {
    let user = if user.is_empty() { "owner" } else { user };
    let ns = format!("food.{user}");
    validate_topic(&ns).map_err(|_| format!("`{user}`: use a-z 0-9 . - _ for a user"))?;
    Ok(ns)
}

pub fn kcal_at(score: f64) -> u32 {
    let top = (BANDS.len() - 1) as f64;
    let s = score.clamp(0.0, top);
    let (lo, hi) = (s.floor() as usize, (s.ceil() as usize).min(BANDS.len() - 1));
    let f = s - s.floor();
    let kcal = BANDS[lo].1 * (1.0 - f) + BANDS[hi].1 * f;
    ((kcal / 10.0).round() as u32) * 10
}

fn pool(host: &dyn Host, ns: &str) -> Result<Vec<(String, Item)>, String> {
    let mut items: Vec<(String, Item)> = host
        .kv_list(ns, "pool/")?
        .into_iter()
        .filter_map(|(k, e)| Some((k, serde_json::from_str::<Item>(&e.value).ok()?)))
        .collect();
    items.sort_by(|a, b| b.1.seen.cmp(&a.1.seen).then(b.1.last.cmp(&a.1.last)));
    Ok(items)
}

fn put_item(
    host: &dyn Host,
    agent: &str,
    ns: &str,
    key: &str,
    item: &Item,
    cause: &Cause,
) -> Result<(), String> {
    let value = serde_json::to_string(item).map_err(|e| e.to_string())?;
    host.kv_put(agent, ns, key, &value, None, cause).map(|_| ())
}

fn photo_dir(host: &dyn Host, agent: &str) -> Result<std::path::PathBuf, String> {
    let dir = host.store().workspace(agent)?.join("food-pool");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// A reference no other text will contain.
fn reference(tag: &str, n: usize) -> String {
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(format!("{tag}/{n}").as_bytes());
    format!("ref-{}", h.iter().take(8).map(|b| format!("{b:02x}")).collect::<String>())
}

fn percent(p: f64) -> u32 {
    (p * 100.0).round().clamp(0.0, 100.0) as u32
}

/// (kcal, meals) in the last day.
fn last_day(host: &dyn Host, ns: &str, now: u64) -> (u32, usize) {
    let meals = host.kv_list(ns, "meal/").unwrap_or_default();
    meals
        .iter()
        .filter_map(|(_, e)| serde_json::from_str::<Value>(&e.value).ok())
        .filter(|m| m["at"].as_u64().unwrap_or(0) + DAY > now)
        .fold((0, 0), |(k, n), m| (k + m["kcal"].as_u64().unwrap_or(0) as u32, n + 1))
}

fn log_meal(
    host: &dyn Host,
    agent: &str,
    ns: &str,
    item: &Item,
    new: bool,
    now: u64,
    cause: &Cause,
) -> Result<(), String> {
    let meal = json!({"at": now, "name": item.name, "variety": item.variety,
                      "kcal": item.kcal, "estimated": item.estimated, "new": new});
    host.kv_put(agent, ns, &format!("meal/{now:012}"), &meal.to_string(), None, cause).map(|_| ())
}

pub fn identify(
    host: &dyn Host,
    jev: &dyn Jev,
    agent: &str,
    user: &str,
    photo: &Image,
    cause: &Cause,
) -> Result<String, String> {
    let ns = namespace(user)?;
    let now = host.now();
    let all = pool(host, &ns)?;
    let dir = photo_dir(host, agent)?;

    let mut assets = Map::new();
    let q_ref = reference("photo", 0);
    assets.insert(q_ref.clone(), jev::asset(&photo.media_type, &photo.data));

    let candidates: Vec<&(String, Item)> =
        all.iter().filter(|(_, i)| i.name != NONE).take(MAX_CANDIDATES).collect();
    let mut options = Map::new();
    for (n, (_, item)) in candidates.iter().enumerate() {
        let about = format!("{} ({}), about {} kcal", item.name, item.variety, item.kcal);
        let bytes = item.photo.as_ref().and_then(|f| {
            let media = if f.ends_with(".png") { "image/png" } else { "image/jpeg" };
            Some((media, std::fs::read(dir.join(f)).ok()?))
        });
        options.insert(
            item.name.clone(),
            match bytes {
                Some((media, b)) => {
                    let r = reference("candidate", n);
                    assets.insert(r.clone(), jev::asset(media, &b));
                    json!({"description": about, "examples": [r]})
                }
                None => json!(about),
            },
        );
    }
    options
        .insert(NONE.into(), json!("A different dish from all of the above, or no food at all."));

    let mut questions = Map::new();
    questions.insert(
        "is_food".into(),
        json!({"type": "noul", "instructions": "Is there food or a drink in `photo`?"}),
    );
    questions.insert(
        "variety".into(),
        json!({"type": "choice",
               "instructions": "What kind of dish is the main food in `photo`?",
               "criteria": VARIETIES.iter().map(|v| (v.to_string(), Value::Null))
                   .collect::<Map<_, _>>()}),
    );
    questions.insert(
        "kcal".into(),
        json!({"type": "score",
               "instructions": "How much food energy is on everything to eat or drink in `photo`, the portion as shown?",
               "criteria": BANDS.iter().map(|b| b.0).collect::<Vec<_>>()}),
    );
    if !candidates.is_empty() {
        questions.insert(
            "match".into(),
            json!({"type": "choice",
                   "instructions": "Which of these is the same dish as the main food in `photo`? Answer 'none of these' unless it clearly is one of them.",
                   "criteria": options}),
        );
    }
    let request = json!({
        "model": jev::MODEL,
        "state": {"photo": q_ref},
        "assets": assets,
        "questions": questions,
    });
    let answers = jev.ask(&request)?;

    let is_food = jev::noul(&answers, "is_food").ok_or("Jev did not say whether it is food")?;
    if is_food < FOOD_AT {
        return Ok(format!(
            "There is no food or drink in the photo (Jev is {}% sure). Nothing was stored.",
            percent(1.0 - is_food)
        ));
    }

    if let Some((picked, p)) = jev::choice(&answers, "match") {
        if picked != NONE && p >= MATCH_AT {
            if let Some((key, item)) = candidates.iter().find(|(_, i)| i.name == picked) {
                let mut item = item.clone();
                item.seen += 1;
                item.last = now;
                put_item(host, agent, &ns, key, &item, cause)?;
                log_meal(host, agent, &ns, &item, false, now, cause)?;
                let (kcal, meals) = last_day(host, &ns, now);
                return Ok(format!(
                    "Known food: {} ({}), {} kcal{}. Jev matched it to the pool at {}%; seen {} times. \
                     Logged. Last 24 h: {kcal} kcal in {meals} meals.",
                    item.name,
                    item.variety,
                    item.kcal,
                    if item.estimated { " (an estimate, not confirmed)" } else { "" },
                    percent(p),
                    item.seen,
                ));
            }
        }
    }

    let (variety, v_p) = jev::choice(&answers, "variety")
        .filter(|(v, _)| VARIETIES.contains(&v.as_str()))
        .unwrap_or(("other".into(), 0.0));
    let kcal = kcal_at(jev::score(&answers, "kcal").ok_or("Jev did not estimate the energy")?);
    let n = all
        .iter()
        .filter_map(|(k, _)| k.strip_prefix("pool/f")?.parse::<u32>().ok())
        .max()
        .unwrap_or(0)
        + 1;
    let same = all.iter().filter(|(_, i)| i.variety == variety).count() + 1;
    let photo_file = crate::attach::shrunk(photo.data.clone(), &photo.media_type, REFERENCE_PX)
        .and_then(|i| {
            let file = format!("f{n}.{}", if i.media_type == "image/png" { "png" } else { "jpg" });
            std::fs::write(dir.join(&file), i.data).ok().map(|_| file)
        });
    let item = Item {
        name: format!("{variety} {same}"),
        variety: variety.clone(),
        kcal,
        seen: 1,
        estimated: true,
        photo: photo_file,
        first: now,
        last: now,
    };
    put_item(host, agent, &ns, &format!("pool/f{n}"), &item, cause)?;
    log_meal(host, agent, &ns, &item, true, now, cause)?;
    let (day_kcal, meals) = last_day(host, &ns, now);
    let near = match jev::choice(&answers, "match") {
        Some((p, pp)) if p != NONE => format!(" The closest was {p} at {}%.", percent(pp)),
        _ => String::new(),
    };
    Ok(format!(
        "New food, added to the pool as \"{}\" (id f{n}): {variety} (Jev {}% sure), about {kcal} kcal. \
         This is an estimate from the photo alone, and the portion may be off.{near} Logged. \
         Last 24 h: {day_kcal} kcal in {meals} meals. If Mark says what it is or how much it really \
         holds, call food_correct with name \"f{n}\".",
        item.name,
        percent(v_p),
    ))
}

pub fn list(host: &dyn Host, user: &str) -> Result<String, String> {
    let ns = namespace(user)?;
    let items = pool(host, &ns)?;
    let (kcal, meals) = last_day(host, &ns, host.now());
    if items.is_empty() {
        return Ok("The pool is empty: no food photo has been read yet.".into());
    }
    let mut out: Vec<String> = items
        .iter()
        .take(60)
        .map(|(k, i)| {
            format!(
                "- {} (id {}): {}, {} kcal{}, seen {}x",
                i.name,
                k.trim_start_matches("pool/"),
                i.variety,
                i.kcal,
                if i.estimated { " (estimate)" } else { "" },
                i.seen
            )
        })
        .collect();
    out.push(format!("Last 24 h: {kcal} kcal in {meals} meals."));
    Ok(out.join("\n"))
}

pub fn correct(
    host: &dyn Host,
    agent: &str,
    user: &str,
    args: &Value,
    cause: &Cause,
) -> Result<String, String> {
    let ns = namespace(user)?;
    let which = args.get("name").and_then(Value::as_str).unwrap_or_default().trim().to_lowercase();
    if which.is_empty() {
        return Err("`name` is empty: give the food's name or its id (f3)".into());
    }
    let items = pool(host, &ns)?;
    let (key, mut item) = items
        .into_iter()
        .find(|(k, i)| k.trim_start_matches("pool/") == which || i.name.to_lowercase() == which)
        .ok_or_else(|| format!("no food `{which}` in the pool; food_pool lists them"))?;
    let mut said = Vec::new();
    if let Some(k) =
        args.get("kcal").and_then(|k| k.as_u64().or_else(|| k.as_str()?.trim().parse().ok()))
    {
        if !(1..=5000).contains(&k) {
            return Err("`kcal` should be between 1 and 5000".into());
        }
        item.kcal = k as u32;
        said.push(format!("{k} kcal"));
    }
    if let Some(n) =
        args.get("new_name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty())
    {
        if n.len() > 60 || n == NONE {
            return Err("`new_name` must be at most 60 characters".into());
        }
        item.name = n.to_string();
        said.push(format!("named \"{n}\""));
    }
    if let Some(v) = args.get("variety").and_then(Value::as_str).filter(|v| !v.trim().is_empty()) {
        item.variety = v.trim().to_string();
        said.push(format!("a {}", item.variety));
    }
    if said.is_empty() {
        return Err("nothing to change: give `kcal`, `new_name` or `variety`".into());
    }
    item.estimated = false;
    put_item(host, agent, &ns, &key, &item, cause)?;
    Ok(format!("\"{}\" is now {} and no longer an estimate.", item.name, said.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testkit::Fake;
    use std::sync::Mutex;

    struct FakeJev {
        reply: Mutex<Value>,
        asked: Mutex<Vec<Value>>,
    }

    impl FakeJev {
        fn new(reply: Value) -> Self {
            Self { reply: Mutex::new(reply), asked: Mutex::new(vec![]) }
        }
    }

    impl Jev for FakeJev {
        fn ask(&self, request: &Value) -> Result<Value, String> {
            self.asked.lock().unwrap().push(request.clone());
            Ok(self.reply.lock().unwrap().clone())
        }
    }

    fn plate() -> Image {
        Image { media_type: "image/png".into(), data: vec![1, 2, 3] }
    }

    fn new_dish(kind: &str, band: f64) -> Value {
        json!({
            "is_food": {"type": "noul", "noul": 0.97},
            "variety": {"type": "choice", "choice": kind, "probabilities": {kind: 0.8}},
            "kcal": {"type": "score", "score": band},
        })
    }

    /// Live: `HOLON_JEV_KEY_FILE=<key> HOLON_FOOD_PHOTO=<jpg|png> cargo test jev_live -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn jev_live_reads_a_photo_then_recognises_it() {
        let key = std::env::var("HOLON_JEV_KEY_FILE").expect("HOLON_JEV_KEY_FILE");
        let file = std::env::var("HOLON_FOOD_PHOTO").expect("HOLON_FOOD_PHOTO");
        let raw = std::fs::read(&file).unwrap();
        let media = if file.ends_with(".png") { "image/png" } else { "image/jpeg" };
        let photo = crate::attach::shrunk(raw, media, QUERY_PX).expect("a usable photo");
        let jev = jev::JevClient::new(key);
        let h = Fake::new("food-live", &[], true);
        let first = identify(&h, &jev, "n", "mark", &photo, &Cause::default()).unwrap();
        println!("1st: {first}");
        let second = identify(&h, &jev, "n", "mark", &photo, &Cause::default()).unwrap();
        println!("2nd: {second}");
        assert!(second.contains("Known food"), "the same photo is recognised: {second}");
    }

    #[test]
    fn a_band_number_between_two_bands_lands_between_their_kcal() {
        assert_eq!(kcal_at(0.0), 80);
        assert_eq!(kcal_at(3.0), 530);
        assert_eq!(kcal_at(3.5), 610);
        assert_eq!(kcal_at(99.0), 1500, "past the top is the top");
        assert_eq!(kcal_at(-2.0), 80);
    }

    #[test]
    fn a_dish_not_in_the_pool_is_estimated_added_and_logged() {
        let h = Fake::new("food-new", &[], true);
        let jev = FakeJev::new(new_dish("pasta", 3.0));
        let said = identify(&h, &jev, "nutritionist", "mark", &plate(), &Cause::default()).unwrap();
        assert!(
            said.contains("New food") && said.contains("pasta 1") && said.contains("530 kcal"),
            "{said}"
        );

        let items = pool(&h, "food.mark").unwrap();
        assert_eq!(items.len(), 1);
        let (key, item) = &items[0];
        assert_eq!(key, "pool/f1");
        assert_eq!(
            (item.variety.as_str(), item.kcal, item.seen, item.estimated),
            ("pasta", 530, 1, true)
        );
        assert_eq!(h.kv.list("food.mark", "meal/").unwrap().len(), 1, "the meal is logged");

        let asked = &jev.asked.lock().unwrap()[0];
        assert!(asked["questions"].get("match").is_none());
        assert_eq!(asked["assets"].as_object().unwrap().len(), 1);
    }

    #[test]
    fn a_dish_that_matches_the_pool_is_counted_again_not_added() {
        let h = Fake::new("food-known", &[], true);
        let first = FakeJev::new(new_dish("soup or stew", 2.0));
        identify(&h, &first, "nutritionist", "mark", &plate(), &Cause::default()).unwrap();
        let again = FakeJev::new(json!({
            "is_food": {"type": "noul", "noul": 0.9},
            "variety": {"type": "choice", "choice": "pasta", "probabilities": {"pasta": 0.9}},
            "kcal": {"type": "score", "score": 7.0},
            "match": {"type": "choice", "choice": "soup or stew 1", "probabilities": {"soup or stew 1": 0.8}},
        }));
        let said =
            identify(&h, &again, "nutritionist", "mark", &plate(), &Cause::default()).unwrap();
        assert!(
            said.contains("Known food: soup or stew 1") && said.contains("seen 2 times"),
            "{said}"
        );
        let items = pool(&h, "food.mark").unwrap();
        assert_eq!((items.len(), items[0].1.seen), (1, 2), "the pool did not grow");
        assert_eq!(h.kv.list("food.mark", "meal/").unwrap().len(), 1, "same second, same key");

        let asked = &again.asked.lock().unwrap()[0];
        assert_eq!(asked["assets"].as_object().unwrap().len(), 2, "the photo and one reference");
        assert!(asked["questions"]["match"]["criteria"].get(NONE).is_some());
    }

    #[test]
    fn none_of_these_adds_a_second_entry_with_its_own_id() {
        let h = Fake::new("food-second", &[], true);
        identify(
            &h,
            &FakeJev::new(new_dish("pasta", 3.0)),
            "n",
            "mark",
            &plate(),
            &Cause::default(),
        )
        .unwrap();
        let other = FakeJev::new(json!({
            "is_food": {"type": "noul", "noul": 0.9},
            "variety": {"type": "choice", "choice": "salad", "probabilities": {"salad": 0.6}},
            "kcal": {"type": "score", "score": 1.0},
            "match": {"type": "choice", "choice": NONE, "probabilities": {NONE: 0.9}},
        }));
        let said = identify(&h, &other, "n", "mark", &plate(), &Cause::default()).unwrap();
        assert!(
            said.contains("salad 1") && said.contains("id f2") && said.contains("230 kcal"),
            "{said}"
        );
        assert_eq!(pool(&h, "food.mark").unwrap().len(), 2);
    }

    #[test]
    fn a_weak_match_is_not_a_match() {
        let h = Fake::new("food-weak", &[], true);
        identify(
            &h,
            &FakeJev::new(new_dish("pasta", 3.0)),
            "n",
            "mark",
            &plate(),
            &Cause::default(),
        )
        .unwrap();
        let weak = FakeJev::new(json!({
            "is_food": {"type": "noul", "noul": 0.9},
            "variety": {"type": "choice", "choice": "pasta", "probabilities": {"pasta": 0.6}},
            "kcal": {"type": "score", "score": 3.0},
            "match": {"type": "choice", "choice": "pasta 1", "probabilities": {"pasta 1": 0.3}},
        }));
        let said = identify(&h, &weak, "n", "mark", &plate(), &Cause::default()).unwrap();
        assert!(said.contains("New food") && said.contains("closest was pasta 1 at 30%"), "{said}");
    }

    #[test]
    fn a_photo_without_food_stores_nothing() {
        let h = Fake::new("food-none", &[], true);
        let jev = FakeJev::new(
            json!({"is_food": {"type": "noul", "noul": 0.04}, "variety": {"type": "choice", "choice": "other"}, "kcal": {"type": "score", "score": 0.0}}),
        );
        let said = identify(&h, &jev, "n", "mark", &plate(), &Cause::default()).unwrap();
        assert!(said.contains("no food"), "{said}");
        assert!(pool(&h, "food.mark").unwrap().is_empty());
        assert!(h.kv.list("food.mark", "meal/").unwrap().is_empty());
    }

    #[test]
    fn each_person_has_their_own_pool() {
        let h = Fake::new("food-users", &[], true);
        identify(
            &h,
            &FakeJev::new(new_dish("pizza", 5.0)),
            "n",
            "mark",
            &plate(),
            &Cause::default(),
        )
        .unwrap();
        assert_eq!(pool(&h, "food.mark").unwrap().len(), 1);
        assert!(pool(&h, "food.anna").unwrap().is_empty());
        assert!(namespace("../x").is_err() && namespace("Mark").is_err());
        assert_eq!(namespace("").unwrap(), "food.owner");
    }

    #[test]
    fn a_correction_replaces_the_estimate() {
        let h = Fake::new("food-fix", &[], true);
        identify(
            &h,
            &FakeJev::new(new_dish("pasta", 3.0)),
            "n",
            "mark",
            &plate(),
            &Cause::default(),
        )
        .unwrap();
        let said = correct(
            &h,
            "n",
            "mark",
            &json!({"name": "f1", "kcal": 640, "new_name": "Mum's carbonara"}),
            &Cause::default(),
        )
        .unwrap();
        assert!(said.contains("Mum's carbonara") && said.contains("640 kcal"), "{said}");
        let (_, item) = &pool(&h, "food.mark").unwrap()[0];
        assert_eq!(
            (item.name.as_str(), item.kcal, item.estimated),
            ("Mum's carbonara", 640, false)
        );
        assert!(correct(
            &h,
            "n",
            "mark",
            &json!({"name": "mum's carbonara", "kcal": 600}),
            &Cause::default()
        )
        .is_ok());
        assert!(correct(&h, "n", "mark", &json!({"name": "nope", "kcal": 1}), &Cause::default())
            .is_err());
        assert!(correct(&h, "n", "mark", &json!({"name": "f1"}), &Cause::default()).is_err());
        assert!(correct(&h, "n", "mark", &json!({"name": "f1", "kcal": 99999}), &Cause::default())
            .is_err());
    }

    #[test]
    fn the_list_says_what_is_in_the_pool_and_the_day() {
        let h = Fake::new("food-list", &[], true);
        assert!(list(&h, "mark").unwrap().contains("empty"));
        identify(
            &h,
            &FakeJev::new(new_dish("fruit", 1.0)),
            "n",
            "mark",
            &plate(),
            &Cause::default(),
        )
        .unwrap();
        let out = list(&h, "mark").unwrap();
        assert!(out.contains("fruit 1 (id f1)") && out.contains("(estimate)"), "{out}");
        assert!(out.contains("Last 24 h: 230 kcal in 1 meals"), "{out}");
    }
}

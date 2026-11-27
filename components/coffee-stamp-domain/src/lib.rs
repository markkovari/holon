//! `coffee-stamp-domain` — coffee shop QR loyalty stamp card app.
//!
//! Composes `records:store`, `id:generate`, `qr:encode`, `wasi:config`, and `wasi:clocks`
//! over WASI HTTP p3 (`0.3.0`).

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../host/wit/deps/comp-store",
            "../record-store/wit",
            "../id-generate/wit",
            "../qr/wit",
            "wit",
        ],
        world: "coffee:stamp/coffee-stamp-domain",
        generate_all,
    });
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0::handler;
        pub use super::wasi::clocks0_3_0 as clocks;
        pub use super::wasi::http0_3_0 as http;
    }
}

mod logic;

use serde::Deserialize;

use bindings::id::generate::generator as ids;
use bindings::p3::clocks::system_clock;
use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Method, Request, Response};
use bindings::qr::encode::encoder as qr;
use bindings::records::store::store as records;
use bindings::wasi::config::store as config;
use logic::{
    apply_claim, apply_redeem, ClaimError, RedeemError, RewardVoucher, StampCard, StampToken,
    DEFAULT_STAMPS_REQUIRED, DEFAULT_TOKEN_TTL_SECS,
};

const APP_HTML: &str = include_str!("app.html");
const MAX_BODY_BYTES: usize = 1024 * 1024;

guestio::guest_p3_read_body!(MAX_BODY_BYTES);
guestio::guest_p3_respond!();

struct Component;

fn shop_name() -> String {
    config::get("shop-name")
        .ok()
        .flatten()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Roast & Ritual Coffee".to_string())
}

fn stamps_required() -> u32 {
    config::get("stamps-required")
        .ok()
        .flatten()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|n| (1..=50).contains(n))
        .unwrap_or(DEFAULT_STAMPS_REQUIRED)
}

fn token_ttl_secs() -> i64 {
    config::get("token-ttl-secs")
        .ok()
        .flatten()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|n| (30..=86_400).contains(n))
        .unwrap_or(DEFAULT_TOKEN_TTL_SECS)
}

fn now_secs() -> i64 {
    system_clock::now().seconds
}

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let method = request.get_method();
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        match (&method, seg.as_slice()) {
            (Method::Get, [""]) | (Method::Get, ["index.html"]) => {
                respond(200, "text/html; charset=utf-8", APP_HTML)
            }
            (Method::Get, ["health"]) => respond(
                200,
                "application/json",
                serde_json::json!({ "ok": true, "app": "coffee-stamp" }).to_string(),
            ),
            (Method::Get, ["api", "config"]) => respond(
                200,
                "application/json",
                serde_json::json!({
                    "shopName": shop_name(),
                    "stampsRequired": stamps_required(),
                    "tokenTtlSecs": token_ttl_secs(),
                })
                .to_string(),
            ),
            (Method::Get, ["api", "stats"]) => get_stats(),
            (Method::Get, ["api", "cards"]) => list_cards(),
            (Method::Post, ["api", "cards"]) => create_or_get_card(request).await,
            (Method::Get, ["api", "cards", id]) => get_card_detail(id),
            (Method::Post, ["api", "cards", id, "claim"]) => claim_stamp(request, id).await,
            (Method::Post, ["api", "barista", "stamp-qr"]) => issue_stamp_qr(request).await,
            (Method::Post, ["api", "barista", "redeem"]) => redeem_voucher(request).await,
            (Method::Get, ["api", "vouchers", code, "qr.svg"]) => voucher_qr_svg(code),
            _ => json_err(404, "not_found"),
        }
    }
}

fn json_ok(status: u16, v: serde_json::Value) -> Result<Response, ErrorCode> {
    respond(status, "application/json", v.to_string())
}

fn json_err(status: u16, msg: &str) -> Result<Response, ErrorCode> {
    respond(status, "application/json", serde_json::json!({ "error": msg }).to_string())
}

async fn parse_json<T: for<'de> Deserialize<'de>>(request: Request) -> Result<T, String> {
    let body = read_body(request).await.map_err(|_| "failed to read request body".to_string())?;
    serde_json::from_slice(&body).map_err(|e| format!("invalid json: {e}"))
}

fn all_cards() -> Vec<StampCard> {
    records::list_records("cards", 0, "")
        .map(|p| {
            p.entries
                .into_iter()
                .filter_map(|e| serde_json::from_str::<StampCard>(&e.data).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn all_vouchers() -> Vec<RewardVoucher> {
    records::list_records("vouchers", 0, "")
        .map(|p| {
            p.entries
                .into_iter()
                .filter_map(|e| serde_json::from_str::<RewardVoucher>(&e.data).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn all_tokens() -> Vec<StampToken> {
    records::list_records("tokens", 0, "")
        .map(|p| {
            p.entries
                .into_iter()
                .filter_map(|e| serde_json::from_str::<StampToken>(&e.data).ok())
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Deserialize)]
struct CreateCardReq {
    name: String,
    email: String,
}

async fn create_or_get_card(request: Request) -> Result<Response, ErrorCode> {
    let req: CreateCardReq = match parse_json(request).await {
        Ok(v) => v,
        Err(m) => return json_err(400, &m),
    };
    let name = req.name.trim();
    let email = req.email.trim().to_lowercase();
    if name.is_empty() || email.is_empty() {
        return json_err(400, "name and email are required");
    }
    // Idempotent by email so returning customers get their existing stamp card.
    if let Some(existing) = all_cards().into_iter().find(|c| c.email == email) {
        return json_ok(200, serde_json::json!({ "card": existing, "created": false }));
    }
    let id = ids::ulid();
    let card = StampCard {
        id: id.clone(),
        customer_name: name.to_string(),
        email,
        stamps: 0,
        total_stamps_earned: 0,
        free_coffees_earned: 0,
        free_coffees_redeemed: 0,
        created_at: now_secs(),
    };
    let data = serde_json::to_string(&card).unwrap_or_default();
    if let Err(e) = records::create("cards", &data, &["id".to_string(), "email".to_string()]) {
        return json_err(500, &format!("store error: {e:?}"));
    }
    json_ok(201, serde_json::json!({ "card": card, "created": true }))
}

fn list_cards() -> Result<Response, ErrorCode> {
    let mut cards = all_cards();
    cards.sort_by_key(|a| std::cmp::Reverse(a.total_stamps_earned));
    json_ok(
        200,
        serde_json::json!({
            "cards": cards,
            "stampsRequired": stamps_required(),
        }),
    )
}

fn find_card_record(card_id: &str) -> Option<(String, u64, StampCard)> {
    let entries = records::list_records("cards", 0, "").ok()?.entries;
    for e in entries {
        if let Ok(card) = serde_json::from_str::<StampCard>(&e.data) {
            if card.id == card_id {
                return Some((e.id, e.revision, card));
            }
        }
    }
    None
}

fn get_card_detail(card_id: &str) -> Result<Response, ErrorCode> {
    let Some((_, _, card)) = find_card_record(card_id) else {
        return json_err(404, "card_not_found");
    };
    let mut vouchers: Vec<RewardVoucher> =
        all_vouchers().into_iter().filter(|v| v.card_id == card_id).collect();
    vouchers.sort_by_key(|a| std::cmp::Reverse(a.issued_at));
    let mut history: Vec<StampToken> =
        all_tokens().into_iter().filter(|t| t.claimed_by.as_deref() == Some(card_id)).collect();
    history.sort_by_key(|a| std::cmp::Reverse(a.claimed_at));

    let goal = stamps_required();
    let remaining = goal.saturating_sub(card.stamps);
    json_ok(
        200,
        serde_json::json!({
            "card": card,
            "stampsRequired": goal,
            "stampsRemaining": remaining,
            "vouchers": vouchers,
            "history": history,
        }),
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IssueStampReq {
    #[serde(default)]
    drink: Option<String>,
    #[serde(default)]
    stamps: Option<u32>,
    #[serde(default)]
    issued_by: Option<String>,
    #[serde(default)]
    origin: Option<String>,
}

async fn issue_stamp_qr(request: Request) -> Result<Response, ErrorCode> {
    let req: IssueStampReq = match parse_json(request).await {
        Ok(v) => v,
        Err(m) => return json_err(400, &m),
    };
    let drink = req
        .drink
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Artisan Espresso".to_string());
    let stamps = req.stamps.unwrap_or(1).clamp(1, 5);
    let issued_by = req
        .issued_by
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Barista Counter".to_string());

    let id = ids::ulid();
    let suffix = &id[id.len().saturating_sub(8)..];
    let code = format!("STAMP-{suffix}");
    let now = now_secs();
    let ttl = token_ttl_secs();
    let token = StampToken {
        id: id.clone(),
        code: code.clone(),
        drink,
        stamps,
        issued_by,
        issued_at: now,
        expires_at: now + ttl,
        claimed_by: None,
        claimed_at: None,
    };
    let data = serde_json::to_string(&token).unwrap_or_default();
    if let Err(e) = records::create("tokens", &data, &["code".to_string()]) {
        return json_err(500, &format!("store error: {e:?}"));
    }

    let claim_url = match req.origin.filter(|o| !o.trim().is_empty()) {
        Some(origin) => format!("{}/?claim={code}", origin.trim_end_matches('/')),
        None => format!("coffee-stamp://claim/{code}"),
    };
    let qr_svg = qr::svg(&claim_url, qr::Ecc::Medium, 2).unwrap_or_default();

    json_ok(
        201,
        serde_json::json!({
            "token": token,
            "claimUrl": claim_url,
            "qrSvg": qr_svg,
        }),
    )
}

#[derive(Deserialize)]
struct ClaimReq {
    code: String,
}

async fn claim_stamp(request: Request, card_id: &str) -> Result<Response, ErrorCode> {
    let req: ClaimReq = match parse_json(request).await {
        Ok(v) => v,
        Err(m) => return json_err(400, &m),
    };
    let code = req.code.trim().to_string();
    if code.is_empty() {
        return json_err(400, "code is required");
    }
    let Some((card_rec_id, card_rev, mut card)) = find_card_record(card_id) else {
        return json_err(404, "card_not_found");
    };
    let token_entry = records::list_records("tokens", 0, "").ok().and_then(|p| {
        p.entries.into_iter().find_map(|e| {
            let t = serde_json::from_str::<StampToken>(&e.data).ok()?;
            if t.code == code {
                Some((e.id, e.revision, t))
            } else {
                None
            }
        })
    });
    let Some((tok_rec_id, tok_rev, mut token)) = token_entry else {
        return json_err(404, "stamp_code_not_found");
    };

    let now = now_secs();
    let goal = stamps_required();
    let outcome = match apply_claim(&mut card, &mut token, goal, now) {
        Ok(o) => o,
        Err(ClaimError::AlreadyClaimed) => return json_err(409, "stamp_already_claimed"),
        Err(ClaimError::Expired) => return json_err(410, "stamp_code_expired"),
    };

    if records::update(
        "tokens",
        &tok_rec_id,
        &serde_json::to_string(&token).unwrap_or_default(),
        tok_rev,
    )
    .is_err()
    {
        return json_err(409, "stamp_already_claimed");
    }
    let _ = records::update(
        "cards",
        &card_rec_id,
        &serde_json::to_string(&card).unwrap_or_default(),
        card_rev,
    );

    let mut new_vouchers = Vec::new();
    for _ in 0..outcome.vouchers_earned {
        let vid = ids::ulid();
        let code = format!("FREE-{}", &vid[vid.len().saturating_sub(8)..]);
        let voucher = RewardVoucher {
            id: vid,
            code,
            card_id: card.id.clone(),
            customer_name: card.customer_name.clone(),
            issued_at: now,
            redeemed: false,
            redeemed_at: None,
            redeemed_by: None,
        };
        let _ = records::create(
            "vouchers",
            &serde_json::to_string(&voucher).unwrap_or_default(),
            &["code".to_string(), "cardId".to_string()],
        );
        new_vouchers.push(voucher);
    }

    json_ok(
        200,
        serde_json::json!({
            "card": card,
            "stampsAdded": token.stamps,
            "stampsRequired": goal,
            "stampsRemaining": goal.saturating_sub(card.stamps),
            "vouchersEarned": new_vouchers,
        }),
    )
}

fn voucher_qr_svg(code: &str) -> Result<Response, ErrorCode> {
    let payload = format!("coffee-stamp://redeem/{code}");
    match qr::svg(&payload, qr::Ecc::Medium, 2) {
        Ok(svg) => respond(200, "image/svg+xml; charset=utf-8", svg),
        Err(e) => json_err(500, &format!("qr error: {e:?}")),
    }
}

#[derive(Deserialize)]
struct RedeemReq {
    code: String,
    #[serde(default)]
    barista: Option<String>,
}

async fn redeem_voucher(request: Request) -> Result<Response, ErrorCode> {
    let req: RedeemReq = match parse_json(request).await {
        Ok(v) => v,
        Err(m) => return json_err(400, &m),
    };
    let code = req.code.trim().to_string();
    let barista = req
        .barista
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Barista Counter".to_string());

    let voucher_entry = records::list_records("vouchers", 0, "").ok().and_then(|p| {
        p.entries.into_iter().find_map(|e| {
            let v = serde_json::from_str::<RewardVoucher>(&e.data).ok()?;
            if v.code == code {
                Some((e.id, e.revision, v))
            } else {
                None
            }
        })
    });
    let Some((v_rec_id, v_rev, mut voucher)) = voucher_entry else {
        return json_err(404, "voucher_not_found");
    };
    let Some((card_rec_id, card_rev, mut card)) = find_card_record(&voucher.card_id) else {
        return json_err(404, "card_not_found");
    };

    if let Err(RedeemError::AlreadyRedeemed) =
        apply_redeem(&mut voucher, &mut card, &barista, now_secs())
    {
        return json_err(409, "voucher_already_redeemed");
    }

    if records::update(
        "vouchers",
        &v_rec_id,
        &serde_json::to_string(&voucher).unwrap_or_default(),
        v_rev,
    )
    .is_err()
    {
        return json_err(409, "voucher_already_redeemed");
    }
    let _ = records::update(
        "cards",
        &card_rec_id,
        &serde_json::to_string(&card).unwrap_or_default(),
        card_rev,
    );

    json_ok(200, serde_json::json!({ "voucher": voucher, "card": card }))
}

fn get_stats() -> Result<Response, ErrorCode> {
    let cards = all_cards();
    let vouchers = all_vouchers();
    let total_stamps: u32 = cards.iter().map(|c| c.total_stamps_earned).sum();
    let earned: u32 = vouchers.len() as u32;
    let redeemed: u32 = vouchers.iter().filter(|v| v.redeemed).count() as u32;
    json_ok(
        200,
        serde_json::json!({
            "shopName": shop_name(),
            "stampsRequired": stamps_required(),
            "totalMembers": cards.len(),
            "totalStampsIssued": total_stamps,
            "freeCoffeesEarned": earned,
            "freeCoffeesRedeemed": redeemed,
            "activeVouchers": earned.saturating_sub(redeemed),
        }),
    )
}

bindings::export!(Component with_types_in bindings);

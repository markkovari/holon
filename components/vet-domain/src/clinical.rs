//! Clinical records & owner experience routes for `vet-domain`:
//! - Vaccination schedules (`rrule:recur`) & RFC 5545 `.ics` calendar export (`ical:codec`)
//! - Printable PDF vaccination certificates & appointment invoices (`pdf:codec`) + QR codes (`qr:encode`)
//! - Pet vitals & weight trend charts (`svg:chart`)

use serde::Deserialize;

use crate::bindings::ical::codec::codec as ical;
use crate::bindings::p3::http::types::Request;
use crate::bindings::pdf::codec::codec as pdf;
use crate::bindings::qr::encode::encoder as qr;
use crate::bindings::records::store::store as records;
use crate::bindings::rrule::recur::recur as rrule;
use crate::bindings::svg::chart::charts as svg_chart;
use crate::{
    datetime, is_privileged, jget_str, js, json_field, now_seconds, parse, query_param, require,
    store_err, strip_braces, Outcome,
};

const ICAL_PROD_ID: &str = "-//Acme Vet Clinic//Holon//EN";

fn load_pet_checked(
    request: &Request,
    pet_id: &str,
    action: &str,
) -> Result<records::Entry, Outcome> {
    let principal = require(request, "pets", action)?;
    let pet =
        records::get("pets", pet_id).map_err(|_| Outcome::Err(404, "pet_not_found".into()))?;
    let owner = json_field(&pet.data, "owner").unwrap_or_default();
    if !is_privileged(&principal) && owner != principal.subject {
        return Err(Outcome::Err(403, "not_your_pet".into()));
    }
    Ok(pet)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VaccinationReq {
    vaccine: String,
    #[serde(default)]
    administered_at: Option<String>,
    #[serde(default)]
    interval_weeks: Option<u32>,
    #[serde(default)]
    boosters: Option<u32>,
    #[serde(default)]
    lot: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}

pub(crate) async fn add_vaccination(request: Request, pet_id: &str) -> Outcome {
    let principal = match require(&request, "pets", "write") {
        Ok(p) => p,
        Err(o) => return o,
    };
    if !is_privileged(&principal) {
        return Outcome::Err(403, "staff_only".into());
    }
    if records::get("pets", pet_id).is_err() {
        return Outcome::Err(404, "pet_not_found".into());
    }
    let req: VaccinationReq = match parse(request).await {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    if req.vaccine.trim().is_empty() {
        return Outcome::Bad("vaccine is required".into());
    }
    let administered_at = req
        .administered_at
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| datetime::format_date(now_seconds()));
    if datetime::parse_unix_seconds(&administered_at).is_none() {
        return Outcome::Bad("invalid administeredAt date (expected YYYY-MM-DD)".into());
    }
    let interval_weeks = req.interval_weeks.unwrap_or(52).max(1);
    let boosters = req.boosters.unwrap_or(1).min(10);
    let window_days = (interval_weeks as i64) * 7 * ((boosters as i64) + 1) + 7;
    let window_to = datetime::add_days(&administered_at, window_days)
        .unwrap_or_else(|| "2099-12-31".to_string());

    let rule = rrule::Rule {
        frequency: rrule::Freq::Weekly,
        interval: interval_weeks,
        by_weekday: vec![],
        count: boosters + 1,
        until: String::new(),
    };
    let series = match rrule::expand(&administered_at, &rule, &administered_at, &window_to) {
        Ok(dates) => dates,
        Err(e) => return Outcome::Bad(format!("schedule_error: {e:?}")),
    };
    // First occurrence is the initial dose; remaining dates are upcoming boosters.
    let upcoming: Vec<String> = series.into_iter().skip(1).collect();
    let next_due = upcoming.first().cloned().unwrap_or_default();
    let lot = req.lot.unwrap_or_default();
    let notes = req.notes.unwrap_or_default();
    let upcoming_json = serde_json::to_string(&upcoming).unwrap_or_else(|_| "[]".into());
    let data = format!(
        "{{\"petId\":{},\"vaccine\":{},\"administeredAt\":{},\"intervalWeeks\":{},\"lot\":{},\"notes\":{},\"nextDue\":{},\"upcomingDates\":{},\"administeredBy\":{}}}",
        js(pet_id),
        js(req.vaccine.trim()),
        js(&administered_at),
        interval_weeks,
        js(&lot),
        js(&notes),
        js(&next_due),
        upcoming_json,
        js(&principal.subject),
    );
    match records::create("vaccinations", &data, &["petId".to_string()]) {
        Ok(e) => Outcome::Json(201, format!("{{\"id\":{},{}}}", js(&e.id), strip_braces(&e.data))),
        Err(e) => store_err(e),
    }
}

pub(crate) fn list_vaccinations(request: &Request, pet_id: &str) -> Outcome {
    if let Err(o) = load_pet_checked(request, pet_id, "read") {
        return o;
    }
    let today = datetime::format_date(now_seconds());
    let mut recs = records::find_by("vaccinations", "petId", &js(pet_id)).unwrap_or_default();
    recs.sort_by(|a, b| {
        let da = json_field(&a.data, "administeredAt").unwrap_or_default();
        let db = json_field(&b.data, "administeredAt").unwrap_or_default();
        db.cmp(&da)
    });
    let items: Vec<String> = recs
        .iter()
        .map(|r| {
            let next_due = json_field(&r.data, "nextDue").unwrap_or_default();
            let overdue = !next_due.is_empty() && next_due < today;
            format!("{{\"id\":{},{},\"overdue\":{overdue}}}", js(&r.id), strip_braces(&r.data))
        })
        .collect();
    Outcome::Json(
        200,
        format!("{{\"petId\":{},\"vaccinations\":[{}]}}", js(pet_id), items.join(",")),
    )
}

pub(crate) fn pet_calendar_ics(request: &Request, pet_id: &str) -> Outcome {
    let pet = match load_pet_checked(request, pet_id, "read") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let pet_name = json_field(&pet.data, "name").unwrap_or_else(|| pet_id.to_string());
    let mut evs = Vec::new();

    for appt in records::find_by("appointments", "pet", &js(pet_id)).unwrap_or_default() {
        let dt = json_field(&appt.data, "datetime").unwrap_or_default();
        let Some(start) = datetime::parse_unix_seconds(&dt).filter(|&s| s >= 0) else {
            continue;
        };
        let reason = json_field(&appt.data, "reason").unwrap_or_else(|| "Clinic Visit".into());
        let status = json_field(&appt.data, "status").unwrap_or_else(|| "booked".into());
        evs.push(ical::Event {
            uid: format!("appt-{}@acme-vet.holon", appt.id),
            start: start as u64,
            end: (start + 1800) as u64,
            summary: format!("{pet_name}: {reason}"),
            description: format!("Appointment status: {status}"),
            location: "Acme Vet Clinic".to_string(),
            organizer: "clinic@acme-vet.test".to_string(),
            rrule: String::new(),
            alarm_minutes: 1440,
        });
    }

    for vac in records::find_by("vaccinations", "petId", &js(pet_id)).unwrap_or_default() {
        let v: serde_json::Value = serde_json::from_str(&vac.data).unwrap_or_default();
        let vaccine = jget_str(&v, "vaccine");
        if let Some(dates) = v.get("upcomingDates").and_then(|d| d.as_array()) {
            for (idx, d) in dates.iter().enumerate() {
                let Some(ds) = d.as_str() else { continue };
                let Some(start) = datetime::parse_unix_seconds(ds).filter(|&s| s >= 0) else {
                    continue;
                };
                // 09:00 UTC reminder slot for vaccine booster due date
                let start_9am = (start + 9 * 3600) as u64;
                evs.push(ical::Event {
                    uid: format!("vac-{}-{idx}@acme-vet.holon", vac.id),
                    start: start_9am,
                    end: start_9am + 1800,
                    summary: format!("{pet_name}: {vaccine} Booster Due"),
                    description: format!("Scheduled {vaccine} vaccination booster for {pet_name}."),
                    location: "Acme Vet Clinic".to_string(),
                    organizer: "clinic@acme-vet.test".to_string(),
                    rrule: String::new(),
                    alarm_minutes: 1440,
                });
            }
        }
    }

    let cal = ical::format_calendar(&evs, ICAL_PROD_ID, &format!("{pet_name} - Acme Vet Clinic"));
    Outcome::Raw(200, "text/calendar; charset=utf-8".into(), cal.into_bytes())
}

pub(crate) fn appointment_calendar_ics(request: &Request, appt_id: &str) -> Outcome {
    let principal = match require(request, "appointments", "read") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let appt = match records::get("appointments", appt_id) {
        Ok(e) => e,
        Err(_) => return Outcome::Err(404, "appointment_not_found".into()),
    };
    let owner = json_field(&appt.data, "owner").unwrap_or_default();
    if !is_privileged(&principal) && owner != principal.subject {
        return Outcome::Err(403, "forbidden".into());
    }
    let dt = json_field(&appt.data, "datetime").unwrap_or_default();
    let Some(start) = datetime::parse_unix_seconds(&dt).filter(|&s| s >= 0) else {
        return Outcome::Bad("appointment has invalid datetime".into());
    };
    let reason = json_field(&appt.data, "reason").unwrap_or_else(|| "Clinic Visit".into());
    let pet_id = json_field(&appt.data, "pet").unwrap_or_default();
    let pet_name = records::get("pets", &pet_id)
        .ok()
        .and_then(|p| json_field(&p.data, "name"))
        .unwrap_or(pet_id);
    let ev = ical::Event {
        uid: format!("appt-{appt_id}@acme-vet.holon"),
        start: start as u64,
        end: (start + 1800) as u64,
        summary: format!("{pet_name}: {reason}"),
        description: format!("Acme Vet Clinic appointment ({appt_id})"),
        location: "Acme Vet Clinic".to_string(),
        organizer: "clinic@acme-vet.test".to_string(),
        rrule: String::new(),
        alarm_minutes: 1440,
    };
    let body = ical::format_event(&ev, ICAL_PROD_ID);
    Outcome::Raw(200, "text/calendar; charset=utf-8".into(), body.into_bytes())
}

pub(crate) fn pet_certificate_pdf(request: &Request, pet_id: &str) -> Outcome {
    let pet = match load_pet_checked(request, pet_id, "read") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let name = json_field(&pet.data, "name").unwrap_or_default();
    let species = json_field(&pet.data, "species").unwrap_or_default();
    let owner = json_field(&pet.data, "owner").unwrap_or_default();
    let today = datetime::format_date(now_seconds());

    let mut blocks = vec![
        pdf::Block {
            text: format!("Pet: {name} ({species})   ID: {pet_id}"),
            size: 12,
            bold: true,
            gap_before: 8,
        },
        pdf::Block {
            text: format!("Owner: {owner}   Issued: {today}"),
            size: 10,
            bold: false,
            gap_before: 2,
        },
        pdf::Block {
            text: "Immunization History".to_string(),
            size: 13,
            bold: true,
            gap_before: 14,
        },
    ];

    let vacs = records::find_by("vaccinations", "petId", &js(pet_id)).unwrap_or_default();
    if vacs.is_empty() {
        blocks.push(pdf::Block {
            text: "No vaccinations recorded.".to_string(),
            size: 10,
            bold: false,
            gap_before: 4,
        });
    } else {
        for vac in &vacs {
            let v: serde_json::Value = serde_json::from_str(&vac.data).unwrap_or_default();
            let line = format!(
                "- {} | Given: {} | Lot: {} | Next Due: {}",
                jget_str(&v, "vaccine"),
                jget_str(&v, "administeredAt"),
                jget_str(&v, "lot"),
                jget_str(&v, "nextDue"),
            );
            blocks.push(pdf::Block { text: line, size: 10, bold: false, gap_before: 3 });
        }
    }

    let vitals = sorted_vitals(pet_id);
    if let Some((_, latest)) = vitals.last() {
        blocks.push(pdf::Block {
            text: "Latest Recorded Vitals".to_string(),
            size: 13,
            bold: true,
            gap_before: 14,
        });
        blocks.push(pdf::Block {
            text: format!(
                "Date: {} | Weight: {} kg | Temp: {} C | Heart Rate: {} bpm",
                jget_str(latest, "recordedAt"),
                jget_str(latest, "weightKg"),
                jget_str(latest, "tempC"),
                jget_str(latest, "heartRateBpm"),
            ),
            size: 10,
            bold: false,
            gap_before: 4,
        });
    }

    blocks.push(pdf::Block {
        text: format!("Verify: holon-vet://pet/{pet_id}"),
        size: 9,
        bold: false,
        gap_before: 16,
    });

    let bytes = pdf::render(&pdf::Document {
        title: "Acme Vet Clinic - Vaccination & Health Certificate".to_string(),
        blocks,
    });
    Outcome::Raw(200, "application/pdf".into(), bytes)
}

pub(crate) fn appointment_invoice_pdf(request: &Request, appt_id: &str) -> Outcome {
    let principal = match require(request, "appointments", "read") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let appt = match records::get("appointments", appt_id) {
        Ok(e) => e,
        Err(_) => return Outcome::Err(404, "appointment_not_found".into()),
    };
    let owner = json_field(&appt.data, "owner").unwrap_or_default();
    if !is_privileged(&principal) && owner != principal.subject {
        return Outcome::Err(403, "forbidden".into());
    }
    let hits = records::find_by("invoices", "appointment", &js(appt_id)).unwrap_or_default();
    let Some(inv) = hits.first() else {
        return Outcome::Err(404, "no_invoice".into());
    };
    let v: serde_json::Value = serde_json::from_str(&inv.data).unwrap_or_default();
    let mut blocks = vec![
        pdf::Block {
            text: format!("Appointment: {appt_id}   Owner: {owner}"),
            size: 11,
            bold: true,
            gap_before: 8,
        },
        pdf::Block { text: "Line Items".to_string(), size: 12, bold: true, gap_before: 12 },
    ];
    if let Some(items) = v.get("items").and_then(|x| x.as_array()) {
        for it in items {
            let desc = jget_str(it, "description");
            let cents = it.get("cents").and_then(|c| c.as_i64()).unwrap_or(0);
            blocks.push(pdf::Block {
                text: format!("- {desc}: ${:.2}", (cents as f64) / 100.0),
                size: 10,
                bold: false,
                gap_before: 3,
            });
        }
    }
    blocks.push(pdf::Block {
        text: format!("Total: {} ({})", jget_str(&v, "totalFormatted"), jget_str(&v, "currency")),
        size: 13,
        bold: true,
        gap_before: 12,
    });
    let bytes = pdf::render(&pdf::Document {
        title: "Acme Vet Clinic - Visit Invoice".to_string(),
        blocks,
    });
    Outcome::Raw(200, "application/pdf".into(), bytes)
}

pub(crate) fn pet_qr_svg(request: &Request, pet_id: &str) -> Outcome {
    let pet = match load_pet_checked(request, pet_id, "read") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let name = json_field(&pet.data, "name").unwrap_or_default();
    let species = json_field(&pet.data, "species").unwrap_or_default();
    let payload = format!("holon-vet://pet/{pet_id}?name={name}&species={species}");
    match qr::svg(&payload, qr::Ecc::Medium, 2) {
        Ok(svg) => Outcome::Raw(200, "image/svg+xml; charset=utf-8".into(), svg.into_bytes()),
        Err(e) => Outcome::Err(500, format!("qr_error: {e:?}")),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VitalsReq {
    #[serde(default)]
    recorded_at: Option<String>,
    weight_kg: f64,
    #[serde(default)]
    temp_c: Option<f64>,
    #[serde(default)]
    heart_rate_bpm: Option<u32>,
    #[serde(default)]
    notes: Option<String>,
}

pub(crate) async fn add_vitals(request: Request, pet_id: &str) -> Outcome {
    let principal = match require(&request, "pets", "write") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let pet = match records::get("pets", pet_id) {
        Ok(e) => e,
        Err(_) => return Outcome::Err(404, "pet_not_found".into()),
    };
    let owner = json_field(&pet.data, "owner").unwrap_or_default();
    if !is_privileged(&principal) && owner != principal.subject {
        return Outcome::Err(403, "not_your_pet".into());
    }
    let req: VitalsReq = match parse(request).await {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    if !(req.weight_kg.is_finite() && req.weight_kg > 0.0 && req.weight_kg < 500.0) {
        return Outcome::Bad("weightKg must be between 0 and 500".into());
    }
    let recorded_at = req
        .recorded_at
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| datetime::format_date(now_seconds()));
    if datetime::parse_unix_seconds(&recorded_at).is_none() {
        return Outcome::Bad("invalid recordedAt date".into());
    }
    let payload = serde_json::json!({
        "petId": pet_id,
        "recordedAt": recorded_at,
        "weightKg": req.weight_kg,
        "tempC": req.temp_c,
        "heartRateBpm": req.heart_rate_bpm,
        "notes": req.notes.unwrap_or_default(),
        "recordedBy": principal.subject,
    });
    match records::create("vitals", &payload.to_string(), &["petId".to_string()]) {
        Ok(e) => Outcome::Json(201, format!("{{\"id\":{},{}}}", js(&e.id), strip_braces(&e.data))),
        Err(e) => store_err(e),
    }
}

fn sorted_vitals(pet_id: &str) -> Vec<(String, serde_json::Value)> {
    let mut rows: Vec<(String, serde_json::Value)> =
        records::find_by("vitals", "petId", &js(pet_id))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| serde_json::from_str(&e.data).ok().map(|v| (e.id, v)))
            .collect();
    rows.sort_by_key(|a| jget_str(&a.1, "recordedAt"));
    rows
}

pub(crate) fn list_vitals(request: &Request, pet_id: &str) -> Outcome {
    if let Err(o) = load_pet_checked(request, pet_id, "read") {
        return o;
    }
    let items: Vec<String> = sorted_vitals(pet_id)
        .into_iter()
        .map(|(id, mut v)| {
            if let Some(m) = v.as_object_mut() {
                m.insert("id".into(), serde_json::Value::String(id));
            }
            v.to_string()
        })
        .collect();
    Outcome::Json(200, format!("{{\"petId\":{},\"vitals\":[{}]}}", js(pet_id), items.join(",")))
}

pub(crate) fn pet_vitals_chart_svg(request: &Request, pet_id: &str, query: &str) -> Outcome {
    let pet = match load_pet_checked(request, pet_id, "read") {
        Ok(p) => p,
        Err(o) => return o,
    };
    let pet_name = json_field(&pet.data, "name").unwrap_or_else(|| pet_id.to_string());
    let metric = query_param(query, "metric").unwrap_or_else(|| "weight".into());
    let kind_str = query_param(query, "kind").unwrap_or_else(|| "line".into());
    let kind = match kind_str.as_str() {
        "bar" => svg_chart::Kind::Bar,
        "sparkline" => svg_chart::Kind::Sparkline,
        _ => svg_chart::Kind::Line,
    };
    let (field, title_unit, color) = match metric.as_str() {
        "temp" => ("tempC", "Temperature (C)", "#e67e22"),
        "hr" => ("heartRateBpm", "Heart Rate (bpm)", "#e74c3c"),
        _ => ("weightKg", "Weight (kg)", "#2563eb"),
    };
    let data: Vec<svg_chart::Slice> = sorted_vitals(pet_id)
        .into_iter()
        .filter_map(|(_, v)| {
            let val = v.get(field)?.as_f64()?;
            Some(svg_chart::Slice {
                label: jget_str(&v, "recordedAt"),
                value: val,
                color: color.to_string(),
            })
        })
        .collect();
    let svg = svg_chart::render(&svg_chart::Chart {
        kind,
        title: format!("{pet_name} - {title_unit}"),
        data,
        width: 560,
        height: 240,
    });
    Outcome::Raw(200, "image/svg+xml; charset=utf-8".into(), svg.into_bytes())
}

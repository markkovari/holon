//! The day's manifest. **This file is the goal of the `manifest` part.**
//!
//! Nothing here is implemented. `CONTRACT.md` is the specification — the response
//! shapes, the CSV columns, the radius filter. Read it first.
//!
//! What this part owns:
//!
//!   * `GET /api/manifest`     — the counts, as JSON
//!   * `GET /api/manifest.csv` — the same set as `text/csv`
//!
//! THE COUNTING IS YOURS; THE CSV IS NOT. Use `csv:codec`'s `format`, because one
//! seeded request is titled `Boiler leaking, badly` and a field with a comma in it
//! has to come back quoted or the row stops having five columns. The gate parses the
//! CSV with a real parser and counts columns, and it also reads the compiled
//! component's imports — so formatting it by hand fails even if you get the quoting
//! right.
//!
//!     use crate::bindings::csv::codec::codec as csv;
//!
//!     pub struct Row     { pub fields: Vec<String> }
//!     pub struct Dialect { pub delimiter: String,   // a STRING, not a char: ",".to_string()
//!                          pub has_header: bool, pub trim: bool }
//!     csv::format(rows: &[Row], opts: &Dialect) -> String
//!
//! A file is `Vec<Row>` — the header is just the first `Row` — and `format` returns
//! the whole document as one `String`.
//!
//! The gate checks the content type as well as the columns, so look at what `Reply`
//! in `src/lib.rs` can actually answer with before you build the response.
//! `Reply::json` cannot carry a CSV: `Value::String` serialises to a JSON string
//! literal, quotes and escapes included, and a CSV parser then reads one blob.
//!
//! THE RADIUS IS NOT YOURS TO COMPUTE. `geo:resolve` is in your world:
//!
//!     use crate::bindings::geo::resolve::coords as geo;
//!
//!     geo::bounding_box(center: geo::Point, radius_meters: f64) -> Result<geo::Bbox, geo::GeoError>
//!     geo::contains(box_: geo::Bbox, p: geo::Point)             -> bool
//!     geo::distance_meters(a: geo::Point, b: geo::Point)        -> Result<f64, geo::GeoError>
//!
//!     pub struct Point { pub lat: f64, pub lon: f64 }
//!     pub struct Bbox  { pub min_lat: f64, pub min_lon: f64, pub max_lat: f64, pub max_lon: f64 }
//!
//! A bounding box is a cheap PRE-filter and is not the answer on its own: its
//! corners are further from the centre than the radius, so a point can be inside the
//! box and outside the circle. `contains` then `distance_meters` is the pair.
//!
//! `schedule` imports the same component to pick an engineer. A hand-rolled
//! haversine here agrees with nothing, including your sibling's — which is a failure
//! neither part's gate can see alone.
//!
//! `by_state` and `by_engineer` include only the states and engineers that OCCUR: no
//! zero-filled keys, and an unassigned request contributes to no engineer at all —
//! `""` is not a key. A day with nothing in it is a manifest of zero, not a 404.

use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};

pub fn handle(_method: &Method, _route: &Route, _body: &str) -> Reply {
    Reply::err(501, "not_implemented")
}

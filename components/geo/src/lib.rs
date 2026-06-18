//! `geo` — reference implementation of `geo:resolve`.
//!
//! Pure coordinate math + IP literal classification. This is NOT a GeoIP
//! database: IP -> country/city lookup needs a licensed dataset the host
//! injects, so it is deliberately out of scope. This component owns only the
//! pure math + parsing every app would otherwise re-implement:
//!   - `distance-meters`: great-circle (haversine) distance between two points.
//!   - `bounding-box`: an axis-aligned box around a point for a "within N m"
//!     pre-filter (cheap rejection before an exact distance check).
//!   - `contains`: inclusive point-in-box test.
//!   - `classify-ip`: parse an IPv4/IPv6 literal and class it as
//!     public/private/loopback/special.
//!
//! No state, no host imports — pure compute.

#[allow(warnings)]
mod bindings;

use std::net::IpAddr;
use std::net::Ipv6Addr;

use bindings::exports::geo::resolve::coords::{Bbox, GeoError, Guest, IpClass, Point};

struct Component;

/// Mean Earth radius in meters (IUGG mean radius R1).
const EARTH_RADIUS_M: f64 = 6_371_008.8;

/// Meters per degree of latitude (approx, treating the Earth as a sphere).
const M_PER_DEG_LAT: f64 = 111_320.0;

/// Validate a coordinate's ranges: lat -90..=90, lon -180..=180.
fn validate(p: &Point) -> Result<(), GeoError> {
    if (-90.0..=90.0).contains(&p.lat) && (-180.0..=180.0).contains(&p.lon) {
        Ok(())
    } else {
        Err(GeoError::BadCoordinate)
    }
}

fn classify(ip: IpAddr) -> IpClass {
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_loopback() {
                IpClass::Loopback
            } else if v4.is_private() {
                IpClass::Private
            } else if v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_broadcast()
            {
                IpClass::Special
            } else {
                IpClass::Public
            }
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() {
                IpClass::Loopback
            } else if is_unique_local(v6) {
                IpClass::Private
            } else if v6.is_unspecified() || v6.is_multicast() || is_link_local_v6(v6) {
                IpClass::Special
            } else {
                IpClass::Public
            }
        }
    }
}

/// IPv6 unique-local address: fc00::/7 (first byte 0xfc or 0xfd).
fn is_unique_local(v6: Ipv6Addr) -> bool {
    (v6.octets()[0] & 0xfe) == 0xfc
}

/// IPv6 link-local: fe80::/10.
fn is_link_local_v6(v6: Ipv6Addr) -> bool {
    let o = v6.octets();
    o[0] == 0xfe && (o[1] & 0xc0) == 0x80
}

impl Guest for Component {
    fn distance_meters(a: Point, b: Point) -> Result<f64, GeoError> {
        validate(&a)?;
        validate(&b)?;

        let lat1 = a.lat.to_radians();
        let lat2 = b.lat.to_radians();
        let dlat = (b.lat - a.lat).to_radians();
        let dlon = (b.lon - a.lon).to_radians();

        let h = (dlat / 2.0).sin().powi(2)
            + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
        let d = 2.0 * EARTH_RADIUS_M * h.sqrt().asin();
        Ok(d)
    }

    fn bounding_box(center: Point, radius_meters: f64) -> Result<Bbox, GeoError> {
        validate(&center)?;

        let dlat_deg = radius_meters / M_PER_DEG_LAT;

        // Meters per degree of longitude shrinks with latitude; near the poles
        // cos(lat) -> 0, so the longitude span blows up. Guard against that and
        // clamp to the full [-180, 180] range when cos is tiny.
        let cos_lat = center.lat.to_radians().cos();
        let (min_lon, max_lon) = if cos_lat.abs() < 1e-12 {
            (-180.0, 180.0)
        } else {
            let dlon_deg = radius_meters / (M_PER_DEG_LAT * cos_lat).abs();
            (
                (center.lon - dlon_deg).clamp(-180.0, 180.0),
                (center.lon + dlon_deg).clamp(-180.0, 180.0),
            )
        };

        Ok(Bbox {
            min_lat: (center.lat - dlat_deg).clamp(-90.0, 90.0),
            min_lon,
            max_lat: (center.lat + dlat_deg).clamp(-90.0, 90.0),
            max_lon,
        })
    }

    fn contains(box_: Bbox, p: Point) -> bool {
        p.lat >= box_.min_lat
            && p.lat <= box_.max_lat
            && p.lon >= box_.min_lon
            && p.lon <= box_.max_lon
    }

    fn classify_ip(ip: String) -> Result<IpClass, GeoError> {
        let parsed: IpAddr = ip.parse().map_err(|_| GeoError::BadIp)?;
        Ok(classify(parsed))
    }
}

bindings::export!(Component with_types_in bindings);

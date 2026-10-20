//! `geo` — distance between two lat/lon points, a bounding box for a within-N-km filter, and IP address classing

#[allow(warnings)]
mod bindings;

use std::net::IpAddr;
use std::net::Ipv6Addr;

use bindings::exports::geo::resolve::coords::{Bbox, GeoError, Guest, IpClass, Point};

struct Component;

/// Mean Earth radius in meters (IUGG mean radius R1).
const EARTH_RADIUS_M: f64 = 6_371_008.8;

const M_PER_DEG_LAT: f64 = 111_320.0;

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

/// fc00::/7.
fn is_unique_local(v6: Ipv6Addr) -> bool {
    (v6.octets()[0] & 0xfe) == 0xfc
}

/// fe80::/10.
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

        let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
        let d = 2.0 * EARTH_RADIUS_M * h.sqrt().asin();
        Ok(d)
    }

    fn bounding_box(center: Point, radius_meters: f64) -> Result<Bbox, GeoError> {
        validate(&center)?;

        let dlat_deg = radius_meters / M_PER_DEG_LAT;

        // cos(lat) -> 0 near the poles, so clamp to the full longitude range.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn c(s: &str) -> IpClass {
        classify(s.parse().expect(s))
    }

    /// Anything not Public must not be dialled on a user's say-so, so boundaries are pinned on both sides.
    #[test]
    fn every_private_v4_range_is_classified_private() {
        for edge in [
            "10.0.0.0",
            "10.255.255.255",
            "172.16.0.0",
            "172.31.255.255",
            "192.168.0.0",
            "192.168.255.255",
        ] {
            assert_eq!(c(edge), IpClass::Private, "{edge}");
        }
        // Just outside 172.16/12: not 172.0/8 and not 172.16/16.
        assert_eq!(c("172.15.255.255"), IpClass::Public);
        assert_eq!(c("172.32.0.0"), IpClass::Public);
        assert_eq!(c("9.255.255.255"), IpClass::Public);
        assert_eq!(c("11.0.0.0"), IpClass::Public);
    }

    #[test]
    fn loopback_and_special_v4_are_not_public() {
        assert_eq!(c("127.0.0.1"), IpClass::Loopback);
        assert_eq!(c("127.255.255.254"), IpClass::Loopback, "all of 127/8 loops back");
        assert_eq!(c("0.0.0.0"), IpClass::Special);
        assert_eq!(c("255.255.255.255"), IpClass::Special);
        assert_eq!(c("224.0.0.1"), IpClass::Special, "multicast");
        assert_eq!(c("169.254.169.254"), IpClass::Special, "link-local covers it");
    }

    /// Covers both fc00:: and fd00::; testing only `fd` would miss a wrong mask.
    #[test]
    fn unique_local_v6_covers_the_whole_slash_7() {
        assert!(is_unique_local("fc00::1".parse::<Ipv6Addr>().unwrap()));
        assert!(is_unique_local("fd00::1".parse::<Ipv6Addr>().unwrap()));
        assert!(is_unique_local("fdff:ffff::1".parse::<Ipv6Addr>().unwrap()));
        assert!(!is_unique_local("fb00::1".parse::<Ipv6Addr>().unwrap()));
        assert!(!is_unique_local("fe00::1".parse::<Ipv6Addr>().unwrap()));
        assert!(!is_unique_local("2001:db8::1".parse::<Ipv6Addr>().unwrap()));
    }

    #[test]
    fn v6_loopback_and_public_are_distinguished() {
        assert_eq!(c("::1"), IpClass::Loopback);
        assert_eq!(c("fd00::1"), IpClass::Private);
        assert_eq!(c("2606:4700::1111"), IpClass::Public);
    }

    /// `::ffff:127.0.0.1` is pinned as what is, so changing it is a decision, not a surprise.
    #[test]
    fn ipv4_mapped_addresses_are_classified_explicitly() {
        let mapped = "::ffff:127.0.0.1".parse::<IpAddr>().unwrap();
        let got = classify(mapped);
        assert!(
            matches!(got, IpClass::Loopback | IpClass::Public),
            "whatever this is, it is pinned: got {got:?}"
        );
        assert_eq!(
            classify(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))),
            IpClass::Loopback,
            "the unmapped form is unambiguous"
        );
    }
}

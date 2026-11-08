fn c(s : String) -> @coords.IpClass {
  match @coords.classify_ip(s) {
    Ok(cls) => cls
    Err(_) => panic()
  }
}

fn assert_class(actual : @coords.IpClass, expected : @coords.IpClass) -> Unit {
  if actual.ordinal() != expected.ordinal() {
    panic()
  }
}

test "every private v4 range is classified private" {
  let private_ips = [
    "10.0.0.0",
    "10.255.255.255",
    "172.16.0.0",
    "172.31.255.255",
    "192.168.0.0",
    "192.168.255.255",
  ]
  for i = 0; i < private_ips.length(); i = i + 1 {
    let edge = private_ips[i]
    assert_class(c(edge), @coords.PRIVATE)
  }

  assert_class(c("172.15.255.255"), @coords.PUBLIC)
  assert_class(c("172.32.0.0"), @coords.PUBLIC)
  assert_class(c("9.255.255.255"), @coords.PUBLIC)
  assert_class(c("11.0.0.0"), @coords.PUBLIC)
}

test "loopback and special v4 are not public" {
  assert_class(c("127.0.0.1"), @coords.LOOPBACK)
  assert_class(c("127.255.255.254"), @coords.LOOPBACK)
  assert_class(c("0.0.0.0"), @coords.SPECIAL)
  assert_class(c("255.255.255.255"), @coords.SPECIAL)
  assert_class(c("224.0.0.1"), @coords.SPECIAL)
  assert_class(c("169.254.169.254"), @coords.SPECIAL)
}

test "v6 classification" {
  assert_class(c("::1"), @coords.LOOPBACK)
  assert_class(c("fd00::1"), @coords.PRIVATE)
  assert_class(c("fc00::1"), @coords.PRIVATE)
  assert_class(c("fdff:ffff::1"), @coords.PRIVATE)
  assert_class(c("2606:4700::1111"), @coords.PUBLIC)
  assert_class(c("::ffff:127.0.0.1"), @coords.LOOPBACK)
}

test "distance and bounding box" {
  let london : @coords.Point = { lat: 51.5074, lon: -0.1278 }
  let paris : @coords.Point = { lat: 48.8566, lon: 2.3522 }
  match @coords.distance_meters(london, paris) {
    Ok(d) => {
      // London to Paris is ~343 km
      assert_eq(d > 340000.0 && d < 350000.0, true)
    }
    Err(_) => panic()
  }

  match @coords.bounding_box(london, 1000.0) {
    Ok(bb) => {
      assert_eq(@coords.contains(bb, london), true)
      assert_eq(@coords.contains(bb, paris), false)
    }
    Err(_) => panic()
  }
}

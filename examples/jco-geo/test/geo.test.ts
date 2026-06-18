// E2E for the geo:resolve component, run in-process via jco. The component is
// pure compute (no WASI imports are actually called), so no shims are needed.
// The exported interface is `coords` (package geo:resolve), aliased to `geo`.

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { coords as geo } from "../gen/geo.js";

const isTag = (tag: string) => (e: { payload?: { tag: string } }) =>
  e?.payload?.tag === tag;

describe("geo:resolve/coords component", () => {
  it("distanceMeters: London -> Paris is ~343.5 km", () => {
    const london = { lat: 51.5074, lon: -0.1278 };
    const paris = { lat: 48.8566, lon: 2.3522 };
    const d = geo.distanceMeters(london, paris);
    assert.ok(
      Math.abs(d - 343500) < 5000,
      `expected ~343500 m, got ${d}`,
    );
  });

  it("distanceMeters: same point is ~0", () => {
    const p = { lat: 51.5074, lon: -0.1278 };
    assert.ok(geo.distanceMeters(p, p) < 1);
  });

  it("distanceMeters: out-of-range latitude throws bad-coordinate", () => {
    assert.throws(
      () => geo.distanceMeters({ lat: 91, lon: 0 }, { lat: 0, lon: 0 }),
      isTag("bad-coordinate"),
    );
  });

  it("boundingBox: brackets the center and contains it", () => {
    const center = { lat: 51.5074, lon: -0.1278 };
    const box = geo.boundingBox(center, 1000);

    assert.ok(box.minLat < center.lat && center.lat < box.maxLat);
    assert.ok(box.minLon < center.lon && center.lon < box.maxLon);

    assert.equal(geo.contains(box, center), true);
    assert.equal(geo.contains(box, { lat: 0, lon: 0 }), false);
  });

  it("classifyIp: classifies common address ranges", () => {
    assert.equal(geo.classifyIp("127.0.0.1"), "loopback");
    assert.equal(geo.classifyIp("10.0.0.1"), "private");
    assert.equal(geo.classifyIp("192.168.1.1"), "private");
    assert.equal(geo.classifyIp("8.8.8.8"), "public");
    assert.equal(geo.classifyIp("::1"), "loopback");
    assert.equal(geo.classifyIp("fc00::1"), "private");
    assert.equal(geo.classifyIp("169.254.0.1"), "special");
  });

  it("classifyIp: garbage input throws bad-ip", () => {
    assert.throws(() => geo.classifyIp("not.an.ip"), isTag("bad-ip"));
  });
});

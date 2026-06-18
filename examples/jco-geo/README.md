# jco-geo

Runs the `geo:resolve` WebAssembly component in-process under Node via
[`jco`](https://github.com/bytecodealliance/jco). It is **pure compute** — the
component does no I/O, so no WASI shims or `--map` flags are required.

## What it does

The exported interface is `coords` (package `geo:resolve`):

- **`distanceMeters(a, b)`** — great-circle distance between two `{lat, lon}`
  points using the haversine formula. Throws `bad-coordinate` if a latitude or
  longitude is out of range.
- **`boundingBox(center, radiusMeters)`** — a `{minLat, minLon, maxLat, maxLon}`
  box that brackets a radius around `center`. Useful as a cheap **prefilter**:
  reject candidates that fall outside the box before paying for a haversine
  distance.
- **`contains(box, p)`** — whether a point lies inside a bounding box.
- **`classifyIp(ip)`** — labels an IPv4/IPv6 address as `public`, `private`,
  `loopback`, or `special` based on its range. Throws `bad-ip` on unparseable
  input.

## Not a GeoIP database

`classifyIp` only classifies an address by its reserved-range semantics. It does
**not** map an IP to a country, city, or ASN — that requires a licensed dataset
(e.g. MaxMind GeoIP2) and is deliberately out of scope here.

## Run

```bash
npm install
npm test
```

`npm test` transpiles `geo.wasm` into `gen/` (producing `gen/geo.js`) and then
runs `test/geo.test.ts` with `tsx --test`.

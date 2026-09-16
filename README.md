# bmkg-edge

**The shared BMKG layer for our projects.** Indonesian earthquake, weather and
severe-weather-warning data, plus the full region-code tree — parsed once, here, so nothing
downstream parses it again.

One Cloudflare Worker (Rust → WASM) serving a REST API, an MCP endpoint and a TypeScript client.

**Live:** https://bmkg.irawan.dev · [OpenAPI](https://bmkg.irawan.dev/v1/openapi.json)

```bash
curl https://bmkg.irawan.dev/v1/earthquake/latest
curl "https://bmkg.irawan.dev/v1/weather/search?q=tebet"
curl https://bmkg.irawan.dev/v1/nowcast
```

## Why

BMKG publishes its data openly, but raw. Magnitudes arrive as strings, one field holds
`"lat,lon"` as text, forecasts nest three arrays deep, and warnings are CAP XML.

So every project that wants BMKG data writes the same parsing again. Then each copy drifts,
and each copy has to be fixed separately when BMKG changes something. **This is that parsing,
done once, in one place.** A consumer gets typed JSON and has nothing of its own to maintain.

The off-the-shelf option was a Python wrapper whose hosting lapsed — its demo returns
`503 DEPLOYMENT_PAUSED`, and its MCP server proxies every tool call to that dead demo.
Depending on someone else's free tier is exactly what this avoids: **no origin server, no paid
plan, no machine that has to stay awake.** Everything fits inside Cloudflare's free tier.

## What you get

- **Numbers as numbers.** `magnitude: 6.2`, not `"6.2"`. `depth_km: 145.0`, not `"145 km"`.
- **One response shape.** `{ok, data, meta}` or `{ok, error:{code, message}}`. A 404 from BMKG
  becomes `upstream_not_found`, never an empty `200`.
- **A parse error is visible.** If BMKG changes a payload, you get `upstream_parse` naming the
  URL — not silently wrong values.
- **Current region codes.** 38 provinces. See below; this one matters.
- **GeoJSON-ready polygons.** CAP publishes `lat,lon`; warning detail returns `[lon, lat]`.

## Endpoints

### Earthquakes
| | |
|---|---|
| `GET /v1/earthquake/latest` | Most recent quake anywhere in Indonesia |
| `GET /v1/earthquake/recent` | Last 15 of magnitude 5.0+ |
| `GET /v1/earthquake/felt` | Quakes people reported feeling, with MMI per place |
| `GET /v1/earthquake/nearby?lat=&lon=&radius_km=` | Nearest first, with distance. Radius defaults to 500 km |

### Weather
| | |
|---|---|
| `GET /v1/weather/search?q=tebet` | **Start here.** Name in, forecast out, one call |
| `GET /v1/weather/{adm4}` | Three-day forecast in ~3-hourly slots |
| `GET /v1/weather/{adm4}/current` | The slot covering now, plus the next three |

### Severe-weather warnings
| | |
|---|---|
| `GET /v1/nowcast?lang=en\|id` | All active warnings |
| `GET /v1/nowcast/check?location=` | Warnings mentioning a place |
| `GET /v1/nowcast/{alert_code}?lang=` | Full CAP detail with the affected polygon |

### Regions
| | |
|---|---|
| `GET /v1/wilayah/provinces` | All 38 |
| `GET /v1/wilayah/districts?province_code=31` | Kabupaten/kota |
| `GET /v1/wilayah/subdistricts?district_code=31.74` | Kecamatan |
| `GET /v1/wilayah/villages?subdistrict_code=31.74.04` | Desa/kelurahan |
| `GET /v1/wilayah/search?q=&limit=&level=` | Any level, each hit with its full path |
| `GET /v1/wilayah/{code}` | One region |

### Meta
`GET /health` · `GET /v1/openapi.json` · `POST /mcp`

## Using it

Copy [`client/bmkg.ts`](client/bmkg.ts) into the consuming project. Zero dependencies, no build
step, works in a Cloudflare Worker, a browser and Node 18+. It is written without TypeScript
parameter properties so it runs under type-stripping toolchains too
(`node --experimental-strip-types`, esbuild, any bundler).

```ts
import { createBmkg, BmkgError } from "./bmkg";

const bmkg = createBmkg();

const quake = await bmkg.earthquake.latest();
quake.magnitude;              // 6.2 — a number
quake.depth_km;               // 145 — a number

const w = await bmkg.weather.find("tebet");
w.matched.path;               // "…› Tebet › Tebet Barat"
w.alternatives;               // what else the name could have meant
w.forecast.slots[0].temperature_c;

const near = await bmkg.earthquake.nearby({ lat: -6.2, lon: 106.8, radiusKm: 800 });
near[0].distance_km;
```

### From another Cloudflare Worker

Bind to it as a service. The call never leaves the edge — no public round trip, no DNS lookup,
no TLS handshake.

```toml
# the consumer's wrangler.toml
[[services]]
binding = "BMKG"
service = "bmkg-edge"
```

```ts
const bmkg = createBmkg({ fetcher: env.BMKG });
```

### Failures are typed

```ts
try {
  await bmkg.weather.byCode("99.99.99.9999");
} catch (e) {
  if (e instanceof BmkgError) {
    e.code;                 // "upstream_not_found"
    e.isMissingUpstream;    // valid code, BMKG has no data for it
    e.isSchemaDrift;        // BMKG moved — this repo needs a fix, not the caller
  }
}
```

### Watching it

`GET /health/deep` exercises every upstream and reports each one, answering `503` if any fails:

```json
{ "ok": true, "data": { "healthy": true, "checks": [
  { "source": "earthquake/latest", "ok": true, "detail": "1 item(s)",  "ms": 6 },
  { "source": "weather/forecast",  "ok": true, "detail": "20 item(s)", "ms": 278 }
] } }
```

That is the failure mode that matters once projects depend on this: not the Worker going down,
but BMKG changing a payload while every response stays `200`. Point a cron at one URL.

## The Papua problem

Indonesia split Papua into six provinces in 2022. BMKG followed; several public datasets did
not. If a wrapper ships the old 34-province list, every Papua lookup fails:

```
91.01.01.2001   old Merauke code   → BMKG returns 404
93.01.01.2001   current code       → BMKG returns 200, same village ("Nasem")
```

This project uses [`cahyadsn/wilayah`](https://github.com/cahyadsn/wilayah), which tracks
Kepmendagri 300.2.2-2430/2025. All 38 provinces, 91,599 regions.

```bash
curl "https://bmkg.irawan.dev/v1/wilayah/search?q=nasem"
# → 93.01.01.2001 · Papua Selatan › Kabupaten Merauke › Merauke › Nasem
```

## MCP

Twelve tools over the Model Context Protocol, streamable HTTP transport.

```bash
claude mcp add --transport http bmkg https://bmkg.irawan.dev/mcp
```

```json
{ "mcpServers": { "bmkg": { "url": "https://bmkg.irawan.dev/mcp" } } }
```

`get_latest_earthquake` · `get_recent_earthquakes` · `get_felt_earthquakes` ·
`get_nearby_earthquakes` · `find_weather` · `get_weather_forecast` · `get_current_weather` ·
`get_weather_warnings` · `check_location_warnings` · `get_warning_detail` · `search_regions` ·
`list_regions`

The tools call the route handlers **in the same isolate**. There is no internal HTTP hop and no
base URL pointing at another deployment, which is the failure mode this project was written to
avoid: a wrapper whose MCP server stayed up while the API it proxied went down answers every
tool call with an error.

## Caching

| Route | Cached |
|---|---|
| Latest / felt earthquakes | 60 s |
| Recent M5.0+ | 5 min |
| Weather | 10 min |
| Warning index | 2 min |
| Warning detail | 15 min |
| Regions | 24 h |

Two layers: upstream calls are held at Cloudflare's edge, and our own responses go through the
Cache API. No rate limit of our own.

## Self-hosting

Needs a Cloudflare account (free tier is enough) and Rust with the `wasm32-unknown-unknown`
target.

```bash
git clone https://github.com/galohot/bmkg-edge
cd bmkg-edge

rustup target add wasm32-unknown-unknown
cargo install worker-build --locked

cp wrangler.example.toml wrangler.toml     # fill in account_id
wrangler d1 create bmkg-wilayah --location apac
# put the printed database_id into wrangler.toml

make seed                                   # builds the region seed, loads it into D1
make deploy
```

`wrangler.toml` is gitignored: it holds account and database ids.

### Development

```bash
make test     # unit tests
make lint     # clippy, warnings as errors
make build    # release wasm
make size     # bundle size against the budget
make clean    # reclaim target/ — the only thing here that grows
```

`make clean` matters. Measured on this repo: **1.9 GB with `target/`, 580 KB after
`make distclean`.** The source itself is 192 KB. Rust build artefacts are the only thing here
that grows, and they are entirely disposable — a rebuild takes under a minute.

## Architecture

```
GET  /v1/*   ─┐
POST /mcp    ─┼─► Cloudflare Worker (Rust → WASM) ─┬─► BMKG (3 hosts, cached at the edge)
GET  /       ─┘                                    └─► D1 (91,599 regions + 130,023 tokens)
```

About 2,300 lines of Rust. The bundle is 802 KB of wasm — 281 KB gzipped — and starts in 8 ms.

Region search uses a token table rather than `LIKE '%x%'`: a full scan would read 91,599 rows
per query, and D1's free tier is billed in rows read, so that would run out at around 55
searches a day.

## What it replaces

Nothing is migrated yet — this is the layer, not the migration. The duplication it exists to
remove, measured in the sibling projects:

| Where | Lines | Doing |
|---|---|---|
| `cakrawala/worker/sources/gempa.ts` | 106 | Its own `BmkgGempa` type, regex number-stripper, coordinate split |
| `cakrawala/worker/sources/cuaca.ts` | 100 | The same again for forecasts |
| `gempa-cek/src/index.js` | — | Parses `autogempa` and `gempaterkini` a third time |

Each of those is a separate copy that has to be fixed separately when BMKG moves.

## Attribution and limits

Data from [BMKG](https://data.bmkg.go.id), Indonesia's Agency for Meteorology, Climatology and
Geophysics — public domain. Region codes from
[`cahyadsn/wilayah`](https://github.com/cahyadsn/wilayah) (MIT).

This service is **not affiliated with or endorsed by BMKG**. It reads their public feeds and
reshapes them. For anything life-safety critical, use BMKG directly.

MIT.

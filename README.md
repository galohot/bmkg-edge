# bmkg-edge

**BMKG's Indonesian open data as a typed JSON API, plus an MCP endpoint, on one Cloudflare Worker.**

Earthquakes, weather forecasts, severe-weather warnings and the full Indonesian region-code
tree. No API key, no account, CORS open.

**Live:** https://bmkg.irawan.dev · [OpenAPI](https://bmkg.irawan.dev/v1/openapi.json)

```bash
curl https://bmkg.irawan.dev/v1/earthquake/latest
curl "https://bmkg.irawan.dev/v1/weather/search?q=tebet"
curl https://bmkg.irawan.dev/v1/nowcast
```

## Why

BMKG publishes its data openly, but raw. Magnitudes arrive as strings, one field holds
`"lat,lon"` as text, forecasts nest three arrays deep, and warnings are CAP XML. Every
consumer writes the same parsing code.

Existing wrappers solved that and then their hosting lapsed. This one is built so there is
nothing to lapse: **no origin server, no paid plan, no machine that has to stay awake.** One
Worker serves the REST API, the MCP endpoint and the documentation page. Everything fits
inside Cloudflare's free tier.

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

## Attribution and limits

Data from [BMKG](https://data.bmkg.go.id), Indonesia's Agency for Meteorology, Climatology and
Geophysics — public domain. Region codes from
[`cahyadsn/wilayah`](https://github.com/cahyadsn/wilayah) (MIT).

This service is **not affiliated with or endorsed by BMKG**. It reads their public feeds and
reshapes them. For anything life-safety critical, use BMKG directly.

MIT.

/**
 * Typed client for bmkg-edge.
 *
 * Zero dependencies, no build step. Works in a Cloudflare Worker, a browser and Node 18+.
 * Copy this file into a project, or import it from the repo.
 *
 * The point of this file: BMKG's raw payloads should be parsed in exactly one place, and
 * that place is the bmkg-edge Worker. A consumer gets types and nothing else to maintain.
 *
 *   import { createBmkg } from "./bmkg";
 *
 *   // 1. Public URL — browsers, Node, anything.
 *   const bmkg = createBmkg();
 *
 *   // 2. Cloudflare service binding — Worker to Worker, no public round trip.
 *   //    wrangler.toml:  [[services]] binding = "BMKG"  service = "bmkg-edge"
 *   const bmkg = createBmkg({ fetcher: env.BMKG });
 *
 *   const quake = await bmkg.earthquake.latest();
 *   quake.magnitude;   // number, not "6.2"
 */

export const DEFAULT_BASE_URL = "https://bmkg.irawan.dev";

// ---------------------------------------------------------------- types

/** 1 province · 2 kabupaten/kota · 3 kecamatan · 4 desa/kelurahan */
export type RegionLevel = 1 | 2 | 3 | 4;

export type Lang = "en" | "id";

export interface Region {
  code: string;
  name: string;
  level: RegionLevel;
  parent?: string;
  /** "DKI Jakarta › Kota Adm. Jakarta Selatan › Pasar Minggu › Pejaten Barat" — search only. */
  path?: string;
}

export interface Quake {
  /** ISO 8601 with offset. */
  datetime: string;
  /** As BMKG prints it for an Indonesian reader. */
  date_local: string;
  time_local: string;
  latitude: number;
  longitude: number;
  magnitude: number;
  depth_km: number;
  region: string;
  /** Present on latest/recent. */
  tsunami_potential?: string;
  /** Present on the felt feed: MMI per place. */
  felt_intensity?: string;
  shakemap_url?: string;
  /** Set by `nearby()` only. */
  distance_km?: number;
}

export interface WeatherLocation {
  adm1: string;
  adm2: string;
  adm3: string;
  adm4: string;
  province: string;
  city: string;
  district: string;
  village: string;
  latitude: number;
  longitude: number;
  timezone: string;
}

export interface WeatherSlot {
  /** UTC, ISO 8601. */
  datetime: string;
  /** Same instant in the location's timezone. */
  datetime_local?: string;
  /** 0 = today, 1 = tomorrow, 2 = the day after. */
  day_index: number;
  temperature_c: number;
  humidity_percent?: number;
  cloud_cover_percent?: number;
  precipitation_mm?: number;
  wind_speed_kmh?: number;
  wind_from?: string;
  wind_to?: string;
  wind_degrees?: number;
  visibility_m?: number;
  visibility_text?: string;
  weather_code?: number;
  /** English description. */
  weather?: string;
  /** Indonesian description. */
  weather_id?: string;
  icon_url?: string;
}

export interface Forecast {
  location: WeatherLocation;
  slots: WeatherSlot[];
}

export interface CurrentWeather {
  location: WeatherLocation;
  /** The slot covering right now. */
  now: WeatherSlot;
  /** The next three. */
  next: WeatherSlot[];
}

export interface ResolvedWeather {
  /** Which region the free-text query was understood as. */
  matched: Region;
  /** Other regions the name could have meant. Check this before trusting `matched`. */
  alternatives: Region[];
  forecast: Forecast;
}

export interface Warning {
  alert_code: string;
  headline: string;
  /** Best effort, read off the headline. The authoritative area is in the detail. */
  area?: string;
  description: string;
  category?: string;
  published?: string;
  detail_url: string;
}

export interface WarningArea {
  description: string;
  /** GeoJSON order: [longitude, latitude]. */
  polygon: [number, number][];
}

export interface WarningDetail {
  alert_code: string;
  identifier: string;
  sender: string;
  sent: string;
  status: string;
  message_type: string;
  language: string;
  category: string;
  event: string;
  urgency: string;
  severity: string;
  certainty: string;
  effective: string;
  expires: string;
  headline: string;
  description: string;
  infographic_url?: string;
  areas: WarningArea[];
}

export interface Health {
  status: string;
  version: string;
  upstreams: string[];
}

export interface DeepHealthCheck {
  source: string;
  ok: boolean;
  detail: string;
  ms: number;
}

export interface DeepHealth {
  /** False when an upstream failed. Distinct from the envelope's `ok`, which is about the request. */
  healthy: boolean;
  version: string;
  checks: DeepHealthCheck[];
}

export interface ResponseMeta {
  source: string;
  cached_seconds: number;
  generated_at: string;
}

/**
 * What a failed call throws. `code` is stable; `message` is for humans.
 *
 * Written with plain field assignment rather than TypeScript parameter properties, so this
 * file works under type-stripping toolchains (`node --experimental-strip-types`, esbuild,
 * bundlers) and not only under a full `tsc`.
 */
export class BmkgError extends Error {
  /** not_found · bad_request · upstream_not_found · upstream_unavailable · upstream_parse · internal · transport */
  code: string;
  status: number;
  url: string;

  constructor(code: string, message: string, status: number, url: string) {
    super(message);
    this.name = "BmkgError";
    this.code = code;
    this.status = status;
    this.url = url;
  }

  /** True when BMKG itself has no record — usually a valid but unmonitored region code. */
  get isMissingUpstream(): boolean {
    return this.code === "upstream_not_found";
  }

  /** True when BMKG answered in a shape the API does not recognise. Means bmkg-edge needs updating. */
  get isSchemaDrift(): boolean {
    return this.code === "upstream_parse";
  }
}

// ---------------------------------------------------------------- client

/** Anything with a `fetch` method: a Cloudflare service binding, or a stub in a test. */
export interface FetchLike {
  fetch(input: string, init?: RequestInit): Promise<Response>;
}

export interface BmkgOptions {
  /**
   * Where to send requests. Defaults to the public deployment. When `fetcher` is a
   * service binding this is only used to build a syntactically valid URL — the request
   * never leaves the edge, so the host is arbitrary but must be present.
   */
  baseUrl?: string;
  /** Global `fetch` by default. Pass `env.BMKG` to go through a service binding. */
  fetcher?: FetchLike | typeof fetch;
  /** Abort a call that takes longer than this. Default 10000. Pass 0 to disable. */
  timeoutMs?: number;
  /** Extra headers on every request — a `user-agent` identifying the caller is polite. */
  headers?: Record<string, string>;
}

/** The last response's `meta` block, if you want to know how stale a value is. */
export interface WithMeta<T> {
  data: T;
  meta: ResponseMeta;
}

export function createBmkg(options: BmkgOptions = {}) {
  const baseUrl = (options.baseUrl ?? DEFAULT_BASE_URL).replace(/\/+$/, "");
  const timeoutMs = options.timeoutMs ?? 10_000;
  const extraHeaders = options.headers ?? {};

  const doFetch: (input: string, init?: RequestInit) => Promise<Response> = (() => {
    const f = options.fetcher;
    if (!f) return (input, init) => fetch(input, init);
    if (typeof f === "function") return (input, init) => f(input, init);
    return (input, init) => f.fetch(input, init);
  })();

  async function request<T>(path: string, query?: Record<string, unknown>): Promise<WithMeta<T>> {
    const url = new URL(baseUrl + path);
    for (const [k, v] of Object.entries(query ?? {})) {
      if (v !== undefined && v !== null && v !== "") url.searchParams.set(k, String(v));
    }
    const href = url.toString();

    let controller: AbortController | undefined;
    let timer: ReturnType<typeof setTimeout> | undefined;
    if (timeoutMs > 0) {
      controller = new AbortController();
      timer = setTimeout(() => controller!.abort(), timeoutMs);
    }

    let res: Response;
    try {
      res = await doFetch(href, {
        headers: { accept: "application/json", ...extraHeaders },
        signal: controller?.signal,
      });
    } catch (err) {
      throw new BmkgError(
        "transport",
        err instanceof Error ? err.message : String(err),
        0,
        href,
      );
    } finally {
      if (timer) clearTimeout(timer);
    }

    let body: unknown;
    try {
      body = await res.json();
    } catch {
      throw new BmkgError("transport", `response was not JSON (HTTP ${res.status})`, res.status, href);
    }

    const envelope = body as
      | { ok: true; data: T; meta: ResponseMeta }
      | { ok: false; error: { code: string; message: string } };

    if (!envelope || typeof envelope !== "object" || !("ok" in envelope)) {
      throw new BmkgError("transport", "response was not a bmkg-edge envelope", res.status, href);
    }
    if (envelope.ok === false) {
      throw new BmkgError(envelope.error.code, envelope.error.message, res.status, href);
    }
    return { data: envelope.data, meta: envelope.meta };
  }

  /** Unwrap to just the data — what almost every caller wants. */
  const get = async <T>(path: string, query?: Record<string, unknown>): Promise<T> =>
    (await request<T>(path, query)).data;

  return {
    /** Escape hatch: any path, with the `meta` block attached. */
    raw: request,

    health: () => get<Health>("/health"),
    /** Exercises every upstream. `healthy:false` means BMKG moved, not that the Worker is down. */
    healthDeep: () => get<DeepHealth>("/health/deep"),

    earthquake: {
      latest: () => get<Quake>("/v1/earthquake/latest"),
      /** Last 15 of magnitude 5.0+. */
      recent: () => get<Quake[]>("/v1/earthquake/recent"),
      /** Quakes people reported feeling, with MMI per place. */
      felt: () => get<Quake[]>("/v1/earthquake/felt"),
      /** Nearest first, each carrying `distance_km`. */
      nearby: (p: { lat: number; lon: number; radiusKm?: number }) =>
        get<Quake[]>("/v1/earthquake/nearby", {
          lat: p.lat,
          lon: p.lon,
          radius_km: p.radiusKm,
        }),
    },

    weather: {
      /** Three-day forecast for a level-4 code, e.g. "31.74.04.1006". */
      byCode: (adm4: string) => get<Forecast>(`/v1/weather/${encodeURIComponent(adm4)}`),
      /** The slot covering now, plus the next three. */
      current: (adm4: string) =>
        get<CurrentWeather>(`/v1/weather/${encodeURIComponent(adm4)}/current`),
      /** Name in, forecast out. Use this when you do not already hold a code. */
      find: (query: string) => get<ResolvedWeather>("/v1/weather/search", { q: query }),
    },

    warnings: {
      list: (lang: Lang = "en") => get<Warning[]>("/v1/nowcast", { lang }),
      /** Text match over the live warning list. */
      check: (location: string, lang: Lang = "en") =>
        get<Warning[]>("/v1/nowcast/check", { location, lang }),
      detail: (alertCode: string, lang: Lang = "en") =>
        get<WarningDetail>(`/v1/nowcast/${encodeURIComponent(alertCode)}`, { lang }),
    },

    regions: {
      provinces: () => get<Region[]>("/v1/wilayah/provinces"),
      districts: (provinceCode: string) =>
        get<Region[]>("/v1/wilayah/districts", { province_code: provinceCode }),
      subdistricts: (districtCode: string) =>
        get<Region[]>("/v1/wilayah/subdistricts", { district_code: districtCode }),
      villages: (subdistrictCode: string) =>
        get<Region[]>("/v1/wilayah/villages", { subdistrict_code: subdistrictCode }),
      /** Word-prefix search across all levels. Each hit carries its full `path`. */
      search: (query: string, opts: { limit?: number; level?: RegionLevel } = {}) =>
        get<Region[]>("/v1/wilayah/search", { q: query, limit: opts.limit, level: opts.level }),
      get: (code: string) => get<Region>(`/v1/wilayah/${encodeURIComponent(code)}`),
    },
  };
}

export type Bmkg = ReturnType<typeof createBmkg>;

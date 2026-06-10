import { useEffect, useMemo, useRef, useState } from "preact/hooks";

// ----------------------------------------------------------------------
// Constants
// ----------------------------------------------------------------------
const FT_TO_KM = 0.0003048; // feet -> km
const MAX_POINTS = 150000; // capped rolling cloud (ring-buffer once full)
const DEG_KM = 111.194; // km per degree of latitude
const REANCHOR_DRIFT_KM = 25; // floating centroid re-anchor threshold

// Accumulation decimation rule (per-icao throttle, mirrors the collector).
const ACC_MIN_MOVE_KM = 1; // store if moved >= 1 km
const ACC_MIN_ALT_FT = 500; // or >= 500 ft alt change
const ACC_MIN_INTERVAL_MS = 8000; // or >= 8 s elapsed

// Bearing x altitude envelope: alt 0..50000 step 2500 (21 levels) x 72 bins of 5deg.
const ENV_ALT_STEP = 2500;
const ENV_ALT_MAX = 50000;
const ENV_ALT_LEVELS = ENV_ALT_MAX / ENV_ALT_STEP + 1; // 21
const ENV_BIN_DEG = 5;
const ENV_BINS = 360 / ENV_BIN_DEG; // 72
const HULL_RING_STRIDE = ENV_BINS + 1; // 73 (duplicated seam vertex; col 72 == col 0)
const SINGLE_INSTANCE = [{ position: [0, 0, 0] }];

// Slice (top-down cross-section) panel.
const SLICE_RINGS = [100, 200, 300]; // km, scope range rings inside GRID_EXTENT=300
const SLICE_RANGE_KM = 300; // outermost ring -> canvas edge
const SLICE_ALT_MAX = 45000; // matches altColor domain + legend
const SLICE_HALF = 1125; // +/- ft -> ~2250 ft slab
const SLICE_CANVAS = 240; // square canvas CSS px (drawn at devicePixelRatio)

// Basemap mosaic.
const BASEMAP_ZOOM = 8; // fixed slippy zoom for the mosaic
const BASEMAP_RADIUS_KM = 360; // cover a square of this radius around the origin
const BASEMAP_Z = -0.05; // km, just below the z=0 ground plane
const BASEMAP_SUBS = ["a", "b", "c"];

// Ground grid: segments every 50 km out to +/-300 km.
const GRID_EXTENT = 300;
const GRID_STEP = 50;

// Range rings (closed 72-seg loops).
const RING_RADII = [25, 50, 100, 150, 200, 250];

// The 7 layer-visibility chips (deck layers only; Profile/Slice are DOM panels).
const LAYER_CHIPS = [
  ["cloud", "Coverage"],
  ["planes", "Live planes"],
  ["trails", "Trails"],
  ["rings", "Range rings"],
  ["grid", "Ground grid"],
  ["basemap", "Map"],
  ["hull", "Hull"],
];

// One module-level plane silhouette data-URL (points +Y / north at angle 0).
// mask:true lets getColor tint it by altitude. Same object reused every frame.
const PLANE_DATAURL = "data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHdpZHRoPSI2NCIgaGVpZ2h0PSI2NCIgdmlld0JveD0iMCAwIDY0IDY0Ij48cGF0aCBmaWxsPSIjZmZmZmZmIiBkPSJNMzIgMyBDMzAgMyAyOC41IDUgMjguMiA5LjUgTDI3LjYgMjQgTDYgMzYgTDYgNDEgTDI3LjQgMzUuMiBMMjcuMSA0OSBMMTkgNTQgTDE5IDU4IEwzMiA1NS41IEw0NSA1OCBMNDUgNTQgTDM2LjkgNDkgTDM2LjYgMzUuMiBMNTggNDEgTDU4IDM2IEwzNi40IDI0IEwzNS44IDkuNSBDMzUuNSA1IDM0IDMgMzIgMyBaIi8+PC9zdmc+";
const PLANE_ICON = { url: PLANE_DATAURL, width: 64, height: 64, anchorX: 32, anchorY: 32, mask: true };

// ----------------------------------------------------------------------
// Colormap — single source of truth. Feeds cloud, hull, slice, and legend.
// Turbo-like, dark -> bright. Domain: altitude in FEET, t = clamp(alt/45000).
// ----------------------------------------------------------------------
const TURBO = [
  [0.00, [48, 18, 59]], [0.13, [62, 74, 211]], [0.25, [40, 156, 252]], [0.38, [24, 214, 203]],
  [0.50, [69, 248, 134]], [0.63, [163, 255, 52]], [0.75, [251, 210, 36]], [0.88, [251, 123, 23]], [1.00, [220, 38, 18]],
];

function turboAt(t) {
  t = t < 0 ? 0 : t > 1 ? 1 : t;
  for (let i = 1; i < TURBO.length; i++) {
    const t2 = TURBO[i][0];
    const c2 = TURBO[i][1];
    if (t <= t2) {
      const t1 = TURBO[i - 1][0];
      const c1 = TURBO[i - 1][1];
      const f = (t - t1) / ((t2 - t1) || 1);
      return [
        Math.round(c1[0] + (c2[0] - c1[0]) * f),
        Math.round(c1[1] + (c2[1] - c1[1]) * f),
        Math.round(c1[2] + (c2[2] - c1[2]) * f),
      ];
    }
  }
  return TURBO[TURBO.length - 1][1];
}

// altColor(): altitude (feet) -> [r,g,b]. Single source for LUT + legend + live layers.
function altColor(altFt) {
  return turboAt(altFt / 45000);
}

// 256-entry altitude->color LUT so the cloud build loop indexes instead of interpolating.
const ALT_LUT = new Uint8Array(256 * 3);
for (let i = 0; i < 256; i++) {
  const c = altColor((i / 255) * 45000);
  ALT_LUT[i * 3] = c[0];
  ALT_LUT[i * 3 + 1] = c[1];
  ALT_LUT[i * 3 + 2] = c[2];
}
function altColorLUT(altFt) {
  let t = altFt / 45000;
  t = t < 0 ? 0 : t > 1 ? 1 : t;
  return Math.round(t * 255) * 3; // returns LUT base index (caller reads 3 bytes)
}
function altColorA(altFt, alpha) {
  const c = altColor(altFt);
  return [c[0], c[1], c[2], alpha];
}
function rgbStr(c) {
  return "rgb(" + c[0] + "," + c[1] + "," + c[2] + ")";
}

// ----------------------------------------------------------------------
// Numeric helpers (rsdb items carry no RSSI; altitude-only coloring).
// ----------------------------------------------------------------------
function numeric(value) {
  return typeof value === "number" && Number.isFinite(value);
}

function hasPositionItem(item) {
  return numeric(item.lat) && numeric(item.lon);
}

// ----------------------------------------------------------------------
// Static geometry (grid, rings, labels) — built once at module load.
// ----------------------------------------------------------------------
const gridLines = [];
for (let g = -GRID_EXTENT; g <= GRID_EXTENT; g += GRID_STEP) {
  if (g === 0) continue; // axis lines drawn separately, brighter
  gridLines.push({ source: [g, -GRID_EXTENT, 0], target: [g, GRID_EXTENT, 0] }); // verticals (vary E)
  gridLines.push({ source: [-GRID_EXTENT, g, 0], target: [GRID_EXTENT, g, 0] }); // horizontals (vary N)
}
const axisLines = [
  { source: [-GRID_EXTENT, 0, 0], target: [GRID_EXTENT, 0, 0] },
  { source: [0, -GRID_EXTENT, 0], target: [0, GRID_EXTENT, 0] },
];

const rings = RING_RADII.map(function (r) {
  const pts = [];
  for (let i = 0; i <= 72; i++) {
    const th = (i / 72) * Math.PI * 2;
    pts.push([r * Math.cos(th), r * Math.sin(th), 0]);
  }
  return { radius: r, points: pts };
});

const ringLabels = RING_RADII.map(function (r) {
  return { text: r + " km", position: [0, r, 0] };
});
const cardinalLabels = [
  { text: "N", position: [0, 260, 0] },
  { text: "E", position: [260, 0, 0] },
  { text: "S", position: [0, -260, 0] },
  { text: "W", position: [-260, 0, 0] },
];
const allTextLabels = ringLabels.concat(cardinalLabels);

// ----------------------------------------------------------------------
// Slippy-tile math (Web Mercator) for the basemap mosaic.
// ----------------------------------------------------------------------
function lngLatToTile(lng, lat, z) {
  const n = Math.pow(2, z);
  const x = ((lng + 180) / 360) * n;
  const latRad = (lat * Math.PI) / 180;
  const y = ((1 - Math.log(Math.tan(latRad) + 1 / Math.cos(latRad)) / Math.PI) / 2) * n;
  return [x, y];
}
function tileToLngLat(x, y, z) {
  const n = Math.pow(2, z);
  const lng = (x / n) * 360 - 180;
  const latRad = Math.atan(Math.sinh(Math.PI * (1 - (2 * y) / n)));
  const lat = (latRad * 180) / Math.PI;
  return [lng, lat];
}

// Compute the FIXED per-tile descriptors for an origin (slippy math + ENU corners + url).
function computeBasemapTiles(origin) {
  const lat0 = origin.lat;
  const lon0 = origin.lon;
  const cosLat0 = Math.cos((lat0 * Math.PI) / 180);
  const z = BASEMAP_ZOOM;
  const n = Math.pow(2, z);
  const dLat = BASEMAP_RADIUS_KM / DEG_KM;
  const dLng = BASEMAP_RADIUS_KM / (DEG_KM * cosLat0);
  const west = lon0 - dLng;
  const east = lon0 + dLng;
  const north = lat0 + dLat;
  const south = lat0 - dLat;
  const tNW = lngLatToTile(west, north, z);
  const tSE = lngLatToTile(east, south, z);
  let xMin = Math.floor(tNW[0]);
  let xMax = Math.floor(tSE[0]);
  let yMin = Math.floor(tNW[1]);
  let yMax = Math.floor(tSE[1]);
  xMin = Math.max(0, Math.min(n - 1, xMin));
  xMax = Math.max(0, Math.min(n - 1, xMax));
  yMin = Math.max(0, Math.min(n - 1, yMin));
  yMax = Math.max(0, Math.min(n - 1, yMax));

  // local ENU forward projection for THIS origin.
  function enuEast(lng) {
    return (lng - lon0) * DEG_KM * cosLat0;
  }
  function enuNorth(lat) {
    return (lat - lat0) * DEG_KM;
  }

  const tiles = [];
  for (let x = xMin; x <= xMax; x++) {
    for (let y = yMin; y <= yMax; y++) {
      const nw = tileToLngLat(x, y, z); // [west lng, north lat]
      const se = tileToLngLat(x + 1, y + 1, z); // [east lng, south lat]
      const wLng = nw[0];
      const nLat = nw[1];
      const eLng = se[0];
      const sLat = se[1];
      const leftKm = enuEast(wLng);
      const rightKm = enuEast(eLng);
      const topKm = enuNorth(nLat);
      const bottomKm = enuNorth(sLat);
      // BitmapLayer bounds 4-corner order: [[left,bottom],[left,top],[right,top],[right,bottom]].
      const bounds = [
        [leftKm, bottomKm, BASEMAP_Z],
        [leftKm, topKm, BASEMAP_Z],
        [rightKm, topKm, BASEMAP_Z],
        [rightKm, bottomKm, BASEMAP_Z],
      ];
      const sub = BASEMAP_SUBS[(x + y) % BASEMAP_SUBS.length];
      const url = "https://" + sub + ".basemaps.cartocdn.com/dark_all/" + z + "/" + x + "/" + y + "@2x.png";
      tiles.push({ id: "basemap-" + z + "-" + x + "-" + y, url: url, bounds: bounds });
    }
  }
  return tiles;
}

// ----------------------------------------------------------------------
// Procedural low-poly 3D aircraft meshes (built once at module load).
// Canonical axes: forward=+X, up=+Z, left=+Y, units ~meters.
// Flat normals (no shared vertices) for crisp low-poly facets.
// ----------------------------------------------------------------------
function meshFromTris(tris) {
  const positions = new Float32Array(tris.length * 9);
  const normals = new Float32Array(tris.length * 9);
  const indices = new Uint16Array(tris.length * 3);
  for (let t = 0; t < tris.length; t++) {
    const a = tris[t][0];
    const b = tris[t][1];
    const c = tris[t][2];
    const ux = b[0] - a[0];
    const uy = b[1] - a[1];
    const uz = b[2] - a[2];
    const vx = c[0] - a[0];
    const vy = c[1] - a[1];
    const vz = c[2] - a[2];
    let nx = uy * vz - uz * vy;
    let ny = uz * vx - ux * vz;
    let nz = ux * vy - uy * vx;
    const L = Math.hypot(nx, ny, nz) || 1;
    nx /= L;
    ny /= L;
    nz /= L;
    for (let k = 0; k < 3; k++) {
      const v = [a, b, c][k];
      const o = (t * 3 + k) * 3;
      positions[o] = v[0];
      positions[o + 1] = v[1];
      positions[o + 2] = v[2];
      normals[o] = nx;
      normals[o + 1] = ny;
      normals[o + 2] = nz;
      indices[t * 3 + k] = t * 3 + k;
    }
  }
  return {
    attributes: {
      positions: { value: positions, size: 3 },
      normals: { value: normals, size: 3 },
    },
    indices: indices,
  };
}
const quad = (p0, p1, p2, p3) => [[p0, p1, p2], [p0, p2, p3]];

function buildAirplane() {
  const T = [];
  const nose = [16, 0, 0];
  const tail = [-14, 0, 0];
  const ring = [[-1.6, -1.4], [1.6, -1.4], [1.6, 1.4], [-1.6, 1.4]];
  const F = ring.map(([y, z]) => [6, y, z]);
  const R = ring.map(([y, z]) => [-10, y * 0.5, z * 0.5]);
  for (let i = 0; i < 4; i++) {
    const j = (i + 1) % 4;
    T.push([nose, F[i], F[j]]);
  }
  for (let i = 0; i < 4; i++) {
    const j = (i + 1) % 4;
    T.push(...quad(F[i], F[j], R[j], R[i]));
  }
  for (let i = 0; i < 4; i++) {
    const j = (i + 1) % 4;
    T.push([R[j], R[i], tail]);
  }
  T.push([[3, 0, 0], [-3, 14, 0.2], [-1, 1, 0]]);
  T.push([[3, 0, 0], [-1, -1, 0], [-3, -14, 0.2]]);
  T.push([[-9, 0, 0], [-13, 5, 0.4], [-12, 0, 0]]);
  T.push([[-9, 0, 0], [-12, 0, 0], [-13, -5, 0.4]]);
  T.push([[-9, 0, 0], [-13, 0, 5], [-13, 0, 0.5]]);
  return meshFromTris(T);
}
function buildHelicopter() {
  const T = [];
  const bx0 = -3;
  const bx1 = 6;
  const by = 2.2;
  const bz0 = -1.8;
  const bz1 = 2.2;
  const c = [
    [bx1, -by, bz0], [bx1, by, bz0], [bx1, by, bz1], [bx1, -by, bz1],
    [bx0, -by, bz0], [bx0, by, bz0], [bx0, by, bz1], [bx0, -by, bz1],
  ];
  T.push(...quad(c[0], c[1], c[2], c[3]));
  T.push(...quad(c[5], c[4], c[7], c[6]));
  T.push(...quad(c[3], c[2], c[6], c[7]));
  T.push(...quad(c[4], c[5], c[1], c[0]));
  T.push(...quad(c[1], c[5], c[6], c[2]));
  T.push(...quad(c[4], c[0], c[3], c[7]));
  const tb0 = -3;
  const tb1 = -16;
  const tt = 0.5;
  const tz = 1.2;
  T.push(...quad([tb0, -tt, tz - tt], [tb0, tt, tz - tt], [tb1, tt, tz - tt], [tb1, -tt, tz - tt]));
  T.push(...quad([tb0, -tt, tz + tt], [tb1, -tt, tz + tt], [tb1, tt, tz + tt], [tb0, tt, tz + tt]));
  const rr = 14;
  const rz = 3;
  const ctr = [0, 0, rz];
  for (let i = 0; i < 12; i++) {
    const a0 = (i / 12) * 2 * Math.PI;
    const a1 = ((i + 1) / 12) * 2 * Math.PI;
    T.push([ctr, [rr * Math.cos(a0), rr * Math.sin(a0), rz], [rr * Math.cos(a1), rr * Math.sin(a1), rz]]);
  }
  const tr = 3;
  const tc = [tb1, 0, tz];
  for (let i = 0; i < 8; i++) {
    const a0 = (i / 8) * 2 * Math.PI;
    const a1 = ((i + 1) / 8) * 2 * Math.PI;
    T.push([tc, [tb1, tr * Math.sin(a0), tz + tr * Math.cos(a0)], [tb1, tr * Math.sin(a1), tz + tr * Math.cos(a1)]]);
  }
  return meshFromTris(T);
}
const AIRPLANE_MESH = buildAirplane();
const HELICOPTER_MESH = buildHelicopter();

// Orientation: verified yaw = 90 - track.
function getOrientation(a) {
  const yaw = 90 - a.track; // track CW-from-north -> deck yaw (CCW about +Z)
  let pitch = 0;
  if (typeof a.vrate === "number" && isFinite(a.vrate)) {
    pitch = Math.max(-15, Math.min(15, -a.vrate / 500)); // negate: +pitch = nose DOWN, climb = nose up
  }
  return [pitch, yaw, 0]; // [pitch, yaw, roll]
}

// ----------------------------------------------------------------------
// ENU projector + accumulator factory.
// enu(lon,lat) = [(lon-lon0)*DEG_KM*cos(lat0), (lat-lat0)*DEG_KM]   (km)
// ----------------------------------------------------------------------
function makeEnu(origin) {
  const lat0 = origin.lat;
  const lon0 = origin.lon;
  const cosLat0 = Math.cos((lat0 * Math.PI) / 180);
  return function enu(lon, lat) {
    return [(lon - lon0) * DEG_KM * cosLat0, (lat - lat0) * DEG_KM];
  };
}

// Centroid of currently positioned items (used when no fixed receiverSite).
function centroidOf(items) {
  let sumLat = 0;
  let sumLon = 0;
  let n = 0;
  for (const item of items) {
    if (hasPositionItem(item)) {
      sumLat += item.lat;
      sumLon += item.lon;
      n += 1;
    }
  }
  if (n === 0) return null;
  return { lat: sumLat / n, lon: sumLon / n };
}

// Haversine km (origin drift check).
function haversineKm(lat1, lon1, lat2, lon2) {
  const R = 6371;
  const toRad = (v) => (v * Math.PI) / 180;
  const phi1 = toRad(lat1);
  const phi2 = toRad(lat2);
  const dPhi = toRad(lat2 - lat1);
  const dLam = toRad(lon2 - lon1);
  const a = Math.sin(dPhi / 2) ** 2 + Math.cos(phi1) * Math.cos(phi2) * Math.sin(dLam / 2) ** 2;
  return R * 2 * Math.atan2(Math.sqrt(a), Math.sqrt(1 - a));
}

// Fresh accumulator anchored at an origin (clears cloud/envelope/lastSeen/icaoSeen).
function makeAccumulator(origin, fixed) {
  return {
    origin: origin,
    fixed: fixed,
    enu: makeEnu(origin),
    cloud: new Float32Array(MAX_POINTS * 3), // [east, north, alt_ft] ring buffer
    cloudCount: 0, // number of valid slots filled
    cloudHead: 0, // next write index (wraps once full)
    cloudFull: false,
    lastSeen: new Map(), // icao -> { east, north, altFt, timeMs }
    icaoSeen: new Set(),
    envelope: new Float32Array(ENV_ALT_LEVELS * ENV_BINS), // max range per [altLevel, bearingBin]
    tiles: computeBasemapTiles(origin),
    maxRangeKm: 0,
    maxAltFt: 0,
  };
}

// Bearing degrees (CW from north) of an ENU offset.
function bearingFromEnu(east, north) {
  let deg = (Math.atan2(east, north) * 180) / Math.PI;
  deg = ((deg % 360) + 360) % 360;
  return deg;
}

// Run one accumulation pass over positioned items at time nowMs.
// Returns true if the cloud or envelope changed (so a deck rebuild is warranted).
function accumulate(acc, items, nowMs) {
  let changed = false;
  for (const item of items) {
    if (!hasPositionItem(item)) continue;
    const icao = item.icao;
    if (!icao) continue;
    const enuPt = acc.enu(item.lon, item.lat);
    const east = enuPt[0];
    const north = enuPt[1];
    const altFt = numeric(item.altitude_baro_ft)
      ? item.altitude_baro_ft
      : numeric(item.altitude_geometric_ft)
        ? item.altitude_geometric_ft
        : 0;
    const time = numeric(item.position_last_seen_ms)
      ? item.position_last_seen_ms
      : numeric(item.last_seen_ms)
        ? item.last_seen_ms
        : nowMs;

    const prev = acc.lastSeen.get(icao);
    let store = false;
    if (!prev) {
      store = true; // new icao
    } else {
      const movedKm = Math.hypot(east - prev.east, north - prev.north);
      const altDelta = Math.abs(altFt - prev.altFt);
      const elapsedMs = time - prev.timeMs;
      if (movedKm >= ACC_MIN_MOVE_KM || altDelta >= ACC_MIN_ALT_FT || elapsedMs >= ACC_MIN_INTERVAL_MS) {
        store = true;
      }
    }
    if (!store) continue;

    acc.lastSeen.set(icao, { east: east, north: north, altFt: altFt, timeMs: time });
    acc.icaoSeen.add(icao);

    // Append into the capped rolling cloud (ring buffer once full).
    const slot = acc.cloudHead;
    acc.cloud[slot * 3] = east;
    acc.cloud[slot * 3 + 1] = north;
    acc.cloud[slot * 3 + 2] = altFt;
    acc.cloudHead = (acc.cloudHead + 1) % MAX_POINTS;
    if (acc.cloudFull) {
      // count stays at MAX_POINTS
    } else if (acc.cloudHead === 0) {
      acc.cloudFull = true;
      acc.cloudCount = MAX_POINTS;
    } else {
      acc.cloudCount = acc.cloudHead;
    }
    changed = true;

    // Update bearing x altitude envelope (max range per bin).
    const rangeKm = Math.hypot(east, north);
    if (rangeKm > acc.maxRangeKm) acc.maxRangeKm = rangeKm;
    if (altFt > acc.maxAltFt) acc.maxAltFt = altFt;
    let lvl = Math.round(altFt / ENV_ALT_STEP);
    if (lvl < 0) lvl = 0;
    if (lvl > ENV_ALT_LEVELS - 1) lvl = ENV_ALT_LEVELS - 1;
    let bin = Math.floor(bearingFromEnu(east, north) / ENV_BIN_DEG);
    if (bin < 0) bin = 0;
    if (bin > ENV_BINS - 1) bin = ENV_BINS - 1;
    const ei = lvl * ENV_BINS + bin;
    if (rangeKm > acc.envelope[ei]) acc.envelope[ei] = rangeKm;
  }
  return changed;
}

// ----------------------------------------------------------------------
// Coverage binary build — EXAG + altitude color baked into typed arrays.
// New object reference -> deck shallow-compare fires the GPU upload once.
// ----------------------------------------------------------------------
function buildCoverageBinary(acc, exag) {
  const N = acc.cloudCount;
  const posF32 = new Float32Array(N * 3);
  const colU8 = new Uint8Array(N * 3);
  for (let i = 0; i < N; i++) {
    const e = acc.cloud[i * 3];
    const n = acc.cloud[i * 3 + 1];
    const altFt = acc.cloud[i * 3 + 2];
    posF32[i * 3] = e;
    posF32[i * 3 + 1] = n;
    posF32[i * 3 + 2] = altFt * FT_TO_KM * exag; // display Z baked here, NOT in the layer
    const idx = altColorLUT(altFt);
    colU8[i * 3] = ALT_LUT[idx];
    colU8[i * 3 + 1] = ALT_LUT[idx + 1];
    colU8[i * 3 + 2] = ALT_LUT[idx + 2];
  }
  return {
    length: N,
    attributes: {
      getPosition: { value: posF32, size: 3 },
      getColor: { value: colU8, size: 3, normalized: true },
    },
  };
}

// ----------------------------------------------------------------------
// Coverage hull mesh (SimpleMeshLayer, single indexed shell) from the envelope.
// ----------------------------------------------------------------------
function vIdx(level, col) {
  return level * HULL_RING_STRIDE + col;
}

function buildHullMesh(acc, exag, hullIndicesRef) {
  const nLev = ENV_ALT_LEVELS; // 21
  const DEG = Math.PI / 180;
  const nVerts = nLev * HULL_RING_STRIDE;
  const positions = new Float32Array(nVerts * 3);
  const colors = new Uint8Array(nVerts * 4);
  let hasAny = false;
  for (let L = 0; L < nLev; L++) {
    const altFt = L * ENV_ALT_STEP;
    const z = altFt * FT_TO_KM * exag;
    const col = altColor(altFt);
    for (let c = 0; c < HULL_RING_STRIDE; c++) {
      const bc = c % ENV_BINS;
      const r = acc.envelope[L * ENV_BINS + bc] || 0;
      if (r > 0) hasAny = true;
      const th = c * ENV_BIN_DEG * DEG; // CW from north
      const east = r * Math.sin(th);
      const north = r * Math.cos(th);
      const k = vIdx(L, c) * 3;
      positions[k] = east;
      positions[k + 1] = north;
      positions[k + 2] = z;
      const ck = vIdx(L, c) * 4;
      colors[ck] = col[0];
      colors[ck + 1] = col[1];
      colors[ck + 2] = col[2];
      colors[ck + 3] = 255;
    }
  }
  if (!hasAny) return null;
  if (!hullIndicesRef.current) {
    const idx = [];
    for (let L = 0; L < nLev - 1; L++) {
      for (let c = 0; c < ENV_BINS; c++) {
        const a = vIdx(L, c);
        const b = vIdx(L, c + 1);
        const cc = vIdx(L + 1, c + 1);
        const d = vIdx(L + 1, c);
        idx.push(a, b, cc, a, cc, d);
      }
    }
    hullIndicesRef.current = new Uint32Array(idx);
  }
  return {
    attributes: {
      positions: { value: positions, size: 3 },
      COLOR_0: { value: colors, size: 4, normalized: true },
    },
    indices: hullIndicesRef.current,
  };
}

// Reduce the envelope to a profile: per alt-level, the max range across bearings.
function reduceProfile(acc) {
  const pts = [];
  for (let L = 0; L < ENV_ALT_LEVELS; L++) {
    let m = 0;
    for (let b = 0; b < ENV_BINS; b++) {
      const v = acc.envelope[L * ENV_BINS + b];
      if (v > m) m = v;
    }
    pts.push({ alt: L * ENV_ALT_STEP, range: m });
  }
  return pts;
}

// Linear-interpolate the coverage envelope's max range (km) at an arbitrary altitude (ft).
function envelopeRangeAt(profileData, altFt) {
  const data = profileData;
  if (!data || data.length === 0) return 0;
  if (altFt <= data[0].alt) return data[0].range;
  const last = data[data.length - 1];
  if (altFt >= last.alt) return last.range;
  for (let i = 1; i < data.length; i++) {
    if (altFt <= data[i].alt) {
      const a0 = data[i - 1];
      const a1 = data[i];
      const f = (altFt - a0.alt) / ((a1.alt - a0.alt) || 1);
      return a0.range + (a1.range - a0.range) * f;
    }
  }
  return last.range;
}

// ----------------------------------------------------------------------
// CoverageScope — the whole deck.gl visualization as a Preact component.
// deck.gl is read only as window.deck (CDN UMD global); never imported.
// ----------------------------------------------------------------------
export default function CoverageScope({ items, trails, receiverSite, nowMs }) {
  const containerRef = useRef(null);
  const deckRef = useRef(null);
  const accRef = useRef(null);
  const hullIndicesRef = useRef(null);
  const sliceCanvasRef = useRef(null);
  const profileSvgRef = useRef(null);
  const profileScaleRef = useRef(null);

  // Controls (Preact state).
  const [exag, setExag] = useState(8);
  const [pointSize, setPointSize] = useState(2);
  const [planeStyle, setPlaneStyle] = useState("model"); // 'model' | 'icon'
  const [modelScale, setModelScale] = useState(0.3);
  const [visible, setVisible] = useState({
    cloud: true,
    planes: true,
    trails: true,
    rings: true,
    grid: true,
    basemap: true,
    hull: false,
  });
  const [sliceOpen, setSliceOpen] = useState(false);
  const [profileOpen, setProfileOpen] = useState(false);
  const [sliceAlt, setSliceAlt] = useState(18000);
  const [sliceCount, setSliceCount] = useState(0); // slice band point count (driven by the redraw)

  // Collapsible overlays so the panels don't bury the map.
  const [collapsed, setCollapsed] = useState({ controls: false, legend: false });
  const togglePanel = (k) => setCollapsed((c) => ({ ...c, [k]: !c[k] }));

  // dataTick forces deck rebuild + panel redraws when refs alone change.
  const [dataTick, setDataTick] = useState(0);

  // ------------------------------------------------------------------
  // Origin resolution + accumulation pass. Re-anchors on first origin,
  // when a fixed receiverSite arrives, or when a floating centroid drifts.
  // ------------------------------------------------------------------
  useEffect(() => {
    const fixedSite = receiverSite && numeric(receiverSite.lat) && numeric(receiverSite.lon)
      ? { lat: receiverSite.lat, lon: receiverSite.lon }
      : null;

    let origin = fixedSite;
    let fixed = Boolean(fixedSite);
    if (!origin) {
      origin = centroidOf(items);
      fixed = false;
    }
    if (!origin) return; // nothing positioned yet and no fixed site

    let acc = accRef.current;
    let reanchored = false;

    if (!acc) {
      acc = makeAccumulator(origin, fixed);
      accRef.current = acc;
      reanchored = true;
    } else if (fixed && !acc.fixed) {
      // A real receiver_site arrived: switch to the fixed anchor (clear accumulation).
      acc = makeAccumulator(origin, true);
      accRef.current = acc;
      reanchored = true;
    } else if (!fixed && !acc.fixed) {
      // Floating centroid: re-anchor if it drifts beyond the threshold.
      const drift = haversineKm(acc.origin.lat, acc.origin.lon, origin.lat, origin.lon);
      if (drift > REANCHOR_DRIFT_KM) {
        acc = makeAccumulator(origin, false);
        accRef.current = acc;
        reanchored = true;
      }
    }

    const positioned = items.filter(hasPositionItem);
    const changed = accumulate(acc, positioned, nowMs);
    if (changed || reanchored) setDataTick((tick) => tick + 1);
  }, [items, receiverSite, nowMs]);

  // ------------------------------------------------------------------
  // Live planes derived from props.items (positioned only), projected
  // through the current accumulator's ENU. Trails come from props.trails.
  // dataTick is a dep so liveData re-projects after a re-anchor.
  // ------------------------------------------------------------------
  const liveData = useMemo(() => {
    const acc = accRef.current;
    if (!acc) return [];
    const out = [];
    for (const item of items) {
      if (!hasPositionItem(item)) continue;
      const enuPt = acc.enu(item.lon, item.lat);
      const east = enuPt[0];
      const north = enuPt[1];
      const altFt = numeric(item.altitude_baro_ft)
        ? item.altitude_baro_ft
        : numeric(item.altitude_geometric_ft)
          ? item.altitude_geometric_ft
          : 0;
      const track = numeric(item.track_deg) ? item.track_deg : numeric(item.heading_deg) ? item.heading_deg : 0;
      // Rolled-up display items key on bare ICAO, but trails are stored per
      // observation (receiverId:icao in aggregate mode). Merge across source_keys
      // (mirrors main.jsx mergedTrail), falling back to item.key for collector mode.
      const trailRaw = trails && trails.get
        ? (item.source_keys ?? [item.key])
            .flatMap((k) => trails.get(k) ?? [])
            .sort((left, right) => Number(left.time ?? 0) - Number(right.time ?? 0))
        : null;
      const trail = Array.isArray(trailRaw)
        ? trailRaw
            .filter((t) => numeric(t.lat) && numeric(t.lon))
            .map((t) => {
              const tp = acc.enu(t.lon, t.lat);
              // Trail points carry no per-vertex altitude; draw at current altitude.
              return [tp[0], tp[1], altFt];
            })
        : [];
      out.push({
        hex: item.icao,
        flight: item.callsign || null,
        east_km: east,
        north_km: north,
        alt_ft: altFt,
        track: track,
        gs: numeric(item.ground_speed_kt) ? item.ground_speed_kt : numeric(item.airspeed_kt) ? item.airspeed_kt : 0,
        dist_km: numeric(item.distance_km) ? item.distance_km : Math.hypot(east, north),
        category: typeof item.category === "string" ? item.category : null,
        vrate: numeric(item.vertical_rate_fpm) ? item.vertical_rate_fpm : null,
        trail: trail,
      });
    }
    return out;
  }, [items, trails, dataTick]);

  // ------------------------------------------------------------------
  // Mount the single deck.DeckGL instance. Cleanup finalizes the GL context.
  // ------------------------------------------------------------------
  useEffect(() => {
    const deck = window.deck;
    if (!deck || !deck.DeckGL) return undefined;

    const orbitView = new deck.OrbitView({
      id: "orbit",
      orbitAxis: "Z",
      near: 0.1,
      far: 100000,
      fovy: 50,
    });

    const initialViewState = {
      target: [0, 40, 14],
      rotationX: 30,
      rotationOrbit: -25,
      zoom: 1,
      minRotationX: 2,
      maxRotationX: 89,
    };

    const getTooltip = function (info) {
      const o = info && info.object;
      if (!o || !o.hex) return null;
      const gs = typeof o.gs === "number" ? Math.round(o.gs) : "—";
      const dist = typeof o.dist_km === "number" ? o.dist_km.toFixed(1) : "—";
      return {
        html:
          "<b>" + (o.flight || o.hex) + "</b><br/>" +
          o.alt_ft + " ft &middot; " + gs + " kt<br/>" +
          dist + " km",
      };
    };

    let raf = 0;
    let cancelled = false;

    // Mount via the scripting wrapper's `container:` option: deck creates its
    // OWN canvas inside this element and wires the OrbitView controller's
    // drag/zoom handling to it. `parent:` is ignored by the scripting DeckGL
    // (it escapes to a window-sized canvas on <html>); BYO `canvas:` stays in
    // place but does NOT get the controller interaction (drag won't orbit).
    // `container:` gives both: contained in the panel AND fully interactive.
    // Defer until the host has a resolved, non-zero size so deck's initial
    // canvas dimensions match the panel.
    const mount = () => {
      if (cancelled) return;
      const el = containerRef.current;
      if (!el || el.clientWidth === 0 || el.clientHeight === 0) {
        raf = requestAnimationFrame(mount);
        return;
      }
      deckRef.current = new deck.DeckGL({
        container: el,
        views: [orbitView],
        initialViewState: initialViewState,
        controller: true,
        getTooltip: getTooltip,
        layers: [],
      });
    };
    mount();

    return () => {
      cancelled = true;
      if (raf) cancelAnimationFrame(raf);
      if (deckRef.current) {
        deckRef.current.finalize(); // release the GL context on unmount (no leak)
        deckRef.current = null;
      }
    };
  }, []);

  // ------------------------------------------------------------------
  // Layer factory. Rebuilt on every layer-affecting change; never recreates
  // the Deck — only setProps({ layers }).
  // ------------------------------------------------------------------
  useEffect(() => {
    const deck = window.deck;
    const instance = deckRef.current;
    const acc = accRef.current;
    if (!deck || !instance || !acc) return;

    const CARTESIAN = deck.COORDINATE_SYSTEM.CARTESIAN;
    const topZ = 14 * exag;
    const layers = [];

    // Basemap mosaic (flat slippy z8 tiles -> ENU bounds). Bottom of the stack.
    if (visible.basemap) {
      for (const t of acc.tiles) {
        layers.push(new deck.BitmapLayer({
          id: t.id,
          image: t.url,
          bounds: t.bounds,
          coordinateSystem: CARTESIAN,
          opacity: 1,
          pickable: false,
          parameters: { depthTest: true },
        }));
      }
    }

    // Ground grid — faint LineLayer + brighter axes.
    layers.push(new deck.LineLayer({
      id: "grid",
      data: gridLines,
      coordinateSystem: CARTESIAN,
      getSourcePosition: (d) => d.source,
      getTargetPosition: (d) => d.target,
      getColor: [120, 140, 170, 28],
      getWidth: 1,
      widthUnits: "pixels",
      widthMinPixels: 1,
      visible: visible.grid,
      parameters: { depthTest: true },
    }));
    layers.push(new deck.LineLayer({
      id: "grid-axes",
      data: axisLines,
      coordinateSystem: CARTESIAN,
      getSourcePosition: (d) => d.source,
      getTargetPosition: (d) => d.target,
      getColor: [130, 155, 195, 70],
      getWidth: 1.2,
      widthUnits: "pixels",
      widthMinPixels: 1,
      visible: visible.grid,
      parameters: { depthTest: true },
    }));

    // Range rings — PathLayer on the ground plane.
    layers.push(new deck.PathLayer({
      id: "rings",
      data: rings,
      coordinateSystem: CARTESIAN,
      getPath: (r) => r.points,
      widthUnits: "pixels",
      getWidth: (r) => (r.radius === 100 ? 2 : 1),
      getColor: [120, 170, 255, 70],
      widthMinPixels: 1,
      billboard: false,
      visible: visible.rings,
      parameters: { depthTest: true },
    }));

    // Ring labels + cardinals — billboard TextLayer.
    layers.push(new deck.TextLayer({
      id: "labels",
      data: allTextLabels,
      coordinateSystem: CARTESIAN,
      getPosition: (d) => d.position,
      getText: (d) => d.text,
      billboard: true,
      getSize: 12,
      sizeUnits: "pixels",
      getColor: [180, 190, 220, 210],
      getTextAnchor: "middle",
      getAlignmentBaseline: "center",
      characterSet: "0123456789km NESW".split(""),
      visible: visible.rings,
      parameters: { depthTest: true },
    }));

    // Coverage hull — SimpleMeshLayer (single indexed shell), BEFORE the cloud.
    if (visible.hull) {
      const hullMesh = buildHullMesh(acc, exag, hullIndicesRef);
      if (hullMesh) {
        layers.push(new deck.SimpleMeshLayer({
          id: "hull",
          data: SINGLE_INSTANCE,
          mesh: hullMesh,
          coordinateSystem: CARTESIAN,
          getPosition: (d) => d.position,
          getColor: [255, 255, 255, 255],
          material: false,
          opacity: 0.22,
          wireframe: false,
          pickable: false,
          updateTriggers: { mesh: [exag, dataTick] },
          parameters: { depthTest: true, depthMask: false, cull: false },
        }));
      }
    }

    // Coverage cloud (THE CONE) — PointCloudLayer, binary typed-array form.
    const coverageBinary = buildCoverageBinary(acc, exag);
    layers.push(new deck.PointCloudLayer({
      id: "coverage",
      data: coverageBinary,
      coordinateSystem: CARTESIAN,
      material: false,
      pointSize: pointSize,
      sizeUnits: "pixels",
      pickable: false,
      visible: visible.cloud,
      parameters: { depthTest: true },
    }));

    // Station marker — vertical reference LineLayer + bright origin dot.
    layers.push(new deck.LineLayer({
      id: "station-line",
      data: [{ s: [0, 0, 0], t: [0, 0, topZ] }],
      coordinateSystem: CARTESIAN,
      getSourcePosition: (d) => d.s,
      getTargetPosition: (d) => d.t,
      getColor: [255, 255, 255, 130],
      getWidth: 2,
      widthUnits: "pixels",
      widthMinPixels: 1,
      updateTriggers: { getTargetPosition: exag },
      parameters: { depthTest: true },
    }));
    layers.push(new deck.ScatterplotLayer({
      id: "station-dot",
      data: [{ p: [0, 0, 0] }],
      coordinateSystem: CARTESIAN,
      getPosition: (d) => d.p,
      getRadius: 6,
      radiusUnits: "pixels",
      getFillColor: [255, 255, 255, 255],
      stroked: true,
      getLineColor: [0, 200, 255, 255],
      lineWidthUnits: "pixels",
      getLineWidth: 2,
      parameters: { depthTest: true },
    }));

    // Motion trails — PathLayer (drawn before planes).
    layers.push(new deck.PathLayer({
      id: "trails",
      data: liveData,
      coordinateSystem: CARTESIAN,
      getPath: (a) => a.trail.map((t) => [t[0], t[1], t[2] * FT_TO_KM * exag]),
      getColor: (a) => altColorA(a.alt_ft, 150),
      getWidth: 2,
      widthUnits: "pixels",
      widthMinPixels: 1.5,
      jointRounded: true,
      capRounded: true,
      visible: visible.trails,
      updateTriggers: { getPath: [exag, dataTick], getColor: dataTick },
      parameters: { depthTest: true },
    }));

    // Altitude drop-lines — LineLayer (depth cue, just under planes).
    layers.push(new deck.LineLayer({
      id: "droplines",
      data: liveData,
      coordinateSystem: CARTESIAN,
      getSourcePosition: (a) => [a.east_km, a.north_km, a.alt_ft * FT_TO_KM * exag],
      getTargetPosition: (a) => [a.east_km, a.north_km, 0],
      getColor: (a) => altColorA(a.alt_ft, 90),
      getWidth: 1.5,
      widthUnits: "pixels",
      widthMinPixels: 1,
      visible: visible.planes,
      updateTriggers: { getSourcePosition: [exag, dataTick], getColor: dataTick },
      parameters: { depthTest: true },
    }));

    // Live planes — 3D models (default) or flat icon.
    if (visible.planes) {
      if (planeStyle === "icon") {
        layers.push(new deck.IconLayer({
          id: "planes",
          data: liveData,
          coordinateSystem: CARTESIAN,
          getIcon: () => PLANE_ICON,
          getPosition: (a) => [a.east_km, a.north_km, a.alt_ft * FT_TO_KM * exag],
          getAngle: (a) => -a.track,
          getColor: (a) => altColorA(a.alt_ft, 255),
          getSize: 28,
          sizeUnits: "pixels",
          sizeMinPixels: 14,
          billboard: true,
          pickable: true,
          visible: visible.planes,
          updateTriggers: { getPosition: [exag, dataTick], getColor: dataTick, getAngle: dataTick },
          parameters: { depthTest: true },
        }));
      } else {
        const air = liveData.filter((a) => a.category !== "A7");
        const heli = liveData.filter((a) => a.category === "A7");
        const getPos = (a) => [a.east_km, a.north_km, a.alt_ft * FT_TO_KM * exag];
        const common = {
          coordinateSystem: CARTESIAN,
          getPosition: getPos,
          getOrientation: getOrientation,
          getColor: (a) => altColorA(a.alt_ft, 255),
          sizeScale: modelScale,
          material: true,
          pickable: true,
          parameters: { depthTest: true },
          updateTriggers: { getPosition: [exag, dataTick], getColor: dataTick, getOrientation: dataTick },
        };
        layers.push(new deck.SimpleMeshLayer(Object.assign({ id: "planes-model-air", data: air, mesh: AIRPLANE_MESH }, common)));
        layers.push(new deck.SimpleMeshLayer(Object.assign({ id: "planes-model-heli", data: heli, mesh: HELICOPTER_MESH }, common)));
      }
    }

    instance.setProps({ layers: layers });
  }, [visible, exag, pointSize, planeStyle, modelScale, liveData, dataTick]);

  // ------------------------------------------------------------------
  // Profile data (range vs altitude) derived from the accumulator envelope.
  // ------------------------------------------------------------------
  const profileData = useMemo(() => {
    const acc = accRef.current;
    if (!acc) return [];
    return reduceProfile(acc);
  }, [dataTick]);

  // ------------------------------------------------------------------
  // Slice canvas redraw (top-down cross-section, station-centered, north-up).
  // ------------------------------------------------------------------
  useEffect(() => {
    if (!sliceOpen) return;
    const canvas = sliceCanvasRef.current;
    const acc = accRef.current;
    if (!canvas) return;
    const dpr = window.devicePixelRatio || 1;
    if (canvas.width !== SLICE_CANVAS * dpr || canvas.height !== SLICE_CANVAS * dpr) {
      canvas.width = SLICE_CANVAS * dpr;
      canvas.height = SLICE_CANVAS * dpr;
    }
    const ctx = canvas.getContext("2d");
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

    const A = sliceAlt;
    const cx = SLICE_CANVAS / 2;
    const cy = SLICE_CANVAS / 2;
    const margin = 10;
    const R = SLICE_CANVAS / 2 - margin;
    const S = R / SLICE_RANGE_KM;

    ctx.clearRect(0, 0, SLICE_CANVAS, SLICE_CANVAS);

    // Range rings + labels (faint).
    ctx.lineWidth = 1;
    ctx.font = "9px Menlo, monospace";
    for (let k = 0; k < SLICE_RINGS.length; k++) {
      const rr = SLICE_RINGS[k] * S;
      ctx.strokeStyle = "rgba(120,170,255,0.22)";
      ctx.beginPath();
      ctx.arc(cx, cy, rr, 0, Math.PI * 2);
      ctx.stroke();
      ctx.fillStyle = "rgba(154,166,189,0.55)";
      ctx.fillText(SLICE_RINGS[k] + " km", cx + 3, cy - rr + 10);
    }
    // Axes (faint cross) + North tick + "N".
    ctx.strokeStyle = "rgba(120,140,170,0.15)";
    ctx.beginPath();
    ctx.moveTo(cx - R, cy);
    ctx.lineTo(cx + R, cy);
    ctx.moveTo(cx, cy - R);
    ctx.lineTo(cx, cy + R);
    ctx.stroke();
    ctx.strokeStyle = "rgba(0,200,255,0.7)";
    ctx.beginPath();
    ctx.moveTo(cx, cy - R);
    ctx.lineTo(cx, cy - R + 8);
    ctx.stroke();
    ctx.fillStyle = "rgba(232,237,246,0.85)";
    ctx.fillText("N", cx + 3, cy - R + 9);

    let count = 0;
    if (acc && acc.cloudCount > 0) {
      const N = acc.cloudCount;
      // Very faint full-footprint hint UNDER the slice (cheap stride sample).
      const stride = Math.max(1, Math.floor(N / 4000));
      ctx.fillStyle = "rgba(120,140,170,0.07)";
      for (let i = 0; i < N; i += stride) {
        const e = acc.cloud[i * 3];
        const n = acc.cloud[i * 3 + 1];
        ctx.fillRect(cx + e * S, cy - n * S, 1, 1);
      }
      // Slice band [A-HALF, A+HALF]: color each dot via altColor(alt_ft).
      const lo = A - SLICE_HALF;
      const hi = A + SLICE_HALF;
      for (let i = 0; i < N; i++) {
        const altFt = acc.cloud[i * 3 + 2];
        if (altFt < lo || altFt > hi) continue;
        const e = acc.cloud[i * 3];
        const n = acc.cloud[i * 3 + 1];
        const px = cx + e * S;
        const py = cy - n * S;
        if (px < 0 || px > SLICE_CANVAS || py < 0 || py > SLICE_CANVAS) continue;
        ctx.fillStyle = rgbStr(altColor(altFt));
        ctx.fillRect(px - 0.75, py - 0.75, 1.5, 1.5);
        count++;
      }
    }
    setSliceCount(count);
  }, [sliceOpen, sliceAlt, dataTick]);

  // ------------------------------------------------------------------
  // Profile SVG redraw (range vs altitude area + line + linked slice marker).
  // ------------------------------------------------------------------
  useEffect(() => {
    if (!profileOpen) return;
    const svg = profileSvgRef.current;
    if (!svg) return;
    const P = { w: 300, h: 180, mL: 34, mR: 10, mT: 8, mB: 22 };
    const x0 = P.mL;
    const x1 = P.w - P.mR;
    const y0 = P.h - P.mB;
    const y1 = P.mT;
    const data = profileData || [];
    let maxRange = 0;
    let maxAlt = 0;
    for (const d of data) {
      if (d.range > maxRange) maxRange = d.range;
      if (d.alt > maxAlt) maxAlt = d.alt;
    }
    const niceCeil = (v, fb) => {
      if (!(v > 0)) return fb;
      const steps = [10, 20, 25, 50, 100, 150, 200, 250, 300, 350, 400, 500];
      for (const s of steps) if (v <= s) return s;
      return Math.ceil(v / 100) * 100;
    };
    const xMax = niceCeil(maxRange * 1.08, 50);
    const yMax = Math.max(50000, Math.ceil(maxAlt / 10000) * 10000);
    profileScaleRef.current = { y0: y0, y1: y1, yMax: yMax };
    const sx = (r) => x0 + (r / xMax) * (x1 - x0);
    const sy = (a) => y0 - (a / yMax) * (y0 - y1);
    let s = "";
    for (let a = 0; a <= yMax; a += 10000) {
      const y = sy(a);
      s += `<line class="cs-pf-grid" x1="${x0}" y1="${y.toFixed(1)}" x2="${x1}" y2="${y.toFixed(1)}"/>`;
      s += `<text class="cs-pf-tick" x="${x0 - 4}" y="${(y + 2.5).toFixed(1)}" text-anchor="end">${a / 1000}k</text>`;
    }
    const xStep = niceCeil(xMax / 5, 10);
    for (let r = 0; r <= xMax + 0.5; r += xStep) {
      const x = sx(r);
      s += `<line class="cs-pf-grid" x1="${x.toFixed(1)}" y1="${y1}" x2="${x.toFixed(1)}" y2="${y0}"/>`;
      s += `<text class="cs-pf-tick" x="${x.toFixed(1)}" y="${y0 + 9}" text-anchor="middle">${r}</text>`;
    }
    s += `<line class="cs-pf-axis" x1="${x0}" y1="${y1}" x2="${x0}" y2="${y0}"/>`;
    s += `<line class="cs-pf-axis" x1="${x0}" y1="${y0}" x2="${x1}" y2="${y0}"/>`;
    s += `<text class="cs-pf-axlabel" x="${(x0 + x1) / 2}" y="${P.h - 3}" text-anchor="middle">Range (km)</text>`;
    s += `<text class="cs-pf-axlabel" x="9" y="${((y0 + y1) / 2).toFixed(1)}" text-anchor="middle" transform="rotate(-90 9 ${((y0 + y1) / 2).toFixed(1)})">Alt (ft)</text>`;
    if (data.length) {
      const pp = data.map((d) => [sx(d.range), sy(d.alt)]);
      let area = `M ${sx(0).toFixed(1)} ${sy(data[0].alt).toFixed(1)} `;
      for (const [px, py] of pp) area += `L ${px.toFixed(1)} ${py.toFixed(1)} `;
      area += `L ${sx(0).toFixed(1)} ${sy(data[data.length - 1].alt).toFixed(1)} Z`;
      s += `<path class="cs-pf-area" d="${area}"/>`;
      s += `<path class="cs-pf-line" d="M ${pp.map((p) => p[0].toFixed(1) + " " + p[1].toFixed(1)).join(" L ")}"/>`;
      for (const [px, py] of pp) s += `<circle class="cs-pf-vertex" cx="${px.toFixed(1)}" cy="${py.toFixed(1)}" r="1.1"/>`;
    }
    const nowMax = maxRange;
    if (nowMax > 0 && nowMax <= xMax) {
      const x = sx(nowMax);
      s += `<line class="cs-pf-nowmark" x1="${x.toFixed(1)}" y1="${y1}" x2="${x.toFixed(1)}" y2="${y0}"/>`;
      s += `<text class="cs-pf-nowlbl" x="${(x - 3).toFixed(1)}" y="${y1 + 8}" text-anchor="end">${nowMax.toFixed(0)} km</text>`;
    }
    // Slice-altitude marker: only when BOTH panels are open. Sits ON the envelope.
    if (sliceOpen && profileOpen) {
      const aMark = Math.max(0, Math.min(yMax, sliceAlt));
      const rMark = envelopeRangeAt(profileData, aMark);
      const my = sy(aMark);
      const mx = sx(Math.min(rMark, xMax));
      s += `<line class="cs-pf-slice-guide" x1="${x0}" y1="${my.toFixed(1)}" x2="${mx.toFixed(1)}" y2="${my.toFixed(1)}"/>`;
      s += `<circle class="cs-pf-slice-dot" cx="${mx.toFixed(1)}" cy="${my.toFixed(1)}" r="2.4"/>`;
      const lblX = Math.max(x0 + 2, mx - 4);
      s += `<text class="cs-pf-slice-lbl" x="${lblX.toFixed(1)}" y="${(my - 4).toFixed(1)}" text-anchor="end">${Math.round(aMark).toLocaleString("en-US")} ft</text>`;
    }
    svg.innerHTML = s;
  }, [profileOpen, sliceOpen, sliceAlt, profileData, dataTick]);

  // Bidirectional link: vertical pointer drag on the profile drives slice altitude.
  function altFromProfileEvent(ev) {
    const scale = profileScaleRef.current;
    const svg = profileSvgRef.current;
    if (!scale || !svg) return null;
    const rect = svg.getBoundingClientRect();
    const yUser = ((ev.clientY - rect.top) / rect.height) * 180;
    let a = (scale.yMax * (scale.y0 - yUser)) / ((scale.y0 - scale.y1) || 1);
    a = Math.max(0, Math.min(SLICE_ALT_MAX, a));
    return Math.round(a / 250) * 250;
  }

  function handleProfilePointerDown(ev) {
    if (!(sliceOpen && profileOpen)) return;
    try {
      profileSvgRef.current.setPointerCapture(ev.pointerId);
    } catch (e) {
      // ignore environments without pointer capture
    }
    const a = altFromProfileEvent(ev);
    if (a !== null) setSliceAlt(a);
    ev.preventDefault();
  }
  function handleProfilePointerMove(ev) {
    if (!(sliceOpen && profileOpen)) return;
    if (ev.buttons !== 1) return;
    const a = altFromProfileEvent(ev);
    if (a !== null) setSliceAlt(a);
    ev.preventDefault();
  }

  // ------------------------------------------------------------------
  // Telemetry HUD figures (derived from the accumulator).
  // ------------------------------------------------------------------
  const acc = accRef.current;
  const liveCount = liveData.length;
  const cloudPoints = acc ? acc.cloudCount : 0;
  const uniqueAircraft = acc ? acc.icaoSeen.size : 0;
  const maxRangeKm = acc ? acc.maxRangeKm : 0;
  const maxAltFt = acc ? acc.maxAltFt : 0;
  const positionedCount = useMemo(() => items.filter(hasPositionItem).length, [items]);
  const anchorLabel = acc ? (acc.fixed ? "Station origin" : "Centroid anchor") : "Awaiting positions";

  const fmtN = (n) => (typeof n === "number" && isFinite(n) ? n.toLocaleString("en-US") : "—");

  function toggleLayer(key) {
    setVisible((current) => ({ ...current, [key]: !current[key] }));
  }

  // Legend ramp gradient + altitude ticks (single-source TURBO).
  const legendRamp =
    "linear-gradient(to top, " +
    TURBO.map((stop) => rgbStr(stop[1]) + " " + Math.round(stop[0] * 100) + "%").join(", ") +
    ")";
  const legendTicks = ["40k+ ft", "30k", "20k", "10k", "0 ft"];

  return (
    <section className="scope-panel coverage-scope">
      <div className="scope-head">
        <div>
          <span className="eyebrow">Scope</span>
          <div className="scope-title-row">
            <h2>Coverage</h2>
            <span className="cs-ephemeral-badge" title="Coverage, hull, slice, and profile accumulate in this browser session from the live feed. They are not yet persisted to or replayed from the submission store, so they reset on reload.">
              session data
            </span>
          </div>
          <p className="scope-summary">{positionedCount} positioned / {liveCount} live &middot; ephemeral, not yet wired to the submission store</p>
        </div>
      </div>
      <div className="cs-deck-wrap">
        <div ref={containerRef} className="cs-deck" aria-label="Live coverage visualization" />

        {/* Controls (top-right) */}
        <div className={`cs-overlay cs-controls${collapsed.controls ? " is-collapsed" : ""}`}>
          <button
            type="button"
            className="cs-panel-head"
            onClick={() => togglePanel("controls")}
            aria-expanded={!collapsed.controls}
          >
            <span className="cs-panel-title">Controls</span>
            <span className="cs-caret" aria-hidden="true">{collapsed.controls ? "▸" : "▾"}</span>
          </button>
          <div className="cs-panel-body">
          <div className="cs-group">
            <div className="cs-group-label">Display</div>
            <div className="cs-slider-row">
              <div className="cs-slider-head">
                <span className="cs-name">Vertical exaggeration</span>
                <span className="cs-val">{exag}×</span>
              </div>
              <input
                type="range"
                min="1"
                max="30"
                step="1"
                value={exag}
                onInput={(e) => setExag(parseInt(e.currentTarget.value, 10))}
              />
            </div>
            <div className="cs-slider-row">
              <div className="cs-slider-head">
                <span className="cs-name">Point size</span>
                <span className="cs-val">{pointSize} px</span>
              </div>
              <input
                type="range"
                min="1"
                max="6"
                step="0.5"
                value={pointSize}
                onInput={(e) => setPointSize(parseFloat(e.currentTarget.value))}
              />
            </div>
          </div>

          <div className="cs-group">
            <div className="cs-group-label">Layers</div>
            <div className="cs-toggle-grid">
              {LAYER_CHIPS.map(([key, labelText]) => (
                <div
                  className={`cs-toggle ${visible[key] ? "on" : ""}`}
                  key={key}
                  onClick={() => toggleLayer(key)}
                >
                  <span className="cs-box" />
                  <span className="cs-lbl">{labelText}</span>
                </div>
              ))}
              <div
                className={`cs-toggle ${profileOpen ? "on" : ""}`}
                onClick={() => setProfileOpen((open) => !open)}
              >
                <span className="cs-box" />
                <span className="cs-lbl">Profile</span>
              </div>
              <div
                className={`cs-toggle ${sliceOpen ? "on" : ""}`}
                onClick={() => setSliceOpen((open) => !open)}
              >
                <span className="cs-box" />
                <span className="cs-lbl">Slice</span>
              </div>
            </div>
          </div>

          <div className="cs-group">
            <div className="cs-group-label">Planes</div>
            <div className="cs-segmented">
              <button
                type="button"
                className={planeStyle === "model" ? "active" : ""}
                onClick={() => setPlaneStyle("model")}
              >
                3D model
              </button>
              <button
                type="button"
                className={planeStyle === "icon" ? "active" : ""}
                onClick={() => setPlaneStyle("icon")}
              >
                Flat icon
              </button>
            </div>
            <div className="cs-slider-row" style={{ marginTop: "10px", opacity: planeStyle === "icon" ? 0.4 : 1 }}>
              <div className="cs-slider-head">
                <span className="cs-name">Model size</span>
                <span className="cs-val">{modelScale.toFixed(2)}</span>
              </div>
              <input
                type="range"
                min="0.05"
                max="1"
                step="0.05"
                value={modelScale}
                disabled={planeStyle === "icon"}
                onInput={(e) => setModelScale(parseFloat(e.currentTarget.value))}
              />
            </div>
          </div>
          </div>
        </div>

        {/* Legend (bottom-right) */}
        <div className={`cs-overlay cs-legend${collapsed.legend ? " is-collapsed" : ""}`}>
          <button
            type="button"
            className="cs-panel-head cs-legend-head"
            onClick={() => togglePanel("legend")}
            aria-expanded={!collapsed.legend}
          >
            <span className="cs-panel-title">Altitude</span>
            <span className="cs-caret" aria-hidden="true">{collapsed.legend ? "▸" : "▾"}</span>
          </button>
          <div className="cs-legend-body">
            <div className="cs-ramp" style={{ background: legendRamp }} />
            <div className="cs-scale">
              {legendTicks.map((t) => (
                <div className="cs-tick" key={t}>{t}</div>
              ))}
            </div>
          </div>
        </div>

        {/* Bottom-center analysis dock: Slice (left) + Profile (right) */}
        <div className="cs-analysis-dock">
          <div className="cs-overlay cs-slice-panel" style={{ display: sliceOpen ? "block" : "none" }}>
            <div className="cs-slice-head">
              <span className="cs-group-label" style={{ margin: 0 }}>Slice</span>
              <span className="cs-slice-now">
                {Math.round(sliceAlt).toLocaleString("en-US")} ft · {sliceCount.toLocaleString("en-US")} pts
              </span>
            </div>
            <div className="cs-slice-body">
              <canvas ref={sliceCanvasRef} className="cs-slice-canvas" width={SLICE_CANVAS} height={SLICE_CANVAS} />
              <div className="cs-slice-slider-wrap">
                <input
                  type="range"
                  className="cs-vslider"
                  min="0"
                  max="45000"
                  step="250"
                  value={sliceAlt}
                  aria-label="Slice altitude"
                  onInput={(e) => setSliceAlt(parseInt(e.currentTarget.value, 10))}
                />
              </div>
            </div>
          </div>

          <div className="cs-overlay cs-profile" style={{ display: profileOpen ? "block" : "none" }}>
            <div className="cs-profile-head">
              <span className="cs-group-label" style={{ margin: 0 }}>Range × Altitude</span>
              <span className="cs-profile-now">
                {maxRangeKm > 0 ? `max ${maxRangeKm.toFixed(0)} km · ${(maxAltFt / 1000).toFixed(0)}k ft` : "—"}
              </span>
            </div>
            <svg
              ref={profileSvgRef}
              className="cs-profile-svg"
              viewBox="0 0 300 180"
              preserveAspectRatio="xMidYMid meet"
              role="img"
              aria-label="Coverage envelope: max range versus altitude"
              onPointerDown={handleProfilePointerDown}
              onPointerMove={handleProfilePointerMove}
            />
          </div>
        </div>
      </div>
      <div className="cs-footer">
        <span>{anchorLabel} · {fmtN(cloudPoints)} cloud points · {fmtN(uniqueAircraft)} unique</span>
        <span>{maxRangeKm > 0 ? `max ${maxRangeKm.toFixed(0)} km` : "building coverage…"}</span>
      </div>
    </section>
  );
}

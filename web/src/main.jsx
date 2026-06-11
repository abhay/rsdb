import { render } from "preact";
import { useCallback, useEffect, useMemo, useRef, useState } from "preact/hooks";
import maplibregl from "maplibre-gl";

const TRAIL_MAX_POINTS = 180;
const TRAIL_MAX_AGE_MS = 30 * 60 * 1000;
const TRAIL_MIN_INTERVAL_MS = 5000;
const TRAIL_MIN_MOVE_KM = 0.05;
const AUTO_RANGES_KM = [10, 25, 50, 100, 250];
const HOVER_RADIUS_PX = 16;
const EARTH_RADIUS_KM = 6371;
const FIELD_RECENT_MS = 30 * 1000;
const FIELD_STALE_MS = 2 * 60 * 1000;
const RECEIVER_COLORS = ["#70d673", "#7fdcff", "#f7cb6f", "#e88a74", "#a78bfa", "#4cc8a3", "#f78fb3"];
const MAP_STYLE_URL = "https://basemaps.cartocdn.com/gl/dark-matter-gl-style/style.json";
const MAP_READY_TIMEOUT_MS = 6000;
const MAP_RECENTER_MIN_INTERVAL_MS = 12 * 1000;
const MAP_RECENTER_MIN_DEADBAND_KM = 1.5;
const MAP_RECENTER_MAX_DEADBAND_KM = 12;
const MAP_RECENTER_DEADBAND_RANGE_FRACTION = 0.12;
const MAP_AUTO_RANGE_SHRINK_INTERVAL_MS = 45 * 1000;
const DEFAULT_CENTER = { lat: 0, lon: 0 };

const EMPTY_STATUS = {
  receiver_connected: false,
  receiver: null,
  receiver_site: null,
  decoded_frames_per_second: 0,
  aircraft_updates_per_second: 0,
  usb_bytes_per_second: 0,
  usb_chunks_per_second: 0,
  decoded_frames_per_megabyte: 0,
  aircraft_updates_per_frame: 0,
  last_frame_ms: null,
  stale_aircraft_removed: 0,
  dropped_usb_chunks: 0,
  dropped_usb_chunk_ratio: 0,
  websocket_clients: 0,
  last_error: null,
};

const FILTERS = [
  ["all", "All"],
  ["positioned", "Positioned"],
  ["moving", "Moving"],
];

const RANGE_OPTIONS = [
  ["auto", "Auto"],
  [10, "10"],
  [25, "25"],
  [50, "50"],
  [100, "100"],
];

const AIRCRAFT_TABLE_COLUMNS = [
  { key: "icao", label: "ICAO", sortKey: "icao", text: (item) => item.icao },
  { key: "callsign", label: "Callsign", sortKey: "callsign", text: (item) => fmt(item.callsign) },
  { key: "altitude", label: "Altitude", sortKey: "altitude_baro_ft", text: (item) => fmt(item.altitude_baro_ft, " ft") },
  { key: "speed", label: "Speed", sortKey: "ground_speed_kt", text: (item) => fmt(speedValue(item), " kt") },
  { key: "range", label: "Range", sortKey: "distance_km", text: (item) => fixed(item.distance_km, 1, " km") },
  { key: "sources", label: "RX", sortKey: "receiver_count", text: (item) => String(item.receiver_count ?? 1) },
  { key: "freshness", label: "Freshness", freshness: true },
];

function App() {
  const aircraftRef = useRef(new Map());
  const trailsRef = useRef(new Map());
  const clockRef = useRef({ serverNow: Date.now(), serverSeenAt: performance.now() });
  const [aircraftVersion, setAircraftVersion] = useState(0);
  const [status, setStatus] = useState(EMPTY_STATUS);
  const [statusReachable, setStatusReachable] = useState(true);
  const [socketState, setSocketState] = useState("Disconnected");
  const [sortKey, setSortKey] = useState("icao");
  const [sortDir, setSortDir] = useState("asc");
  const [filter, setFilter] = useState("all");
  const [search, setSearch] = useState("");
  const [selectedKey, setSelectedKey] = useState(null);
  const [hoverKey, setHoverKey] = useState(null);
  const [rangeKm, setRangeKm] = useState("auto");
  const [overlays, setOverlays] = useState({ radar: true, trails: true, labels: true });
  const [focusReceiverId, setFocusReceiverId] = useState(null);
  const [receiverOnly, setReceiverOnly] = useState(false);
  const [clockTick, setClockTick] = useState(0);
  const [bootstrapReady, setBootstrapReady] = useState(false);

  const nowMs = serverClock(clockRef);
  const aggregateMode = isAggregateStatus(status);
  const receiverSite = status.receiver_site ?? null;
  const receiverSites = useMemo(() => knownReceiverSites(status, receiverSite), [status, receiverSite]);

  const handleFeed = useCallback((message) => {
    setServerTime(clockRef, message.now_ms);
    applyFeedMessage(message, aircraftRef.current, trailsRef.current, clockRef);
    setAircraftVersion((version) => version + 1);
  }, []);

  useEffect(() => {
    let stopped = false;

    async function loadBootstrap() {
      try {
        const response = await fetch("/bootstrap.json", { cache: "no-store" });
        if (!response.ok) return;
        const bootstrap = await response.json();
        if (stopped) return;
        applyBootstrap(bootstrap, aircraftRef.current, trailsRef.current, clockRef);
        setAircraftVersion((version) => version + 1);
      } catch {
        // Older collector builds do not expose bootstrap; WebSocket state is enough.
      } finally {
        if (!stopped) setBootstrapReady(true);
      }
    }

    loadBootstrap();
    return () => {
      stopped = true;
    };
  }, []);

  useEffect(() => {
    if (!bootstrapReady) return undefined;
    return connectWebSocket(handleFeed, setSocketState);
  }, [bootstrapReady, handleFeed]);

  useEffect(() => {
    let stopped = false;

    async function poll() {
      try {
        const response = await fetch("/status.json", { cache: "no-store" });
        const nextStatus = await response.json();
        if (stopped) return;
        setServerTime(clockRef, nextStatus.now_ms);
        setStatus(nextStatus);
        setStatusReachable(true);
      } catch {
        if (!stopped) setStatusReachable(false);
      }
    }

    poll();
    const timer = window.setInterval(poll, 2000);
    return () => {
      stopped = true;
      window.clearInterval(timer);
    };
  }, []);

  useEffect(() => {
    const timer = window.setInterval(() => setClockTick((tick) => tick + 1), 1000);
    return () => window.clearInterval(timer);
  }, []);

  const aircraftItems = useMemo(() => [...aircraftRef.current.values()], [aircraftVersion]);
  const displayItems = useMemo(() => rollupAircraftItems(aircraftItems, focusReceiverId), [aircraftItems, focusReceiverId]);
  const scopedItems = useMemo(() => {
    if (!focusReceiverId || !receiverOnly) return displayItems;
    return displayItems.filter((item) => aircraftReceiverIds(item).has(focusReceiverId));
  }, [displayItems, focusReceiverId, receiverOnly]);
  const receiverHandleCollisions = useMemo(
    () => receiverHandleCollisionSet(aircraftItems, status.receivers ?? []),
    [aircraftItems, status.receivers],
  );
  const rows = useMemo(() => visibleRows(
    scopedItems,
    { filter, search, sortKey, sortDir, receiverHandleCollisions },
  ), [scopedItems, filter, search, sortKey, sortDir, receiverHandleCollisions]);

  const displayItemMap = useMemo(() => new Map(displayItems.map((item) => [item.key, item])), [displayItems]);
  useEffect(() => {
    if (selectedKey && !displayItemMap.has(selectedKey)) setSelectedKey(null);
    if (hoverKey && !displayItemMap.has(hoverKey)) setHoverKey(null);
  }, [displayItemMap, selectedKey, hoverKey]);
  useEffect(() => {
    if (!focusReceiverId) setReceiverOnly(false);
  }, [focusReceiverId]);

  const selectedItem = selectedKey ? displayItemMap.get(selectedKey) ?? null : null;
  const hoveredItem = hoverKey ? displayItemMap.get(hoverKey) ?? null : null;
  const selectedTrail = selectedItem ? mergedTrail(selectedItem, trailsRef.current) : [];
  const totalAircraft = displayItems.length;
  const displayedAircraftCount = rows.length === totalAircraft ? totalAircraft : `${rows.length}/${totalAircraft}`;

  return (
    <main className="shell">
      <Header
        status={status}
        statusReachable={statusReachable}
        aircraftCount={displayedAircraftCount}
        nowMs={nowMs}
        socketState={socketState}
      />
      <section className="ops-layout" aria-label="Live aircraft workspace">
        <aside className="left-rail">
          <Toolbar
            filter={filter}
            search={search}
            onFilterChange={setFilter}
            onSearchChange={setSearch}
          />
          <ReceiverSummaryPanel
            receivers={status.receivers ?? []}
            localReceiver={status.receiver ?? null}
            rawAircraftItems={aircraftItems}
            nowMs={nowMs}
            focusReceiverId={focusReceiverId}
            receiverOnly={receiverOnly}
            receiverHandleCollisions={receiverHandleCollisions}
            onFocusChange={setFocusReceiverId}
            onReceiverOnlyChange={setReceiverOnly}
          />
          <AircraftTable
            rows={rows}
            sortKey={sortKey}
            sortDir={sortDir}
            selectedKey={selectedKey}
            nowMs={nowMs}
            receiverHandleCollisions={receiverHandleCollisions}
            onSort={(key) => updateSort(key, sortKey, setSortKey, setSortDir)}
            onSelect={(key) => setSelectedKey(selectedKey === key ? null : key)}
          />
        </aside>
        <section className="center-panel" aria-label="Live aircraft map">
          <SpatialPanel
            rows={rows}
            trails={trailsRef.current}
            receiverSite={receiverSite}
            receiverSites={receiverSites}
            focusReceiverId={focusReceiverId}
            rangeKm={rangeKm}
            selectedKey={selectedKey}
            selectedItem={selectedItem}
            hoverKey={hoverKey}
            hoveredItem={hoveredItem}
            overlays={overlays}
            onOverlayChange={setOverlays}
            onRangeChange={setRangeKm}
            onHoverChange={setHoverKey}
            onSelect={setSelectedKey}
            receiverHandleCollisions={receiverHandleCollisions}
          />
        </section>
        <aside className={`right-rail ${selectedItem ? "has-selection" : "summary-open"}`}>
          {selectedItem ? (
          <DetailsPanel
            item={selectedItem}
            trail={selectedTrail}
            nowMs={nowMs}
            receiverHandleCollisions={receiverHandleCollisions}
            onClose={() => setSelectedKey(null)}
          />
          ) : (
            <AggregateSummaryPanel status={status} statusReachable={statusReachable} nowMs={nowMs} />
          )}
        </aside>
      </section>
      <footer className="footer">
        <span>{aggregateMode ? "Aggregate" : "Collector"}</span>
        <span id="last-error">{status.last_error ?? ""}</span>
      </footer>
      <span hidden>{clockTick}</span>
    </main>
  );
}

function Header({ status, statusReachable, aircraftCount, nowMs, socketState }) {
  const aggregateMode = isAggregateStatus(status);
  const title = "RSDB";
  const metrics = aggregateMode
    ? [
        ["RX", status.receiver_count ?? 0],
        ["AC", aircraftCount],
        ["Accepted", status.submissions_accepted ?? 0],
        ["Last", age(status.last_submission_ms, nowMs)],
      ]
    : [
        ["RX", statusReachable ? status.receiver_connected ? "Live" : "Retry" : "Offline"],
        ["AC", aircraftCount],
        ["Frames", `${Number(status.decoded_frames_per_second ?? 0).toFixed(1)}/s`],
        ["USB", megabytesPerSecond(status.usb_bytes_per_second)],
      ];
  const subhead = aggregateMode ? aggregateSubhead(status) : receiverSiteLabel(status.receiver_site);

  return (
    <header className="topbar">
      <div className="brand-block">
        <h1>{title}</h1>
        <p className="subhead">{subhead}</p>
      </div>
      <section className="status-grid" aria-label="Service status">
        {metrics.map(([labelText, value]) => (
          <div className="metric" key={labelText}>
            <span>{labelText}</span>
            <strong>{value}</strong>
          </div>
        ))}
      </section>
      <span className={`connection-state connection-${socketState.toLowerCase()}`}>{socketState}</span>
    </header>
  );
}

function collectorMetrics(status, statusReachable, aircraftCount, nowMs) {
  const receiverState = statusReachable
    ? status.receiver_connected ? "Connected" : "Retrying"
    : "Offline";

  return [
    ["Receiver", receiverState],
    ["Aircraft", aircraftCount],
    ["Frame Rate", `${Number(status.decoded_frames_per_second ?? 0).toFixed(1)}/s`],
    ["Decode Rate", `${Number(status.aircraft_updates_per_second ?? 0).toFixed(1)}/s`],
    ["USB Rate", megabytesPerSecond(status.usb_bytes_per_second)],
    ["USB Chunks", `${Number(status.usb_chunks_per_second ?? 0).toFixed(1)}/s`],
    ["Frame Yield", `${Number(status.decoded_frames_per_megabyte ?? 0).toFixed(1)}/MB`],
    ["Update Yield", `${Number(status.aircraft_updates_per_frame ?? 0).toFixed(3)}/frame`],
    ["Last Frame", age(status.last_frame_ms, nowMs)],
    ["Stale", status.stale_aircraft_removed ?? 0],
    ["Dropped USB", status.dropped_usb_chunks ?? 0],
    ["USB Drop", `${(Number(status.dropped_usb_chunk_ratio ?? 0) * 100).toFixed(1)}%`],
    ["Clients", status.websocket_clients ?? 0],
  ];
}

function aggregateMetrics(status, nowMs) {
  const persistence = status.persistence ?? {};

  return [
    ["Accepted", status.submissions_accepted ?? 0],
    ["Duplicate", status.submissions_duplicate ?? 0],
    ["Rejected", status.submissions_rejected ?? 0],
    ["Last Submit", age(status.last_submission_ms, nowMs)],
    ["Persistence", persistence.enabled ? persistence.last_error ? "Error" : "On" : "Memory"],
    ["Clients", status.websocket_clients ?? 0],
    ["Uptime", duration(status.uptime_ms ?? 0)],
  ];
}

function aggregateSubhead(status) {
  const receiverCount = status.receiver_count ?? 0;
  const aircraftCount = status.aircraft_count ?? 0;
  return `${receiverCount} receiver${receiverCount === 1 ? "" : "s"} - ${aircraftCount} aircraft`;
}

function AggregateSummaryPanel({ status, statusReachable, nowMs }) {
  return (
    <section className="aggregate-summary" aria-label="Aggregate status">
      <div className="receiver-summary-head">
        <span className="eyebrow">{isAggregateStatus(status) ? "Network" : "Receiver"}</span>
        <strong>{statusReachable ? "Online" : "Offline"}</strong>
      </div>
      <section className="status-grid aggregate-status-grid">
        {(isAggregateStatus(status) ? aggregateMetrics(status, nowMs) : collectorMetrics(status, statusReachable, status.aircraft_count ?? 0, nowMs))
          .map(([labelText, value]) => (
          <div className="metric" key={labelText}>
            <span>{labelText}</span>
            <strong>{value}</strong>
          </div>
        ))}
      </section>
    </section>
  );
}

function ReceiverSummaryPanel({
  receivers,
  localReceiver,
  rawAircraftItems,
  nowMs,
  focusReceiverId,
  receiverOnly,
  receiverHandleCollisions,
  onFocusChange,
  onReceiverOnlyChange,
}) {
  const receiverRows = receiverRailRows(receivers, localReceiver, rawAircraftItems, receiverHandleCollisions);

  return (
    <section className="receiver-summary" aria-label="Aggregate receivers">
      <div className="receiver-summary-head">
        <span className="eyebrow">Receivers</span>
        <strong>{receiverRows.length}</strong>
      </div>
      <div className="receiver-cards">
        <button
          type="button"
          className={`receiver-row ${focusReceiverId ? "" : "active"}`}
          onClick={() => onFocusChange(null)}
        >
          <span className="receiver-color receiver-color-all" aria-hidden="true" />
          <span className="receiver-row-main">
            <strong>All receivers</strong>
            <span>{rawAircraftItems.length} observations</span>
          </span>
        </button>
        {receiverRows.length === 0 ? (
          <div className="receiver-card empty-card">No receivers yet</div>
        ) : receiverRows.map((summary) => (
          <ReceiverCard
            summary={summary}
            active={summary.receiver.id === focusReceiverId}
            nowMs={nowMs}
            receiverHandleCollisions={receiverHandleCollisions}
            onFocusChange={onFocusChange}
            key={summary.receiver.id}
          />
        ))}
      </div>
      <label className="receiver-only">
        <input
          type="checkbox"
          checked={receiverOnly}
          disabled={!focusReceiverId}
          onChange={(event) => onReceiverOnlyChange(event.currentTarget.checked)}
        />
        <span>Show focused receiver only</span>
      </label>
    </section>
  );
}

function ReceiverCard({ summary, active, nowMs, receiverHandleCollisions, onFocusChange }) {
  const health = receiverHealth(summary, nowMs);

  return (
    <button
      type="button"
      className={`receiver-row receiver-row-${health.level} ${active ? "active" : ""}`}
      onClick={() => onFocusChange(summary.receiver.id)}
    >
      <span className="receiver-color" style={{ background: receiverColor({ receiver: summary.receiver }) }} aria-hidden="true" />
      <span className="receiver-row-main">
        <strong>{receiverDisplay(summary.receiver, receiverHandleCollisions)}</strong>
        <span>{summary.aircraft_count ?? 0} aircraft - {summary.messages_accepted ?? 0} messages</span>
      </span>
        <span className={`receiver-health receiver-health-${health.level}`} title={health.title}>
          <span aria-hidden="true" />
          {health.label}
        </span>
    </button>
  );
}

function Toolbar({ filter, search, onFilterChange, onSearchChange }) {
  return (
    <section className="toolbar" aria-label="Aircraft controls">
      <input
        id="search"
        type="search"
        autoComplete="off"
        placeholder="Search ICAO or callsign"
        value={search}
        onInput={(event) => onSearchChange(event.currentTarget.value)}
      />
      <div className="segments" role="group" aria-label="Aircraft filter">
        {FILTERS.map(([value, labelText]) => (
          <button
            type="button"
            className={segmentClass(filter === value)}
            key={value}
            onClick={() => onFilterChange(value)}
          >
            {labelText}
          </button>
        ))}
      </div>
    </section>
  );
}

function SpatialPanel({
  rows,
  trails,
  receiverSite,
  receiverSites,
  focusReceiverId,
  rangeKm,
  selectedKey,
  selectedItem,
  hoverKey,
  hoveredItem,
  overlays,
  receiverHandleCollisions,
  onOverlayChange,
  onRangeChange,
  onHoverChange,
  onSelect,
}) {
  const mapContainerRef = useRef(null);
  const canvasRef = useRef(null);
  const mapRef = useRef(null);
  const targetsRef = useRef([]);
  const [mapStatus, setMapStatus] = useState("loading");
  const [resizeVersion, setResizeVersion] = useState(0);
  const positioned = useMemo(() => rows.filter(hasPosition), [rows]);
  const focusSite = focusedReceiverSite(receiverSites, focusReceiverId) ?? receiverSite;
  const targetCenter = spatialCenter(rows, receiverSites, focusSite);
  const targetRange = effectiveRangeKm(rows, positioned, rangeKm, targetCenter, trails, receiverSites);
  const centerSourceKey = spatialCenterSourceKey(receiverSites, focusReceiverId);
  const viewport = useStableSpatialViewport(targetCenter, targetRange, centerSourceKey, rangeKm);
  const centerSite = viewport.center;
  const effectiveRange = viewport.range;
  const readoutItem = hoveredItem ?? selectedItem;

  useEffect(() => {
    if (!mapContainerRef.current) return undefined;

    let readyTimer = null;
    let resizeObserver = null;
    let ready = false;
    let map = null;

    try {
      const center = spatialCenter(rows, receiverSites, focusSite);
      map = new maplibregl.Map({
        container: mapContainerRef.current,
        style: MAP_STYLE_URL,
        center: [center.lon, center.lat],
        zoom: 8,
        attributionControl: false,
        dragRotate: false,
        pitchWithRotate: false,
      });
      mapRef.current = map;

      const markReady = () => {
        if (ready) return;
        ready = true;
        if (readyTimer !== null) window.clearTimeout(readyTimer);
        setMapStatus("ok");
        setResizeVersion((version) => version + 1);
      };

      readyTimer = window.setTimeout(() => {
        setMapStatus("failed");
        setResizeVersion((version) => version + 1);
      }, MAP_READY_TIMEOUT_MS);
      map.on("load", markReady);
      map.on("styledata", markReady);
      map.on("moveend", () => setResizeVersion((version) => version + 1));

      resizeObserver = new ResizeObserver(() => {
        map.resize();
        setResizeVersion((version) => version + 1);
      });
      resizeObserver.observe(mapContainerRef.current);
    } catch {
      setMapStatus("failed");
      setResizeVersion((version) => version + 1);
    }

    return () => {
      if (readyTimer !== null) window.clearTimeout(readyTimer);
      if (resizeObserver) resizeObserver.disconnect();
      if (map) map.remove();
      mapRef.current = null;
    };
  }, []);

  useEffect(() => {
    if (mapStatus !== "ok" || !mapRef.current) return;
    fitSpatialMap(mapRef.current, centerSite, effectiveRange);
  }, [mapStatus, centerSite.lat, centerSite.lon, effectiveRange]);

  useEffect(() => {
    const onResize = () => setResizeVersion((version) => version + 1);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  useEffect(() => {
    targetsRef.current = drawSpatialOverlay({
      canvas: canvasRef.current,
      rows,
      trails,
      receiverSites,
      focusSite,
      centerSite,
      effectiveRange,
      map: mapStatus === "ok" ? mapRef.current : null,
      selectedKey,
      hoverKey,
      overlays,
      receiverHandleCollisions,
    });
  }, [rows, trails, receiverSites, focusSite, centerSite.lat, centerSite.lon, effectiveRange, mapStatus, selectedKey, hoverKey, overlays, receiverHandleCollisions, resizeVersion]);

  function handlePointerMove(event) {
    const target = targetFromEvent(event, canvasRef.current, targetsRef.current);
    const nextHoverKey = target ? target.key : null;
    if (hoverKey !== nextHoverKey) onHoverChange(nextHoverKey);
  }

  function handleClick(event) {
    const target = targetFromEvent(event, canvasRef.current, targetsRef.current);
    if (target) onSelect(target.key);
  }

  function toggleOverlay(key) {
    onOverlayChange((current) => ({ ...current, [key]: !current[key] }));
  }

  return (
    <section className={`scope-panel spatial-panel map-${mapStatus}`}>
      <div className="scope-head">
        <div>
          <span className="eyebrow">Scope</span>
          <h2>Map</h2>
          <p className="scope-summary">{positioned.length} positioned / {rows.length} visible</p>
        </div>
        <div className="scope-controls">
          <div className="segments compact" role="group" aria-label="Overlays">
            {[
              ["radar", "Radar"],
              ["trails", "Trails"],
              ["labels", "Labels"],
            ].map(([value, labelText]) => (
              <button
                type="button"
                className={segmentClass(overlays[value])}
                key={value}
                onClick={() => toggleOverlay(value)}
              >
                {labelText}
              </button>
            ))}
          </div>
          <div className="segments compact" role="group" aria-label="Range scale">
            {RANGE_OPTIONS.map(([value, labelText]) => (
              <button
                type="button"
                className={segmentClass(rangeKm === value)}
                key={String(value)}
                onClick={() => onRangeChange(value)}
              >
                {labelText}
              </button>
            ))}
          </div>
        </div>
      </div>
      <div className="scope-canvas-wrap">
        <div ref={mapContainerRef} className="map-base" aria-hidden="true" />
        {mapStatus === "failed" && <div className="map-fallback" aria-hidden="true" />}
        <div className="map-scrim" aria-hidden="true" />
        <canvas
          ref={canvasRef}
          id="scope-canvas"
          className="radar-canvas"
          aria-label="Live aircraft map"
          onPointerMove={handlePointerMove}
          onPointerLeave={() => onHoverChange(null)}
          onClick={handleClick}
        />
        {mapStatus === "failed" && (
          <span className="map-badge">Map tiles unavailable, radar overlay active</span>
        )}
      </div>
      <div className="scope-footer">
        <span>{scopeReadout(readoutItem, positioned.length, rows.length, receiverSite, receiverHandleCollisions)}</span>
        <span>{rangeKm === "auto" ? "Auto" : "Fixed"} range {effectiveRange} km</span>
      </div>
    </section>
  );
}

function DetailsPanel({ item, trail, nowMs, receiverHandleCollisions, onClose }) {
  const groups = detailsGroups(item, trail, receiverHandleCollisions);
  const coverage = dataCoverage(item, nowMs);
  const decode = decodeState(item);

  return (
    <aside className="details">
      <div className="details-head">
        <div className="details-title">
          <span className="eyebrow">Aircraft</span>
          <h2>{item.icao}{item.callsign ? ` - ${item.callsign}` : ""}</h2>
          <div className="details-badges">
            <span className={`decode-badge decode-badge-${decode.level}`} title={decode.title}>
              {decode.label}
            </span>
            <FreshnessBadge item={item} nowMs={nowMs} />
          </div>
        </div>
        <button type="button" className="close" aria-label="Close details" onClick={onClose}>x</button>
      </div>
      <div className="coverage-strip" aria-label="Aircraft data coverage">
        {coverage.map((entry) => (
          <span className={`coverage-pill coverage-${entry.level}`} title={entry.title} key={entry.key}>
            <span>{entry.label}</span>
            <strong>{entry.value}</strong>
          </span>
        ))}
      </div>
      <ObservationList item={item} nowMs={nowMs} receiverHandleCollisions={receiverHandleCollisions} />
      {groups.map((group) => (
        <section className="details-section" key={group.title}>
          <h3>{group.title}</h3>
          <dl className="details-grid">
            {group.fields.map((field) => {
              const value = field.ageMs === undefined ? field.value : age(field.ageMs, nowMs);
              const quality = fieldQuality(field, nowMs);
              return (
                <div className={`detail-field detail-field-${quality.level}`} title={quality.title} key={field.label}>
                  <dt>{field.label}</dt>
                  <dd>{value}</dd>
                </div>
              );
            })}
          </dl>
        </section>
      ))}
      <h3>Raw Frames</h3>
      <ol className="raw-list">
        {(item.raw_messages ?? []).slice().reverse().map((raw, index) => (
          <li key={`${raw}-${index}`}>{raw}</li>
        ))}
      </ol>
    </aside>
  );
}

function ObservationList({ item, nowMs, receiverHandleCollisions }) {
  const observations = item.observations ?? [item];

  return (
    <section className="details-section observation-section">
      <h3>Sources</h3>
      <div className="observation-list">
        {observations.map((observation) => {
          const state = freshness(observation, nowMs);
          return (
            <article className="observation-row" key={observation.key}>
              <div className="observation-row-top">
                <span className="receiver-color" style={{ background: receiverColor(observation) }} aria-hidden="true" />
                <strong>{receiverDisplay(observation.receiver, receiverHandleCollisions)}</strong>
                <span className={`observation-age observation-age-${state.level}`}>{state.age || state.label}</span>
              </div>
              <div className="observation-metrics">
                <span>{fixed(observation.distance_km, 1, " km")}</span>
                <span>{fixed(observation.bearing_deg, 0, " deg")}</span>
                <span>{fmt(observation.altitude_baro_ft, " ft")}</span>
                <span>{fmt(speedValue(observation), " kt")}</span>
              </div>
            </article>
          );
        })}
      </div>
    </section>
  );
}

function AircraftTable({
  rows,
  sortKey,
  sortDir,
  selectedKey,
  nowMs,
  receiverHandleCollisions,
  onSort,
  onSelect,
}) {
  const tableContext = { receiverHandleCollisions };

  return (
    <section className="table-wrap">
      <table>
        <thead>
          <tr>
            {AIRCRAFT_TABLE_COLUMNS.map((column) => {
              const active = sortKey === column.sortKey;
              const className = ["sort", active && "active", active && sortDir === "desc" && "desc"]
                .filter(Boolean)
                .join(" ");
              return (
                <th key={column.key}>
                  {column.sortKey ? (
                    <button type="button" className={className} onClick={() => onSort(column.sortKey)}>
                      {column.label}
                    </button>
                  ) : (
                    <span className="column-label">{column.label}</span>
                  )}
                </th>
              );
            })}
          </tr>
        </thead>
        <tbody>
          {rows.length === 0 ? (
            <tr>
              <td colSpan={AIRCRAFT_TABLE_COLUMNS.length} className="empty">No matching aircraft</td>
            </tr>
          ) : rows.map((item) => (
            <AircraftRow
              item={item}
              key={item.key}
              selected={item.key === selectedKey}
              nowMs={nowMs}
              tableContext={tableContext}
              onSelect={onSelect}
            />
          ))}
        </tbody>
      </table>
    </section>
  );
}

function AircraftRow({ item, selected, nowMs, tableContext, onSelect }) {
  return (
    <tr className={selected ? "selected" : ""} onClick={() => onSelect(item.key)}>
      {AIRCRAFT_TABLE_COLUMNS.map((column) => (
        <td key={column.key}>
          {column.freshness ? <FreshnessBadge item={item} nowMs={nowMs} /> : column.text(item, tableContext)}
        </td>
      ))}
    </tr>
  );
}

function FreshnessBadge({ item, nowMs }) {
  const state = freshness(item, nowMs);

  return (
    <span className={`freshness freshness-${state.level}`} title={state.title}>
      <span className="freshness-dot" aria-hidden="true" />
      <span>{state.label}</span>
      {state.age && <span className="freshness-age">{state.age}</span>}
    </span>
  );
}

function segmentClass(active) {
  return active ? "segment active" : "segment";
}

function updateSort(key, sortKey, setSortKey, setSortDir) {
  if (sortKey === key) {
    setSortDir((direction) => direction === "asc" ? "desc" : "asc");
  } else {
    setSortKey(key);
    setSortDir(key === "icao" || key === "callsign" ? "asc" : "desc");
  }
}

function detailsGroups(item, trail, receiverHandleCollisions) {
  return [
    {
      title: "Track",
      fields: [
        { label: "Primary Receiver", value: receiverDisplay(item.receiver, receiverHandleCollisions) },
        { label: "Sources", value: `${item.receiver_count ?? 1}` },
        { label: "Position", value: hasPosition(item) ? `${fixed(item.lat, 5)}, ${fixed(item.lon, 5)}` : "-", sourceMs: item.position_last_seen_ms },
        { label: "Position Status", value: label(item.position_status), sourceMs: item.position_last_seen_ms },
        { label: "Position Age", ageMs: item.position_last_seen_ms, sourceMs: item.position_last_seen_ms },
        { label: "Range", value: fixed(item.distance_km, 1, " km"), sourceMs: item.position_last_seen_ms },
        { label: "Bearing", value: fixed(item.bearing_deg, 1, " deg"), sourceMs: item.position_last_seen_ms },
        { label: "Trail", value: `${trail.length} pts`, sourceMs: item.position_last_seen_ms },
        { label: "Last Seen", ageMs: item.last_seen_ms, sourceMs: item.last_seen_ms },
      ],
    },
    {
      title: "Identity",
      fields: [
        { label: "Callsign", value: fmt(item.callsign), sourceMs: item.callsign_last_seen_ms },
        { label: "Callsign Age", ageMs: item.callsign_last_seen_ms, sourceMs: item.callsign_last_seen_ms },
        { label: "Category", value: fmt(item.category), sourceMs: item.callsign_last_seen_ms },
        { label: "ICAO", value: item.icao },
      ],
    },
    {
      title: "Altitude And Speed",
      fields: [
        { label: "Baro Altitude", value: fmt(item.altitude_baro_ft, " ft"), sourceMs: item.altitude_last_seen_ms },
        { label: "Geom Altitude", value: fmt(item.altitude_geometric_ft, " ft"), sourceMs: item.altitude_last_seen_ms },
        { label: "Altitude Age", ageMs: item.altitude_last_seen_ms, sourceMs: item.altitude_last_seen_ms },
        { label: "Ground Speed", value: fmt(item.ground_speed_kt, " kt"), sourceMs: item.velocity_last_seen_ms },
        { label: "Airspeed", value: fmt(item.airspeed_kt, " kt"), sourceMs: item.velocity_last_seen_ms },
        { label: "Speed Type", value: label(item.speed_type), sourceMs: item.velocity_last_seen_ms },
        { label: "Velocity Age", ageMs: item.velocity_last_seen_ms, sourceMs: item.velocity_last_seen_ms },
        { label: "Track", value: fixed(item.track_deg, 1, " deg"), sourceMs: item.velocity_last_seen_ms },
        { label: "Heading", value: fixed(item.heading_deg, 1, " deg"), sourceMs: item.velocity_last_seen_ms },
        { label: "Vertical", value: fmt(item.vertical_rate_fpm, " fpm"), sourceMs: item.velocity_last_seen_ms },
        { label: "Vertical Source", value: label(item.vertical_rate_source), sourceMs: item.velocity_last_seen_ms },
      ],
    },
    {
      title: "Integrity",
      fields: [
        { label: "Surveillance", value: fmt(item.surveillance_status), sourceMs: item.altitude_last_seen_ms },
        { label: "NIC B", value: boolLabel(item.nic_supplement_b), sourceMs: item.altitude_last_seen_ms },
        { label: "NACp", value: fmt(item.nac_p), sourceMs: item.operational_status_last_seen_ms },
        { label: "SIL", value: fmt(item.source_integrity_level), sourceMs: item.operational_status_last_seen_ms },
        { label: "ADS-B Version", value: fmt(item.adsb_version), sourceMs: item.operational_status_last_seen_ms },
        { label: "Operational Age", ageMs: item.operational_status_last_seen_ms, sourceMs: item.operational_status_last_seen_ms },
      ],
    },
    {
      title: "Status",
      fields: [
        { label: "Aircraft Status", value: fmt(item.aircraft_status_subtype), sourceMs: item.aircraft_status_last_seen_ms },
        { label: "Aircraft Status Age", ageMs: item.aircraft_status_last_seen_ms, sourceMs: item.aircraft_status_last_seen_ms },
        { label: "Emergency", value: label(item.emergency_state), sourceMs: item.aircraft_status_last_seen_ms },
        { label: "Emergency Code", value: fmt(item.emergency_state_code), sourceMs: item.aircraft_status_last_seen_ms },
        { label: "Mode A Identity", value: fmt(item.mode_a_identity_code), sourceMs: item.aircraft_status_last_seen_ms },
        { label: "Target Subtype", value: fmt(item.target_state_subtype), sourceMs: item.target_state_last_seen_ms },
        { label: "Target Age", ageMs: item.target_state_last_seen_ms, sourceMs: item.target_state_last_seen_ms },
        { label: "Last Type", value: fmt(item.last_type_code), sourceMs: item.last_seen_ms },
      ],
    },
  ];
}

function connectWebSocket(handleFeed, setSocketState) {
  let stopped = false;
  let reconnectTimer = null;
  let socket = null;

  function connect() {
    if (stopped) return;
    const scheme = location.protocol === "https:" ? "wss" : "ws";
    socket = new WebSocket(`${scheme}://${location.host}/ws`);

    setSocketState("Connecting");
    socket.addEventListener("open", () => setSocketState("Connected"));
    socket.addEventListener("message", (event) => handleFeed(JSON.parse(event.data)));
    socket.addEventListener("close", () => {
      setSocketState("Disconnected");
      if (!stopped) reconnectTimer = window.setTimeout(connect, 1500);
    });
    socket.addEventListener("error", () => setSocketState("Error"));
  }

  connect();

  return () => {
    stopped = true;
    if (reconnectTimer !== null) window.clearTimeout(reconnectTimer);
    if (socket) socket.close();
  };
}

function applyBootstrap(bootstrap, aircraft, trails, clockRef) {
  setServerTime(clockRef, bootstrap.now_ms);
  aircraft.clear();
  trails.clear();

  const recentMessages = [...(bootstrap.recent_messages ?? [])].sort((left, right) => (
    Number(left.now_ms ?? 0) - Number(right.now_ms ?? 0)
  ));
  for (const message of recentMessages) {
    applyFeedMessage(message, aircraft, trails, clockRef, { reconcileSnapshot: false });
  }

  const snapshot = bootstrap.snapshot ?? bootstrap;
  if (snapshot?.aircraft) {
    reconcileSnapshot(snapshot.aircraft, snapshot.receiver ?? null, snapshot.now_ms ?? bootstrap.now_ms, aircraft, trails, clockRef);
  }
}

function applyFeedMessage(message, aircraft, trails, clockRef, options = {}) {
  if (message.type === "snapshot") {
    if (options.reconcileSnapshot === false) return;
    reconcileSnapshot(message.aircraft, message.receiver ?? null, message.now_ms, aircraft, trails, clockRef);
  } else if (message.type === "aircraft") {
    upsertAircraft(normalizeAircraftItem(message.aircraft, message.receiver), message.now_ms, aircraft, trails, clockRef);
  } else if (message.type === "stale_aircraft") {
    const key = aircraftKey(message.icao, message.receiver);
    aircraft.delete(key);
    trails.delete(key);
  }
}

function reconcileSnapshot(items, receiver, nowMs, aircraft, trails, clockRef) {
  const nextKeys = new Set();

  for (const value of items ?? []) {
    const item = normalizeAircraftItem(value, receiver);
    nextKeys.add(item.key);
    upsertAircraft(item, nowMs, aircraft, trails, clockRef);
  }

  for (const key of aircraft.keys()) {
    if (!nextKeys.has(key)) {
      aircraft.delete(key);
      trails.delete(key);
    }
  }
}

function setServerTime(clockRef, now) {
  if (typeof now === "number") {
    clockRef.current.serverNow = now;
    clockRef.current.serverSeenAt = performance.now();
  }
}

function serverClock(clockRef) {
  return clockRef.current.serverNow + performance.now() - clockRef.current.serverSeenAt;
}

function numeric(value) {
  return typeof value === "number" && Number.isFinite(value);
}

function fmt(value, suffix = "") {
  return value === null || value === undefined ? "-" : `${value}${suffix}`;
}

function fixed(value, digits, suffix = "") {
  return numeric(value) ? `${Number(value).toFixed(digits)}${suffix}` : "-";
}

function label(value) {
  if (value === null || value === undefined) return "-";
  return String(value).replaceAll("_", " ");
}

function boolLabel(value) {
  if (value === null || value === undefined) return "-";
  return value ? "Yes" : "No";
}

function megabytesPerSecond(bytesPerSecond) {
  return `${(Number(bytesPerSecond ?? 0) / 1_000_000).toFixed(2)} MB/s`;
}

function receiverSiteLabel(site) {
  if (!site) return "1090 MHz ADS-B receiver";
  return `${Number(site.lat).toFixed(4)}, ${Number(site.lon).toFixed(4)}`;
}

function receiverLabel(receiverIdentity, site) {
  if (!receiverIdentity) return receiverSiteLabel(site);
  return receiverDisplay(receiverIdentity);
}

function receiverDisplay(receiverIdentity, receiverHandleCollisions = null) {
  if (!receiverIdentity) return "-";
  const handle = receiverIdentity.handle ?? null;
  if (handle?.base) {
    if (receiverHandleCollisions?.has(handle.base) && handle.suffix) {
      return `${handle.base}-${handle.suffix}`;
    }
    return handle.base;
  }
  return receiverIdentity.id ?? receiverIdentity.name ?? "-";
}

function receiverHandleCollisionSet(items, receiverSummaries) {
  const receivers = new Map();
  for (const item of items) {
    if (item.receiver?.id) receivers.set(item.receiver.id, item.receiver);
  }
  for (const summary of receiverSummaries) {
    if (summary.receiver?.id) receivers.set(summary.receiver.id, summary.receiver);
  }

  const baseToIds = new Map();
  for (const receiver of receivers.values()) {
    const base = receiver.handle?.base;
    if (!base) continue;
    if (!baseToIds.has(base)) baseToIds.set(base, new Set());
    baseToIds.get(base).add(receiver.id);
  }

  return new Set([...baseToIds.entries()]
    .filter(([, ids]) => ids.size > 1)
    .map(([base]) => base));
}

function age(ms, nowMs) {
  if (ms === null || ms === undefined) return "None";
  const seconds = Math.max(0, Math.round((nowMs - ms) / 1000));
  return `${seconds}s`;
}

function receiverHealth(summary, nowMs) {
  const submission = summary.submission ?? null;
  if (Number(submission?.targets_with_error ?? 0) > 0 || submission?.has_error) {
    return {
      level: "error",
      label: "Error",
      title: "Receiver reported a submission target error",
    };
  }
  if (Number(submission?.outbox_pending ?? 0) > 0) {
    return {
      level: "queued",
      label: "Queued",
      title: `${submission.outbox_pending} submissions waiting for delivery`,
    };
  }
  if (summary.receiver_connected === false) {
    return {
      level: "stale",
      label: "Retrying",
      title: "Receiver reports that the USB stream is not connected",
    };
  }
  if (summary.last_submission_ms === null || summary.last_submission_ms === undefined) {
    return {
      level: "unknown",
      label: "Waiting",
      title: "No submission has been accepted yet",
    };
  }

  const ageMs = Math.max(0, nowMs - summary.last_submission_ms);
  if (ageMs > 120_000) {
    return {
      level: "stale",
      label: "Stale",
      title: `Last submission ${age(summary.last_submission_ms, nowMs)} ago`,
    };
  }
  if (ageMs > 30_000) {
    return {
      level: "recent",
      label: "Idle",
      title: `Last submission ${age(summary.last_submission_ms, nowMs)} ago`,
    };
  }
  return {
    level: "live",
    label: "Live",
    title: `Last submission ${age(summary.last_submission_ms, nowMs)} ago`,
  };
}

function targetHealthLabel(submission) {
  const targetCount = Number(submission?.target_count ?? 0);
  const targetsWithError = Number(submission?.targets_with_error ?? 0);
  if (targetCount === 0) return "-";
  return targetsWithError === 0 ? `${targetCount} ok` : `${targetCount - targetsWithError}/${targetCount} ok`;
}

function duration(ms) {
  const seconds = Math.max(0, Math.round(Number(ms ?? 0) / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h`;
  return `${Math.floor(hours / 24)}d`;
}

function isAggregateStatus(status) {
  return Array.isArray(status.receivers) && typeof status.receiver_count === "number";
}

function hasPosition(item) {
  return numeric(item.lat) && numeric(item.lon);
}

function speedValue(item) {
  return numeric(item.ground_speed_kt) ? item.ground_speed_kt : item.airspeed_kt;
}

function hasVelocity(item) {
  return numeric(speedValue(item));
}

function displayName(item) {
  return item.callsign || item.icao;
}

function aircraftKey(icao, receiverIdentity) {
  return receiverIdentity?.id ? `${receiverIdentity.id}:${icao}` : icao;
}

function normalizeAircraftItem(value, receiverIdentity) {
  if (value?.aircraft && value?.receiver) {
    const { aircraft, receiver, ...metadata } = value;
    return normalizeAircraftItem({ ...aircraft, ...metadata }, receiver);
  }

  const receiver = receiverIdentity ?? value.receiver ?? null;
  const item = receiver ? { ...value, receiver } : { ...value };
  item.key = aircraftKey(item.icao, receiver);
  return item;
}

function rollupAircraftItems(items, focusReceiverId) {
  const groups = new Map();
  for (const item of items) {
    if (!item.icao) continue;
    if (!groups.has(item.icao)) groups.set(item.icao, []);
    groups.get(item.icao).push(item);
  }

  return [...groups.entries()].map(([icao, observations]) => {
    const sortedObservations = observations
      .slice()
      .sort((left, right) => Number(right.last_seen_ms ?? 0) - Number(left.last_seen_ms ?? 0));
    const primary = choosePrimaryObservation(sortedObservations, focusReceiverId);
    const merged = {
      ...primary,
      key: icao,
      icao,
      observations: sortedObservations,
      source_keys: sortedObservations.map((item) => item.key),
      receiver_count: aircraftReceiverIds({ observations: sortedObservations }).size,
      message_count: sortedObservations.reduce((total, item) => total + Number(item.message_count ?? 0), 0),
      raw_messages: sortedObservations.flatMap((item) => item.raw_messages ?? []).slice(-24),
    };

    mergeObservationFields(merged, sortedObservations);
    return merged;
  });
}

function choosePrimaryObservation(observations, focusReceiverId) {
  if (focusReceiverId) {
    const focused = observations.find((item) => item.receiver?.id === focusReceiverId);
    if (focused) return focused;
  }
  return observations.find(hasPosition) ?? observations[0];
}

function mergeObservationFields(merged, observations) {
  for (const group of OBSERVATION_FIELD_GROUPS) {
    const source = latestObservationForGroup(observations, group);
    if (!source) continue;
    for (const field of group.fields) merged[field] = source[field];
  }
}

function latestObservationForGroup(observations, group) {
  return observations
    .filter(group.present)
    .sort((left, right) => Number(right[group.timeField] ?? right.last_seen_ms ?? 0) - Number(left[group.timeField] ?? left.last_seen_ms ?? 0))[0]
    ?? null;
}

const OBSERVATION_FIELD_GROUPS = [
  {
    timeField: "callsign_last_seen_ms",
    fields: ["callsign", "category", "callsign_last_seen_ms"],
    present: (item) => item.callsign !== null && item.callsign !== undefined,
  },
  {
    timeField: "position_last_seen_ms",
    fields: ["lat", "lon", "position_status", "position_last_seen_ms", "distance_km", "bearing_deg"],
    present: hasPosition,
  },
  {
    timeField: "altitude_last_seen_ms",
    fields: [
      "altitude_baro_ft",
      "altitude_geometric_ft",
      "altitude_last_seen_ms",
      "surveillance_status",
      "nic_supplement_b",
    ],
    present: (item) => item.altitude_baro_ft !== null && item.altitude_baro_ft !== undefined
      || item.altitude_geometric_ft !== null && item.altitude_geometric_ft !== undefined,
  },
  {
    timeField: "velocity_last_seen_ms",
    fields: [
      "ground_speed_kt",
      "airspeed_kt",
      "speed_type",
      "track_deg",
      "heading_deg",
      "vertical_rate_fpm",
      "vertical_rate_source",
      "velocity_last_seen_ms",
    ],
    present: hasVelocity,
  },
  {
    timeField: "operational_status_last_seen_ms",
    fields: [
      "nac_p",
      "source_integrity_level",
      "adsb_version",
      "operational_status_last_seen_ms",
    ],
    present: (item) => item.operational_status_last_seen_ms !== null && item.operational_status_last_seen_ms !== undefined,
  },
  {
    timeField: "aircraft_status_last_seen_ms",
    fields: [
      "aircraft_status_subtype",
      "aircraft_status_last_seen_ms",
      "emergency_state",
      "emergency_state_code",
      "mode_a_identity_code",
    ],
    present: (item) => item.aircraft_status_last_seen_ms !== null && item.aircraft_status_last_seen_ms !== undefined,
  },
  {
    timeField: "target_state_last_seen_ms",
    fields: ["target_state_subtype", "target_state_last_seen_ms"],
    present: (item) => item.target_state_last_seen_ms !== null && item.target_state_last_seen_ms !== undefined,
  },
];

function aircraftReceiverIds(item) {
  const ids = new Set();
  for (const observation of item.observations ?? [item]) {
    if (observation.receiver?.id) ids.add(observation.receiver.id);
  }
  return ids;
}

function mergedTrail(item, trails) {
  const points = (item.source_keys ?? [item.key])
    .flatMap((key) => trails.get(key) ?? [])
    .sort((left, right) => Number(left.time ?? 0) - Number(right.time ?? 0));
  if (points.length <= TRAIL_MAX_POINTS) return points;
  return points.slice(points.length - TRAIL_MAX_POINTS);
}

function receiverRailRows(receivers, localReceiver, rawAircraftItems, receiverHandleCollisions) {
  const summaries = new Map();
  for (const summary of receivers) {
    if (summary.receiver?.id) summaries.set(summary.receiver.id, summary);
  }

  if (localReceiver?.id && !summaries.has(localReceiver.id)) {
    summaries.set(localReceiver.id, {
      receiver: localReceiver,
      aircraft_count: rawAircraftItems.filter((item) => item.receiver?.id === localReceiver.id).length,
      messages_accepted: rawAircraftItems.reduce((total, item) => (
        item.receiver?.id === localReceiver.id ? total + Number(item.message_count ?? 0) : total
      ), 0),
      last_message_ms: latestReceiverMessageMs(rawAircraftItems, localReceiver.id),
      last_submission_ms: null,
      receiver_connected: null,
      submission: null,
    });
  }

  for (const item of rawAircraftItems) {
    const receiver = item.receiver;
    if (!receiver?.id || summaries.has(receiver.id)) continue;
    summaries.set(receiver.id, {
      receiver,
      aircraft_count: rawAircraftItems.filter((candidate) => candidate.receiver?.id === receiver.id).length,
      messages_accepted: rawAircraftItems.reduce((total, candidate) => (
        candidate.receiver?.id === receiver.id ? total + Number(candidate.message_count ?? 0) : total
      ), 0),
      last_message_ms: latestReceiverMessageMs(rawAircraftItems, receiver.id),
      last_submission_ms: null,
      receiver_connected: null,
      submission: null,
    });
  }

  return [...summaries.values()].sort((left, right) => (
    receiverDisplay(left.receiver, receiverHandleCollisions)
      .localeCompare(receiverDisplay(right.receiver, receiverHandleCollisions))
  ));
}

function latestReceiverMessageMs(items, receiverId) {
  return items
    .filter((item) => item.receiver?.id === receiverId)
    .reduce((latest, item) => Math.max(latest, Number(item.last_seen_ms ?? 0)), 0)
    || null;
}

function knownReceiverSites(status, localSite) {
  const sites = new Map();
  for (const summary of status.receivers ?? []) {
    if (summary.receiver?.id && validSite(summary.receiver_site)) {
      sites.set(summary.receiver.id, {
        receiver: summary.receiver,
        site: summary.receiver_site,
      });
    }
  }

  if (status.receiver?.id && validSite(localSite) && !sites.has(status.receiver.id)) {
    sites.set(status.receiver.id, {
      receiver: status.receiver,
      site: localSite,
    });
  }

  return [...sites.values()];
}

function focusedReceiverSite(receiverSites, focusReceiverId) {
  if (!focusReceiverId) return null;
  return receiverSites.find((entry) => entry.receiver?.id === focusReceiverId)?.site ?? null;
}

function validSite(site) {
  return Boolean(site) && numeric(site.lat) && numeric(site.lon);
}

function receiverColor(item) {
  const receiverId = item.receiver?.id ?? "local";
  return RECEIVER_COLORS[stringHash(receiverId) % RECEIVER_COLORS.length];
}

function colorWithAlpha(hex, alpha) {
  const value = Number.parseInt(hex.slice(1), 16);
  const red = (value >> 16) & 255;
  const green = (value >> 8) & 255;
  const blue = value & 255;
  return `rgba(${red}, ${green}, ${blue}, ${alpha})`;
}

function stringHash(value) {
  let hash = 0;
  for (let index = 0; index < value.length; index += 1) {
    hash = ((hash << 5) - hash + value.charCodeAt(index)) >>> 0;
  }
  return hash;
}

function matchesFilter(item, filter, search, receiverHandleCollisions) {
  if (filter === "positioned" && !hasPosition(item)) return false;
  if (filter === "moving" && !hasVelocity(item)) return false;

  const term = search.trim().toUpperCase();
  if (!term) return true;

  return item.icao.includes(term)
    || (item.callsign ?? "").toUpperCase().includes(term)
    || receiverDisplay(item.receiver, receiverHandleCollisions).toUpperCase().includes(term);
}

function sortValue(item, key, receiverHandleCollisions) {
  if (key === "callsign") return item.callsign ?? "";
  if (key === "ground_speed_kt") return speedValue(item);
  if (key === "receiver") return receiverDisplay(item.receiver, receiverHandleCollisions);
  return item[key];
}

function compareRows(left, right, sortKey, sortDir, receiverHandleCollisions) {
  const leftValue = sortValue(left, sortKey, receiverHandleCollisions);
  const rightValue = sortValue(right, sortKey, receiverHandleCollisions);
  const direction = sortDir === "asc" ? 1 : -1;

  if (leftValue === null || leftValue === undefined || leftValue === "") return 1;
  if (rightValue === null || rightValue === undefined || rightValue === "") return -1;

  if (typeof leftValue === "number" && typeof rightValue === "number") {
    const result = (leftValue - rightValue) * direction;
    return result === 0 ? left.key.localeCompare(right.key) : result;
  }
  const result = String(leftValue).localeCompare(String(rightValue)) * direction;
  return result === 0 ? left.key.localeCompare(right.key) : result;
}

function freshness(item, nowMs) {
  if (item.lifecycle === "expired") {
    return {
      level: "expired",
      label: "Expired",
      age: item.message_age_ms === undefined ? "" : duration(item.message_age_ms),
      title: "Aircraft has not reported recently",
    };
  }

  if (item.last_seen_ms === null || item.last_seen_ms === undefined) {
    return {
      level: "unknown",
      label: "Unknown",
      age: "",
      title: "Last seen time unavailable",
    };
  }

  const ageMs = Math.max(0, nowMs - item.last_seen_ms);
  const seconds = Math.round(ageMs / 1000);
  const level = item.lifecycle === "stale" || item.position_status === "stale" || seconds >= 30
    ? "stale"
    : seconds >= 10 ? "recent" : "live";
  const labelText = level === "stale" ? "Stale" : level === "recent" ? "Recent" : "Live";
  const ageText = `${seconds}s`;

  return {
    level,
    label: labelText,
    age: ageText,
    title: `Last seen ${ageText} ago`,
  };
}

function decodeState(item) {
  const status = item.last_decode_status ?? "unknown";
  if (status === "updated") {
    return {
      level: "updated",
      label: "Updated",
      title: "Latest ADS-B payload updated aircraft state",
    };
  }
  if (status === "partial") {
    return {
      level: "partial",
      label: "Partial",
      title: "Latest ADS-B payload contributed partial state",
    };
  }
  if (status === "rejected") {
    return {
      level: "rejected",
      label: "Rejected",
      title: "Latest ADS-B payload was rejected by state validation",
    };
  }
  if (status === "unsupported") {
    return {
      level: "unsupported",
      label: "Unsupported",
      title: "Latest ADS-B payload type is not decoded yet",
    };
  }
  return {
    level: "unknown",
    label: "Unknown",
    title: "Decode status is unavailable",
  };
}

function dataCoverage(item, nowMs) {
  return [
    coverageEntry("callsign", "Callsign", Boolean(item.callsign), item.callsign_last_seen_ms, nowMs),
    coverageEntry("position", "Position", hasPosition(item), item.position_last_seen_ms, nowMs),
    coverageEntry("altitude", "Altitude", item.altitude_baro_ft !== null && item.altitude_baro_ft !== undefined, item.altitude_last_seen_ms, nowMs),
    coverageEntry("velocity", "Velocity", hasVelocity(item), item.velocity_last_seen_ms, nowMs),
    coverageEntry("status", "Status", item.aircraft_status_subtype !== null && item.aircraft_status_subtype !== undefined, item.aircraft_status_last_seen_ms, nowMs),
    coverageEntry("target", "Target", item.target_state_subtype !== null && item.target_state_subtype !== undefined, item.target_state_last_seen_ms, nowMs),
    coverageEntry("ops", "Ops", item.adsb_version !== null && item.adsb_version !== undefined, item.operational_status_last_seen_ms, nowMs),
  ];
}

function coverageEntry(key, labelText, available, sourceMs, nowMs) {
  if (!available) {
    return {
      key,
      label: labelText,
      level: "missing",
      value: "-",
      title: `${labelText} has not been observed`,
    };
  }

  const quality = sourceQuality(sourceMs, nowMs);
  return {
    key,
    label: labelText,
    level: quality.level,
    value: quality.age,
    title: quality.titleFor(labelText),
  };
}

function fieldQuality(field, nowMs) {
  if (field.value === "-") {
    return { level: "missing", title: "Value has not been observed" };
  }

  if (field.sourceMs === undefined) {
    return { level: "neutral", title: "" };
  }

  const quality = sourceQuality(field.sourceMs, nowMs);
  return {
    level: quality.level,
    title: quality.titleFor(field.label),
  };
}

function sourceQuality(sourceMs, nowMs) {
  if (sourceMs === null || sourceMs === undefined) {
    return {
      level: "missing",
      age: "-",
      titleFor: (labelText) => `${labelText} has not been observed`,
    };
  }

  const ageMs = Math.max(0, nowMs - sourceMs);
  const ageText = duration(ageMs);
  if (ageMs > FIELD_STALE_MS) {
    return {
      level: "stale",
      age: ageText,
      titleFor: (labelText) => `${labelText} was last updated ${ageText} ago`,
    };
  }
  if (ageMs > FIELD_RECENT_MS) {
    return {
      level: "recent",
      age: ageText,
      titleFor: (labelText) => `${labelText} was last updated ${ageText} ago`,
    };
  }
  return {
    level: "live",
    age: ageText,
    titleFor: (labelText) => `${labelText} was last updated ${ageText} ago`,
  };
}

function visibleRows(items, { filter, search, sortKey, sortDir, receiverHandleCollisions }) {
  return items
    .filter((item) => matchesFilter(item, filter, search, receiverHandleCollisions))
    .sort((left, right) => compareRows(left, right, sortKey, sortDir, receiverHandleCollisions));
}

function useStableSpatialViewport(targetCenter, targetRange, centerSourceKey, rangeSetting) {
  const [viewport, setViewport] = useState(() => ({
    center: targetCenter,
    range: targetRange,
    centerSourceKey,
    rangeSetting,
    centerUpdatedAt: performance.now(),
    rangeUpdatedAt: performance.now(),
  }));

  useEffect(() => {
    setViewport((current) => {
      const now = performance.now();
      const centerSourceChanged = current.centerSourceKey !== centerSourceKey;
      const rangeSettingChanged = current.rangeSetting !== rangeSetting;
      const hasCenter = validSite(current.center);
      const movedKm = hasCenter
        ? haversineDistanceKm(current.center.lat, current.center.lon, targetCenter.lat, targetCenter.lon)
        : Infinity;
      const centerElapsedMs = now - current.centerUpdatedAt;
      const shouldRecenter = centerSourceChanged
        || !hasCenter
        || (
          movedKm >= spatialCenterDeadbandKm(targetRange)
          && centerElapsedMs >= MAP_RECENTER_MIN_INTERVAL_MS
        );
      const rangeElapsedMs = now - current.rangeUpdatedAt;
      const nextRange = stableSpatialRange(
        current.range,
        targetRange,
        rangeSetting,
        centerSourceChanged || rangeSettingChanged,
        rangeElapsedMs,
      );
      const rangeChanged = nextRange !== current.range;

      if (!shouldRecenter && !rangeChanged && !rangeSettingChanged) return current;

      return {
        center: shouldRecenter ? targetCenter : current.center,
        range: nextRange,
        centerSourceKey,
        rangeSetting,
        centerUpdatedAt: shouldRecenter ? now : current.centerUpdatedAt,
        rangeUpdatedAt: rangeChanged ? now : current.rangeUpdatedAt,
      };
    });
  }, [targetCenter.lat, targetCenter.lon, targetRange, centerSourceKey, rangeSetting]);

  return viewport;
}

function spatialCenterSourceKey(receiverSites, focusReceiverId) {
  if (focusReceiverId) {
    const focusSite = receiverSites.find((entry) => entry.receiver?.id === focusReceiverId)?.site;
    return validSite(focusSite)
      ? `focus:${focusReceiverId}:${focusSite.lat},${focusSite.lon}`
      : `focus:${focusReceiverId}:missing`;
  }
  const receiverIds = receiverSites
    .filter((entry) => validSite(entry.site))
    .map((entry) => `${entry.receiver?.id ?? "unknown"}:${entry.site.lat},${entry.site.lon}`)
    .sort();
  return receiverIds.length > 0 ? `receivers:${receiverIds.join("|")}` : "aircraft";
}

function spatialCenterDeadbandKm(rangeKm) {
  return Math.min(
    MAP_RECENTER_MAX_DEADBAND_KM,
    Math.max(MAP_RECENTER_MIN_DEADBAND_KM, Number(rangeKm) * MAP_RECENTER_DEADBAND_RANGE_FRACTION),
  );
}

function stableSpatialRange(currentRange, targetRange, rangeSetting, force, elapsedMs) {
  if (rangeSetting !== "auto" || force || !numeric(currentRange)) return targetRange;
  if (targetRange > currentRange) return targetRange;
  if (targetRange < currentRange && elapsedMs >= MAP_AUTO_RANGE_SHRINK_INTERVAL_MS) return targetRange;
  return currentRange;
}

function drawSpatialOverlay({
  canvas,
  rows,
  trails,
  receiverSites,
  focusSite,
  centerSite,
  effectiveRange,
  map,
  selectedKey,
  hoverKey,
  overlays,
  receiverHandleCollisions,
}) {
  if (!canvas) return [];

  const context = canvas.getContext("2d");
  const rect = canvas.getBoundingClientRect();
  if (rect.width === 0 || rect.height === 0) return [];

  const scale = window.devicePixelRatio || 1;
  const width = Math.round(rect.width);
  const height = Math.round(rect.height);
  const canvasWidth = Math.round(width * scale);
  const canvasHeight = Math.round(height * scale);

  if (canvas.width !== canvasWidth || canvas.height !== canvasHeight) {
    canvas.width = canvasWidth;
    canvas.height = canvasHeight;
  }

  context.save();
  context.scale(scale, scale);
  context.clearRect(0, 0, width, height);

  const positioned = rows.filter(hasPosition);
  const projector = makeSpatialProjector(width, height, effectiveRange, positioned, centerSite, map);
  const targets = [];

  if (!map) {
    drawRadarBackground(context, projector, width, height, effectiveRange);
  } else if (overlays.radar) {
    drawRadarOverlay(context, projector, width, height, effectiveRange, centerSite);
  }

  if (overlays.trails) {
    for (const item of rows) drawTrail(context, projector, item, trails, selectedKey, hoverKey);
  }
  drawReceiverSites(context, projector, receiverSites, focusSite);
  for (const item of rows) {
    drawAircraftTarget(
      context,
      projector,
      item,
      positioned.length,
      selectedKey,
      hoverKey,
      targets,
      overlays.labels,
    );
  }
  drawReceiverLegend(context, rows, width, receiverHandleCollisions);

  context.restore();
  return targets;
}

function makeSpatialProjector(width, height, rangeKm, positioned, receiverSite, map) {
  const fallback = makeMapProjector(width, height, rangeKm, positioned, receiverSite);
  const center = receiverSite ?? fallback.center;

  return {
    ...fallback,
    mode: map ? "spatial" : fallback.mode,
    center,
    projectLatLon(lat, lon) {
      if (!numeric(lat) || !numeric(lon)) return null;
      if (!map) return mapPoint(fallback, lat, lon);
      const point = map.project([lon, lat]);
      return {
        x: point.x,
        y: point.y,
        distanceKm: haversineDistanceKm(center.lat, center.lon, lat, lon),
        bearingDeg: initialBearingDeg(center.lat, center.lon, lat, lon),
      };
    },
    projectItem(item) {
      return hasPosition(item) ? this.projectLatLon(item.lat, item.lon) : null;
    },
    projectTrail(point) {
      return this.projectLatLon(point.lat, point.lon);
    },
    ringRadius(distanceKm) {
      if (!map || !receiverSite) return distanceKm / rangeKm * fallback.radius;
      const centerPoint = map.project([receiverSite.lon, receiverSite.lat]);
      const edge = destinationPoint(receiverSite.lat, receiverSite.lon, 0, distanceKm);
      const edgePoint = map.project([edge.lon, edge.lat]);
      return Math.hypot(edgePoint.x - centerPoint.x, edgePoint.y - centerPoint.y);
    },
  };
}

function drawRadarOverlay(context, projector, width, height, rangeKm, receiverSite) {
  const centerPoint = receiverSite
    ? projector.projectLatLon(receiverSite.lat, receiverSite.lon)
    : { x: width / 2, y: height / 2 };
  if (!centerPoint) return;

  context.save();
  context.fillStyle = "rgba(0, 0, 0, 0.15)";
  context.fillRect(0, 0, width, height);
  context.strokeStyle = "rgba(122, 171, 155, 0.34)";
  context.lineWidth = 1;

  for (let ring = 1; ring <= 4; ring += 1) {
    const ringKm = rangeKm * ring / 4;
    const radius = projector.ringRadius(ringKm);
    context.beginPath();
    context.arc(centerPoint.x, centerPoint.y, radius, 0, Math.PI * 2);
    context.stroke();
    drawScopeText(context, `${Math.round(ringKm)} km`, centerPoint.x + 8, centerPoint.y - radius + 13, "#9db9ae", "left");
  }

  drawScopeLine(context, centerPoint.x, centerPoint.y - projector.ringRadius(rangeKm), centerPoint.x, centerPoint.y + projector.ringRadius(rangeKm));
  drawScopeLine(context, centerPoint.x - projector.ringRadius(rangeKm), centerPoint.y, centerPoint.x + projector.ringRadius(rangeKm), centerPoint.y);
  context.restore();
}

function fitSpatialMap(map, center, effectiveRange) {
  const latDelta = effectiveRange / 111.32;
  const lonDelta = effectiveRange / Math.max(1, 111.32 * Math.cos(degreesToRadians(center.lat)));

  map.fitBounds(
    [
      [center.lon - lonDelta, center.lat - latDelta],
      [center.lon + lonDelta, center.lat + latDelta],
    ],
    { padding: 58, duration: 0, maxZoom: 12 },
  );
}

function spatialCenter(rows, receiverSites, focusSite = null) {
  if (validSite(focusSite)) {
    return { lat: focusSite.lat, lon: focusSite.lon };
  }

  const sites = receiverSites
    .map((entry) => entry.site)
    .filter(validSite);
  if (sites.length > 0) return averagePosition(sites, null);

  const positioned = rows.filter(hasPosition);
  if (positioned.length > 0) return averagePosition(positioned, null);

  return DEFAULT_CENTER;
}

function destinationPoint(lat, lon, bearingDeg, distanceKm) {
  const angularDistance = distanceKm / EARTH_RADIUS_KM;
  const bearing = degreesToRadians(bearingDeg);
  const lat1 = degreesToRadians(lat);
  const lon1 = degreesToRadians(lon);
  const lat2 = Math.asin(
    Math.sin(lat1) * Math.cos(angularDistance)
      + Math.cos(lat1) * Math.sin(angularDistance) * Math.cos(bearing),
  );
  const lon2 = lon1 + Math.atan2(
    Math.sin(bearing) * Math.sin(angularDistance) * Math.cos(lat1),
    Math.cos(angularDistance) - Math.sin(lat1) * Math.sin(lat2),
  );
  return { lat: radiansToDegrees(lat2), lon: normalizeLongitude(radiansToDegrees(lon2)) };
}

function effectiveRangeKm(rows, positioned, rangeSetting, center, trails, receiverSites = []) {
  if (rangeSetting !== "auto") return Number(rangeSetting);

  const centerSite = validSite(center) ? center : spatialCenter(rows, receiverSites);
  let maxDistance = 0;
  for (const item of rows) {
    if (hasPosition(item)) {
      maxDistance = Math.max(maxDistance, haversineDistanceKm(centerSite.lat, centerSite.lon, item.lat, item.lon));
    }
    for (const point of mergedTrail(item, trails)) {
      maxDistance = Math.max(maxDistance, haversineDistanceKm(centerSite.lat, centerSite.lon, point.lat, point.lon));
    }
  }

  for (const entry of receiverSites) {
    if (validSite(entry.site)) {
      maxDistance = Math.max(maxDistance, haversineDistanceKm(centerSite.lat, centerSite.lon, entry.site.lat, entry.site.lon));
    }
  }

  const padded = Math.max(5, maxDistance * 1.15);
  return AUTO_RANGES_KM.find((range) => range >= padded) ?? AUTO_RANGES_KM[AUTO_RANGES_KM.length - 1];
}

function drawScope({
  canvas,
  rows,
  trails,
  receiverSite,
  viewMode,
  rangeKm,
  selectedKey,
  hoverKey,
  receiverHandleCollisions,
}) {
  if (!canvas) return [];

  const context = canvas.getContext("2d");
  const rect = canvas.getBoundingClientRect();
  if (rect.width === 0 || rect.height === 0) return [];

  const scale = window.devicePixelRatio || 1;
  const width = Math.round(rect.width);
  const height = Math.round(rect.height);
  const canvasWidth = Math.round(width * scale);
  const canvasHeight = Math.round(height * scale);

  if (canvas.width !== canvasWidth || canvas.height !== canvasHeight) {
    canvas.width = canvasWidth;
    canvas.height = canvasHeight;
  }

  context.save();
  context.scale(scale, scale);
  context.clearRect(0, 0, width, height);

  const positioned = rows.filter(hasPosition);
  const effectiveRange = effectiveRangeKm(rows, positioned, rangeKm, receiverSite, trails);
  const projector = viewMode === "map"
    ? makeMapProjector(width, height, effectiveRange, positioned, receiverSite)
    : makeRadarProjector(width, height, effectiveRange, receiverSite);
  const targets = [];

  if (viewMode === "map") {
    drawMapBackground(context, projector, width, height, effectiveRange);
  } else {
    drawRadarBackground(context, projector, width, height, effectiveRange);
  }

  for (const item of rows) drawTrail(context, projector, item, trails, selectedKey, hoverKey);
  for (const item of rows) drawAircraftTarget(context, projector, item, positioned.length, selectedKey, hoverKey, targets);
  drawReceiverLegend(context, rows, width, receiverHandleCollisions);

  context.restore();
  return targets;
}

function makeRadarProjector(width, height, rangeKm, receiverSite) {
  return {
    mode: "radar",
    centerX: width / 2,
    centerY: height / 2,
    radius: Math.min(width, height) * 0.43,
    rangeKm,
    projectItem(item) {
      const rangeBearing = rangeBearingForItem(item, receiverSite);
      return rangeBearing ? polarPoint(this, rangeBearing.distanceKm, rangeBearing.bearingDeg) : null;
    },
    projectTrail(point) {
      const rangeBearing = rangeBearingForPoint(point, receiverSite);
      return rangeBearing ? polarPoint(this, rangeBearing.distanceKm, rangeBearing.bearingDeg) : null;
    },
  };
}

function makeMapProjector(width, height, rangeKm, positioned, receiverSite) {
  const center = receiverSite ?? averagePosition(positioned, receiverSite);
  const radius = Math.min(width, height) * 0.43;

  return {
    mode: "map",
    centerX: width / 2,
    centerY: height / 2,
    radius,
    rangeKm,
    center,
    projectItem(item) {
      return hasPosition(item) ? mapPoint(this, item.lat, item.lon) : null;
    },
    projectTrail(point) {
      return mapPoint(this, point.lat, point.lon);
    },
  };
}

function polarPoint(projector, distanceKm, bearingDeg) {
  if (!numeric(distanceKm) || !numeric(bearingDeg)) return null;
  const distance = (distanceKm / projector.rangeKm) * projector.radius;
  if (distance > projector.radius * 1.08) return null;

  const angle = degreesToRadians(bearingDeg);
  return {
    x: projector.centerX + Math.sin(angle) * distance,
    y: projector.centerY - Math.cos(angle) * distance,
    distanceKm,
    bearingDeg,
  };
}

function mapPoint(projector, lat, lon) {
  if (!numeric(lat) || !numeric(lon)) return null;
  const kmPerLon = 111.32 * Math.cos(degreesToRadians(projector.center.lat));
  const dxKm = (lon - projector.center.lon) * kmPerLon;
  const dyKm = (lat - projector.center.lat) * 110.574;
  const x = projector.centerX + (dxKm / projector.rangeKm) * projector.radius;
  const y = projector.centerY - (dyKm / projector.rangeKm) * projector.radius;
  const distanceKm = Math.hypot(dxKm, dyKm);
  if (distanceKm > projector.rangeKm * 1.08) return null;

  return {
    x,
    y,
    distanceKm,
    bearingDeg: normalizeDegrees(radiansToDegrees(Math.atan2(dxKm, dyKm))),
  };
}

function drawRadarBackground(context, projector, width, height, rangeKm) {
  drawScopeBase(context, width, height);
  context.strokeStyle = "rgba(133, 177, 163, 0.38)";
  context.lineWidth = 1;

  for (let ring = 1; ring <= 4; ring += 1) {
    const radius = projector.radius * ring / 4;
    context.beginPath();
    context.arc(projector.centerX, projector.centerY, radius, 0, Math.PI * 2);
    context.stroke();
    drawScopeText(context, `${Math.round(rangeKm * ring / 4)} km`, projector.centerX + 8, projector.centerY - radius + 14, "#9db9ae", "left");
  }

  drawScopeLine(context, projector.centerX, projector.centerY - projector.radius, projector.centerX, projector.centerY + projector.radius);
  drawScopeLine(context, projector.centerX - projector.radius, projector.centerY, projector.centerX + projector.radius, projector.centerY);
  drawScopeText(context, "N", projector.centerX, projector.centerY - projector.radius - 10, "#f1f7f3", "center");
  drawScopeText(context, "E", projector.centerX + projector.radius + 10, projector.centerY + 4, "#f1f7f3", "center");
  drawScopeText(context, "S", projector.centerX, projector.centerY + projector.radius + 20, "#f1f7f3", "center");
  drawScopeText(context, "W", projector.centerX - projector.radius - 10, projector.centerY + 4, "#f1f7f3", "center");
}

function drawMapBackground(context, projector, width, height, rangeKm) {
  drawScopeBase(context, width, height);
  context.strokeStyle = "rgba(120, 151, 182, 0.32)";
  context.lineWidth = 1;

  for (let index = 1; index < 4; index += 1) {
    const x = width * index / 4;
    const y = height * index / 4;
    drawScopeLine(context, x, 0, x, height);
    drawScopeLine(context, 0, y, width, y);
  }

  context.strokeStyle = "rgba(247, 203, 111, 0.52)";
  drawScopeLine(context, projector.centerX, projector.centerY - projector.radius, projector.centerX, projector.centerY + projector.radius);
  drawScopeLine(context, projector.centerX - projector.radius, projector.centerY, projector.centerX + projector.radius, projector.centerY);
  drawScopeText(context, `${fixed(projector.center.lat, 3)}, ${fixed(projector.center.lon, 3)}`, 14, height - 14, "#b9c9c1", "left");
  drawScopeText(context, `${rangeKm} km`, width - 14, height - 14, "#b9c9c1", "right");
}

function drawScopeBase(context, width, height) {
  context.fillStyle = "#0e181b";
  context.fillRect(0, 0, width, height);
  context.fillStyle = "rgba(255, 255, 255, 0.02)";
  context.fillRect(0, 0, width, height);
}

function drawReceiverSites(context, projector, receiverSites, focusSite) {
  for (const entry of receiverSites) {
    if (!validSite(entry.site)) continue;
    const point = projector.projectLatLon(entry.site.lat, entry.site.lon);
    if (!point) continue;
    const focused = focusSite === entry.site;

    context.save();
    context.translate(point.x, point.y);
    context.fillStyle = focused ? "#f5cb62" : colorWithAlpha(receiverColor({ receiver: entry.receiver }), 0.95);
    context.strokeStyle = "#061014";
    context.lineWidth = focused ? 3 : 2;
    context.beginPath();
    context.moveTo(0, focused ? -9 : -7);
    context.lineTo(focused ? 9 : 7, 0);
    context.lineTo(0, focused ? 9 : 7);
    context.lineTo(focused ? -9 : -7, 0);
    context.closePath();
    context.fill();
    context.stroke();
    context.restore();
  }
}

function drawScopeLine(context, x1, y1, x2, y2) {
  context.beginPath();
  context.moveTo(x1, y1);
  context.lineTo(x2, y2);
  context.stroke();
}

function drawScopeText(context, text, x, y, color, align) {
  context.font = "12px ui-sans-serif, system-ui, sans-serif";
  context.fillStyle = color;
  context.textAlign = align;
  context.textBaseline = "middle";
  context.fillText(text, x, y);
}

function drawTrail(context, projector, item, trails, selectedKey, hoverKey) {
  const trail = mergedTrail(item, trails);
  if (trail.length < 2) return;

  const points = trail
    .map((point) => projector.projectTrail(point))
    .filter((point) => point !== null);
  if (points.length < 2) return;

  const isHighlighted = item.key === selectedKey || item.key === hoverKey;
  context.save();
  context.strokeStyle = isHighlighted ? "rgba(247, 203, 111, 0.95)" : colorWithAlpha(receiverColor(item), 0.48);
  context.lineWidth = isHighlighted ? 2.25 : 1.5;
  context.beginPath();
  context.moveTo(points[0].x, points[0].y);
  for (const point of points.slice(1)) context.lineTo(point.x, point.y);
  context.stroke();
  context.restore();
}

function drawAircraftTarget(context, projector, item, positionedCount, selectedKey, hoverKey, targets, showLabels = true) {
  const point = projector.projectItem(item);
  if (!point) return;

  const selected = item.key === selectedKey;
  const hovered = item.key === hoverKey;
  const stale = item.position_status === "stale";
  const color = selected ? "#f7cb6f" : hovered ? "#7fdcff" : stale ? "#b9c9c1" : receiverColor(item);
  const track = numeric(item.track_deg) ? item.track_deg : item.heading_deg;

  targets.push({ key: item.key, x: point.x, y: point.y, item });

  context.save();
  context.translate(point.x, point.y);
  if (numeric(track)) {
    context.rotate(degreesToRadians(track));
    context.beginPath();
    context.moveTo(0, -10);
    context.lineTo(6, 7);
    context.lineTo(0, 4);
    context.lineTo(-6, 7);
    context.closePath();
  } else {
    context.beginPath();
    context.arc(0, 0, selected || hovered ? 6 : 5, 0, Math.PI * 2);
  }
  context.fillStyle = color;
  context.strokeStyle = "#0e181b";
  context.lineWidth = 2;
  context.fill();
  context.stroke();
  context.restore();

  if (showLabels && (selected || hovered || positionedCount <= 14)) {
    drawAircraftLabel(context, item, point.x + 10, point.y - 10, selected || hovered);
  }
}

function drawAircraftLabel(context, item, x, y, emphasized) {
  const labelText = `${displayName(item)} ${fmt(item.altitude_baro_ft)}`;
  context.save();
  context.font = emphasized ? "700 12px ui-sans-serif, system-ui, sans-serif" : "12px ui-sans-serif, system-ui, sans-serif";
  const metrics = context.measureText(labelText);
  const width = Math.ceil(metrics.width + 10);
  context.fillStyle = emphasized ? "rgba(247, 203, 111, 0.95)" : "rgba(14, 24, 27, 0.78)";
  context.fillRect(x - 5, y - 14, width, 18);
  context.fillStyle = emphasized ? "#18201c" : "#f1f7f3";
  context.textAlign = "left";
  context.textBaseline = "middle";
  context.fillText(labelText, x, y - 5);
  context.restore();
}

function drawReceiverLegend(context, rows, width, receiverHandleCollisions) {
  const receivers = receiverLegendItems(rows);
  if (receivers.length <= 1) return;

  const x = 14;
  let y = 16;
  context.save();
  context.font = "12px ui-sans-serif, system-ui, sans-serif";
  context.textBaseline = "middle";

  for (const receiver of receivers.slice(0, 5)) {
    const labelText = receiverDisplay(receiver, receiverHandleCollisions);
    const textWidth = context.measureText(labelText).width;
    context.fillStyle = "rgba(14, 24, 27, 0.78)";
    context.fillRect(x - 6, y - 9, Math.ceil(textWidth + 28), 18);
    context.fillStyle = receiverColor({ receiver });
    context.fillRect(x, y - 5, 10, 10);
    context.fillStyle = "#f1f7f3";
    context.textAlign = "left";
    context.fillText(labelText, x + 16, y);
    y += 22;
  }

  if (receivers.length > 5) {
    const labelText = `+${receivers.length - 5} receivers`;
    const textWidth = context.measureText(labelText).width;
    context.fillStyle = "rgba(14, 24, 27, 0.78)";
    context.fillRect(x - 6, y - 9, Math.ceil(textWidth + 12), 18);
    context.fillStyle = "#f1f7f3";
    context.fillText(labelText, x, y);
  }

  context.restore();
}

function receiverLegendItems(rows) {
  const receivers = new Map();
  for (const item of rows) {
    if (item.receiver?.id && !receivers.has(item.receiver.id)) {
      receivers.set(item.receiver.id, item.receiver);
    }
  }
  return [...receivers.values()];
}

function scopeReadout(item, positionedCount, visibleCount, receiverSite, receiverHandleCollisions) {
  if (!item) return `${positionedCount} positioned of ${visibleCount} visible`;

  const rangeBearing = rangeBearingForItem(item, receiverSite);
  const range = rangeBearing ? `${rangeBearing.distanceKm.toFixed(1)} km ${rangeBearing.bearingDeg.toFixed(0)} deg` : "range unknown";
  const speed = numeric(speedValue(item)) ? `${speedValue(item)} kt` : "speed unknown";
  const altitude = item.altitude_baro_ft === null || item.altitude_baro_ft === undefined ? "alt unknown" : `${item.altitude_baro_ft} ft`;
  return `${displayName(item)} - ${receiverDisplay(item.receiver, receiverHandleCollisions)} - ${altitude} - ${speed} - ${range}`;
}

function rangeBearingForItem(item, receiverSite = null) {
  if (numeric(item.distance_km) && numeric(item.bearing_deg)) {
    return { distanceKm: item.distance_km, bearingDeg: item.bearing_deg };
  }
  if (!receiverSite || !hasPosition(item)) return null;
  return rangeBearing(receiverSite.lat, receiverSite.lon, item.lat, item.lon);
}

function rangeBearingForPoint(point, receiverSite = null) {
  if (numeric(point.distanceKm) && numeric(point.bearingDeg)) {
    return { distanceKm: point.distanceKm, bearingDeg: point.bearingDeg };
  }
  if (!receiverSite) return null;
  return rangeBearing(receiverSite.lat, receiverSite.lon, point.lat, point.lon);
}

function rangeBearing(lat1, lon1, lat2, lon2) {
  return {
    distanceKm: haversineDistanceKm(lat1, lon1, lat2, lon2),
    bearingDeg: initialBearingDeg(lat1, lon1, lat2, lon2),
  };
}

function averagePosition(items, receiverSite) {
  if (items.length === 0) return receiverSite ?? { lat: 0, lon: 0 };
  const total = items.reduce((sum, item) => ({
    lat: sum.lat + item.lat,
    lon: sum.lon + item.lon,
  }), { lat: 0, lon: 0 });
  return {
    lat: total.lat / items.length,
    lon: total.lon / items.length,
  };
}

function haversineDistanceKm(lat1, lon1, lat2, lon2) {
  const phi1 = degreesToRadians(lat1);
  const phi2 = degreesToRadians(lat2);
  const deltaPhi = degreesToRadians(lat2 - lat1);
  const deltaLambda = degreesToRadians(lon2 - lon1);
  const a = Math.sin(deltaPhi / 2) ** 2
    + Math.cos(phi1) * Math.cos(phi2) * Math.sin(deltaLambda / 2) ** 2;
  return EARTH_RADIUS_KM * 2 * Math.atan2(Math.sqrt(a), Math.sqrt(1 - a));
}

function initialBearingDeg(lat1, lon1, lat2, lon2) {
  const phi1 = degreesToRadians(lat1);
  const phi2 = degreesToRadians(lat2);
  const deltaLambda = degreesToRadians(lon2 - lon1);
  const y = Math.sin(deltaLambda) * Math.cos(phi2);
  const x = Math.cos(phi1) * Math.sin(phi2)
    - Math.sin(phi1) * Math.cos(phi2) * Math.cos(deltaLambda);
  return normalizeDegrees(radiansToDegrees(Math.atan2(y, x)));
}

function degreesToRadians(value) {
  return value * Math.PI / 180;
}

function radiansToDegrees(value) {
  return value * 180 / Math.PI;
}

function normalizeDegrees(value) {
  return ((value % 360) + 360) % 360;
}

function normalizeLongitude(value) {
  return ((value + 540) % 360) - 180;
}

function upsertAircraft(item, nowMs, aircraft, trails, clockRef) {
  const existing = aircraft.get(item.key);
  recordTrail(item, nowMs, trails, clockRef);
  if (isOlderAircraftUpdate(item, existing)) return;

  aircraft.set(item.key, item);
}

function isOlderAircraftUpdate(next, existing) {
  return numeric(next?.last_seen_ms)
    && numeric(existing?.last_seen_ms)
    && next.last_seen_ms < existing.last_seen_ms;
}

function recordTrail(item, nowMs, trails, clockRef) {
  if (!hasPosition(item)) return;

  const time = item.position_last_seen_ms ?? item.last_seen_ms ?? nowMs ?? serverClock(clockRef);
  const point = {
    lat: item.lat,
    lon: item.lon,
    distanceKm: item.distance_km,
    bearingDeg: item.bearing_deg,
    time,
  };
  const trail = trails.get(item.key) ?? [];
  const last = trail.at(-1);

  if (last) {
    const movedKm = haversineDistanceKm(last.lat, last.lon, point.lat, point.lon);
    const elapsedMs = Math.max(0, point.time - last.time);
    if (movedKm < TRAIL_MIN_MOVE_KM && elapsedMs < TRAIL_MIN_INTERVAL_MS) {
      last.time = point.time;
      last.distanceKm = point.distanceKm;
      last.bearingDeg = point.bearingDeg;
      pruneTrail(trail, nowMs, clockRef);
      trails.set(item.key, trail);
      return;
    }
  }

  trail.push(point);
  pruneTrail(trail, nowMs, clockRef);
  trails.set(item.key, trail);
}

function pruneTrail(trail, nowMs, clockRef) {
  const newestMs = nowMs ?? serverClock(clockRef);
  while (trail.length > 0 && newestMs - trail[0].time > TRAIL_MAX_AGE_MS) trail.shift();
  while (trail.length > TRAIL_MAX_POINTS) trail.shift();
}

function targetFromEvent(event, canvas, targets) {
  if (!canvas) return null;
  const rect = canvas.getBoundingClientRect();
  const x = event.clientX - rect.left;
  const y = event.clientY - rect.top;
  let nearest = null;
  let nearestDistance = HOVER_RADIUS_PX ** 2;

  for (const target of targets) {
    const distance = (target.x - x) ** 2 + (target.y - y) ** 2;
    if (distance <= nearestDistance) {
      nearest = target;
      nearestDistance = distance;
    }
  }

  return nearest;
}

render(<App />, document.getElementById("app"));

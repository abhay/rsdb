import { render } from "preact";
import { useCallback, useEffect, useMemo, useRef, useState } from "preact/hooks";
import CoverageScope from "./coverage_scope.jsx";

const TRAIL_MAX_POINTS = 180;
const TRAIL_MAX_AGE_MS = 30 * 60 * 1000;
const TRAIL_MIN_INTERVAL_MS = 5000;
const TRAIL_MIN_MOVE_KM = 0.05;
const EARTH_RADIUS_KM = 6371;
const ROUTE_ENDPOINT_NEAR_KM = 120;
const ROUTE_MISMATCH_MIN_EXCESS_KM = 300;
const ROUTE_MISMATCH_EXCESS_RATIO = 0.25;
const FIELD_RECENT_MS = 30 * 1000;
const FIELD_STALE_MS = 2 * 60 * 1000;
const RECEIVER_COLORS = ["#70d673", "#7fdcff", "#f7cb6f", "#e88a74", "#a78bfa", "#4cc8a3", "#f78fb3"];
const REPOSITORY_URL = "https://github.com/abhay/rsdb";
const AIRCRAFT_LOOKUP_BASE = "https://api.adsbdb.com/v0";

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
  const [focusReceiverId, setFocusReceiverId] = useState(null);
  const [receiverOnly, setReceiverOnly] = useState(false);
  const [clockTick, setClockTick] = useState(0);
  const [bootstrapReady, setBootstrapReady] = useState(false);
  const lookupCacheRef = useRef(new Map());
  const [lookupVersion, setLookupVersion] = useState(0);

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
  }, [displayItemMap, selectedKey]);
  useEffect(() => {
    if (!focusReceiverId) setReceiverOnly(false);
  }, [focusReceiverId]);

  const selectedItem = selectedKey ? displayItemMap.get(selectedKey) ?? null : null;
  const selectedLookupKey = selectedItem ? lookupCacheKey(selectedItem) : null;

  useEffect(() => {
    if (!selectedItem || !selectedLookupKey) return;

    const cached = lookupCacheRef.current.get(selectedLookupKey);
    if (cached?.status === "loading" || cached?.status === "ready") return;

    lookupCacheRef.current.set(selectedLookupKey, { status: "loading" });
    setLookupVersion((version) => version + 1);

    fetchAircraftLookup(selectedItem)
      .then((lookup) => {
        lookupCacheRef.current.set(selectedLookupKey, lookup);
        setLookupVersion((version) => version + 1);
      })
      .catch((error) => {
        lookupCacheRef.current.set(selectedLookupKey, {
          status: "ready",
          aircraft: null,
          route: null,
          errors: [error?.message ?? "Lookup unavailable"],
        });
        setLookupVersion((version) => version + 1);
      });
  }, [selectedItem, selectedLookupKey]);

  const selectedLookup = selectedLookupKey
    ? lookupCacheRef.current.get(selectedLookupKey) ?? { status: "idle" }
    : null;
  const selectedTrail = selectedItem ? mergedTrail(selectedItem, trailsRef.current) : [];
  const totalAircraft = displayItems.length;
  const displayedAircraftCount = rows.length === totalAircraft ? totalAircraft : `${rows.length}/${totalAircraft}`;
  const handleAircraftSelect = useCallback((key) => {
    setSelectedKey((current) => current === key ? null : key);
  }, []);

  return (
    <main className="shell">
      <Header
        status={status}
        statusReachable={statusReachable}
        aircraftCount={displayedAircraftCount}
        nowMs={nowMs}
        socketState={socketState}
      />
      <section className={`ops-layout ${selectedItem ? "has-selection" : "summary-collapsed"}`} aria-label="Live aircraft workspace">
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
            onSelect={handleAircraftSelect}
          />
        </aside>
        <section className="center-panel" aria-label="Live coverage visualization">
          <CoverageScope
            items={scopedItems}
            trails={trailsRef.current}
            receiverSite={receiverSite}
            receiverSites={receiverSites}
            focusReceiverId={focusReceiverId}
            selectedKey={selectedKey}
            nowMs={nowMs}
            onSelectAircraft={handleAircraftSelect}
          />
        </section>
        <aside className={`right-rail ${selectedItem ? "has-selection" : "summary-open"}`}>
          {selectedItem ? (
            <DetailsPanel
              item={selectedItem}
              trail={selectedTrail}
              lookup={selectedLookup}
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
      <span hidden>{clockTick}{lookupVersion}</span>
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
        ["Last", age(status.last_submission_ms, nowMs)],
      ]
    : [
        ["RX", statusReachable ? status.receiver_connected ? "Live" : "Retry" : "Offline"],
        ["AC", aircraftCount],
        ["Frames", `${Number(status.decoded_frames_per_second ?? 0).toFixed(1)}/s`],
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
      <nav className="topbar-actions" aria-label="Project links">
        <a className="topbar-link github-link" href={REPOSITORY_URL} target="_blank" rel="noreferrer" aria-label="RSDB on GitHub" title="RSDB on GitHub">
          <GitHubIcon />
        </a>
        <a className="topbar-link agents-link" href="/agents.md" aria-label="Agent guide" title="Agent guide">
          <RobotIcon />
        </a>
        <span className={`connection-state connection-${socketState.toLowerCase()}`}>{socketState}</span>
      </nav>
    </header>
  );
}

function RobotIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
      <path className="robot-antenna" d="M12 4V2.5" />
      <circle className="robot-dot" cx="12" cy="2.5" r="1" />
      <rect className="robot-face" x="5" y="6" width="14" height="12" rx="3" />
      <circle className="robot-eye" cx="9.5" cy="11.5" r="1.35" />
      <circle className="robot-eye" cx="14.5" cy="11.5" r="1.35" />
      <path className="robot-mouth" d="M9 15h6" />
      <path className="robot-ear" d="M3.5 10v4" />
      <path className="robot-ear" d="M20.5 10v4" />
    </svg>
  );
}

function GitHubIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path fill="currentColor" d="M8 0C3.58 0 0 3.67 0 8.19c0 3.62 2.29 6.69 5.47 7.77.4.08.55-.18.55-.39v-1.53c-2.23.5-2.7-.97-2.7-.97-.36-.95-.89-1.2-.89-1.2-.73-.51.06-.5.06-.5.81.06 1.23.85 1.23.85.72 1.26 1.87.9 2.33.69.07-.53.28-.9.51-1.1-1.78-.21-3.64-.91-3.64-4.04 0-.89.31-1.62.82-2.2-.08-.21-.36-1.04.08-2.17 0 0 .68-.22 2.2.84A7.43 7.43 0 0 1 8 3.97c.68 0 1.36.09 2 .27 1.52-1.06 2.19-.84 2.19-.84.44 1.13.16 1.96.08 2.17.51.58.82 1.31.82 2.2 0 3.14-1.87 3.83-3.65 4.03.29.26.54.76.54 1.53v2.24c0 .21.14.47.55.39A8.08 8.08 0 0 0 16 8.19C16 3.67 12.42 0 8 0Z" />
    </svg>
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

function DetailsPanel({ item, trail, lookup, nowMs, receiverHandleCollisions, onClose }) {
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
      <LookupPanel lookup={lookup} item={item} />
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

function LookupPanel({ lookup, item }) {
  const routeView = routeDisplayForItem(lookup?.route ?? null, item, lookup?.observed_route ?? null);
  const state = lookupState(lookup, item, routeView);
  const aircraft = lookup?.aircraft ?? null;
  const route = routeView.route;
  const hasResult = Boolean(aircraft || route || routeView.warning);

  return (
    <section className="details-section lookup-section">
      <div className="lookup-head">
        <h3>Lookup</h3>
        <span className={`lookup-state lookup-state-${state.level}`}>{state.label}</span>
      </div>
      {lookup?.status === "loading" ? (
        <div className="lookup-empty">Loading</div>
      ) : hasResult ? (
        <div className="lookup-body">
          {aircraft && <AircraftLookupCard aircraft={aircraft} />}
          {routeView.warning && <RouteLookupWarning route={routeView.withheldRoute} check={routeView.check} />}
          {route && <RouteLookupCard route={route} />}
        </div>
      ) : (
        <div className="lookup-empty">{lookup?.errors?.length ? "Lookup unavailable" : "No public lookup result"}</div>
      )}
    </section>
  );
}

function RouteLookupWarning({ route, check }) {
  return (
    <article className="lookup-warning">
      <strong>Route withheld</strong>
      <p>
        Public callsign lookup returned {routeAirportPair(route)}, but RSDB could not corroborate it from this aircraft's observed position or route.
      </p>
      {check?.level === "mismatch" && check?.excessKm > 0 && (
        <span>{Math.round(check.excessKm).toLocaleString("en-US")} km off route</span>
      )}
    </article>
  );
}

function AircraftLookupCard({ aircraft }) {
  const photo = textOrNull(aircraft.url_photo_thumbnail) ?? textOrNull(aircraft.url_photo);

  return (
    <article className={`lookup-card ${photo ? "has-photo" : ""}`}>
      {photo && (
        <img
          src={photo}
          alt={lookupText(aircraft.registration, aircraft.icao_type, "Aircraft")}
          loading="lazy"
          referrerPolicy="no-referrer"
        />
      )}
      <dl className="details-grid lookup-grid">
        <DetailField label="Registration" value={lookupText(aircraft.registration)} />
        <DetailField label="Type" value={aircraftTypeLabel(aircraft)} />
        <DetailField label="Manufacturer" value={lookupText(aircraft.manufacturer)} />
        <DetailField label="Owner" value={lookupText(aircraft.registered_owner)} />
        <DetailField label="Operator" value={lookupText(aircraft.registered_owner_operator_flag_code)} />
        <DetailField label="Country" value={lookupText(aircraft.registered_owner_country_name)} />
      </dl>
    </article>
  );
}

function RouteLookupCard({ route }) {
  const origin = route.origin ?? null;
  const destination = route.destination ?? null;

  return (
    <article className="route-card">
      <dl className="details-grid lookup-grid">
        <DetailField label="Flight" value={lookupText(route.callsign_iata, route.callsign_icao, route.callsign)} />
        <DetailField label="Airline" value={airlineLabel(route.airline)} />
        <DetailField label="Source" value={routeSourceLabel(route)} />
        <DetailField label="Confidence" value={routeConfidenceLabel(route)} />
      </dl>
      <div className="route-pair">
        <AirportBlock label="Origin" airport={origin} />
        <AirportBlock label="Destination" airport={destination} />
      </div>
    </article>
  );
}

function AirportBlock({ label: labelText, airport }) {
  return (
    <section className="airport-block">
      <span>{labelText}</span>
      <strong>{airportCodeLabel(airport)}</strong>
      <p>{airportNameLabel(airport)}</p>
    </section>
  );
}

function DetailField({ label: labelText, value }) {
  return (
    <div className={value === "-" ? "detail-field detail-field-missing" : "detail-field"}>
      <dt>{labelText}</dt>
      <dd>{value}</dd>
    </div>
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

async function fetchAircraftLookup(item) {
  const icao = normalizedIcao(item.icao);
  const callsign = normalizedCallsign(item.callsign);
  const [aircraftResult, routeResult, observedRouteResult] = await Promise.all([
    lookupResult(icao ? fetchLookupRecord(`/aircraft/${encodeURIComponent(icao)}`) : Promise.resolve(null)),
    lookupResult(callsign ? fetchLookupRecord(`/callsign/${encodeURIComponent(callsign)}`) : Promise.resolve(null)),
    lookupResult(fetchObservedRouteLookup(icao, callsign)),
  ]);
  const errors = [aircraftResult.error, routeResult.error, observedRouteResult.error]
    .filter(Boolean)
    .map((error) => error.message);

  return {
    status: "ready",
    aircraft: aircraftResult.value?.aircraft ?? null,
    route: routeResult.value?.flightroute ?? null,
    observed_route: observedRouteResult.value?.route ?? null,
    errors,
  };
}

async function lookupResult(promise) {
  try {
    return { value: await promise, error: null };
  } catch (error) {
    return { value: null, error };
  }
}

async function fetchLookupRecord(path) {
  const response = await fetch(`${AIRCRAFT_LOOKUP_BASE}${path}`);
  if (response.status === 404) return null;
  if (!response.ok) throw new Error(`Lookup HTTP ${response.status}`);

  const payload = await response.json();
  return payload?.response && typeof payload.response === "object" ? payload.response : null;
}

async function fetchObservedRouteLookup(icao, callsign) {
  const query = new URLSearchParams();
  if (icao) query.set("icao", icao);
  if (callsign) query.set("callsign", callsign);
  if (!query.toString()) return null;

  const response = await fetch(`/route-lookup.json?${query.toString()}`, { cache: "no-store" });
  if (!response.ok) throw new Error(`Observed route HTTP ${response.status}`);
  return response.json();
}

function lookupCacheKey(item) {
  return `${normalizedIcao(item.icao)}|${normalizedCallsign(item.callsign)}`;
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

function textOrNull(value) {
  if (value === null || value === undefined) return null;
  const text = String(value).trim();
  return text === "" ? null : text;
}

function lookupText(...values) {
  for (const value of values) {
    const text = textOrNull(value);
    if (text) return text;
  }
  return "-";
}

function normalizedIcao(value) {
  return (textOrNull(value) ?? "").toUpperCase();
}

function normalizedCallsign(value) {
  return (textOrNull(value) ?? "").replace(/\s+/g, "").toUpperCase();
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

function lookupState(lookup, item, routeView = null) {
  if (!lookup || lookup.status === "idle") return { level: "idle", label: "Waiting" };
  if (lookup.status === "loading") return { level: "loading", label: "Loading" };
  const view = routeView ?? routeDisplayForItem(lookup.route, item, lookup.observed_route);
  if (view.warning) {
    return { level: "warning", label: lookup.aircraft ? "Partial" : "Unverified" };
  }
  if (lookup.aircraft || view.route) return { level: "ready", label: view.route?.source === "rsdb_observed" ? "Inferred" : "Found" };
  if (lookup.errors?.length) return { level: "error", label: "Error" };
  return { level: "missing", label: "None" };
}

function routeDisplayForItem(route, item, observedRoute = null) {
  const check = routePositionCheck(item, route);
  if (route && routeConflictsWithObserved(route, observedRoute)) {
    return {
      route: observedRoute,
      warning: true,
      check: { ...check, level: "mismatch" },
      withheldRoute: route,
    };
  }
  if (route && publicRouteCorroborated(route, observedRoute, check)) {
    return { route, warning: false, check, withheldRoute: null };
  }
  if (route) {
    return {
      route: observedRoute,
      warning: true,
      check: check.level === "unknown" ? { ...check, level: "unverified" } : check,
      withheldRoute: route,
    };
  }
  return { route: observedRoute, warning: false, check, withheldRoute: null };
}

function routePositionCheck(item, route) {
  const origin = airportPoint(route?.origin);
  const destination = airportPoint(route?.destination);
  if (!route || !origin || !destination || !item || !hasPosition(item)) {
    return { level: "unknown" };
  }

  const routeKm = haversineDistanceKm(origin.lat, origin.lon, destination.lat, destination.lon);
  if (!(routeKm > 0)) return { level: "unknown" };

  const fromOriginKm = haversineDistanceKm(origin.lat, origin.lon, item.lat, item.lon);
  const toDestinationKm = haversineDistanceKm(item.lat, item.lon, destination.lat, destination.lon);
  const nearestEndpointKm = Math.min(fromOriginKm, toDestinationKm);
  const excessKm = Math.max(0, fromOriginKm + toDestinationKm - routeKm);
  const allowedExcessKm = Math.max(ROUTE_MISMATCH_MIN_EXCESS_KM, routeKm * ROUTE_MISMATCH_EXCESS_RATIO);

  if (nearestEndpointKm <= ROUTE_ENDPOINT_NEAR_KM || excessKm <= allowedExcessKm) {
    return { level: "plausible", routeKm, excessKm, nearestEndpointKm };
  }
  return { level: "mismatch", routeKm, excessKm, nearestEndpointKm };
}

function airportPoint(airport) {
  if (!airport || !numeric(airport.latitude) || !numeric(airport.longitude)) return null;
  return { lat: airport.latitude, lon: airport.longitude };
}

function routeAirportPair(route) {
  return `${airportShortCode(route?.origin)} to ${airportShortCode(route?.destination)}`;
}

function airportShortCode(airport) {
  return lookupText(airport?.iata_code, airport?.icao_code);
}

function publicRouteCorroborated(route, observedRoute, check) {
  if (check?.nearestEndpointKm <= ROUTE_ENDPOINT_NEAR_KM) return true;
  return routeSharesObservedEndpoint(route, observedRoute);
}

function routeConflictsWithObserved(route, observedRoute) {
  if (!route || !observedRoute) return false;
  const observedEndpoints = [observedRoute.origin, observedRoute.destination].filter(Boolean);
  if (observedEndpoints.length === 0) return false;
  return observedEndpoints.every((airport) => !routeHasAirport(route, airport));
}

function routeSharesObservedEndpoint(route, observedRoute) {
  if (!route || !observedRoute) return false;
  return [observedRoute.origin, observedRoute.destination]
    .filter(Boolean)
    .some((airport) => routeHasAirport(route, airport));
}

function routeHasAirport(route, airport) {
  return sameAirport(route.origin, airport) || sameAirport(route.destination, airport);
}

function sameAirport(left, right) {
  if (!left || !right) return false;
  const leftCodes = airportCodes(left);
  const rightCodes = airportCodes(right);
  return leftCodes.some((code) => rightCodes.includes(code));
}

function airportCodes(airport) {
  return [airport.icao_code, airport.iata_code]
    .map(textOrNull)
    .filter(Boolean)
    .map((code) => code.toUpperCase());
}

function routeSourceLabel(route) {
  if (route?.source_label) return route.source_label;
  if (route?.source === "rsdb_observed") return "RSDB observed";
  return "Public lookup";
}

function routeConfidenceLabel(route) {
  if (route?.confidence === "medium") return "Medium";
  if (route?.confidence === "low") return "Low";
  return route?.source === "rsdb_observed" ? "Low" : "Public";
}

function aircraftTypeLabel(aircraft) {
  const type = textOrNull(aircraft.type);
  const icaoType = textOrNull(aircraft.icao_type);
  if (type && icaoType && type !== icaoType) return `${type} / ${icaoType}`;
  return lookupText(type, icaoType);
}

function airlineLabel(airline) {
  if (!airline) return "-";
  const name = textOrNull(airline.name);
  const codes = [airline.iata, airline.icao].map(textOrNull).filter(Boolean).join(" / ");
  return lookupText(name && codes ? `${name} / ${codes}` : name, codes);
}

function airportCodeLabel(airport) {
  if (!airport) return "-";
  const codes = [airport.icao_code, airport.iata_code].map(textOrNull).filter(Boolean).join(" / ");
  return lookupText(codes);
}

function airportNameLabel(airport) {
  if (!airport) return "-";
  const name = textOrNull(airport.name);
  const municipality = textOrNull(airport.municipality);
  if (name && municipality && !name.includes(municipality)) return `${name}, ${municipality}`;
  return lookupText(name, municipality);
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

function validSite(site) {
  return Boolean(site) && numeric(site.lat) && numeric(site.lon);
}

function receiverColor(item) {
  const receiverId = item.receiver?.id ?? "local";
  return RECEIVER_COLORS[stringHash(receiverId) % RECEIVER_COLORS.length];
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

function haversineDistanceKm(lat1, lon1, lat2, lon2) {
  const phi1 = degreesToRadians(lat1);
  const phi2 = degreesToRadians(lat2);
  const deltaPhi = degreesToRadians(lat2 - lat1);
  const deltaLambda = degreesToRadians(lon2 - lon1);
  const a = Math.sin(deltaPhi / 2) ** 2
    + Math.cos(phi1) * Math.cos(phi2) * Math.sin(deltaLambda / 2) ** 2;
  return EARTH_RADIUS_KM * 2 * Math.atan2(Math.sqrt(a), Math.sqrt(1 - a));
}

function degreesToRadians(value) {
  return value * Math.PI / 180;
}

render(<App />, document.getElementById("app"));

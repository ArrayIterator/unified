// Maps page (/react/maps) — Map & Tagging administration.
//
// Everything here is ajaxified: locations, layers, geofences,
// kinds, roles, providers, and settings all load through the
// maps API and every change re-fetches instead of reloading
// the page. The map itself renders from the resolved settings
// (GET /api/v1/maps/settings); clicking the map fills a new
// location's coordinates, clicking a pin selects it.

import { useMemo, useState } from "react";
import {
  attachMapLocation,
  createMapGeofence,
  createMapKind,
  createMapLayer,
  createMapLocation,
  createMapProvider,
  createMapRole,
  deleteMapGeofence,
  deleteMapKind,
  deleteMapLayer,
  deleteMapLocation,
  deleteMapProvider,
  deleteMapRole,
  fetchEntityLocations,
  fetchMapGeofences,
  fetchMapKinds,
  fetchMapLayers,
  fetchMapLocations,
  fetchMapProviders,
  fetchMapRoles,
  fetchMapSettings,
  fetchNearbyLocations,
  itemsFromData,
  saveMapSettings,
  testGeofenceContains,
  type MapGeofence,
  type EntityLocation,
  type MapLocation,
  type NearbyLocation,
} from "../api";
import { useLoad } from "../hooks";
import { Alert, Badge, Card, EmptyState, Skeleton } from "../components/ui";
import MapView from "../components/MapView";

const inputClass =
  "w-full rounded-md border border-border bg-surface px-3 py-2 text-sm text-text";
const labelClass = "mb-1 block text-sm font-medium";
const buttonClass =
  "rounded-md border border-border px-3 py-1.5 text-sm text-muted hover:bg-surface-hover";
const primaryButtonClass =
  "rounded-md bg-accent px-3 py-1.5 text-sm font-medium text-on-accent hover:opacity-90";
const dangerButtonClass =
  "rounded-md border border-border px-2 py-1 text-xs text-muted hover:bg-surface-hover";

function numberOrNull(value: string): number | null {
  if (value.trim() === "") return null;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

export default function MapsPage() {
  const settingsLoad = useLoad(() => fetchMapSettings(), []);
  const locationsLoad = useLoad(() => fetchMapLocations(), []);
  const layersLoad = useLoad(() => fetchMapLayers(), []);
  const geofencesLoad = useLoad(() => fetchMapGeofences(), []);
  const kindsLoad = useLoad(() => fetchMapKinds(), []);
  const rolesLoad = useLoad(() => fetchMapRoles(), []);
  const providersLoad = useLoad(() => fetchMapProviders(), []);

  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);

  const [label, setLabel] = useState("");
  const [kind, setKind] = useState("pin");
  const [latitude, setLatitude] = useState("");
  const [longitude, setLongitude] = useState("");
  const [address, setAddress] = useState("");
  const [layerId, setLayerId] = useState("");

  const [nearbyLat, setNearbyLat] = useState("");
  const [nearbyLng, setNearbyLng] = useState("");
  const [nearbyRadius, setNearbyRadius] = useState("10");
  const [nearbyResults, setNearbyResults] = useState<NearbyLocation[] | null>(null);

  const [layerName, setLayerName] = useState("");
  const [geofenceName, setGeofenceName] = useState("");
  const [geofenceGeometry, setGeofenceGeometry] = useState("");
  const [kindSlug, setKindSlug] = useState("");
  const [kindLabel, setKindLabel] = useState("");
  const [roleSlug, setRoleSlug] = useState("");
  const [roleLabel, setRoleLabel] = useState("");
  const [providerSlug, setProviderSlug] = useState("");
  const [providerName, setProviderName] = useState("");
  const [providerTemplate, setProviderTemplate] = useState("");
  const [providerAttribution, setProviderAttribution] = useState("");
  const [providerNeedsKey, setProviderNeedsKey] = useState(false);
  const [activeProvider, setActiveProvider] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [defaultLat, setDefaultLat] = useState("");
  const [defaultLng, setDefaultLng] = useState("");
  const [defaultZoom, setDefaultZoom] = useState("");

  const [entityType, setEntityType] = useState("shipment");
  const [entityId, setEntityId] = useState("");
  const [linkRole, setLinkRole] = useState("delivery");
  const [entityLocations, setEntityLocations] = useState<EntityLocation[] | null>(null);

  const settings = settingsLoad.status === "ready" ? settingsLoad.data : null;
  const locations = locationsLoad.data ? itemsFromData<MapLocation>(locationsLoad.data) : [];
  const layers = layersLoad.data ?? [];
  const geofences: MapGeofence[] = geofencesLoad.data ?? [];
  const kinds = kindsLoad.data ?? [];
  const roles = rolesLoad.data ?? [];
  const providers = providersLoad.data ?? [];

  const kindColors = useMemo(() => {
    const map: Record<string, string> = {};
    for (const item of kinds) map[item.slug] = item.color;
    return map;
  }, [kinds]);

  const selected = locations.find((location) => location.id === selectedId) ?? null;

  function reloadAll(): void {
    settingsLoad.reload();
    locationsLoad.reload();
    layersLoad.reload();
    geofencesLoad.reload();
    kindsLoad.reload();
    rolesLoad.reload();
    providersLoad.reload();
  }

  async function run(action: () => Promise<unknown>, message?: string): Promise<boolean> {
    setError(null);
    setNotice(null);
    try {
      await action();
      reloadAll();
      if (message) setNotice(message);
      return true;
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
      return false;
    }
  }

  const loading =
    settingsLoad.status === "loading" ||
    locationsLoad.status === "loading" ||
    layersLoad.status === "loading" ||
    geofencesLoad.status === "loading" ||
    kindsLoad.status === "loading" ||
    rolesLoad.status === "loading" ||
    providersLoad.status === "loading";

  if (loading) return <Skeleton lines={6} />;
  if (settingsLoad.status === "error") {
    return <Alert title="Could not load map settings">{settingsLoad.error.message}</Alert>;
  }
  if (locationsLoad.status === "error") {
    return <Alert title="Could not load locations">{locationsLoad.error.message}</Alert>;
  }

  async function saveLocation(): Promise<void> {
    const lat = numberOrNull(latitude);
    const lng = numberOrNull(longitude);
    if (lat === null || lng === null) {
      setError("Latitude and longitude are required (click the map to fill them).");
      return;
    }
    const saved = await run(
      () =>
        createMapLocation({
          label,
          kind,
          latitude: lat,
          longitude: lng,
          address: address.trim() === "" ? null : address.trim(),
          layer_id: layerId === "" ? null : layerId,
        }),
      "Location saved.",
    );
    if (!saved) return;
    setLabel("");
    setAddress("");
    setLatitude("");
    setLongitude("");
  }

  async function searchNearby(): Promise<void> {
    const lat = numberOrNull(nearbyLat);
    const lng = numberOrNull(nearbyLng);
    const radius = numberOrNull(nearbyRadius);
    if (lat === null || lng === null || radius === null) {
      setError("Nearby search needs a latitude, a longitude, and a radius.");
      return;
    }
    setError(null);
    try {
      setNearbyResults(await fetchNearbyLocations(lat, lng, radius));
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  async function loadEntityLocations(): Promise<void> {
    if (entityId.trim() === "") {
      setError("Enter an entity id to look up its locations.");
      return;
    }
    setError(null);
    try {
      const found = await fetchEntityLocations(entityType.trim(), entityId.trim());
      setEntityLocations(found);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  async function saveSettings(): Promise<void> {
    const input: {
      active_provider?: string;
      api_key?: string;
      default_latitude?: number;
      default_longitude?: number;
      default_zoom?: number;
    } = {};
    if (activeProvider !== "") input.active_provider = activeProvider;
    if (apiKey !== "") input.api_key = apiKey;
    const lat = numberOrNull(defaultLat);
    const lng = numberOrNull(defaultLng);
    const zoom = numberOrNull(defaultZoom);
    if (lat !== null) input.default_latitude = lat;
    if (lng !== null) input.default_longitude = lng;
    if (zoom !== null) input.default_zoom = Math.round(zoom);
    await run(() => saveMapSettings(input), "Map settings saved.");
    setApiKey("");
  }

  return (
    <div className="space-y-4">
      <div>
        <h2 className="text-xl font-bold">Maps</h2>
        <p className="mt-1 text-sm text-muted">
          Shared locations, layers, geofences, and tile providers. Click
          the map to fill a new location&apos;s coordinates; click a pin to
          select it. Provider: {settings?.display_name ?? "—"}
          {settings && settings.requires_api_key && !settings.key_configured
            ? " (needs an API key — see provider settings below)"
            : ""}
        </p>
      </div>

      {error && <Alert title="Action failed">{error}</Alert>}
      {notice && <Alert title="Done">{notice}</Alert>}

      <MapView
        settings={settings}
        locations={locations}
        kindColors={kindColors}
        selectedId={selectedId}
        onSelect={setSelectedId}
        onMapClick={(lat, lng) => {
          setLatitude(String(lat.toFixed(6)));
          setLongitude(String(lng.toFixed(6)));
        }}
      />

      {selected && (
        <Card title={selected.label}>
          <p className="text-sm text-muted">
            {selected.kind} · {selected.latitude.toFixed(6)}, {selected.longitude.toFixed(6)}
            {selected.address ? ` · ${selected.address}` : ""}
          </p>
          <div className="mt-3">
            <button
              type="button"
              className={dangerButtonClass}
              onClick={() =>
                void run(() => deleteMapLocation(selected.id), "Location deleted.")
              }
            >
              Delete location
            </button>
          </div>
        </Card>
      )}

      <Card title="New location">
        <div className="grid gap-3 sm:grid-cols-2">
          <div>
            <label className={labelClass} htmlFor="maps-label">Label</label>
            <input
              id="maps-label"
              className={inputClass}
              value={label}
              onChange={(event) => setLabel(event.target.value)}
              placeholder="Head office"
            />
          </div>
          <div>
            <label className={labelClass} htmlFor="maps-kind">Kind</label>
            <select
              id="maps-kind"
              className={inputClass}
              value={kind}
              onChange={(event) => setKind(event.target.value)}
            >
              {kinds
                .filter((item) => item.enabled)
                .map((item) => (
                  <option key={item.slug} value={item.slug}>
                    {item.label}
                  </option>
                ))}
            </select>
          </div>
          <div>
            <label className={labelClass} htmlFor="maps-lat">Latitude</label>
            <input
              id="maps-lat"
              className={inputClass}
              inputMode="decimal"
              value={latitude}
              onChange={(event) => setLatitude(event.target.value)}
              placeholder="-6.200000"
            />
          </div>
          <div>
            <label className={labelClass} htmlFor="maps-lng">Longitude</label>
            <input
              id="maps-lng"
              className={inputClass}
              inputMode="decimal"
              value={longitude}
              onChange={(event) => setLongitude(event.target.value)}
              placeholder="106.816666"
            />
          </div>
          <div>
            <label className={labelClass} htmlFor="maps-address">Address</label>
            <input
              id="maps-address"
              className={inputClass}
              value={address}
              onChange={(event) => setAddress(event.target.value)}
              placeholder="Street address (optional)"
            />
          </div>
          <div>
            <label className={labelClass} htmlFor="maps-layer">Layer</label>
            <select
              id="maps-layer"
              className={inputClass}
              value={layerId}
              onChange={(event) => setLayerId(event.target.value)}
            >
              <option value="">No layer</option>
              {layers.map((layer) => (
                <option key={layer.id} value={layer.id}>
                  {layer.name}
                </option>
              ))}
            </select>
          </div>
        </div>
        <div className="mt-3">
          <button type="button" className={primaryButtonClass} onClick={() => void saveLocation()}>
            Save location
          </button>
        </div>
      </Card>

      <Card title={`Locations (${locations.length})`}>
        {locations.length === 0 ? (
          <EmptyState>
            No locations yet — save one above, or click the map to fill its coordinates first.
          </EmptyState>
        ) : (
          <ul className="divide-y divide-border">
            {locations.map((location) => (
              <li key={location.id} className="flex items-center gap-3 py-2">
                <button
                  type="button"
                  className="min-w-0 flex-1 text-left"
                  onClick={() => setSelectedId(location.id)}
                >
                  <span className="block truncate text-sm font-medium">{location.label}</span>
                  <span className="block truncate text-xs text-muted">
                    {location.kind} · {location.latitude.toFixed(4)}, {location.longitude.toFixed(4)}
                  </span>
                </button>
                <Badge>{location.kind}</Badge>
                <button
                  type="button"
                  className={dangerButtonClass}
                  onClick={() =>
                    void run(() => deleteMapLocation(location.id), "Location deleted.")
                  }
                >
                  Delete
                </button>
              </li>
            ))}
          </ul>
        )}
      </Card>

      <Card title="Nearby search">
        <div className="grid gap-3 sm:grid-cols-4">
          <div>
            <label className={labelClass} htmlFor="maps-nearby-lat">Latitude</label>
            <input
              id="maps-nearby-lat"
              className={inputClass}
              inputMode="decimal"
              value={nearbyLat}
              onChange={(event) => setNearbyLat(event.target.value)}
            />
          </div>
          <div>
            <label className={labelClass} htmlFor="maps-nearby-lng">Longitude</label>
            <input
              id="maps-nearby-lng"
              className={inputClass}
              inputMode="decimal"
              value={nearbyLng}
              onChange={(event) => setNearbyLng(event.target.value)}
            />
          </div>
          <div>
            <label className={labelClass} htmlFor="maps-nearby-radius">Radius (km)</label>
            <input
              id="maps-nearby-radius"
              className={inputClass}
              inputMode="decimal"
              value={nearbyRadius}
              onChange={(event) => setNearbyRadius(event.target.value)}
            />
          </div>
          <div className="flex items-end">
            <button type="button" className={primaryButtonClass} onClick={() => void searchNearby()}>
              Search
            </button>
          </div>
        </div>
        {nearbyResults !== null && (
          <ul className="mt-3 divide-y divide-border">
            {nearbyResults.length === 0 && (
              <li className="py-2 text-sm text-muted">Nothing within that radius.</li>
            )}
            {nearbyResults.map((found) => (
              <li key={found.id} className="py-2 text-sm">
                {found.label} · {found.distance_km.toFixed(2)} km
              </li>
            ))}
          </ul>
        )}
      </Card>

      <Card title="Locations of one record">
        <p className="mb-3 text-sm text-muted">
          Look up every location attached to another module&apos;s record
          (a shipment, warehouse, customer, ODP, …) through the generic
          location link.
        </p>
        <div className="grid gap-3 sm:grid-cols-4">
          <div>
            <label className={labelClass} htmlFor="maps-entity-type">Entity type</label>
            <input
              id="maps-entity-type"
              className={inputClass}
              value={entityType}
              onChange={(event) => setEntityType(event.target.value)}
            />
          </div>
          <div>
            <label className={labelClass} htmlFor="maps-entity-id">Entity id</label>
            <input
              id="maps-entity-id"
              className={inputClass}
              value={entityId}
              onChange={(event) => setEntityId(event.target.value)}
              placeholder="UUID of the record"
            />
          </div>
          <div className="flex items-end">
            <button type="button" className={buttonClass} onClick={() => void loadEntityLocations()}>
              Look up
            </button>
          </div>
        </div>
        {entityLocations !== null && (
          <ul className="mt-3 divide-y divide-border">
            {entityLocations.length === 0 && (
              <li className="py-2 text-sm text-muted">No locations attached to this record.</li>
            )}
            {entityLocations.map((found) => (
              <li key={found.id} className="py-2 text-sm">
                {found.label} · role: {found.role}
              </li>
            ))}
          </ul>
        )}
        {selected && (
          <div className="mt-4 border-t border-border pt-3">
            <p className="mb-2 text-sm font-medium">
              Attach the selected location ({selected.label}) to the record above
            </p>
            <div className="flex flex-wrap items-center gap-3">
              <select
                className="rounded-md border border-border bg-surface px-3 py-2 text-sm text-text"
                value={linkRole}
                onChange={(event) => setLinkRole(event.target.value)}
                aria-label="Link role"
              >
                {roles
                  .filter((role) => role.enabled)
                  .map((role) => (
                    <option key={role.slug} value={role.slug}>
                      {role.label}
                    </option>
                  ))}
              </select>
              <button
                type="button"
                className={primaryButtonClass}
                disabled={entityId.trim() === ""}
                onClick={() =>
                  void run(
                    () =>
                      attachMapLocation({
                        entity_type: entityType.trim(),
                        entity_id: entityId.trim(),
                        location_id: selected.id,
                        role: linkRole,
                      }),
                    "Location attached.",
                  )
                }
              >
                Attach
              </button>
            </div>
          </div>
        )}
      </Card>

      <Card title="Layers">
        <div className="mb-3 flex flex-wrap gap-2">
          <input
            className="min-w-40 flex-1 rounded-md border border-border bg-surface px-3 py-2 text-sm text-text"
            value={layerName}
            onChange={(event) => setLayerName(event.target.value)}
            placeholder="New layer name"
            aria-label="New layer name"
          />
          <button
            type="button"
            className={primaryButtonClass}
            onClick={() => {
              if (layerName.trim() === "") return;
              void run(() => createMapLayer({ name: layerName.trim() }), "Layer created.").then(
                (saved) => {
                  if (saved) setLayerName("");
                },
              );
            }}
          >
            Add layer
          </button>
        </div>
        {layers.length === 0 ? (
          <EmptyState>No layers — layers group pins on the map.</EmptyState>
        ) : (
          <ul className="divide-y divide-border">
            {layers.map((layer) => (
              <li key={layer.id} className="flex items-center gap-3 py-2">
                <span className="min-w-0 flex-1 truncate text-sm">{layer.name}</span>
                <Badge>{layer.visible ? "visible" : "hidden"}</Badge>
                <button
                  type="button"
                  className={dangerButtonClass}
                  onClick={() => void run(() => deleteMapLayer(layer.id), "Layer deleted.")}
                >
                  Delete
                </button>
              </li>
            ))}
          </ul>
        )}
      </Card>

      <Card title="Geofences">
        <div className="mb-3 grid gap-3 sm:grid-cols-2">
          <input
            className={inputClass}
            value={geofenceName}
            onChange={(event) => setGeofenceName(event.target.value)}
            placeholder="Zone name"
            aria-label="Geofence name"
          />
          <input
            className={inputClass}
            value={geofenceGeometry}
            onChange={(event) => setGeofenceGeometry(event.target.value)}
            placeholder='GeoJSON Polygon, e.g. {"type":"Polygon","coordinates":[[[106.8,-6.2],[106.9,-6.2],[106.9,-6.1],[106.8,-6.1],[106.8,-6.2]]]}'
            aria-label="Geofence geometry (GeoJSON)"
          />
        </div>
        <button
          type="button"
          className={primaryButtonClass}
          onClick={() => {
            if (geofenceName.trim() === "" || geofenceGeometry.trim() === "") return;
            let geometry: unknown;
            try {
              geometry = JSON.parse(geofenceGeometry) as unknown;
            } catch {
              setError("Geofence geometry must be valid JSON.");
              return;
            }
            void run(
              () => createMapGeofence({ name: geofenceName.trim(), geometry }),
              "Geofence created.",
            ).then((saved) => {
              if (saved) {
                setGeofenceName("");
                setGeofenceGeometry("");
              }
            });
          }}
        >
          Add geofence
        </button>
        {geofences.length === 0 ? (
          <EmptyState>No geofences — a geofence is a named GeoJSON Polygon zone.</EmptyState>
        ) : (
          <ul className="mt-3 divide-y divide-border">
            {geofences.map((fence) => (
              <li key={fence.id} className="flex items-center gap-3 py-2">
                <span className="min-w-0 flex-1 truncate text-sm">{fence.name}</span>
                <button
                  type="button"
                  className={buttonClass}
                  onClick={() => {
                    if (selected === null) {
                      setError("Select a location first to test it against this zone.");
                      return;
                    }
                    setError(null);
                    void testGeofenceContains(fence.id, selected.latitude, selected.longitude)
                      .then((result) =>
                        setNotice(
                          result.contains
                            ? `${selected.label} is inside ${fence.name}.`
                            : `${selected.label} is outside ${fence.name}.`,
                        ),
                      )
                      .catch((caught: unknown) =>
                        setError(caught instanceof Error ? caught.message : String(caught)),
                      );
                  }}
                >
                  Test selected
                </button>
                <button
                  type="button"
                  className={dangerButtonClass}
                  onClick={() => void run(() => deleteMapGeofence(fence.id), "Geofence deleted.")}
                >
                  Delete
                </button>
              </li>
            ))}
          </ul>
        )}
      </Card>

      <Card title="Location kinds">
        <div className="mb-3 flex flex-wrap gap-2">
          <input
            className="min-w-32 flex-1 rounded-md border border-border bg-surface px-3 py-2 text-sm text-text"
            value={kindSlug}
            onChange={(event) => setKindSlug(event.target.value)}
            placeholder="slug, e.g. odp"
            aria-label="Kind slug"
          />
          <input
            className="min-w-32 flex-1 rounded-md border border-border bg-surface px-3 py-2 text-sm text-text"
            value={kindLabel}
            onChange={(event) => setKindLabel(event.target.value)}
            placeholder="Label, e.g. ODP"
            aria-label="Kind label"
          />
          <button
            type="button"
            className={primaryButtonClass}
            onClick={() => {
              if (kindSlug.trim() === "" || kindLabel.trim() === "") return;
              void run(
                () => createMapKind({ slug: kindSlug.trim(), label: kindLabel.trim() }),
                "Kind created.",
              ).then((saved) => {
                if (saved) {
                  setKindSlug("");
                  setKindLabel("");
                }
              });
            }}
          >
            Add kind
          </button>
        </div>
        <ul className="divide-y divide-border">
          {kinds.map((item) => (
            <li key={item.slug} className="flex items-center gap-3 py-2">
              <span
                className="inline-block h-3 w-3 shrink-0 rounded-full"
                style={{ background: item.color }}
              />
              <span className="min-w-0 flex-1 truncate text-sm">
                {item.label} <span className="text-muted">({item.slug})</span>
              </span>
              {item.is_builtin && <Badge>built-in</Badge>}
              {!item.enabled && <Badge>disabled</Badge>}
              {item.id !== null && (
                <button
                  type="button"
                  className={dangerButtonClass}
                  onClick={() => void run(() => deleteMapKind(item.id as string), "Kind deleted.")}
                >
                  Delete
                </button>
              )}
            </li>
          ))}
        </ul>
      </Card>

      <Card title="Link roles">
        <div className="mb-3 flex flex-wrap gap-2">
          <input
            className="min-w-32 flex-1 rounded-md border border-border bg-surface px-3 py-2 text-sm text-text"
            value={roleSlug}
            onChange={(event) => setRoleSlug(event.target.value)}
            placeholder="slug, e.g. site"
            aria-label="Role slug"
          />
          <input
            className="min-w-32 flex-1 rounded-md border border-border bg-surface px-3 py-2 text-sm text-text"
            value={roleLabel}
            onChange={(event) => setRoleLabel(event.target.value)}
            placeholder="Label, e.g. Site"
            aria-label="Role label"
          />
          <button
            type="button"
            className={primaryButtonClass}
            onClick={() => {
              if (roleSlug.trim() === "" || roleLabel.trim() === "") return;
              void run(
                () => createMapRole({ slug: roleSlug.trim(), label: roleLabel.trim() }),
                "Role created.",
              ).then((saved) => {
                if (saved) {
                  setRoleSlug("");
                  setRoleLabel("");
                }
              });
            }}
          >
            Add role
          </button>
        </div>
        <ul className="divide-y divide-border">
          {roles.map((role) => (
            <li key={role.slug} className="flex items-center gap-3 py-2">
              <span className="min-w-0 flex-1 truncate text-sm">
                {role.label} <span className="text-muted">({role.slug})</span>
              </span>
              {role.is_builtin && <Badge>built-in</Badge>}
              {!role.enabled && <Badge>disabled</Badge>}
              {role.id !== null && (
                <button
                  type="button"
                  className={dangerButtonClass}
                  onClick={() => void run(() => deleteMapRole(role.id as string), "Role deleted.")}
                >
                  Delete
                </button>
              )}
            </li>
          ))}
        </ul>
      </Card>

      <Card title="Tile providers & settings">
        <p className="mb-3 text-sm text-muted">
          The map tiles come from the active provider. OpenStreetMap is
          the free default (no key). Mapbox and Google Maps need a
          browser key — store only a referrer-restricted key; it is
          encrypted at rest and never shown again. Any other tile
          server is a custom provider below.
        </p>
        <div className="grid gap-3 sm:grid-cols-2">
          <div>
            <label className={labelClass} htmlFor="maps-provider">Active provider</label>
            <select
              id="maps-provider"
              className={inputClass}
              value={activeProvider}
              onChange={(event) => setActiveProvider(event.target.value)}
            >
              <option value="">Keep current ({settings?.display_name ?? "—"})</option>
              {providers
                .filter((provider) => provider.enabled)
                .map((provider) => (
                  <option key={provider.slug} value={provider.slug}>
                    {provider.display_name}
                    {provider.requires_api_key ? " (needs key)" : ""}
                  </option>
                ))}
            </select>
          </div>
          <div>
            <label className={labelClass} htmlFor="maps-api-key">
              Browser API key {settings?.key_configured ? "(stored — leave blank to keep)" : "(not stored)"}
            </label>
            <input
              id="maps-api-key"
              className={inputClass}
              type="password"
              autoComplete="off"
              value={apiKey}
              onChange={(event) => setApiKey(event.target.value)}
              placeholder="Leave blank to keep the stored key"
            />
          </div>
          <div>
            <label className={labelClass} htmlFor="maps-default-lat">Default latitude</label>
            <input
              id="maps-default-lat"
              className={inputClass}
              inputMode="decimal"
              value={defaultLat}
              onChange={(event) => setDefaultLat(event.target.value)}
              placeholder={settings ? String(settings.default_latitude) : ""}
            />
          </div>
          <div>
            <label className={labelClass} htmlFor="maps-default-lng">Default longitude</label>
            <input
              id="maps-default-lng"
              className={inputClass}
              inputMode="decimal"
              value={defaultLng}
              onChange={(event) => setDefaultLng(event.target.value)}
              placeholder={settings ? String(settings.default_longitude) : ""}
            />
          </div>
          <div>
            <label className={labelClass} htmlFor="maps-default-zoom">Default zoom</label>
            <input
              id="maps-default-zoom"
              className={inputClass}
              inputMode="numeric"
              value={defaultZoom}
              onChange={(event) => setDefaultZoom(event.target.value)}
              placeholder={settings ? String(settings.default_zoom) : ""}
            />
          </div>
        </div>
        <div className="mt-3 flex flex-wrap gap-2">
          <button type="button" className={primaryButtonClass} onClick={() => void saveSettings()}>
            Save settings
          </button>
          {settings?.key_configured && (
            <button
              type="button"
              className={buttonClass}
              onClick={() =>
                void run(() => saveMapSettings({ api_key: "" }), "Stored API key cleared.")
              }
            >
              Clear stored key
            </button>
          )}
        </div>

        <h3 className="mt-6 mb-2 text-sm font-semibold">Providers</h3>
        <ul className="mb-4 divide-y divide-border">
          {providers.map((provider) => (
            <li key={provider.slug} className="flex items-center gap-3 py-2">
              <span className="min-w-0 flex-1 truncate text-sm">
                {provider.display_name} <span className="text-muted">({provider.slug})</span>
              </span>
              {provider.requires_api_key && <Badge>needs key</Badge>}
              {!provider.enabled && <Badge>disabled</Badge>}
              {provider.tenant_id !== null && (
                <button
                  type="button"
                  className={dangerButtonClass}
                  onClick={() =>
                    void run(() => deleteMapProvider(provider.id), "Provider deleted.")
                  }
                >
                  Delete
                </button>
              )}
            </li>
          ))}
        </ul>
        <h3 className="mb-2 text-sm font-semibold">Add a custom provider</h3>
        <div className="grid gap-3 sm:grid-cols-2">
          <input
            className={inputClass}
            value={providerSlug}
            onChange={(event) => setProviderSlug(event.target.value)}
            placeholder="slug, e.g. my-tiles"
            aria-label="Provider slug"
          />
          <input
            className={inputClass}
            value={providerName}
            onChange={(event) => setProviderName(event.target.value)}
            placeholder="Display name"
            aria-label="Provider display name"
          />
          <input
            className={inputClass}
            value={providerTemplate}
            onChange={(event) => setProviderTemplate(event.target.value)}
            placeholder="Tile URL template, e.g. https://tiles.example.com/{z}/{x}/{y}.png"
            aria-label="Tile URL template"
          />
          <input
            className={inputClass}
            value={providerAttribution}
            onChange={(event) => setProviderAttribution(event.target.value)}
            placeholder="Attribution, e.g. © Example Tiles"
            aria-label="Attribution"
          />
        </div>
        <label className="mt-3 flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={providerNeedsKey}
            onChange={(event) => setProviderNeedsKey(event.target.checked)}
          />
          Requires a browser API key (substituted into {"{key}"} in the template)
        </label>
        <div className="mt-3">
          <button
            type="button"
            className={primaryButtonClass}
            onClick={() => {
              if (providerSlug.trim() === "" || providerTemplate.trim() === "") return;
              void run(
                () =>
                  createMapProvider({
                    slug: providerSlug.trim(),
                    display_name:
                      providerName.trim() === "" ? providerSlug.trim() : providerName.trim(),
                    tile_url_template: providerTemplate.trim(),
                    attribution: providerAttribution.trim(),
                    requires_api_key: providerNeedsKey,
                  }),
                "Provider created.",
              ).then((saved) => {
                if (saved) {
                  setProviderSlug("");
                  setProviderName("");
                  setProviderTemplate("");
                  setProviderAttribution("");
                  setProviderNeedsKey(false);
                }
              });
            }}
          >
            Add provider
          </button>
        </div>
      </Card>
    </div>
  );
}

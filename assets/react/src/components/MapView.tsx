// Map view (Map & Tagging, task 358).
//
// Renders the tenant's locations as pins on a Leaflet map.
// Leaflet itself is BUNDLED (the `leaflet` npm dependency —
// its JS and CSS ship inside the React build, so the map
// needs no CDN script). The TILES still come from the
// configured tile provider: the tile URL, attribution, max
// zoom, and subdomains all arrive in `settings` from
// GET /api/v1/maps/settings, so the client never builds a
// provider URL itself. The default provider is OpenStreetMap
// (tile.openstreetmap.org); a tenant that must not depend on
// it points a custom provider row at its own tile server.
//
// Markers use div icons (a coloured dot per location kind),
// never Leaflet's default image markers, so no marker image
// asset can 404 under the bundler. Clicking the map reports
// the coordinates (the page uses it to fill a new location's
// latitude/longitude); clicking a pin selects it.

import { useEffect, useRef } from "react";
import L from "leaflet";
import "leaflet/dist/leaflet.css";
import type { MapLocation, ResolvedMapSettings } from "../api";

export interface MapViewProps {
  /** Resolved settings from GET /api/v1/maps/settings. Null
   * while loading; the map waits for it (no tile URL, no map). */
  settings: ResolvedMapSettings | null;
  /** Pins to render. */
  locations: MapLocation[];
  /** Kind slug -> pin colour (from GET /api/v1/maps/kinds). */
  kindColors?: Record<string, string>;
  /** Selected location id, rendered with a ring. */
  selectedId?: string | null;
  /** Fired when a pin is clicked. */
  onSelect?: (id: string) => void;
  /** Fired when the map itself is clicked. */
  onMapClick?: (latitude: number, longitude: number) => void;
  height?: number;
}

function pinIcon(color: string, selected: boolean): L.DivIcon {
  const size = selected ? 22 : 18;
  return L.divIcon({
    className: "maps-pin",
    html:
      `<span style="display:block;width:${size}px;height:${size}px;` +
      `border-radius:9999px;background:${color};` +
      `border:2px solid #ffffff;` +
      `box-shadow:0 1px 4px rgba(0,0,0,0.45)` +
      `${selected ? ";outline:2px solid #111827;outline-offset:1px" : ""}"></span>`,
    iconSize: [size, size],
    iconAnchor: [size / 2, size / 2],
  });
}

export default function MapView({
  settings,
  locations,
  kindColors = {},
  selectedId = null,
  onSelect,
  onMapClick,
  height = 420,
}: MapViewProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const mapRef = useRef<L.Map | null>(null);
  const markersRef = useRef<L.LayerGroup | null>(null);
  const tileRef = useRef<L.TileLayer | null>(null);
  const selectRef = useRef(onSelect);
  const clickRef = useRef(onMapClick);
  selectRef.current = onSelect;
  clickRef.current = onMapClick;

  // Create the map once the settings (and therefore the tile
  // URL) are known. StrictMode mounts effects twice in
  // development: the cleanup removes the map, so the second
  // mount starts clean instead of reusing a dead instance.
  useEffect(() => {
    const container = containerRef.current;
    if (!container || !settings || !settings.tile_url) return;
    if (mapRef.current) return;
    const map = L.map(container, {
      center: [settings.default_latitude, settings.default_longitude],
      zoom: settings.default_zoom,
      maxZoom: settings.max_zoom,
    });
    const tiles = L.tileLayer(settings.tile_url, {
      attribution: settings.attribution,
      maxZoom: settings.max_zoom,
      subdomains: settings.subdomains.length > 0 ? settings.subdomains : "abc",
    });
    tiles.addTo(map);
    const markers = L.layerGroup();
    markers.addTo(map);
    map.on("click", (event: L.LeafletMouseEvent) => {
      clickRef.current?.(event.latlng.lat, event.latlng.lng);
    });
    mapRef.current = map;
    tileRef.current = tiles;
    markersRef.current = markers;
    return () => {
      map.remove();
      mapRef.current = null;
      tileRef.current = null;
      markersRef.current = null;
    };
  }, [settings]);

  // Keep the tile layer in sync when the provider changes
  // (a settings save swaps the active provider without a
  // reload: the effect above does not re-run, this one does).
  useEffect(() => {
    const map = mapRef.current;
    if (!map || !settings || !settings.tile_url) return;
    if (tileRef.current) {
      tileRef.current.setUrl(settings.tile_url);
      tileRef.current.options.attribution = settings.attribution;
      tileRef.current.options.maxZoom = settings.max_zoom;
    }
    map.setMaxZoom(settings.max_zoom);
  }, [settings]);

  // Keep the pins in sync with the locations list.
  useEffect(() => {
    const markers = markersRef.current;
    if (!markers) return;
    markers.clearLayers();
    for (const location of locations) {
      const color = kindColors[location.kind] ?? "#0b6b3a";
      const marker = L.marker([location.latitude, location.longitude], {
        icon: pinIcon(color, location.id === selectedId),
        title: location.label,
      });
      const address = location.address
        ? `<br>${location.address.replace(/</g, "&lt;")}`
        : "";
      marker.bindPopup(
        `<strong>${location.label.replace(/</g, "&lt;")}</strong>` +
          `<br>${location.kind.replace(/</g, "&lt;")}${address}`,
      );
      marker.on("click", () => {
        selectRef.current?.(location.id);
      });
      marker.addTo(markers);
    }
  }, [locations, kindColors, selectedId]);

  if (!settings) {
    return (
      <div
        className="flex items-center justify-center rounded-lg border border-border bg-surface-muted text-sm text-muted"
        style={{ height }}
      >
        Loading map settings…
      </div>
    );
  }
  if (!settings.tile_url) {
    return (
      <div
        className="flex items-center justify-center rounded-lg border border-border bg-surface-muted p-6 text-sm text-muted"
        style={{ height }}
      >
        <p>
          The active provider ({settings.display_name}) needs an API key and
          none is stored. Add one in the provider settings below to load
          the map tiles.
        </p>
      </div>
    );
  }
  return (
    <div
      ref={containerRef}
      className="z-0 rounded-lg border border-border"
      style={{ height }}
      aria-label="Map"
    />
  );
}

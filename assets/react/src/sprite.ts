// Module icon sprite for the React track.
//
// The Handlebars track embeds the same sprite server-side
// ({{{module_icons_sprite}}} in templates/default/dashboard.hbs,
// built from assets/icons/modules-sprite.svg - see
// src/themes/module_icons.rs). There is no sprite endpoint, but the
// file itself is served as a static asset: GET
// /assets/icons/modules-sprite.svg (src/http/server.rs:390,
// /assets/{path:.*} -> serve_asset). This module fetches it once and
// injects it into the document, so <use href="#module-icon-<id>">
// resolves exactly like in the Handlebars track.
//
// When the sprite cannot be fetched, or a module has no symbol,
// callers fall back to a neutral tile with the module's initials -
// icon art is never invented.

import { useEffect, useState } from "react";

const SPRITE_URL = "/assets/icons/modules-sprite.svg";
const SPRITE_CONTAINER_ID = "module-icons-sprite";

let spritePromise: Promise<boolean> | null = null;

function loadSprite(): Promise<boolean> {
  if (spritePromise) return spritePromise;
  spritePromise = window
    .fetch(SPRITE_URL, { credentials: "include" })
    .then(async (response): Promise<string | null> => {
      if (!response.ok) return null;
      return response.text();
    })
    .then((svg) => {
      if (typeof svg !== "string" || !svg.includes("<symbol")) return false;
      if (document.getElementById(SPRITE_CONTAINER_ID)) return true;
      const container = document.createElement("div");
      container.id = SPRITE_CONTAINER_ID;
      container.setAttribute("aria-hidden", "true");
      container.style.display = "none";
      container.innerHTML = svg;
      document.body.appendChild(container);
      return true;
    })
    .catch(() => false);
  return spritePromise;
}

/** True once the sprite is injected and its symbols are addressable. */
export function useModuleSprite(): boolean {
  const [ready, setReady] = useState(
    () => document.getElementById(SPRITE_CONTAINER_ID) !== null,
  );
  useEffect(() => {
    let cancelled = false;
    void loadSprite().then((loaded) => {
      if (!cancelled) setReady(loaded);
    });
    return () => {
      cancelled = true;
    };
  }, []);
  return ready;
}

/** Only `[a-z0-9-]` names may address the sprite (same rule as module_icon_use). */
export function isValidModuleId(id: string): boolean {
  return id !== "" && id.length <= 64 && /^[a-z0-9-]+$/.test(id);
}

/** True when the injected sprite holds a real symbol for this module. */
export function hasModuleIcon(id: string): boolean {
  return (
    isValidModuleId(id) &&
    document.getElementById(`module-icon-${id}`) !== null
  );
}

/** Initials for the neutral fallback tile, e.g. "service-monitor" -> "SM". */
export function moduleInitials(name: string): string {
  const words = name
    .split(/[^a-zA-Z0-9]+/)
    .filter((word) => word.length > 0);
  if (words.length === 0) return "?";
  const first = words[0].charAt(0).toUpperCase();
  const second = words.length > 1 ? words[1].charAt(0).toUpperCase() : "";
  return first + second;
}

// One module's icon: the real sprite symbol when the sprite holds
// one, otherwise a neutral tile with the module's initials. Icon art
// is never invented; the tile only carries the name.

import { hasModuleIcon, moduleInitials, useModuleSprite } from "../sprite";

export default function ModuleIcon({
  id,
  name,
  size = "md",
}: {
  id: string;
  name: string;
  size?: "sm" | "md" | "lg";
}) {
  const spriteReady = useModuleSprite();
  const sizes = {
    sm: "h-8 w-8 text-xs",
    md: "h-12 w-12 text-base",
    lg: "h-16 w-16 text-xl",
  } as const;
  const box = sizes[size];

  if (spriteReady && hasModuleIcon(id)) {
    return (
      <span
        className={`inline-flex shrink-0 items-center justify-center ${box}`}
        aria-hidden="true"
      >
        <svg className="h-full w-full" aria-hidden="true">
          <use href={`#module-icon-${id}`} />
        </svg>
      </span>
    );
  }

  return (
    <span
      className={`inline-flex shrink-0 items-center justify-center rounded-md bg-surface-muted font-bold text-muted ${box}`}
      aria-hidden="true"
      title={name}
    >
      {moduleInitials(name)}
    </span>
  );
}

// Shared presentational pieces for the React track pages, built on
// Untitled UI React (free tier) tokens. This is the theme's component
// library: pages import from `@theme/components/ui`, never from the
// vendored primitives directly. The props API matches the previous
// hand-rolled `components/ui.tsx` so page migration is mechanical.
//
// Loading, error, and empty states live here so no page ever renders
// a blank area or invents numbers while data is in flight.
//
// NOTE: Untitled UI's free tier has no alerts component (alerts are
// PRO-gated), so `Alert` is hand-built from the theme's utility
// tokens, in the same visual language as the vendored badges.

import type { ReactNode } from "react";
import { Badge as UntitledBadge } from "./base/badges/badges";

export function Card({
  title,
  action,
  children,
}: {
  title?: string;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="rounded-xl border border-primary bg-primary p-5 shadow-xs">
      {title !== undefined && (
        <div className="mb-4 flex items-start justify-between gap-3">
          <h2 className="text-md font-semibold text-primary">{title}</h2>
          {action}
        </div>
      )}
      {children}
    </section>
  );
}

export function StatCard({
  label,
  value,
  sub,
}: {
  label: string;
  value: string;
  sub?: string;
}) {
  return (
    <div className="rounded-xl border border-primary bg-primary p-4 shadow-xs">
      <p className="text-sm text-tertiary">{label}</p>
      <p className="mt-1 text-2xl font-bold text-primary">{value}</p>
      {sub !== undefined && <p className="mt-1 text-xs text-tertiary">{sub}</p>}
    </div>
  );
}

const badgeColors = {
  neutral: "gray",
  success: "success",
  warning: "warning",
  danger: "error",
  info: "blue",
} as const;

export function Badge({
  tone = "neutral",
  children,
}: {
  tone?: "neutral" | "success" | "warning" | "danger" | "info";
  children: ReactNode;
}) {
  return (
    <UntitledBadge type="pill-color" size="md" color={badgeColors[tone]}>
      {children}
    </UntitledBadge>
  );
}

export function Skeleton({ lines = 3 }: { lines?: number }) {
  return (
    <div aria-label="Loading" role="status" className="space-y-2">
      {Array.from({ length: lines }, (_, index) => (
        <div
          key={index}
          className="h-4 animate-pulse rounded bg-secondary"
          style={{ width: `${100 - index * 12}%` }}
        />
      ))}
    </div>
  );
}

export function Alert({
  tone = "error",
  title,
  children,
}: {
  tone?: "error" | "info" | "warning";
  title?: string;
  children: ReactNode;
}) {
  const tones: Record<string, string> = {
    error:
      "border-utility-red-200 bg-utility-red-50 text-utility-red-700",
    info: "border-utility-blue-200 bg-utility-blue-50 text-utility-blue-700",
    warning:
      "border-utility-yellow-200 bg-utility-yellow-50 text-utility-yellow-700",
  };
  return (
    <div role="alert" className={`rounded-xl border p-4 text-sm ${tones[tone]}`}>
      {title !== undefined && <p className="font-semibold">{title}</p>}
      <div className={title !== undefined ? "mt-1" : undefined}>{children}</div>
    </div>
  );
}

export function EmptyState({ children }: { children: ReactNode }) {
  return <p className="text-sm text-tertiary">{children}</p>;
}

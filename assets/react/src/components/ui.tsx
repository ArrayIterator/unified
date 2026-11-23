// Shared presentational pieces for the React track pages.
// Loading, error, and empty states live here so no page ever renders
// a blank area or invents numbers while data is in flight.

import type { ReactNode } from "react";

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
    <section className="rounded-lg border border-border bg-surface p-5 shadow-md">
      {title !== undefined && (
        <div className="mb-4 flex items-start justify-between gap-3">
          <h2 className="text-base font-semibold">{title}</h2>
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
    <div className="rounded-lg border border-border bg-surface p-4 shadow-md">
      <p className="text-sm text-muted">{label}</p>
      <p className="mt-1 text-2xl font-bold">{value}</p>
      {sub !== undefined && <p className="mt-1 text-xs text-faint">{sub}</p>}
    </div>
  );
}

export function Badge({
  tone = "neutral",
  children,
}: {
  tone?: "neutral" | "success" | "warning" | "danger" | "info";
  children: ReactNode;
}) {
  const tones: Record<string, string> = {
    neutral: "bg-surface-muted text-muted",
    success: "bg-success-soft text-success",
    warning: "bg-warning-soft text-warning",
    danger: "bg-danger-soft text-danger",
    info: "bg-info-soft text-info",
  };
  return (
    <span
      className={`inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium ${tones[tone]}`}
    >
      {children}
    </span>
  );
}

export function Skeleton({ lines = 3 }: { lines?: number }) {
  return (
    <div aria-label="Loading" role="status" className="space-y-2">
      {Array.from({ length: lines }, (_, index) => (
        <div
          key={index}
          className="h-4 animate-pulse rounded bg-surface-muted"
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
    error: "border-danger bg-danger-soft text-danger",
    info: "border-info bg-info-soft text-info",
    warning: "border-warning bg-warning-soft text-warning",
  };
  return (
    <div role="alert" className={`rounded-lg border p-4 text-sm ${tones[tone]}`}>
      {title !== undefined && <p className="font-semibold">{title}</p>}
      <div className={title !== undefined ? "mt-1" : undefined}>{children}</div>
    </div>
  );
}

export function EmptyState({ children }: { children: ReactNode }) {
  return <p className="text-sm text-muted">{children}</p>;
}

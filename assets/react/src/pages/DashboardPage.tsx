// Dashboard overview (/react/).
//
// Stat cards come from GET /api/v1/dashboards/user/overview, the
// system widget from GET /api/v1/service-monitor/status, tasks from
// GET /api/v1/tasks, notifications from GET
// /api/v1/notifications, and the module grid from GET
// /api/v1/extensions - the same endpoints the Handlebars dashboard
// uses (assets/js/dashboard.ts). Each widget loads independently and
// degrades to an honest "unavailable" note when its endpoint does
// not answer; nothing here invents a number.

import { Fragment, useMemo, useState, type ReactNode } from "react";
import { Link } from "react-router-dom";
import {
  ApiError,
  fetchAllExtensions,
  fetchNotifications,
  fetchSystemStatus,
  fetchTasks,
  fetchUserOverview,
  fetchWidgetLayout,
  formatBytes,
  formatRelativeTime,
  formatUptime,
  markNotificationRead,
  orderWidgetSections,
  type ExtensionInfo,
  type NotificationItem,
  type TaskItem,
} from "../api";
import { useLoad } from "../hooks";
import { Alert, Badge, Card, EmptyState, Skeleton, StatCard } from "@theme/components/ui";
import { Button, Input } from "@theme/components/controls";
import ModuleIcon from "../components/ModuleIcon";

function errorText(error: Error): string {
  return error instanceof ApiError && error.status === 401
    ? "You are not signed in. Sign in to load this section."
    : error.message;
}

const TASK_STATUS_LABELS: Record<string, string> = {
  todo: "To do",
  in_progress: "In progress",
  review: "In review",
  done: "Done",
  blocked: "Blocked",
  cancelled: "Cancelled",
};

function taskStatusTone(status?: string): "success" | "danger" | "info" | "neutral" {
  if (status === "done") return "success";
  if (status === "blocked") return "danger";
  if (status === "in_progress" || status === "review") return "info";
  return "neutral";
}

function taskPriorityTone(priority?: string): "danger" | "warning" | "info" | "neutral" {
  if (priority === "urgent") return "danger";
  if (priority === "high") return "warning";
  if (priority === "medium") return "info";
  return "neutral";
}

function SystemStatusWidget() {
  const state = useLoad(fetchSystemStatus, []);
  if (state.status === "loading") return <Skeleton lines={4} />;
  if (state.status === "error") {
    return (
      <EmptyState>
        System status is unavailable right now. The service monitor did not
        answer ({errorText(state.error)}).
      </EmptyState>
    );
  }
  const data = state.data;
  const total = Number(data.total_memory_bytes) || 0;
  const used = Number(data.used_memory_bytes) || 0;
  const memoryPercent = total > 0 ? Math.round((used / total) * 100) : 0;
  const cpu = Number(data.cpu_usage_percent) || 0;
  const statusTone =
    data.status === "ok" ? "success" : data.status === "maintenance" || data.status === "paused" ? "warning" : "neutral";
  const statusLabel =
    data.status === "ok"
      ? "Operational"
      : data.status === "maintenance"
        ? "Maintenance"
        : data.status === "paused"
          ? "Paused"
          : data.status || "Unknown";
  return (
    <dl className="grid grid-cols-2 gap-4 sm:grid-cols-4">
      <div>
        <dt className="text-sm text-muted">Status</dt>
        <dd className="mt-1">
          <Badge tone={statusTone}>{statusLabel}</Badge>
        </dd>
      </div>
      <div>
        <dt className="text-sm text-muted">Uptime</dt>
        <dd className="mt-1 font-semibold">{formatUptime(data.uptime_seconds)}</dd>
        <dd className="text-xs text-faint">Version {data.version || "unknown"}</dd>
      </div>
      <div>
        <dt className="text-sm text-muted">CPU</dt>
        <dd className="mt-1 font-semibold">{cpu.toFixed(1)}%</dd>
        <dd className="text-xs text-faint">
          {data.cpu_count || 0} cores - {data.workers || 0} workers
        </dd>
      </div>
      <div>
        <dt className="text-sm text-muted">Memory</dt>
        <dd className="mt-1 font-semibold">
          {formatBytes(used)} / {formatBytes(total)}
        </dd>
        <dd className="mt-2">
          <div
            className="h-2 overflow-hidden rounded-full bg-surface-muted"
            role="img"
            aria-label={`Memory usage ${memoryPercent} percent`}
          >
            <div
              className={`h-full rounded-full ${memoryPercent >= 90 ? "bg-danger" : memoryPercent >= 75 ? "bg-warning" : "bg-accent"}`}
              style={{ width: `${memoryPercent}%` }}
            />
          </div>
        </dd>
      </div>
    </dl>
  );
}

function TasksWidget() {
  const state = useLoad(
    () =>
      fetchTasks().then((tasks) =>
        tasks
          .filter((task) => task.status !== "done" && task.status !== "cancelled")
          .slice(0, 5),
      ),
    [],
  );
  if (state.status === "loading") return <Skeleton lines={4} />;
  if (state.status === "error") {
    return (
      <EmptyState>
        Tasks are unavailable right now. The tasks service did not answer (
        {errorText(state.error)}).
      </EmptyState>
    );
  }
  if (state.data.length === 0) {
    return <EmptyState>No open tasks. New tasks appear here once they are created.</EmptyState>;
  }
  return (
    <ul className="divide-y divide-border">
      {state.data.map((task: TaskItem, index: number) => (
        <li key={task.id ?? `${task.title}-${index}`} className="py-3 first:pt-0 last:pb-0">
          <p className="font-medium">{task.title || "Untitled task"}</p>
          <p className="mt-1 flex flex-wrap items-center gap-2 text-xs text-muted">
            <Badge tone={taskPriorityTone(task.priority)}>
              {task.priority
                ? task.priority.charAt(0).toUpperCase() + task.priority.slice(1)
                : "Medium"}
            </Badge>
            <Badge tone={taskStatusTone(task.status)}>
              {TASK_STATUS_LABELS[task.status ?? ""] || task.status || "Unknown"}
            </Badge>
            {task.overdue === true && <Badge tone="danger">Overdue</Badge>}
            {task.project_name !== undefined && task.project_name !== "" && (
              <span>{task.project_name}</span>
            )}
            {task.due_date !== undefined && task.due_date !== "" && (
              <span>Due {formatRelativeTime(task.due_date)}</span>
            )}
          </p>
        </li>
      ))}
    </ul>
  );
}

function NotificationsWidget() {
  const state = useLoad(fetchNotifications, []);
  const [markingId, setMarkingId] = useState<string | null>(null);
  const [markError, setMarkError] = useState<string | null>(null);

  const handleMarkRead = (id: string) => {
    setMarkingId(id);
    setMarkError(null);
    void markNotificationRead(id)
      .then(() => state.reload())
      .catch((error: unknown) => {
        setMarkError(
          error instanceof Error ? error.message : "Could not mark the notification as read.",
        );
      })
      .finally(() => setMarkingId(null));
  };

  if (state.status === "loading") return <Skeleton lines={4} />;
  if (state.status === "error") {
    return (
      <EmptyState>
        Notifications are unavailable right now. The notifications service did
        not answer ({errorText(state.error)}).
      </EmptyState>
    );
  }
  const notifications: NotificationItem[] = state.data;
  const unread = notifications.filter((item) => !item.read).length;
  if (notifications.length === 0) {
    return <EmptyState>No notifications yet. New notifications appear here as they arrive.</EmptyState>;
  }
  const kindTone = (kind?: string): "info" | "warning" | "danger" | "success" | "neutral" => {
    if (kind === "info") return "info";
    if (kind === "warning") return "warning";
    if (kind === "error") return "danger";
    if (kind === "success") return "success";
    return "neutral";
  };
  return (
    <div>
      {unread > 0 && <p className="mb-3 text-sm text-muted">{unread} unread</p>}
      {markError !== null && (
        <div className="mb-3">
          <Alert tone="error">{markError}</Alert>
        </div>
      )}
      <ul className="divide-y divide-border">
        {notifications.slice(0, 5).map((item) => (
          <li key={item.id} className="py-3 first:pt-0 last:pb-0">
            <div className="flex items-start justify-between gap-3">
              <div className="min-w-0">
                <p className="font-medium">{item.title || "Notification"}</p>
                {item.body !== undefined && item.body !== "" && (
                  <p className="mt-0.5 text-sm text-muted">{item.body}</p>
                )}
                <p className="mt-1 flex flex-wrap items-center gap-2 text-xs text-muted">
                  <Badge tone={kindTone(item.kind)}>{item.kind || "info"}</Badge>
                  {item.created_at !== undefined && item.created_at !== "" && (
                    <span>{formatRelativeTime(item.created_at)}</span>
                  )}
                </p>
              </div>
              {item.read !== true && (
                <Button
                  color="tertiary"
                  size="xs"
                  isDisabled={markingId === item.id}
                  onClick={() => handleMarkRead(item.id)}
                  className="shrink-0"
                >
                  {markingId === item.id ? "Saving..." : "Mark read"}
                </Button>
              )}
            </div>
          </li>
        ))}
      </ul>
    </div>
  );
}

function ModuleGrid() {
  const state = useLoad(fetchAllExtensions, []);
  const [query, setQuery] = useState("");

  const modules = useMemo(() => {
    if (state.status !== "ready") return [] as ExtensionInfo[];
    const needle = query.trim().toLowerCase();
    if (needle === "") return state.data;
    return state.data.filter((module) => {
      const haystack = `${module.id} ${module.name} ${module.description}`.toLowerCase();
      return haystack.includes(needle);
    });
  }, [state, query]);

  return (
    <Card
      title="Modules"
      action={
        state.status === "ready" ? (
          <span className="text-sm text-muted">
            {modules.length === state.data.length
              ? `${state.data.length} registered`
              : `${modules.length} of ${state.data.length}`}
          </span>
        ) : undefined
      }
    >
      <div className="mb-4">
        <Input
          type="search"
          value={query}
          onChange={setQuery}
          placeholder="Search modules by name or description..."
          className="w-full max-w-md"
        />
      </div>
      {state.status === "loading" && (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {Array.from({ length: 6 }, (_, index) => (
            <div key={index} className="rounded-lg border border-border p-4">
              <Skeleton lines={3} />
            </div>
          ))}
        </div>
      )}
      {state.status === "error" && (
        <Alert tone="error" title="Could not load the module list">
          {errorText(state.error)}
          <div className="mt-2">
            <Button color="secondary" size="sm" onClick={state.reload}>
              Retry
            </Button>
          </div>
        </Alert>
      )}
      {state.status === "ready" && state.data.length === 0 && (
        <EmptyState>
          No modules registered yet. Modules appear here once they are registered
          with the application.
        </EmptyState>
      )}
      {state.status === "ready" && state.data.length > 0 && modules.length === 0 && (
        <EmptyState>
          No modules match "{query}". Try a different search.
        </EmptyState>
      )}
      {state.status === "ready" && modules.length > 0 && (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {modules.map((module) => (
            <article
              key={module.id}
              className="flex flex-col rounded-lg border border-border bg-surface p-4"
            >
              <div className="flex items-start gap-3">
                <ModuleIcon id={module.id} name={module.name || module.id} />
                <div className="min-w-0">
                  <h3 className="truncate font-semibold">{module.name || module.id}</h3>
                  <p className="truncate font-mono text-xs text-faint">{module.id}</p>
                </div>
              </div>
              <p className="mt-3 line-clamp-3 flex-1 text-sm text-muted">
                {module.description || "No description."}
              </p>
              <p className="mt-3 flex flex-wrap items-center gap-2 text-xs">
                {module.enabled ? (
                  <Badge tone="success">Enabled</Badge>
                ) : (
                  <Badge tone="neutral">Disabled</Badge>
                )}
                {module.version !== "" && <span className="text-faint">v{module.version}</span>}
                {module.is_core && <Badge tone="info">Core</Badge>}
                {module.warnings.length > 0 && (
                  <Badge tone="warning">{module.warnings.length} warning(s)</Badge>
                )}
              </p>
              <Link
                to={`/modules/${encodeURIComponent(module.id)}`}
                className="mt-3 inline-flex w-fit rounded-md border border-border px-3 py-1 text-sm text-muted hover:bg-surface-hover"
              >
                Open
              </Link>
            </article>
          ))}
        </div>
      )}
    </Card>
  );
}

export default function DashboardPage() {
  const overview = useLoad(fetchUserOverview, []);
  const counts = overview.status === "ready" ? overview.data.counts : undefined;
  // Widget layout (GET /api/v1/widgets/layouts/user-dashboard).
  // A failed load (signed out, endpoint down) keeps the fixed
  // section order below; the layout only reorders, never hides.
  const layout = useLoad(() => fetchWidgetLayout("user-dashboard"), []);

  const sections: { id: string; node: ReactNode }[] = [
    {
      id: "stat-cards",
      node: (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-5">
          {overview.status === "loading" &&
            Array.from({ length: 5 }, (_, index) => (
              <div key={index} className="rounded-lg border border-border bg-surface p-4">
                <Skeleton lines={2} />
              </div>
            ))}
          {counts !== undefined && (
            <>
              <StatCard label="Licenses" value={String(counts.licenses ?? 0)} />
              <StatCard label="Active licenses" value={String(counts.active_licenses ?? 0)} />
              <StatCard label="Orders" value={String(counts.orders ?? 0)} />
              <StatCard label="Entitlements" value={String(counts.entitlements ?? 0)} />
              <StatCard label="Downloads" value={String(counts.downloads ?? 0)} />
            </>
          )}
        </div>
      ),
    },
    {
      id: "system-status",
      node: (
        <Card title="System status">
          <SystemStatusWidget />
        </Card>
      ),
    },
    {
      id: "tasks",
      node: (
        <Card title="Open tasks">
          <TasksWidget />
        </Card>
      ),
    },
    {
      id: "notifications",
      node: (
        <Card title="Notifications">
          <NotificationsWidget />
        </Card>
      ),
    },
    {
      id: "media-library",
      node: (
        <Card
          title="Media library"
          action={
            <Link to="/media" className="text-sm text-accent hover:underline">
              Open media library ->
            </Link>
          }
        >
          <p className="text-sm text-muted">
            Files and folders in the shared documents store. Authors see
            public files plus their own private files; the picker lists
            public files only, so mark a file public before embedding it.
          </p>
        </Card>
      ),
    },
    { id: "module-grid", node: <ModuleGrid /> },
  ];
  const ordered = orderWidgetSections(
    sections,
    layout.status === "ready" ? layout.data : null,
  );

  return (
    <div className="space-y-6">
      {overview.status === "error" && overview.error instanceof ApiError && overview.error.status === 401 && (
        <Alert tone="info" title="You are not signed in">
          <Link to="/login" className="underline">
            Sign in
          </Link>{" "}
          to see your licenses, orders, and downloads. Public sections below
          load without signing in where the server allows it.
        </Alert>
      )}
      {overview.status === "error" &&
        !(overview.error instanceof ApiError && overview.error.status === 401) && (
          <Alert tone="error" title="Could not load your overview">
            {overview.error.message}
          </Alert>
        )}

      {renderSections(ordered)}
    </div>
  );
}

/** Render the ordered sections. System status and open tasks share
 * a two-column row when they end up next to each other (the default
 * layout); apart, each renders full width. Tasks and notifications
 * have no widget in the catalogue, so the layout never moves them
 * away - they always render, after the mapped widgets. */
function renderSections(ordered: { id: string; node: ReactNode }[]): ReactNode[] {
  const nodes: ReactNode[] = [];
  for (let index = 0; index < ordered.length; index += 1) {
    const section = ordered[index];
    const next = ordered[index + 1];
    const pair =
      (section.id === "system-status" && next?.id === "tasks") ||
      (section.id === "tasks" && next?.id === "system-status");
    if (pair && next !== undefined) {
      nodes.push(
        <div key="status-tasks" className="grid gap-6 lg:grid-cols-2">
          {section.node}
          {next.node}
        </div>,
      );
      index += 1;
    } else {
      nodes.push(<Fragment key={section.id}>{section.node}</Fragment>);
    }
  }
  return nodes;
}

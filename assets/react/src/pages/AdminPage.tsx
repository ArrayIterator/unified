// Admin overview (/react/admin).
//
// Counts and system state come from GET
// /api/v1/dashboards/admin/overview, the module table from GET
// /api/v1/dashboards/admin/modules, and the user table from GET
// /api/v1/dashboards/admin/users (src/modules/dashboards/module.rs).
// Sections the React track does not cover yet are listed honestly
// with a link to the matching classic /admin page instead of a
// fabricated panel. Sections mapped to widgets (stat-cards,
// system-status, module-grid) follow the saved widget layout from
// GET /api/v1/widgets/layouts/admin-dashboard; a failed layout load
// keeps the fixed order below.

import { Fragment, type ReactNode } from "react";
import {
  ApiError,
  fetchAdminModules,
  fetchAdminOverview,
  fetchAdminUsers,
  fetchWidgetLayout,
  formatDateTime,
  orderWidgetSections,
} from "../api";
import { useLoad } from "../hooks";
import { Alert, Badge, Card, EmptyState, Skeleton, StatCard } from "../components/ui";

// Classic admin sections with no React page yet. The /admin/{section}
// route exists in the dashboards module (module.rs:386); these links
// open the server-rendered page.
const CLASSIC_SECTIONS = [
  { href: "/admin/settings", label: "Settings", note: "Dashboard runtime settings." },
  { href: "/admin/security", label: "Security", note: "Security checks, hardening, and alerts." },
  { href: "/admin/logs", label: "Logs", note: "System and audit logs." },
  { href: "/admin/servers", label: "Servers", note: "Server inventory and health." },
];

function AdminUsersTable() {
  const state = useLoad(fetchAdminUsers, []);
  if (state.status === "loading") return <Skeleton lines={5} />;
  if (state.status === "error") {
    return (
      <Alert tone="error" title="Could not load users">
        {state.error.message}
      </Alert>
    );
  }
  if (state.data.length === 0) {
    return <EmptyState>No users found.</EmptyState>;
  }
  return (
    <div className="overflow-x-auto">
      <table className="w-full text-left text-sm">
        <thead>
          <tr className="border-b border-border text-muted">
            <th scope="col" className="py-2 pr-3 font-medium">Name</th>
            <th scope="col" className="py-2 pr-3 font-medium">Email</th>
            <th scope="col" className="py-2 pr-3 font-medium">Active</th>
            <th scope="col" className="py-2 pr-3 font-medium">Roles</th>
            <th scope="col" className="py-2 font-medium">Created</th>
          </tr>
        </thead>
        <tbody>
          {state.data.map((user) => (
            <tr key={user.id ?? user.email} className="border-b border-border last:border-0">
              <td className="py-2 pr-3 font-medium">{user.name || "—"}</td>
              <td className="py-2 pr-3">{user.email || "—"}</td>
              <td className="py-2 pr-3">
                {user.is_active ? <Badge tone="success">Yes</Badge> : <Badge tone="neutral">No</Badge>}
              </td>
              <td className="py-2 pr-3 text-muted">{user.roles?.length ?? 0}</td>
              <td className="py-2 text-muted">
                {user.created_at ? formatDateTime(user.created_at) : "—"}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function AdminModulesTable() {
  const state = useLoad(fetchAdminModules, []);
  if (state.status === "loading") return <Skeleton lines={5} />;
  if (state.status === "error") {
    return (
      <Alert tone="error" title="Could not load modules">
        {state.error.message}
      </Alert>
    );
  }
  const modules = state.data.modules ?? [];
  if (modules.length === 0) {
    return <EmptyState>No modules registered.</EmptyState>;
  }
  return (
    <div>
      <div className="overflow-x-auto">
        <table className="w-full text-left text-sm">
          <thead>
            <tr className="border-b border-border text-muted">
              <th scope="col" className="py-2 pr-3 font-medium">Module</th>
              <th scope="col" className="py-2 pr-3 font-medium">Version</th>
              <th scope="col" className="py-2 font-medium">Description</th>
            </tr>
          </thead>
          <tbody>
            {modules.map((module) => (
              <tr key={module.id} className="border-b border-border last:border-0">
                <td className="py-2 pr-3 font-medium">
                  {module.name || module.id}{" "}
                  <span className="font-mono text-xs text-faint">({module.id})</span>
                </td>
                <td className="py-2 pr-3">{module.version || "—"}</td>
                <td className="py-2 text-muted">{module.description || "—"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {state.data.dependencies_valid === false && (
        <p className="mt-3 text-sm text-warning">
          Dependency issues found; see the module detail pages for details.
        </p>
      )}
    </div>
  );
}

export default function AdminPage() {
  const overview = useLoad(fetchAdminOverview, []);
  // Widget layout (GET /api/v1/widgets/layouts/admin-dashboard).
  // A failed load keeps the fixed section order below.
  const layout = useLoad(() => fetchWidgetLayout("admin-dashboard"), []);

  if (
    overview.status === "error" &&
    overview.error instanceof ApiError &&
    (overview.error.status === 401 || overview.error.status === 403)
  ) {
    return (
      <Alert tone="warning" title="Admin access required">
        {overview.error.status === 401 ? (
          <>
            You are not signed in.{" "}
            <a href="/react/login" className="underline">
              Sign in
            </a>{" "}
            with an admin account to open this page.
          </>
        ) : (
          "Your account does not have admin access, so this page is not available to you."
        )}
      </Alert>
    );
  }

  const counts = overview.status === "ready" ? overview.data.counts : undefined;
  const system = overview.status === "ready" ? overview.data.system : undefined;

  const sections: { id: string; node: ReactNode }[] = [
    {
      id: "stat-cards",
      node: (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
          {overview.status === "loading" &&
            Array.from({ length: 4 }, (_, index) => (
              <div key={index} className="rounded-lg border border-border bg-surface p-4">
                <Skeleton lines={2} />
              </div>
            ))}
          {counts !== undefined && (
            <>
              <StatCard label="Users" value={String(counts.users ?? 0)} />
              <StatCard label="Roles" value={String(counts.roles ?? 0)} />
              <StatCard label="Licenses" value={String(counts.licenses ?? 0)} />
              <StatCard label="Orders" value={String(counts.orders ?? 0)} />
            </>
          )}
        </div>
      ),
    },
    {
      id: "system-status",
      node: system !== undefined ? (
        <Card title="System">
          <dl className="grid grid-cols-2 gap-4 sm:grid-cols-3 lg:grid-cols-5">
            <div>
              <dt className="text-sm text-muted">Modules</dt>
              <dd className="mt-1 font-semibold">{system.modules_total ?? 0}</dd>
            </div>
            <div>
              <dt className="text-sm text-muted">Dependencies</dt>
              <dd className="mt-1">
                {system.dependencies_valid === false ? (
                  <Badge tone="warning">Issues found</Badge>
                ) : (
                  <Badge tone="success">Valid</Badge>
                )}
              </dd>
            </div>
            <div>
              <dt className="text-sm text-muted">Database</dt>
              <dd className="mt-1">
                {system.db_available ? (
                  <Badge tone="success">Available</Badge>
                ) : (
                  <Badge tone="warning">Unavailable</Badge>
                )}
              </dd>
            </div>
            <div>
              <dt className="text-sm text-muted">Paused</dt>
              <dd className="mt-1 font-semibold">{system.paused ? "Yes" : "No"}</dd>
            </div>
            <div>
              <dt className="text-sm text-muted">Maintenance</dt>
              <dd className="mt-1 font-semibold">{system.maintenance ? "Yes" : "No"}</dd>
            </div>
          </dl>
        </Card>
      ) : null,
    },
    {
      id: "module-grid",
      node: (
        <Card title="Modules">
          <AdminModulesTable />
        </Card>
      ),
    },
    {
      id: "users",
      node: (
        <Card title="Users">
          <AdminUsersTable />
        </Card>
      ),
    },
    {
      id: "classic",
      node: (
        <Card title="More in the classic dashboard">
          <p className="mb-3 text-sm text-muted">
            These admin sections are not available in the React track yet. They
            open in the classic server-rendered dashboard.
          </p>
          <ul className="divide-y divide-border">
            {CLASSIC_SECTIONS.map((section) => (
              <li key={section.href} className="py-3 first:pt-0 last:pb-0">
                <a href={section.href} className="font-medium text-accent hover:underline">
                  {section.label}
                </a>
                <p className="text-sm text-muted">{section.note}</p>
              </li>
            ))}
          </ul>
        </Card>
      ),
    },
  ];
  const ordered = orderWidgetSections(
    sections,
    layout.status === "ready" ? layout.data : null,
  );

  return (
    <div className="space-y-6">
      {overview.status === "error" && (
        <Alert tone="error" title="Could not load the admin overview">
          {overview.error.message}
        </Alert>
      )}

      {ordered.map((section) => (
        <Fragment key={section.id}>{section.node}</Fragment>
      ))}
    </div>
  );
}

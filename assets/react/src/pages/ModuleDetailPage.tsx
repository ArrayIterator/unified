// Module detail page (/react/modules/:id).
//
// The detail endpoint is GET /api/v1/extensions/{id}
// (src/modules/extensions/module.rs:91); it returns the same
// ExtensionInfo shape as the list. Enable/disable use the matching
// POST endpoints (module.rs:95-96) and require the modules:manage
// capability - a forbidden answer is shown honestly instead of
// pretending the change worked.

import { useState } from "react";
import { Link, useParams } from "react-router-dom";
import {
  ApiError,
  disableExtension,
  enableExtension,
  fetchExtension,
} from "../api";
import { useLoad } from "../hooks";
import { Alert, Badge, Card, EmptyState, Skeleton } from "@theme/components/ui";
import { Button } from "@theme/components/controls";
import ModuleIcon from "../components/ModuleIcon";

export default function ModuleDetailPage() {
  const { id } = useParams<{ id: string }>();
  const decodedId = id === undefined ? "" : decodeURIComponent(id);
  const state = useLoad(() => fetchExtension(decodedId), [decodedId]);
  const [mutating, setMutating] = useState(false);
  const [notice, setNotice] = useState<{ tone: "success" | "error"; text: string } | null>(null);

  if (decodedId === "") {
    return <Alert tone="error">No module id was given in the URL.</Alert>;
  }

  if (state.status === "loading") {
    return (
      <Card title="Module">
        <Skeleton lines={6} />
      </Card>
    );
  }

  if (state.status === "error") {
    if (state.error instanceof ApiError && state.error.status === 404) {
      return (
        <Alert tone="warning" title="Module not found">
          No module with id "{decodedId}" is registered.{" "}
          <Link to="/" className="underline">
            Back to the dashboard
          </Link>
          .
        </Alert>
      );
    }
    return (
      <Alert tone="error" title="Could not load this module">
        {state.error.message}
      </Alert>
    );
  }

  const module = state.data;

  const handleToggle = (enable: boolean) => {
    setMutating(true);
    setNotice(null);
    const action = enable ? enableExtension : disableExtension;
    void action(module.id)
      .then(() => {
        setNotice({
          tone: "success",
          text: enable
            ? `Module "${module.name || module.id}" was enabled.`
            : `Module "${module.name || module.id}" was disabled.`,
        });
        state.reload();
      })
      .catch((error: unknown) => {
        setNotice({
          tone: "error",
          text: error instanceof Error ? error.message : "The change did not go through.",
        });
      })
      .finally(() => setMutating(false));
  };

  return (
    <div className="space-y-6">
      <Card>
        <div className="flex flex-wrap items-start gap-4">
          <ModuleIcon id={module.id} name={module.name || module.id} size="lg" />
          <div className="min-w-0 flex-1">
            <h2 className="text-xl font-bold">{module.name || module.id}</h2>
            <p className="font-mono text-sm text-faint">{module.id}</p>
            <p className="mt-2 flex flex-wrap items-center gap-2 text-sm">
              {module.enabled ? <Badge tone="success">Enabled</Badge> : <Badge tone="neutral">Disabled</Badge>}
              {module.version !== "" && <span className="text-muted">v{module.version}</span>}
              {module.is_core && <Badge tone="info">Core module</Badge>}
            </p>
          </div>
          <Button
            color="secondary"
            isDisabled={mutating}
            onClick={() => handleToggle(!module.enabled)}
          >
            {mutating ? "Saving..." : module.enabled ? "Disable" : "Enable"}
          </Button>
        </div>
        <p className="mt-4 text-muted">{module.description || "No description."}</p>
        {notice !== null && (
          <div className="mt-4">
            <Alert tone={notice.tone === "success" ? "info" : "error"}>{notice.text}</Alert>
          </div>
        )}
      </Card>

      {module.warnings.length > 0 && (
        <Card title="Warnings">
          <ul className="space-y-2">
            {module.warnings.map((warning, index) => (
              <li key={`${warning.code}-${index}`} className="text-sm">
                <Badge tone="warning">{warning.code}</Badge>{" "}
                <span className="text-muted">{warning.message}</span>
                {warning.required_modules.length > 0 && (
                  <span className="block pl-1 pt-1 text-xs text-faint">
                    Requires: {warning.required_modules.join(", ")}
                    {warning.will_auto_activate.length > 0 &&
                      ` - enabling this module would also enable: ${warning.will_auto_activate.join(", ")}`}
                  </span>
                )}
              </li>
            ))}
          </ul>
        </Card>
      )}

      <Card title="Requirements">
        {module.requires.length === 0 && module.dependencies.length === 0 ? (
          <EmptyState>This module declares no requirements.</EmptyState>
        ) : (
          <ul className="divide-y divide-border">
            {module.requires.map((required) => (
              <li key={required.id} className="flex items-center justify-between gap-3 py-2 first:pt-0 last:pb-0">
                <span className="font-medium">
                  <Link
                    to={`/modules/${encodeURIComponent(required.id)}`}
                    className="text-accent hover:underline"
                  >
                    {required.name || required.id}
                  </Link>{" "}
                  <span className="font-mono text-xs text-faint">({required.id})</span>
                </span>
                <Badge
                  tone={
                    required.status === "enabled"
                      ? "success"
                      : required.status === "missing"
                        ? "danger"
                        : "warning"
                  }
                >
                  {required.status}
                </Badge>
              </li>
            ))}
          </ul>
        )}
        {module.missing_required.length > 0 && (
          <p className="mt-3 text-sm text-danger">
            Missing required modules: {module.missing_required.join(", ")}
          </p>
        )}
      </Card>

      <Card title="External libraries">
        {module.requires_external_libraries.length === 0 &&
        module.requires_libraries.length === 0 ? (
          <EmptyState>This module needs no external libraries.</EmptyState>
        ) : (
          <div className="space-y-2 text-sm text-muted">
            {module.requires_libraries.map((library) => (
              <p key={library}>Requires library: {library}</p>
            ))}
            {module.library_notices.length > 0 && (
              <ul className="space-y-1">
                {module.library_notices.map((notice, index) => (
                  <li key={index}>
                    <Badge tone="warning">Notice</Badge>{" "}
                    {notice.message || notice.library || "Library check reported an issue."}
                  </li>
                ))}
              </ul>
            )}
          </div>
        )}
      </Card>

      <p>
        <Link to="/" className="text-sm text-accent hover:underline">
          <- Back to all modules
        </Link>
      </p>
    </div>
  );
}

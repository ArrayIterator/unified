// File explorer + media manager core: folder tree, file grid/list,
// search, type/visibility filters, drag-drop multi-upload with real
// per-file progress, and the per-item actions (copy URL, rename,
// move, visibility flip, delete with confirm).
//
// One component serves two modes over the same data:
//   - "manage" - the full media manager (`GET /api/v1/documents`,
//     role-scoped by the server: admins see public + private, author
//     roles see public + their own private rows; another owner's
//     private document never reaches this UI).
//   - "pick" - the embed picker (`GET /api/v1/documents/picker`,
//     public-only for every role: content embeds a public URL, and
//     a private file would 404 for readers, so it never appears
//     here until it is flipped public).
// All data comes from fetch calls - zero full page reloads. The
// server enforces every permission (owner-or-admin for rename /
// move / flip / delete, scan gate for making a file public); the
// UI hides actions it knows the caller may not use, but never
// relies on that hiding.

import { useCallback, useEffect, useRef, useState } from "react";
import {
  createFolder,
  deleteDocument,
  deleteFolder,
  documentUrl,
  fetchDocuments,
  fetchFolderTree,
  fetchPickerItems,
  flipDocumentVisibility,
  formatBytes,
  formatRelativeTime,
  itemsFromData,
  moveDocument,
  moveFolder,
  renameDocument,
  renameFolder,
  uploadDocuments,
  type DocumentFolderTree,
  type DocumentItem,
  type PickerItem,
  type UploadProgress,
} from "../api";
import { Alert, Badge, EmptyState, Skeleton } from "@theme/components/ui";
import { Button, Dialog, Input, Select } from "@theme/components/controls";

/** One row in the explorer, adapted from either list source. */
export interface ExplorerItem {
  id: string;
  name: string;
  mime: string;
  size: number;
  visibility: string;
  folder_id: string;
  storage_key: string;
  url: string;
  created_at: string;
  /** Empty in pick mode (the picker payload carries no scan state). */
  scan_status: string;
  /** Empty in pick mode. */
  owner_id: string;
}

function fromDocument(document: DocumentItem): ExplorerItem {
  return {
    id: document.id,
    name: document.name,
    mime: document.mime,
    size: document.size,
    visibility: document.visibility,
    folder_id: document.folder_id ?? "",
    storage_key: document.storage_key,
    url: documentUrl(document),
    created_at: document.created_at,
    scan_status: document.scan_status,
    owner_id: document.owner_id,
  };
}

function fromPickerItem(item: PickerItem): ExplorerItem {
  return {
    id: item.document_id,
    name: item.name,
    mime: item.mime,
    size: item.size,
    visibility: item.visibility,
    folder_id: "",
    storage_key: item.storage_key,
    url: item.url,
    created_at: item.created_at,
    scan_status: "",
    owner_id: "",
  };
}

const TYPE_OPTIONS = [
  { value: "", label: "All types" },
  { value: "image", label: "Images" },
  { value: "video", label: "Videos" },
  { value: "audio", label: "Audio" },
  { value: "document", label: "Documents" },
  { value: "archive", label: "Archives" },
  { value: "other", label: "Other" },
] as const;

interface FlatFolder {
  id: string;
  name: string;
  depth: number;
}

function flattenTree(
  nodes: DocumentFolderTree[],
  depth = 0,
  out: FlatFolder[] = [],
): FlatFolder[] {
  for (const node of nodes) {
    out.push({ id: node.folder.id, name: node.folder.name, depth });
    flattenTree(node.children, depth + 1, out);
  }
  return out;
}

function folderName(
  nodes: DocumentFolderTree[],
  folderId: string,
): string | null {
  for (const node of nodes) {
    if (node.folder.id === folderId) return node.folder.name;
    const nested = folderName(node.children, folderId);
    if (nested !== null) return nested;
  }
  return null;
}

export interface FileExplorerProps {
  mode?: "manage" | "pick";
  /** Pick mode: allow selecting several files before attaching. */
  multiple?: boolean;
  /** Pick mode: called with the selected files. */
  onPick?: (items: ExplorerItem[]) => void;
  /** Pick mode: close the picker dialog. */
  onClose?: () => void;
}

export default function FileExplorer({
  mode = "manage",
  multiple = false,
  onPick,
  onClose,
}: FileExplorerProps) {
  const isPick = mode === "pick";
  const [tree, setTree] = useState<DocumentFolderTree[]>([]);
  const [treeError, setTreeError] = useState<string | null>(null);
  const [currentFolderId, setCurrentFolderId] = useState("");
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [items, setItems] = useState<ExplorerItem[]>([]);
  const [totalPages, setTotalPages] = useState(1);
  const [total, setTotal] = useState(0);
  const [page, setPage] = useState(1);
  const [search, setSearch] = useState("");
  const [typeFilter, setTypeFilter] = useState("");
  const [visibilityFilter, setVisibilityFilter] = useState("");
  const [view, setView] = useState<"grid" | "list">("grid");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [nonce, setNonce] = useState(0);
  const [dragOver, setDragOver] = useState(false);
  const [uploadVisibility, setUploadVisibility] = useState<"public" | "private">(
    isPick ? "public" : "private",
  );
  const [uploadProgress, setUploadProgress] = useState<UploadProgress[]>([]);
  const [uploading, setUploading] = useState(false);
  const [showNewFolder, setShowNewFolder] = useState(false);
  const [newFolderName, setNewFolderName] = useState("");
  const [renameTarget, setRenameTarget] = useState<{
    kind: "document" | "folder";
    id: string;
    name: string;
  } | null>(null);
  const [renameValue, setRenameValue] = useState("");
  const [moveTarget, setMoveTarget] = useState<{
    kind: "document" | "folder";
    id: string;
    name: string;
  } | null>(null);
  const [moveDestination, setMoveDestination] = useState("");
  const [deleteTarget, setDeleteTarget] = useState<{
    kind: "document" | "folder";
    id: string;
    name: string;
  } | null>(null);
  const [busy, setBusy] = useState(false);
  const [justUploaded, setJustUploaded] = useState<ExplorerItem[]>([]);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const reload = useCallback(() => setNonce((value) => value + 1), []);

  // Folder tree (both modes): the picker navigates the same tree;
  // its list endpoint filters by folder server-side.
  useEffect(() => {
    let cancelled = false;
    fetchFolderTree()
      .then((nodes) => {
        if (!cancelled) {
          setTree(nodes);
          setTreeError(null);
        }
      })
      .catch((caught: unknown) => {
        if (!cancelled) {
          setTreeError(
            caught instanceof Error ? caught.message : "Could not load folders.",
          );
        }
      });
    return () => {
      cancelled = true;
    };
  }, [nonce]);

  // File list. A search spans the whole library (the folder filter
  // is dropped while searching); otherwise the current folder
  // scopes the list.
  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    const searching = search.trim() !== "";
    const load = isPick
      ? fetchPickerItems({
          search: searching ? search.trim() : undefined,
          type: typeFilter || undefined,
          folder_id: searching ? undefined : currentFolderId,
          page,
        }).then((data) => ({
          items: itemsFromData<PickerItem>(data).map(fromPickerItem),
          total: data.pagination?.total ?? 0,
          totalPages: data.pagination?.total_pages ?? 1,
        }))
      : fetchDocuments({
          search: searching ? search.trim() : undefined,
          type: typeFilter || undefined,
          visibility: visibilityFilter || undefined,
          folder_id: searching ? undefined : currentFolderId,
          page,
        }).then((data) => ({
          items: itemsFromData<DocumentItem>(data).map(fromDocument),
          total: data.pagination?.total ?? 0,
          totalPages: data.pagination?.total_pages ?? 1,
        }));
    void load
      .then((result) => {
        if (!cancelled) {
          setItems(result.items);
          setTotal(result.total);
          setTotalPages(result.totalPages);
          setLoading(false);
        }
      })
      .catch((caught: unknown) => {
        if (!cancelled) {
          setError(
            caught instanceof Error ? caught.message : "Could not load files.",
          );
          setLoading(false);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [currentFolderId, page, search, typeFilter, visibilityFilter, isPick, nonce]);

  // A new search starts back at page one.
  useEffect(() => {
    setPage(1);
  }, [search, typeFilter, visibilityFilter, currentFolderId]);

  const openFolder = (folderId: string) => {
    setCurrentFolderId(folderId);
    setSelected(new Set());
  };

  const toggleExpanded = (folderId: string) => {
    setExpanded((previous) => {
      const next = new Set(previous);
      if (next.has(folderId)) next.delete(folderId);
      else next.add(folderId);
      return next;
    });
  };

  const toggleSelected = (item: ExplorerItem) => {
    setSelected((previous) => {
      const next = new Set(previous);
      if (multiple) {
        if (next.has(item.id)) next.delete(item.id);
        else next.add(item.id);
      } else {
        next.clear();
        next.add(item.id);
      }
      return next;
    });
  };

  const attachSelected = () => {
    if (!onPick) return;
    const picked = items.filter((item) => selected.has(item.id));
    if (picked.length > 0) onPick(picked);
  };

  const runUpload = async (files: File[]) => {
    if (files.length === 0 || uploading) return;
    setUploading(true);
    setUploadProgress([]);
    setNotice(null);
    setError(null);
    try {
      const outcome = await uploadDocuments(files, {
        visibility: uploadVisibility,
        folderId: currentFolderId,
        onProgress: (progress) => {
          setUploadProgress((previous) => {
            const next = [...previous];
            next[progress.fileIndex] = progress;
            return next;
          });
        },
      });
      const failed = outcome.failed;
      if (failed.length > 0) {
        setError(
          failed.map((entry) => `${entry.name}: ${entry.error}`).join("; "),
        );
      }
      if (outcome.uploaded.length > 0) {
        setNotice(
          `${outcome.uploaded.length} file${outcome.uploaded.length === 1 ? "" : "s"} uploaded.`,
        );
        // In pick mode a private upload stays invisible in the
        // picker list until it is public: surface the new files
        // here with a "Make public" action so the author can flip
        // them without hunting through the manager.
        setJustUploaded(outcome.uploaded.map(fromDocument));
        reload();
      }
    } catch (caught: unknown) {
      setError(caught instanceof Error ? caught.message : "Upload failed.");
    } finally {
      setUploading(false);
      setUploadProgress([]);
    }
  };

  const handleFiles = (list: FileList | null) => {
    if (!list) return;
    void runUpload(Array.from(list));
  };

  const runAction = async (work: () => Promise<unknown>, done: string) => {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      await work();
      setNotice(done);
      reload();
    } catch (caught: unknown) {
      setError(caught instanceof Error ? caught.message : "That action failed.");
    } finally {
      setBusy(false);
    }
  };

  const copyUrl = async (item: ExplorerItem) => {
    try {
      await navigator.clipboard.writeText(item.url);
      setNotice(`Copied ${item.url}`);
    } catch {
      setError("Could not copy the URL (clipboard unavailable).");
    }
  };

  const flipItem = (item: ExplorerItem) =>
    runAction(
      () =>
        flipDocumentVisibility(
          item.id,
          item.visibility === "public" ? "private" : "public",
        ),
      item.visibility === "public"
        ? `"${item.name}" is now private.`
        : `"${item.name}" is now public.`,
    );

  const flipUploaded = (item: ExplorerItem) =>
    runAction(
      () => flipDocumentVisibility(item.id, "public"),
      `"${item.name}" is now public and appears in the picker.`,
    );

  const submitRename = () => {
    if (!renameTarget || !renameValue.trim()) return;
    const target = renameTarget;
    const name = renameValue.trim();
    setRenameTarget(null);
    void runAction(
      () =>
        target.kind === "document"
          ? renameDocument(target.id, name)
          : renameFolder(target.id, name),
      `Renamed to "${name}".`,
    );
  };

  const submitMove = () => {
    if (!moveTarget) return;
    const target = moveTarget;
    const destination = moveDestination;
    setMoveTarget(null);
    void runAction(
      () =>
        target.kind === "document"
          ? moveDocument(target.id, destination)
          : moveFolder(target.id, destination),
      destination === ""
        ? `Moved "${target.name}" to the library root.`
        : `Moved "${target.name}".`,
    );
  };

  const submitDelete = () => {
    if (!deleteTarget) return;
    const target = deleteTarget;
    setDeleteTarget(null);
    void runAction(
      () =>
        target.kind === "document"
          ? deleteDocument(target.id)
          : deleteFolder(target.id),
      `Deleted "${target.name}".`,
    );
  };

  const submitNewFolder = () => {
    const name = newFolderName.trim();
    if (!name) return;
    setShowNewFolder(false);
    setNewFolderName("");
    void runAction(
      () => createFolder(name, currentFolderId),
      `Folder "${name}" created.`,
    );
  };

  const flatFolders = flattenTree(tree);
  const currentFolderName =
    currentFolderId === "" ? null : folderName(tree, currentFolderId);

  return (
    <div className="space-y-4">
      {error !== null && (
        <Alert tone="error" title="Error">
          {error}
        </Alert>
      )}
      {notice !== null && (
        <Alert tone="info" title="Done">
          {notice}
        </Alert>
      )}

      {/* Toolbar: search, filters, view toggle. */}
      <div className="flex flex-wrap items-center gap-2">
        <Input
          type="search"
          value={search}
          onChange={setSearch}
          placeholder="Search by name"
          className="min-w-40 flex-1"
        />
        <Select
          ariaLabel="Filter by type"
          items={TYPE_OPTIONS.map((option) => ({ id: option.value, label: option.label }))}
          selectedKey={typeFilter}
          onSelectionChange={setTypeFilter}
        />
        {!isPick && (
          <Select
            ariaLabel="Filter by visibility"
            items={[
              { id: "", label: "Public + private" },
              { id: "public", label: "Public only" },
              { id: "private", label: "Private only" },
            ]}
            selectedKey={visibilityFilter}
            onSelectionChange={setVisibilityFilter}
          />
        )}
        <div className="flex rounded-md border border-border" role="group" aria-label="View">
          <button
            type="button"
            onClick={() => setView("grid")}
            aria-pressed={view === "grid"}
            className={`rounded-l-md px-3 py-2 text-sm ${
              view === "grid" ? "bg-accent-soft text-accent" : "text-muted"
            }`}
          >
            Grid
          </button>
          <button
            type="button"
            onClick={() => setView("list")}
            aria-pressed={view === "list"}
            className={`rounded-r-md px-3 py-2 text-sm ${
              view === "list" ? "bg-accent-soft text-accent" : "text-muted"
            }`}
          >
            List
          </button>
        </div>
      </div>
      {search.trim() !== "" && (
        <p className="text-xs text-faint">
          Searching the whole library - the folder filter is off while searching.
        </p>
      )}

      <div className="grid gap-4 lg:grid-cols-[240px_1fr]">
        {/* Folder tree. */}
        <div className="rounded-lg border border-border bg-surface p-3">
          <div className="mb-2 flex items-center justify-between gap-2">
            <h3 className="text-sm font-semibold">Folders</h3>
            {!isPick && (
              <Button color="secondary" size="xs" onClick={() => setShowNewFolder(true)}>
                + New folder
              </Button>
            )}
          </div>
          {treeError !== null && (
            <p className="text-xs text-danger">{treeError}</p>
          )}
          <ul className="space-y-1 text-sm">
            <li>
              <button
                type="button"
                onClick={() => openFolder("")}
                className={`w-full rounded-md px-2 py-1 text-left ${
                  currentFolderId === ""
                    ? "bg-accent-soft font-medium text-accent"
                    : "text-muted hover:bg-surface-hover"
                }`}
              >
                All files (library root)
              </button>
            </li>
            {tree.map((node) => (
              <FolderNode
                key={node.folder.id}
                node={node}
                depth={0}
                currentFolderId={currentFolderId}
                expanded={expanded}
                onOpen={openFolder}
                onToggle={toggleExpanded}
                onRename={
                  isPick
                    ? undefined
                    : (id, name) => {
                        setRenameTarget({ kind: "folder", id, name });
                        setRenameValue(name);
                      }
                }
                onMove={
                  isPick
                    ? undefined
                    : (id, name) => {
                        setMoveTarget({ kind: "folder", id, name });
                        setMoveDestination("");
                      }
                }
                onDelete={
                  isPick
                    ? undefined
                    : (id, name) => setDeleteTarget({ kind: "folder", id, name })
                }
              />
            ))}
          </ul>
          {showNewFolder && (
            <form
              className="mt-3 space-y-2 border-t border-border pt-3"
              onSubmit={(event) => {
                event.preventDefault();
                submitNewFolder();
              }}
            >
              <Input
                label={`New folder${currentFolderName ? ` in "${currentFolderName}"` : ""}:`}
                value={newFolderName}
                onChange={setNewFolderName}
                placeholder="Folder name"
                maxLength={128}
                autoFocus
              />
              <div className="flex gap-2">
                <Button
                  color="primary"
                  size="xs"
                  isDisabled={busy || newFolderName.trim() === ""}
                  onClick={submitNewFolder}
                >
                  Create
                </Button>
                <Button color="secondary" size="xs" onClick={() => setShowNewFolder(false)}>
                  Cancel
                </Button>
              </div>
            </form>
          )}
        </div>

        {/* Files. */}
        <div>
          <p className="mb-2 text-sm text-muted" aria-live="polite">
            {currentFolderName === null
              ? "Library root"
              : `Folder: ${currentFolderName}`}{" "}
            - {total} file{total === 1 ? "" : "s"}
            {isPick && " - public files only"}
          </p>

          {loading ? (
            <Skeleton lines={6} />
          ) : items.length === 0 ? (
            <EmptyState>
              {isPick
                ? "No public files here yet. Upload one below - it appears once it is public."
                : "No files here yet. Drop files below to upload the first one."}
            </EmptyState>
          ) : view === "grid" ? (
            <ul className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
              {items.map((item) => (
                <FileCard
                  key={item.id}
                  item={item}
                  isPick={isPick}
                  selected={selected.has(item.id)}
                  busy={busy}
                  onSelect={() => toggleSelected(item)}
                  onCopy={() => void copyUrl(item)}
                  onFlip={() => flipItem(item)}
                  onRename={() => {
                    setRenameTarget({ kind: "document", id: item.id, name: item.name });
                    setRenameValue(item.name);
                  }}
                  onMove={() => {
                    setMoveTarget({ kind: "document", id: item.id, name: item.name });
                    setMoveDestination(item.folder_id);
                  }}
                  onDelete={() =>
                    setDeleteTarget({ kind: "document", id: item.id, name: item.name })
                  }
                />
              ))}
            </ul>
          ) : (
            <ul className="divide-y divide-border rounded-lg border border-border bg-surface">
              {items.map((item) => (
                <FileRow
                  key={item.id}
                  item={item}
                  isPick={isPick}
                  selected={selected.has(item.id)}
                  busy={busy}
                  onSelect={() => toggleSelected(item)}
                  onCopy={() => void copyUrl(item)}
                  onFlip={() => flipItem(item)}
                  onRename={() => {
                    setRenameTarget({ kind: "document", id: item.id, name: item.name });
                    setRenameValue(item.name);
                  }}
                  onMove={() => {
                    setMoveTarget({ kind: "document", id: item.id, name: item.name });
                    setMoveDestination(item.folder_id);
                  }}
                  onDelete={() =>
                    setDeleteTarget({ kind: "document", id: item.id, name: item.name })
                  }
                />
              ))}
            </ul>
          )}

          {/* Pagination. */}
          {totalPages > 1 && (
            <div className="mt-3 flex items-center gap-3 text-sm">
              <Button
                color="secondary"
                size="sm"
                isDisabled={page <= 1}
                onClick={() => setPage((value) => Math.max(1, value - 1))}
              >
                <- Previous
              </Button>
              <span className="text-muted">
                Page {page} of {totalPages}
              </span>
              <Button
                color="secondary"
                size="sm"
                isDisabled={page >= totalPages}
                onClick={() => setPage((value) => Math.min(totalPages, value + 1))}
              >
                Next ->
              </Button>
            </div>
          )}

          {/* Pick-mode attach bar. */}
          {isPick && (
            <div className="mt-4 flex flex-wrap items-center gap-2">
              <Button
                color="primary"
                size="sm"
                isDisabled={selected.size === 0}
                onClick={attachSelected}
              >
                Attach selected ({selected.size})
              </Button>
              {onClose !== undefined && (
                <Button color="secondary" size="sm" onClick={onClose}>
                  Cancel
                </Button>
              )}
            </div>
          )}

          {/* Upload zone: drag-drop multi-upload with real progress. */}
          <div
            className={`mt-4 rounded-lg border-2 border-dashed p-4 text-center ${
              dragOver ? "border-accent bg-accent-soft" : "border-border"
            }`}
            onDragOver={(event) => {
              event.preventDefault();
              setDragOver(true);
            }}
            onDragLeave={() => setDragOver(false)}
            onDrop={(event) => {
              event.preventDefault();
              setDragOver(false);
              handleFiles(event.dataTransfer.files);
            }}
          >
            <p className="text-sm font-medium">
              Drop files here to upload
              {currentFolderName ? ` into "${currentFolderName}"` : ""} - or{" "}
              <button
                type="button"
                onClick={() => fileInputRef.current?.click()}
                className="text-accent underline"
              >
                choose files
              </button>
            </p>
            <p className="mt-1 text-xs text-muted">
              Files start {uploadVisibility}. A public file appears in the
              picker only when its virus scan is clean.
            </p>
            <span className="mt-2 inline-flex items-center gap-2 text-xs text-muted">
              Upload as
              <Select
                ariaLabel="Upload as"
                items={[
                  { id: "private", label: "Private" },
                  { id: "public", label: "Public" },
                ]}
                selectedKey={uploadVisibility}
                onSelectionChange={(key) =>
                  setUploadVisibility(key as "public" | "private")
                }
              />
            </span>
            <input
              ref={fileInputRef}
              type="file"
              multiple
              className="hidden"
              onChange={(event) => {
                handleFiles(event.target.files);
                event.target.value = "";
              }}
            />
            {uploading && uploadProgress.length > 0 && (
              <ul className="mx-auto mt-3 max-w-md space-y-2 text-left">
                {uploadProgress.map((progress, index) => (
                  <li key={`${progress.fileName}-${index}`} className="text-xs">
                    <span className="flex justify-between">
                      <span className="truncate">{progress.fileName}</span>
                      <span>
                        {Math.round((progress.loaded / progress.total) * 100)}%
                      </span>
                    </span>
                    <span className="mt-1 block h-2 overflow-hidden rounded-full bg-surface-muted">
                      <span
                        className="block h-full bg-accent"
                        style={{
                          width: `${Math.min(
                            100,
                            (progress.loaded / Math.max(1, progress.total)) * 100,
                          )}%`,
                        }}
                      />
                    </span>
                  </li>
                ))}
              </ul>
            )}
          </div>

          {/* Just-uploaded files: the mark-as-public step lives here
            in pick mode, where a private upload would otherwise be
            invisible until flipped. */}
          {justUploaded.length > 0 && (
            <div className="mt-4 rounded-lg border border-border bg-surface p-3">
              <h3 className="text-sm font-semibold">Just uploaded</h3>
              <ul className="mt-2 space-y-2 text-sm">
                {justUploaded.map((item) => (
                  <li key={item.id} className="flex flex-wrap items-center gap-2">
                    <span className="min-w-0 flex-1 truncate">{item.name}</span>
                    <Badge tone={item.visibility === "public" ? "success" : "warning"}>
                      {item.visibility}
                    </Badge>
                    {item.visibility === "private" && (
                      <Button
                        color="primary"
                        size="xs"
                        isDisabled={busy}
                        onClick={() => flipUploaded(item)}
                      >
                        Make public
                      </Button>
                    )}
                    <Button color="secondary" size="xs" onClick={() => void copyUrl(item)}>
                      Copy URL
                    </Button>
                  </li>
                ))}
              </ul>
              <button
                type="button"
                onClick={() => setJustUploaded([])}
                className="mt-2 text-xs text-muted underline"
              >
                Dismiss
              </button>
            </div>
          )}
        </div>
      </div>

      {/* Rename dialog. */}
      {renameTarget !== null && (
        <Dialog
          title={`Rename ${renameTarget.kind}`}
          isOpen
          onOpenChange={(open) => {
            if (!open) setRenameTarget(null);
          }}
        >
          <form
            onSubmit={(event) => {
              event.preventDefault();
              submitRename();
            }}
            className="space-y-3"
          >
            <Input
              label="New name"
              value={renameValue}
              onChange={setRenameValue}
              maxLength={renameTarget.kind === "folder" ? 128 : 255}
              autoFocus
            />
            <div className="flex gap-2">
              <Button
                color="primary"
                size="sm"
                type="submit"
                isDisabled={busy || renameValue.trim() === ""}
              >
                Rename
              </Button>
              <Button color="secondary" size="sm" onClick={() => setRenameTarget(null)}>
                Cancel
              </Button>
            </div>
          </form>
        </Dialog>
      )}

      {/* Move dialog. */}
      {moveTarget !== null && (
        <Dialog
          title={`Move "${moveTarget.name}"`}
          isOpen
          onOpenChange={(open) => {
            if (!open) setMoveTarget(null);
          }}
        >
          <form
            onSubmit={(event) => {
              event.preventDefault();
              submitMove();
            }}
            className="space-y-3"
          >
            <Select
              label="Destination folder"
              items={[
                { id: "", label: "Library root" },
                ...flatFolders
                  .filter((folder) =>
                    moveTarget.kind === "folder" ? folder.id !== moveTarget.id : true,
                  )
                  .map((folder) => ({
                    id: folder.id,
                    label: `${"- ".repeat(folder.depth)}${folder.name}`,
                  })),
              ]}
              selectedKey={moveDestination}
              onSelectionChange={setMoveDestination}
            />
            <div className="flex gap-2">
              <Button color="primary" size="sm" type="submit" isDisabled={busy}>
                Move
              </Button>
              <Button color="secondary" size="sm" onClick={() => setMoveTarget(null)}>
                Cancel
              </Button>
            </div>
          </form>
        </Dialog>
      )}

      {/* Delete confirm - the server re-checks ownership and refuses
        a non-empty folder delete (409) regardless of this dialog. */}
      {deleteTarget !== null && (
        <Dialog
          title={`Delete ${deleteTarget.kind}`}
          isOpen
          onOpenChange={(open) => {
            if (!open) setDeleteTarget(null);
          }}
        >
          <p className="text-sm">
            Delete "{deleteTarget.name}" permanently?
            {deleteTarget.kind === "folder" &&
              " The folder must be empty - move or delete its contents first."}
            {deleteTarget.kind === "document" && " All versions are deleted too."}
          </p>
          <div className="mt-4 flex gap-2">
            <Button
              color="destructive"
              size="sm"
              isDisabled={busy}
              onClick={submitDelete}
            >
              Delete permanently
            </Button>
            <Button color="secondary" size="sm" onClick={() => setDeleteTarget(null)}>
              Cancel
            </Button>
          </div>
        </Dialog>
      )}
    </div>
  );
}

function FolderNode({
  node,
  depth,
  currentFolderId,
  expanded,
  onOpen,
  onToggle,
  onRename,
  onMove,
  onDelete,
}: {
  node: DocumentFolderTree;
  depth: number;
  currentFolderId: string;
  expanded: Set<string>;
  onOpen: (folderId: string) => void;
  onToggle: (folderId: string) => void;
  onRename?: (id: string, name: string) => void;
  onMove?: (id: string, name: string) => void;
  onDelete?: (id: string, name: string) => void;
}) {
  const isOpen = expanded.has(node.folder.id);
  const isCurrent = currentFolderId === node.folder.id;
  return (
    <li>
      <div
        className={`flex items-center gap-1 rounded-md px-2 py-1 ${
          isCurrent ? "bg-accent-soft font-medium text-accent" : "text-muted"
        }`}
        style={{ paddingLeft: `${8 + depth * 12}px` }}
      >
        {node.children.length > 0 && (
          <button
            type="button"
            onClick={() => onToggle(node.folder.id)}
            aria-label={isOpen ? "Collapse folder" : "Expand folder"}
            className="shrink-0 rounded px-1 hover:bg-surface-hover"
          >
            {isOpen ? "v" : ">"}
          </button>
        )}
        <button
          type="button"
          onClick={() => onOpen(node.folder.id)}
          className="min-w-0 flex-1 truncate text-left hover:underline"
        >
          {node.folder.name}
          <span className="text-faint"> ({node.document_count})</span>
        </button>
        {onRename !== undefined && (
          <span className="flex shrink-0 gap-1">
            <button
              type="button"
              title="Rename folder"
              aria-label={`Rename folder ${node.folder.name}`}
              onClick={() => onRename(node.folder.id, node.folder.name)}
              className="rounded px-1 text-xs hover:bg-surface-hover"
            >
              Rename
            </button>
            <button
              type="button"
              title="Move folder"
              aria-label={`Move folder ${node.folder.name}`}
              onClick={() => onMove?.(node.folder.id, node.folder.name)}
              className="rounded px-1 text-xs hover:bg-surface-hover"
            >
              Move
            </button>
            <button
              type="button"
              title="Delete folder"
              aria-label={`Delete folder ${node.folder.name}`}
              onClick={() => onDelete?.(node.folder.id, node.folder.name)}
              className="rounded px-1 text-xs hover:bg-surface-hover"
            >
              x
            </button>
          </span>
        )}
      </div>
      {isOpen && node.children.length > 0 && (
        <ul className="space-y-1">
          {node.children.map((child) => (
            <FolderNode
              key={child.folder.id}
              node={child}
              depth={depth + 1}
              currentFolderId={currentFolderId}
              expanded={expanded}
              onOpen={onOpen}
              onToggle={onToggle}
              onRename={onRename}
              onMove={onMove}
              onDelete={onDelete}
            />
          ))}
        </ul>
      )}
    </li>
  );
}

function itemActions(props: {
  item: ExplorerItem;
  isPick: boolean;
  busy: boolean;
  onCopy: () => void;
  onFlip: () => void;
  onRename: () => void;
  onMove: () => void;
  onDelete: () => void;
}) {
  const { item, isPick, busy } = props;
  if (isPick) return null;
  const isPublic = item.visibility === "public";
  const isClean = item.scan_status === "" || item.scan_status === "clean";
  return (
    <>
      <Button color="secondary" size="xs" onClick={props.onCopy}>
        Copy URL
      </Button>
      {/* Mark as public is the action authors need: a private file
        stays invisible in the picker until it is flipped public.
        The server enforces the scan gate, so an unclean file's
        flip fails honestly instead of silently succeeding. */}
      {/* The tooltip lives on a wrapper span: the theme Button has
        no title prop, and a span still shows it while disabled. */}
      <span
        title={
          !isPublic && !isClean
            ? `Cannot make public: scan status is ${item.scan_status}`
            : undefined
        }
      >
        <Button
          color={isPublic ? "secondary" : "primary"}
          size="xs"
          isDisabled={busy || (!isPublic && !isClean)}
          onClick={props.onFlip}
        >
          {isPublic ? "Make private" : "Make public"}
        </Button>
      </span>
      <Button color="secondary" size="xs" isDisabled={busy} onClick={props.onRename}>
        Rename
      </Button>
      <Button color="secondary" size="xs" isDisabled={busy} onClick={props.onMove}>
        Move
      </Button>
      <Button color="destructive" size="xs" isDisabled={busy} onClick={props.onDelete}>
        Delete
      </Button>
    </>
  );
}

function FileCard(props: {
  item: ExplorerItem;
  isPick: boolean;
  selected: boolean;
  busy: boolean;
  onSelect: () => void;
  onCopy: () => void;
  onFlip: () => void;
  onRename: () => void;
  onMove: () => void;
  onDelete: () => void;
}) {
  const { item, isPick, selected } = props;
  const isImage = item.mime.startsWith("image/");
  return (
    <li
      className={`rounded-lg border p-3 ${
        selected ? "border-accent bg-accent-soft" : "border-border bg-surface"
      }`}
    >
      <button
        type="button"
        onClick={props.onSelect}
        className="block w-full text-left"
        aria-pressed={isPick ? selected : undefined}
      >
        {isImage && item.visibility === "public" ? (
          <img
            src={item.url}
            alt=""
            loading="lazy"
            className="mb-2 h-32 w-full rounded-md object-cover"
          />
        ) : (
          <span className="mb-2 flex h-32 items-center justify-center rounded-md bg-surface-muted text-2xl text-muted">
            {item.mime.startsWith("video/")
              ? "Play"
              : item.mime.startsWith("audio/")
                ? "Audio"
                : item.mime === "application/pdf"
                  ? "PDF"
                  : "File"}
          </span>
        )}
        <span className="block truncate text-sm font-medium" title={item.name}>
          {item.name}
        </span>
        <span className="mt-1 block text-xs text-muted">
          {formatBytes(item.size)} - {formatRelativeTime(item.created_at)}
        </span>
        <span className="mt-2 flex gap-1">
          <Badge tone={item.visibility === "public" ? "success" : "warning"}>
            {item.visibility}
          </Badge>
          {item.scan_status !== "" && item.scan_status !== "clean" && (
            <Badge tone="danger">{item.scan_status}</Badge>
          )}
        </span>
      </button>
      <span className="mt-3 flex flex-wrap gap-1">
        {itemActions(props)}
      </span>
      {isPick && selected && (
        <span className="mt-2 block text-xs font-medium text-accent">
          Selected - attach below.
        </span>
      )}
    </li>
  );
}

function FileRow(props: {
  item: ExplorerItem;
  isPick: boolean;
  selected: boolean;
  busy: boolean;
  onSelect: () => void;
  onCopy: () => void;
  onFlip: () => void;
  onRename: () => void;
  onMove: () => void;
  onDelete: () => void;
}) {
  const { item, isPick, selected } = props;
  return (
    <li
      className={`flex flex-wrap items-center gap-2 px-3 py-2 ${
        selected ? "bg-accent-soft" : ""
      }`}
    >
      <button
        type="button"
        onClick={props.onSelect}
        className="min-w-0 flex-1 truncate text-left text-sm font-medium"
        aria-pressed={isPick ? selected : undefined}
      >
        {item.name}
      </button>
      <span className="text-xs text-muted">{item.mime}</span>
      <span className="text-xs text-muted">{formatBytes(item.size)}</span>
      <span className="text-xs text-muted">
        {formatRelativeTime(item.created_at)}
      </span>
      <Badge tone={item.visibility === "public" ? "success" : "warning"}>
        {item.visibility}
      </Badge>
      {item.scan_status !== "" && item.scan_status !== "clean" && (
        <Badge tone="danger">{item.scan_status}</Badge>
      )}
      <span className="flex flex-wrap gap-1">{itemActions(props)}</span>
    </li>
  );
}

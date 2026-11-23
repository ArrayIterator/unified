// Generic custom-entry screens (/react/content/custom/:typeSlug).
//
// ONE component renders any custom post type: it loads the
// definition (GET /api/v1/content/post-types) and builds the list
// + form from `supports` and `fields` — no per-type code exists.
// Field inputs follow the field type (see
// docs/integration/CUSTOM_POST_TYPES.md for the value shapes).

import { useState } from "react";
import { useParams } from "react-router-dom";
import {
  ApiError,
  createCustomEntry,
  deleteCustomEntry,
  fetchContentTags,
  fetchCustomEntries,
  fetchPostTypes,
  itemsFromData,
  updateCustomEntry,
  type ContentStatus,
  type ContentTag,
  type CustomEntry,
  type CustomFieldDef,
  type PostTypeDefinition,
} from "../api";
import { useLoad } from "../hooks";
import { Alert, Badge, Card, EmptyState, Skeleton } from "../components/ui";

const inputClass =
  "w-full rounded-md border border-border bg-surface px-3 py-2 text-sm text-text";
const labelClass = "mb-1 block text-sm font-medium";
const buttonClass =
  "rounded-md border border-border px-3 py-1.5 text-sm text-muted hover:bg-surface-hover";
const primaryButtonClass =
  "rounded-md bg-accent px-3 py-1.5 text-sm font-medium text-on-accent hover:opacity-90";

function fieldValueLabel(value: unknown): string {
  if (value === null || value === undefined) return "—";
  if (typeof value === "boolean") return value ? "Yes" : "No";
  if (Array.isArray(value)) return value.map(String).join(", ");
  if (typeof value === "object") return JSON.stringify(value);
  return String(value);
}

function FieldInput({
  def,
  value,
  onChange,
}: {
  def: CustomFieldDef;
  value: unknown;
  onChange: (value: unknown) => void;
}) {
  const options = def.options ?? [];
  switch (def.type) {
    case "textarea":
    case "richtext":
      return (
        <textarea
          className={inputClass}
          rows={4}
          value={typeof value === "string" ? value : ""}
          onChange={(event) => onChange(event.target.value)}
        />
      );
    case "number":
      return (
        <input
          className={inputClass}
          type="number"
          step="any"
          value={typeof value === "number" ? value : ""}
          onChange={(event) =>
            onChange(event.target.value === "" ? "" : Number(event.target.value))
          }
        />
      );
    case "date":
      return (
        <input
          className={inputClass}
          type="date"
          value={typeof value === "string" ? value : ""}
          onChange={(event) => onChange(event.target.value)}
        />
      );
    case "datetime":
      return (
        <input
          className={inputClass}
          type="datetime-local"
          value={typeof value === "string" ? value.slice(0, 16) : ""}
          onChange={(event) =>
            onChange(
              event.target.value
                ? new Date(event.target.value).toISOString()
                : "",
            )
          }
        />
      );
    case "select":
      return (
        <select
          className={inputClass}
          value={typeof value === "string" ? value : ""}
          onChange={(event) => onChange(event.target.value)}
        >
          <option value="">— Select —</option>
          {options.map((option) => (
            <option key={option} value={option}>
              {option}
            </option>
          ))}
        </select>
      );
    case "multiselect":
      return (
        <div className="flex flex-wrap gap-3">
          {options.map((option) => {
            const selected =
              Array.isArray(value) && (value as unknown[]).includes(option);
            return (
              <label key={option} className="flex items-center gap-1 text-sm">
                <input
                  type="checkbox"
                  checked={selected}
                  onChange={(event) => {
                    const current = Array.isArray(value)
                      ? [...(value as unknown[])]
                      : [];
                    onChange(
                      event.target.checked
                        ? [...current, option]
                        : current.filter((item) => item !== option),
                    );
                  }}
                />
                {option}
              </label>
            );
          })}
        </div>
      );
    case "checkbox":
      return (
        <input
          type="checkbox"
          checked={value === true}
          onChange={(event) => onChange(event.target.checked)}
        />
      );
    case "media":
    case "relation":
    case "text":
    default:
      return (
        <input
          className={inputClass}
          type="text"
          placeholder={def.type === "media" ? "Document id" : def.type === "relation" ? "Post, page, or entry UUID" : undefined}
          value={typeof value === "string" ? value : ""}
          onChange={(event) => onChange(event.target.value)}
        />
      );
  }
}

function defaultFieldValues(definition: PostTypeDefinition): Record<string, unknown> {
  const values: Record<string, unknown> = {};
  for (const field of definition.fields) {
    if (field.default !== undefined) values[field.key] = field.default;
  }
  return values;
}

export default function CustomEntriesPage() {
  const { typeSlug = "" } = useParams<{ typeSlug: string }>();
  const [statusFilter, setStatusFilter] = useState("");
  const [search, setSearch] = useState("");
  const [editing, setEditing] = useState<CustomEntry | "new" | null>(null);
  const [formError, setFormError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  const definitionLoad = useLoad(async () => {
    const page = await fetchPostTypes();
    const found = itemsFromData<PostTypeDefinition>(page).find(
      (item) => item.slug === typeSlug,
    );
    if (!found) throw new ApiError(404, `Post type "${typeSlug}" not found`);
    return found;
  }, [typeSlug]);

  const entriesLoad = useLoad(
    () => fetchCustomEntries(typeSlug, { status: statusFilter || undefined, search: search || undefined }),
    [typeSlug, statusFilter, search],
  );
  const tagsLoad = useLoad(() => fetchContentTags(), []);

  const definition = definitionLoad.data;
  const supports = (feature: string): boolean =>
    definition !== null && definition.supports.includes(feature);

  async function saveEntry(body: Record<string, unknown>): Promise<void> {
    setFormError(null);
    setSaving(true);
    try {
      if (editing === "new") {
        await createCustomEntry(typeSlug, body);
      } else if (editing) {
        await updateCustomEntry(typeSlug, editing.id, body);
      }
      setEditing(null);
      entriesLoad.reload();
    } catch (error) {
      setFormError(error instanceof Error ? error.message : String(error));
    } finally {
      setSaving(false);
    }
  }

  async function removeEntry(entry: CustomEntry): Promise<void> {
    if (!window.confirm(`Delete "${entry.title || entry.slug}"?`)) return;
    try {
      await deleteCustomEntry(typeSlug, entry.id);
      entriesLoad.reload();
    } catch (error) {
      setFormError(error instanceof Error ? error.message : String(error));
    }
  }

  if (definitionLoad.status === "loading") return <Skeleton lines={5} />;
  if (definitionLoad.status === "error") {
    return (
      <Alert title="Could not load this post type">{definitionLoad.error.message}</Alert>
    );
  }
  if (!definition) return <EmptyState>Post type not found.</EmptyState>;

  const entries = entriesLoad.data ? itemsFromData<CustomEntry>(entriesLoad.data) : [];
  const tags = tagsLoad.data ? itemsFromData<ContentTag>(tagsLoad.data) : [];

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="text-xl font-bold">{definition.plural_label}</h2>
          <p className="mt-1 text-sm text-muted">
            {definition.description ?? `Entries of the "${definition.slug}" type.`}{" "}
            <Badge tone={definition.enabled ? "success" : "warning"}>
              {definition.enabled ? "enabled" : "disabled"}
            </Badge>{" "}
            <Badge tone={definition.visibility === "public" ? "info" : "neutral"}>
              {definition.visibility}
            </Badge>
          </p>
        </div>
        <button
          type="button"
          className={primaryButtonClass}
          onClick={() => setEditing("new")}
        >
          New {definition.singular_label}
        </button>
      </div>

      {formError && <Alert title="Could not save">{formError}</Alert>}

      {editing && (
        <EntryForm
          definition={definition}
          entry={editing === "new" ? null : editing}
          tags={tags}
          saving={saving}
          onCancel={() => setEditing(null)}
          onSave={saveEntry}
        />
      )}

      <Card title="Entries">
        <div className="mb-4 flex flex-wrap gap-2">
          <select
            className="rounded-md border border-border bg-surface px-2 py-1.5 text-sm"
            value={statusFilter}
            onChange={(event) => setStatusFilter(event.target.value)}
            aria-label="Filter by status"
          >
            <option value="">All statuses</option>
            <option value="published">Published</option>
            <option value="draft">Draft</option>
          </select>
          <input
            className="rounded-md border border-border bg-surface px-3 py-1.5 text-sm"
            type="search"
            placeholder="Search entries"
            value={search}
            onChange={(event) => setSearch(event.target.value)}
          />
        </div>
        {entriesLoad.status === "loading" ? (
          <Skeleton lines={4} />
        ) : entriesLoad.status === "error" ? (
          <Alert title="Could not load entries">{entriesLoad.error.message}</Alert>
        ) : entries.length === 0 ? (
          <EmptyState>No entries yet.</EmptyState>
        ) : (
          <ul className="divide-y divide-border">
            {entries.map((entry) => (
              <li key={entry.id} className="flex flex-wrap items-center gap-3 py-3">
                <div className="min-w-0 flex-1">
                  <p className="truncate text-sm font-medium">
                    {entry.title || entry.slug}
                  </p>
                  <p className="mt-0.5 text-xs text-faint">
                    /{entry.slug} · {entry.status}
                    {definition.fields.length > 0 &&
                      ` · ${definition.fields
                        .map((field) => `${field.label}: ${fieldValueLabel(entry.fields[field.key])}`)
                        .join(" · ")}`}
                  </p>
                </div>
                <button
                  type="button"
                  className={buttonClass}
                  onClick={() => setEditing(entry)}
                >
                  Edit
                </button>
                <button
                  type="button"
                  className={buttonClass}
                  onClick={() => void removeEntry(entry)}
                >
                  Delete
                </button>
              </li>
            ))}
          </ul>
        )}
      </Card>
    </div>
  );
}

function EntryForm({
  definition,
  entry,
  tags,
  saving,
  onCancel,
  onSave,
}: {
  definition: PostTypeDefinition;
  entry: CustomEntry | null;
  tags: { id: string; name: string }[];
  saving: boolean;
  onCancel: () => void;
  onSave: (body: Record<string, unknown>) => Promise<void>;
}) {
  const supports = (feature: string): boolean => definition.supports.includes(feature);
  const [title, setTitle] = useState(entry?.title ?? "");
  const [slug, setSlug] = useState(entry?.slug ?? "");
  const [content, setContent] = useState(entry?.content ?? "");
  const [excerpt, setExcerpt] = useState(entry?.excerpt ?? "");
  const [thumbnail, setThumbnail] = useState(entry?.thumbnail ?? "");
  const [status, setStatus] = useState<ContentStatus>(entry?.status ?? "draft");
  const [fields, setFields] = useState<Record<string, unknown>>(
    entry ? entry.fields : defaultFieldValues(definition),
  );
  const [tagIds, setTagIds] = useState<string[]>(entry?.tag_ids ?? []);

  function submit(): void {
    const body: Record<string, unknown> = { status };
    if (entry === null || title !== entry.title) body.title = title;
    if (entry === null || slug !== entry.slug) body.slug = slug || undefined;
    if (supports("editor")) body.content = content;
    if (supports("excerpt")) body.excerpt = excerpt;
    if (supports("thumbnail")) body.thumbnail = thumbnail || undefined;
    if (supports("custom-fields")) body.fields = fields;
    if (supports("tags")) body.tag_ids = tagIds;
    if (entry === null && body.slug === undefined) delete body.slug;
    void onSave(body);
  }

  return (
    <Card title={entry ? `Edit ${definition.singular_label}` : `New ${definition.singular_label}`}>
      <div className="space-y-4">
        {supports("title") && (
          <div>
            <label className={labelClass} htmlFor="entry-title">Title</label>
            <input
              id="entry-title"
              className={inputClass}
              value={title}
              onChange={(event) => setTitle(event.target.value)}
            />
          </div>
        )}
        <div>
          <label className={labelClass} htmlFor="entry-slug">Slug</label>
          <input
            id="entry-slug"
            className={inputClass}
            value={slug}
            onChange={(event) => setSlug(event.target.value)}
            placeholder="Leave empty to derive from the title"
          />
        </div>
        {supports("editor") && (
          <div>
            <label className={labelClass} htmlFor="entry-content">Content</label>
            <textarea
              id="entry-content"
              className={inputClass}
              rows={6}
              value={content}
              onChange={(event) => setContent(event.target.value)}
            />
          </div>
        )}
        {supports("excerpt") && (
          <div>
            <label className={labelClass} htmlFor="entry-excerpt">Excerpt</label>
            <input
              id="entry-excerpt"
              className={inputClass}
              value={excerpt}
              onChange={(event) => setExcerpt(event.target.value)}
            />
          </div>
        )}
        {supports("thumbnail") && (
          <div>
            <label className={labelClass} htmlFor="entry-thumbnail">Thumbnail (document id)</label>
            <input
              id="entry-thumbnail"
              className={inputClass}
              value={thumbnail}
              onChange={(event) => setThumbnail(event.target.value)}
            />
          </div>
        )}
        {supports("custom-fields") &&
          definition.fields.map((field) => (
            <div key={field.key}>
              <label className={labelClass}>
                {field.label}
                {field.required ? " *" : ""}
              </label>
              <FieldInput
                def={field}
                value={fields[field.key]}
                onChange={(value) =>
                  setFields((current) => ({ ...current, [field.key]: value }))
                }
              />
            </div>
          ))}
        {supports("tags") && tags.length > 0 && (
          <div>
            <span className={labelClass}>Tags</span>
            <div className="flex flex-wrap gap-3">
              {tags.map((tag) => (
                <label key={tag.id} className="flex items-center gap-1 text-sm">
                  <input
                    type="checkbox"
                    checked={tagIds.includes(tag.id)}
                    onChange={(event) =>
                      setTagIds((current) =>
                        event.target.checked
                          ? [...current, tag.id]
                          : current.filter((id) => id !== tag.id),
                      )
                    }
                  />
                  {tag.name}
                </label>
              ))}
            </div>
          </div>
        )}
        <div>
          <label className={labelClass} htmlFor="entry-status">Status</label>
          <select
            id="entry-status"
            className={inputClass}
            value={status}
            onChange={(event) => setStatus(event.target.value as ContentStatus)}
          >
            <option value="draft">Draft</option>
            <option value="published">Published</option>
          </select>
        </div>
        <div className="flex gap-2">
          <button
            type="button"
            className={primaryButtonClass}
            disabled={saving}
            onClick={submit}
          >
            {saving ? "Saving…" : "Save"}
          </button>
          <button type="button" className={buttonClass} onClick={onCancel}>
            Cancel
          </button>
        </div>
      </div>
    </Card>
  );
}

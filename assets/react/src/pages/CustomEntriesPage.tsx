// Generic custom-entry screens (/react/content/custom/:typeSlug).
//
// ONE component renders any custom post type: it loads the
// definition (GET /api/v1/content/post-types) and builds the list
// + form from `supports` and `fields` - no per-type code exists.
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
import { Alert, Badge, Card, EmptyState, Skeleton } from "@theme/components/ui";
import {
  Button,
  Checkbox,
  DatePicker,
  Input,
  Select,
  TextArea,
} from "@theme/components/controls";

function fieldValueLabel(value: unknown): string {
  if (value === null || value === undefined) return "-";
  if (typeof value === "boolean") return value ? "Yes" : "No";
  if (Array.isArray(value)) return value.map(String).join(", ");
  if (typeof value === "object") return JSON.stringify(value);
  return String(value);
}

// ISO string -> local "YYYY-MM-DDTHH:mm" (the shape a
// datetime-local input shows), for the datetime field type.
function isoToLocalInput(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "";
  const pad = (n: number): string => String(n).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
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
        <TextArea
          rows={4}
          value={typeof value === "string" ? value : ""}
          onChange={onChange}
        />
      );
    case "number":
      // No step prop: the vendored Input has none (React Aria), and
      // this field is controlled (value/onChange -> Number) with no
      // native form validation, so decimal entry is unaffected.
      return (
        <Input
          type="number"
          value={typeof value === "number" ? String(value) : ""}
          onChange={(next) => onChange(next === "" ? "" : Number(next))}
        />
      );
    case "date":
      return (
        <DatePicker
          value={typeof value === "string" ? value : null}
          onChange={(iso) => onChange(iso ?? "")}
        />
      );
    case "datetime": {
      // The free date picker has no time selection, so the date and
      // the time are two theme controls combined back into the same
      // ISO string the API stores.
      const local = typeof value === "string" ? isoToLocalInput(value) : "";
      const datePart = local.slice(0, 10);
      const timePart = local.slice(11, 16);
      const emit = (date: string, time: string): void => {
        if (!date) {
          onChange("");
          return;
        }
        onChange(new Date(`${date}T${time || "00:00"}`).toISOString());
      };
      return (
        <div className="flex gap-2">
          <DatePicker
            value={datePart || null}
            onChange={(iso) => emit(iso ?? "", timePart)}
          />
          <Input
            type="time"
            value={timePart}
            onChange={(next) => emit(datePart, next)}
          />
        </div>
      );
    }
    case "select":
      return (
        <Select
          items={[
            { id: "", label: "- Select -" },
            ...options.map((option) => ({ id: option, label: option })),
          ]}
          selectedKey={typeof value === "string" ? value : ""}
          onSelectionChange={onChange}
        />
      );
    case "multiselect":
      return (
        <div className="flex flex-wrap gap-3">
          {options.map((option) => {
            const selected =
              Array.isArray(value) && (value as unknown[]).includes(option);
            return (
              <Checkbox
                key={option}
                label={option}
                isSelected={selected}
                onChange={(checked) => {
                  const current = Array.isArray(value)
                    ? [...(value as unknown[])]
                    : [];
                  onChange(
                    checked
                      ? [...current, option]
                      : current.filter((item) => item !== option),
                  );
                }}
              />
            );
          })}
        </div>
      );
    case "checkbox":
      return (
        <Checkbox
          isSelected={value === true}
          onChange={onChange}
        />
      );
    case "media":
    case "relation":
    case "text":
    default:
      return (
        <Input
          type="text"
          placeholder={def.type === "media" ? "Document id" : def.type === "relation" ? "Post, page, or entry UUID" : undefined}
          value={typeof value === "string" ? value : ""}
          onChange={onChange}
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
        <Button color="primary" size="sm" onClick={() => setEditing("new")}>
          New {definition.singular_label}
        </Button>
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
          <Select
            ariaLabel="Filter by status"
            items={[
              { id: "", label: "All statuses" },
              { id: "published", label: "Published" },
              { id: "draft", label: "Draft" },
            ]}
            selectedKey={statusFilter}
            onSelectionChange={setStatusFilter}
          />
          <Input
            type="search"
            placeholder="Search entries"
            value={search}
            onChange={setSearch}
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
                    /{entry.slug} - {entry.status}
                    {definition.fields.length > 0 &&
                      ` - ${definition.fields
                        .map((field) => `${field.label}: ${fieldValueLabel(entry.fields[field.key])}`)
                        .join(" - ")}`}
                  </p>
                </div>
                <Button
                  color="secondary"
                  size="sm"
                  onClick={() => setEditing(entry)}
                >
                  Edit
                </Button>
                <Button
                  color="secondary"
                  size="sm"
                  onClick={() => void removeEntry(entry)}
                >
                  Delete
                </Button>
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
          <Input label="Title" value={title} onChange={setTitle} />
        )}
        <Input
          label="Slug"
          value={slug}
          onChange={setSlug}
          placeholder="Leave empty to derive from the title"
        />
        {supports("editor") && (
          <TextArea
            label="Content"
            rows={6}
            value={content}
            onChange={setContent}
          />
        )}
        {supports("excerpt") && (
          <Input label="Excerpt" value={excerpt} onChange={setExcerpt} />
        )}
        {supports("thumbnail") && (
          <Input
            label="Thumbnail (document id)"
            value={thumbnail}
            onChange={setThumbnail}
          />
        )}
        {supports("custom-fields") &&
          definition.fields.map((field) => (
            <div key={field.key}>
              <span className="mb-1 block text-sm font-medium">
                {field.label}
                {field.required ? " *" : ""}
              </span>
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
            <span className="mb-1 block text-sm font-medium">Tags</span>
            <div className="flex flex-wrap gap-3">
              {tags.map((tag) => (
                <Checkbox
                  key={tag.id}
                  label={tag.name}
                  isSelected={tagIds.includes(tag.id)}
                  onChange={(checked) =>
                    setTagIds((current) =>
                      checked
                        ? [...current, tag.id]
                        : current.filter((id) => id !== tag.id),
                    )
                  }
                />
              ))}
            </div>
          </div>
        )}
        <Select
          label="Status"
          items={[
            { id: "draft", label: "Draft" },
            { id: "published", label: "Published" },
          ]}
          selectedKey={status}
          onSelectionChange={(value) => setStatus(value as ContentStatus)}
        />
        <div className="flex gap-2">
          <Button color="primary" isDisabled={saving} onClick={submit}>
            {saving ? "Saving..." : "Save"}
          </Button>
          <Button color="secondary" onClick={onCancel}>
            Cancel
          </Button>
        </div>
      </div>
    </Card>
  );
}

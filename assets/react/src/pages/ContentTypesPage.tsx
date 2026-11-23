// Post type + tag administration (/react/content/types).
//
// Definitions are data: labels, icon, supports, dynamic field
// definitions, visibility, archive/menu/REST flags. Creating one
// makes the generic routes and the generic entry screens serve the
// type immediately — no per-type code exists. Deleting a type is a
// soft-disable: its entries are preserved and reappear on enable.

import { useState } from "react";
import {
  createContentTag,
  createPostType,
  deleteContentTag,
  deletePostType,
  disablePostType,
  enablePostType,
  fetchContentTags,
  fetchPostTypes,
  itemsFromData,
  type ContentTag,
  type CustomFieldDef,
  type CustomFieldType,
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

const SUPPORT_OPTIONS = [
  "title",
  "editor",
  "excerpt",
  "thumbnail",
  "categories",
  "tags",
  "custom-fields",
] as const;

const FIELD_TYPES: CustomFieldType[] = [
  "text",
  "textarea",
  "richtext",
  "number",
  "date",
  "datetime",
  "select",
  "multiselect",
  "checkbox",
  "media",
  "relation",
];

interface FieldDraft {
  key: string;
  label: string;
  type: CustomFieldType;
  required: boolean;
  defaultValue: string;
  options: string;
}

function emptyFieldDraft(): FieldDraft {
  return { key: "", label: "", type: "text", required: false, defaultValue: "", options: "" };
}

export default function ContentTypesPage() {
  const typesLoad = useLoad(() => fetchPostTypes(), []);
  const tagsLoad = useLoad(() => fetchContentTags(), []);
  const [error, setError] = useState<string | null>(null);
  const [showForm, setShowForm] = useState(false);

  const types = typesLoad.data ? itemsFromData<PostTypeDefinition>(typesLoad.data) : [];
  const tags = tagsLoad.data ? itemsFromData<ContentTag>(tagsLoad.data) : [];

  async function run(action: () => Promise<unknown>, reload: () => void): Promise<void> {
    setError(null);
    try {
      await action();
      reload();
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  if (typesLoad.status === "loading" || tagsLoad.status === "loading") {
    return <Skeleton lines={5} />;
  }
  if (typesLoad.status === "error") {
    return <Alert title="Could not load post types">{typesLoad.error.message}</Alert>;
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="text-xl font-bold">Content types</h2>
          <p className="mt-1 text-sm text-muted">
            Admin-definable post types. Each type gets generic API
            routes, a menu entry, and definition-driven list/form
            screens — no code per type.
          </p>
        </div>
        <button
          type="button"
          className={primaryButtonClass}
          onClick={() => setShowForm((value) => !value)}
        >
          {showForm ? "Close form" : "New post type"}
        </button>
      </div>

      {error && <Alert title="Action failed">{error}</Alert>}

      {showForm && (
        <TypeForm
          onCancel={() => setShowForm(false)}
          onSaved={() => {
            setShowForm(false);
            typesLoad.reload();
          }}
          onError={setError}
        />
      )}

      <Card title="Post types">
        {types.length === 0 ? (
          <EmptyState>No custom post types yet.</EmptyState>
        ) : (
          <ul className="divide-y divide-border">
            {types.map((type) => (
              <li key={type.id} className="flex flex-wrap items-center gap-3 py-3">
                <div className="min-w-0 flex-1">
                  <p className="text-sm font-medium">
                    {type.plural_label}{" "}
                    <span className="font-normal text-faint">/{type.slug}</span>
                  </p>
                  <p className="mt-0.5 text-xs text-faint">
                    {type.fields.length} field{type.fields.length === 1 ? "" : "s"} ·{" "}
                    {type.supports.join(", ") || "no built-in features"} · icon{" "}
                    {type.icon}
                  </p>
                </div>
                <Badge tone={type.enabled ? "success" : "warning"}>
                  {type.enabled ? "enabled" : "disabled"}
                </Badge>
                <Badge tone={type.visibility === "public" ? "info" : "neutral"}>
                  {type.visibility}
                </Badge>
                {type.enabled ? (
                  <button
                    type="button"
                    className={buttonClass}
                    onClick={() =>
                      void run(() => disablePostType(type.id), typesLoad.reload)
                    }
                  >
                    Disable
                  </button>
                ) : (
                  <button
                    type="button"
                    className={buttonClass}
                    onClick={() =>
                      void run(() => enablePostType(type.id), typesLoad.reload)
                    }
                  >
                    Enable
                  </button>
                )}
                <button
                  type="button"
                  className={buttonClass}
                  onClick={() => {
                    if (
                      window.confirm(
                        `Disable "${type.plural_label}"? Its entries are preserved and reappear on enable.`,
                      )
                    ) {
                      void run(() => deletePostType(type.id), typesLoad.reload);
                    }
                  }}
                >
                  Delete
                </button>
              </li>
            ))}
          </ul>
        )}
      </Card>

      <TagsCard
        tags={tags}
        onError={setError}
        onChanged={tagsLoad.reload}
        onRun={run}
      />
    </div>
  );
}

function TypeForm({
  onCancel,
  onSaved,
  onError,
}: {
  onCancel: () => void;
  onSaved: () => void;
  onError: (message: string) => void;
}) {
  const [slug, setSlug] = useState("");
  const [singular, setSingular] = useState("");
  const [plural, setPlural] = useState("");
  const [icon, setIcon] = useState("document");
  const [description, setDescription] = useState("");
  const [visibility, setVisibility] = useState("private");
  const [supports, setSupports] = useState<string[]>(["title", "editor"]);
  const [hasArchive, setHasArchive] = useState(false);
  const [showInMenu, setShowInMenu] = useState(true);
  const [menuPosition, setMenuPosition] = useState(0);
  const [restExposed, setRestExposed] = useState(true);
  const [fieldDrafts, setFieldDrafts] = useState<FieldDraft[]>([]);
  const [saving, setSaving] = useState(false);

  function toggleSupport(feature: string): void {
    setSupports((current) =>
      current.includes(feature)
        ? current.filter((item) => item !== feature)
        : [...current, feature],
    );
  }

  function fieldDefs(): CustomFieldDef[] {
    return fieldDrafts
      .filter((draft) => draft.key.trim() !== "" && draft.label.trim() !== "")
      .map((draft) => {
        const def: CustomFieldDef = {
          key: draft.key.trim(),
          label: draft.label.trim(),
          type: draft.type,
          required: draft.required,
        };
        if (draft.type === "select" || draft.type === "multiselect") {
          def.options = draft.options
            .split(",")
            .map((option) => option.trim())
            .filter((option) => option !== "");
        }
        if (draft.defaultValue !== "") {
          if (draft.type === "number") def.default = Number(draft.defaultValue);
          else if (draft.type === "checkbox")
            def.default = draft.defaultValue === "true";
          else def.default = draft.defaultValue;
        }
        return def;
      });
  }

  async function save(): Promise<void> {
    setSaving(true);
    try {
      await createPostType({
        slug,
        singular_label: singular,
        plural_label: plural,
        icon: icon || undefined,
        description: description || undefined,
        supports,
        fields: fieldDefs(),
        visibility,
        has_archive: hasArchive,
        show_in_menu: showInMenu,
        menu_position: menuPosition,
        rest_exposed: restExposed,
      });
      onSaved();
    } catch (caught) {
      onError(caught instanceof Error ? caught.message : String(caught));
    } finally {
      setSaving(false);
    }
  }

  function updateDraft(index: number, patch: Partial<FieldDraft>): void {
    setFieldDrafts((current) =>
      current.map((draft, i) => (i === index ? { ...draft, ...patch } : draft)),
    );
  }

  return (
    <Card title="New post type">
      <div className="space-y-4">
        <div className="grid gap-4 sm:grid-cols-3">
          <div>
            <label className={labelClass} htmlFor="type-slug">Slug *</label>
            <input
              id="type-slug"
              className={inputClass}
              value={slug}
              onChange={(event) => setSlug(event.target.value)}
              placeholder="events"
            />
          </div>
          <div>
            <label className={labelClass} htmlFor="type-singular">Singular label *</label>
            <input
              id="type-singular"
              className={inputClass}
              value={singular}
              onChange={(event) => setSingular(event.target.value)}
              placeholder="Event"
            />
          </div>
          <div>
            <label className={labelClass} htmlFor="type-plural">Plural label *</label>
            <input
              id="type-plural"
              className={inputClass}
              value={plural}
              onChange={(event) => setPlural(event.target.value)}
              placeholder="Events"
            />
          </div>
        </div>
        <div className="grid gap-4 sm:grid-cols-2">
          <div>
            <label className={labelClass} htmlFor="type-icon">Icon (sprite symbol)</label>
            <input
              id="type-icon"
              className={inputClass}
              value={icon}
              onChange={(event) => setIcon(event.target.value)}
            />
          </div>
          <div>
            <label className={labelClass} htmlFor="type-visibility">Visibility</label>
            <select
              id="type-visibility"
              className={inputClass}
              value={visibility}
              onChange={(event) => setVisibility(event.target.value)}
            >
              <option value="private">Private</option>
              <option value="public">Public</option>
            </select>
          </div>
        </div>
        <div>
          <label className={labelClass} htmlFor="type-description">Description</label>
          <input
            id="type-description"
            className={inputClass}
            value={description}
            onChange={(event) => setDescription(event.target.value)}
          />
        </div>
        <div>
          <span className={labelClass}>Supports</span>
          <div className="flex flex-wrap gap-3">
            {SUPPORT_OPTIONS.map((feature) => (
              <label key={feature} className="flex items-center gap-1 text-sm">
                <input
                  type="checkbox"
                  checked={supports.includes(feature)}
                  onChange={() => toggleSupport(feature)}
                />
                {feature}
              </label>
            ))}
          </div>
        </div>
        <div className="flex flex-wrap gap-4 text-sm">
          <label className="flex items-center gap-1">
            <input
              type="checkbox"
              checked={hasArchive}
              onChange={(event) => setHasArchive(event.target.checked)}
            />
            Has archive
          </label>
          <label className="flex items-center gap-1">
            <input
              type="checkbox"
              checked={showInMenu}
              onChange={(event) => setShowInMenu(event.target.checked)}
            />
            Show in menu
          </label>
          <label className="flex items-center gap-1">
            <input
              type="checkbox"
              checked={restExposed}
              onChange={(event) => setRestExposed(event.target.checked)}
            />
            REST exposed
          </label>
          <label className="flex items-center gap-1">
            Menu position
            <input
              type="number"
              className="w-20 rounded-md border border-border bg-surface px-2 py-1 text-sm"
              value={menuPosition}
              onChange={(event) => setMenuPosition(Number(event.target.value))}
            />
          </label>
        </div>
        <div>
          <div className="mb-2 flex items-center justify-between">
            <span className={labelClass}>Custom fields</span>
            <button
              type="button"
              className={buttonClass}
              onClick={() => setFieldDrafts((current) => [...current, emptyFieldDraft()])}
            >
              Add field
            </button>
          </div>
          {fieldDrafts.length === 0 ? (
            <EmptyState>No custom fields. Add one to store structured data per entry.</EmptyState>
          ) : (
            <div className="space-y-3">
              {fieldDrafts.map((draft, index) => (
                <div
                  key={index}
                  className="grid gap-2 rounded-md border border-border p-3 sm:grid-cols-6"
                >
                  <input
                    className={inputClass}
                    placeholder="key (snake_case)"
                    value={draft.key}
                    onChange={(event) => updateDraft(index, { key: event.target.value })}
                  />
                  <input
                    className={inputClass}
                    placeholder="Label"
                    value={draft.label}
                    onChange={(event) => updateDraft(index, { label: event.target.value })}
                  />
                  <select
                    className={inputClass}
                    value={draft.type}
                    onChange={(event) =>
                      updateDraft(index, { type: event.target.value as CustomFieldType })
                    }
                  >
                    {FIELD_TYPES.map((type) => (
                      <option key={type} value={type}>
                        {type}
                      </option>
                    ))}
                  </select>
                  <input
                    className={inputClass}
                    placeholder="Options (a, b, c)"
                    value={draft.options}
                    onChange={(event) => updateDraft(index, { options: event.target.value })}
                  />
                  <input
                    className={inputClass}
                    placeholder="Default"
                    value={draft.defaultValue}
                    onChange={(event) =>
                      updateDraft(index, { defaultValue: event.target.value })
                    }
                  />
                  <div className="flex items-center gap-2 text-sm">
                    <label className="flex items-center gap-1">
                      <input
                        type="checkbox"
                        checked={draft.required}
                        onChange={(event) =>
                          updateDraft(index, { required: event.target.checked })
                        }
                      />
                      Required
                    </label>
                    <button
                      type="button"
                      className={buttonClass}
                      onClick={() =>
                        setFieldDrafts((current) =>
                          current.filter((_, i) => i !== index),
                        )
                      }
                    >
                      Remove
                    </button>
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
        <div className="flex gap-2">
          <button
            type="button"
            className={primaryButtonClass}
            disabled={saving}
            onClick={() => void save()}
          >
            {saving ? "Saving…" : "Create post type"}
          </button>
          <button type="button" className={buttonClass} onClick={onCancel}>
            Cancel
          </button>
        </div>
      </div>
    </Card>
  );
}

function TagsCard({
  tags,
  onError,
  onChanged,
  onRun,
}: {
  tags: ContentTag[];
  onError: (message: string) => void;
  onChanged: () => void;
  onRun: (action: () => Promise<unknown>, reload: () => void) => Promise<void>;
}) {
  const [name, setName] = useState("");
  const [slug, setSlug] = useState("");
  const [description, setDescription] = useState("");

  async function saveTag(): Promise<void> {
    try {
      await createContentTag({
        name,
        slug: slug || undefined,
        description: description || undefined,
      });
      setName("");
      setSlug("");
      setDescription("");
      onChanged();
    } catch (caught) {
      onError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  return (
    <Card title="Tags">
      <div className="mb-4 grid gap-2 sm:grid-cols-4">
        <input
          className={inputClass}
          placeholder="Tag name *"
          value={name}
          onChange={(event) => setName(event.target.value)}
        />
        <input
          className={inputClass}
          placeholder="Slug (optional)"
          value={slug}
          onChange={(event) => setSlug(event.target.value)}
        />
        <input
          className={inputClass}
          placeholder="Description (optional)"
          value={description}
          onChange={(event) => setDescription(event.target.value)}
        />
        <button
          type="button"
          className={primaryButtonClass}
          disabled={name.trim() === ""}
          onClick={() => void saveTag()}
        >
          Add tag
        </button>
      </div>
      {tags.length === 0 ? (
        <EmptyState>No tags yet.</EmptyState>
      ) : (
        <ul className="divide-y divide-border">
          {tags.map((tag) => (
            <li key={tag.id} className="flex flex-wrap items-center gap-3 py-2">
              <div className="min-w-0 flex-1">
                <p className="text-sm font-medium">{tag.name}</p>
                <p className="text-xs text-faint">/{tag.slug}</p>
              </div>
              <button
                type="button"
                className={buttonClass}
                onClick={() => {
                  if (window.confirm(`Delete tag "${tag.name}"?`)) {
                    void onRun(() => deleteContentTag(tag.id), onChanged);
                  }
                }}
              >
                Delete
              </button>
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}

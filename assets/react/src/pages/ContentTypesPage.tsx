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
import { Alert, Badge, Card, EmptyState, Skeleton } from "@theme/components/ui";
import { Button, Checkbox, Input, Select } from "@theme/components/controls";

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
        <Button
          color="primary"
          size="sm"
          onClick={() => setShowForm((value) => !value)}
        >
          {showForm ? "Close form" : "New post type"}
        </Button>
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
                  <Button
                    color="secondary"
                    size="sm"
                    onClick={() =>
                      void run(() => disablePostType(type.id), typesLoad.reload)
                    }
                  >
                    Disable
                  </Button>
                ) : (
                  <Button
                    color="secondary"
                    size="sm"
                    onClick={() =>
                      void run(() => enablePostType(type.id), typesLoad.reload)
                    }
                  >
                    Enable
                  </Button>
                )}
                <Button
                  color="secondary"
                  size="sm"
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
                </Button>
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
          <Input
            label="Slug *"
            value={slug}
            onChange={setSlug}
            placeholder="events"
          />
          <Input
            label="Singular label *"
            value={singular}
            onChange={setSingular}
            placeholder="Event"
          />
          <Input
            label="Plural label *"
            value={plural}
            onChange={setPlural}
            placeholder="Events"
          />
        </div>
        <div className="grid gap-4 sm:grid-cols-2">
          <Input
            label="Icon (sprite symbol)"
            value={icon}
            onChange={setIcon}
          />
          <Select
            label="Visibility"
            items={[
              { id: "private", label: "Private" },
              { id: "public", label: "Public" },
            ]}
            selectedKey={visibility}
            onSelectionChange={setVisibility}
          />
        </div>
        <Input
          label="Description"
          value={description}
          onChange={setDescription}
        />
        <div>
          <span className="mb-1 block text-sm font-medium">Supports</span>
          <div className="flex flex-wrap gap-3">
            {SUPPORT_OPTIONS.map((feature) => (
              <Checkbox
                key={feature}
                label={feature}
                isSelected={supports.includes(feature)}
                onChange={() => toggleSupport(feature)}
              />
            ))}
          </div>
        </div>
        <div className="flex flex-wrap gap-4 text-sm">
          <Checkbox
            label="Has archive"
            isSelected={hasArchive}
            onChange={setHasArchive}
          />
          <Checkbox
            label="Show in menu"
            isSelected={showInMenu}
            onChange={setShowInMenu}
          />
          <Checkbox
            label="REST exposed"
            isSelected={restExposed}
            onChange={setRestExposed}
          />
          <Input
            label="Menu position"
            type="number"
            value={String(menuPosition)}
            onChange={(value) => setMenuPosition(Number(value))}
          />
        </div>
        <div>
          <div className="mb-2 flex items-center justify-between">
            <span className="mb-1 block text-sm font-medium">Custom fields</span>
            <Button
              color="secondary"
              size="sm"
              onClick={() => setFieldDrafts((current) => [...current, emptyFieldDraft()])}
            >
              Add field
            </Button>
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
                  <Input
                    placeholder="key (snake_case)"
                    value={draft.key}
                    onChange={(value) => updateDraft(index, { key: value })}
                  />
                  <Input
                    placeholder="Label"
                    value={draft.label}
                    onChange={(value) => updateDraft(index, { label: value })}
                  />
                  <Select
                    items={FIELD_TYPES.map((type) => ({ id: type, label: type }))}
                    selectedKey={draft.type}
                    onSelectionChange={(value) =>
                      updateDraft(index, { type: value as CustomFieldType })
                    }
                  />
                  <Input
                    placeholder="Options (a, b, c)"
                    value={draft.options}
                    onChange={(value) => updateDraft(index, { options: value })}
                  />
                  <Input
                    placeholder="Default"
                    value={draft.defaultValue}
                    onChange={(value) => updateDraft(index, { defaultValue: value })}
                  />
                  <div className="flex items-center gap-2 text-sm">
                    <Checkbox
                      label="Required"
                      isSelected={draft.required}
                      onChange={(value) => updateDraft(index, { required: value })}
                    />
                    <Button
                      color="secondary"
                      size="sm"
                      onClick={() =>
                        setFieldDrafts((current) =>
                          current.filter((_, i) => i !== index),
                        )
                      }
                    >
                      Remove
                    </Button>
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
        <div className="flex gap-2">
          <Button
            color="primary"
            isDisabled={saving}
            onClick={() => void save()}
          >
            {saving ? "Saving…" : "Create post type"}
          </Button>
          <Button color="secondary" onClick={onCancel}>
            Cancel
          </Button>
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
        <Input
          placeholder="Tag name *"
          value={name}
          onChange={setName}
        />
        <Input
          placeholder="Slug (optional)"
          value={slug}
          onChange={setSlug}
        />
        <Input
          placeholder="Description (optional)"
          value={description}
          onChange={setDescription}
        />
        <Button
          color="primary"
          isDisabled={name.trim() === ""}
          onClick={() => void saveTag()}
        >
          Add tag
        </Button>
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
              <Button
                color="secondary"
                size="sm"
                onClick={() => {
                  if (window.confirm(`Delete tag "${tag.name}"?`)) {
                    void onRun(() => deleteContentTag(tag.id), onChanged);
                  }
                }}
              >
                Delete
              </Button>
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}

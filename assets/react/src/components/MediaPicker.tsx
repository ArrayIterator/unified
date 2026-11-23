// React media picker dialog for post / custom-post editors: the
// shared documents picker as a React component over
// `GET /api/v1/documents/picker` (public-only for every role —
// content embeds a public URL, and a private file would 404 for
// readers, so it never appears here until it is flipped public).
// A private upload made from this picker surfaces in the
// "Just uploaded" panel with a "Make public" action, so the author
// can flip it and attach it without leaving the editor.
//
// This replaces the vanilla picker (`assets/js/media-picker.ts` +
// `templates/default/documents/picker.hbs`) for React editors only.
// The vanilla picker is KEPT: the classic Handlebars edit forms
// still open it via `data-media-picker-open`.

import FileExplorer, { type ExplorerItem } from "./FileExplorer";

export interface MediaPickerProps {
  /** Allow attaching several files without closing the picker. */
  multiple?: boolean;
  /** Called once per attach with the picked files. */
  onAttach: (items: ExplorerItem[]) => void;
  /** Close the picker dialog. */
  onClose: () => void;
}

export default function MediaPicker({ multiple = false, onAttach, onClose }: MediaPickerProps) {
  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
      role="dialog"
      aria-modal="true"
      aria-label="Media picker"
      onClick={onClose}
    >
      <div
        className="max-h-[90vh] w-full max-w-5xl overflow-auto rounded-lg border border-border bg-surface p-5 shadow-md"
        onClick={(event) => event.stopPropagation()}
      >
        <div className="mb-4 flex items-start justify-between gap-3">
          <div>
            <h2 className="text-lg font-semibold">Media picker</h2>
            <p className="mt-1 text-sm text-muted">
              Public files only — a private file appears here once it is
              marked public. Uploads default to public here; switch to
              private in the upload box if a file is not ready to embed yet.
            </p>
          </div>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close media picker"
            className="rounded-md border border-border px-2 py-1 text-sm text-muted hover:bg-surface-hover"
          >
            ✕
          </button>
        </div>
        <FileExplorer
          mode="pick"
          multiple={multiple}
          onPick={(items) => {
            onAttach(items);
            onClose();
          }}
          onClose={onClose}
        />
      </div>
    </div>
  );
}

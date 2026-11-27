// Media manager page body: the full file explorer in manage mode,
// with the role-scoped listing rules surfaced next to it so an
// author understands why some files are missing:
//   - the picker (embed) lists public files only, for every role;
//   - this manager list shows admins public + private, and authors
//     public files plus their own private files - another owner's
//     private file never appears here (the server filters the list
//     endpoint itself, `list_visible`);
//   - a private file becomes visible in the picker once it is
//     marked public ("Make public" on the file; the scan must be
//     clean, enforced server-side).

import FileExplorer from "../components/FileExplorer";
import { Card } from "@theme/components/ui";

export default function MediaManager() {
  return (
    <div className="space-y-4">
      <Card title="Media library">
        <p className="text-sm text-muted">
          Your files, organized in folders. Authors see public files plus
          their own private files - another author&apos;s private file never
          appears here. Mark your own private file public ("Make public") and
          it shows up in the picker for everyone to embed.
        </p>
      </Card>
      <FileExplorer mode="manage" />
    </div>
  );
}

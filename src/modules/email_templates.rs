// Email Template Builder module (GrapeJS).
//
// Templates are designed in GrapeJS on the client side and stored here as
// HTML plus the GrapeJS design JSON. Every update creates a new version,
// so templates are fully restorable.
//
// Routes (all under `/api/v1/email-templates`):
//   GET    /                              - paginated list (filter by category, search)
//   POST   /                              - create a template (becomes version 1)
//   GET    /by-category/{category}        - paginated list for one category
//   GET    /{id}                          - one template, including its versions
//   PUT    /{id}                          - update (creates a new version)
//   DELETE /{id}                          - delete a template
//   GET    /{id}/versions                 - version history
//   POST   /{id}/restore/{version}        - restore a version (creates a new version)
//   POST   /{id}/variables                - placeholders found in the template
//   POST   /{id}/preview                  - render with variables substituted
//   POST   /{id}/test-send                - render + send through the Mail Manager
//
// Hook contracts:
//   Answers `template.render_requested` with `{ template_id, variables }`
//   by adding a result `{ rendered_html, subject, missing_variables }`
//   (or `{ error }` when the template does not exist). The Mail Manager
//   uses this to render templates for sending.
//   Test-send emits `mail.send_requested`; when no mail handler answers,
//   it returns 503 with a clear message.
//   Emits `template.created`, `template.updated`, `template.deleted` and
//   `template.restored` for observability.

pub mod ai_capabilities;
pub mod render;
pub mod types;
mod handlers;
pub mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::render::*;
pub use self::types::*;
pub use self::handlers::*;
pub use self::module::*;

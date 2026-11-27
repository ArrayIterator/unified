// hosting - compatibility shim (DEPRECATED).
//
// The hosting module now lives at `crate::modules::cloud::hosting`.
// This file only re-exports it, so the old module id `hosting` and the
// old routes `/api/v1/hosting/*` keep working; the canonical home is
// the `cloud` module (`/api/v1/cloud/hosting/*`). Do not add new code
// here - new code belongs under `src/modules/cloud/hosting/`.

pub use crate::modules::cloud::hosting::*;

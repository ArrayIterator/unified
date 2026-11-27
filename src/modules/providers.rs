// providers - compatibility shim (DEPRECATED).
//
// The providers module now lives at `crate::modules::cloud::providers`.
// This file only re-exports it, so the old module id `providers` and
// the old routes `/api/v1/providers/*` keep working; the canonical home
// is the `cloud` module (`/api/v1/cloud/providers/*`). Do not add new
// code here - new code belongs under `src/modules/cloud/providers/`.

pub use crate::modules::cloud::providers::*;

// storage - events.
// Hook event names emitted by the storage module.
//

use super::*;
use crate::error::{UnifiedError, UnifiedResult};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use reqwest::header;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest;
use std::collections::HashMap;
use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, RwLock};

// Hook event names (emitted by the storage module).

pub const TARGET_CREATED: &str = "storage.target.created";
pub const TARGET_DELETED: &str = "storage.target.deleted";
pub const OBJECT_UPLOADED: &str = "storage.object.uploaded";
pub const OBJECT_DELETED: &str = "storage.object.deleted";
/// Emitted when a download or verification finds bytes whose
/// SHA-256 does not match the recorded checksum. The corrupt
/// bytes are never served.
pub const OBJECT_CHECKSUM_MISMATCH: &str = "storage.object.checksum_mismatch";

// storage - tests.
// Unit tests for the storage module.
//

use super::keys::hex_encode;
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

#[test]
fn hex_encode_matches_reference_for_every_byte_value() {
    // Exhaustive oracle: the lookup-table encoding must agree
    // with plain per-byte formatting for all 256 byte values.
    // (`format!` is the reference here, in test code only.)
    for byte in 0..=u8::MAX {
        assert_eq!(hex_encode(&[byte]), format!("{byte:02x}"));
    }
    assert_eq!(hex_encode(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
}

// storage - s3.
// S3 and R2 (S3-compatible) providers.
//

use super::keys::encode_key;
use super::keys::hex_encode;
use super::keys::hmac_sha256;
use super::keys::percent_encode;
use super::validation::build_http_client;
use super::validation::host_header;
use super::validation::map_provider_status;
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

// S3 provider (AWS S3, SigV4).

/// Configuration for `S3Provider`.
#[derive(Debug, Clone)]
pub struct S3Config {
    /// Endpoint URL, for example `https://s3.us-east-1.amazonaws.com`.
    /// A custom S3-compatible endpoint must use `https` and its host
    /// must be listed in `storage.allowed_hosts`.
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key: String,
    /// Never logged, never returned by any API response.
    pub secret_key: String,
}

impl S3Config {
    /// True when the endpoint is a standard AWS S3 endpoint, in which
    /// case virtual-host style URLs (`{bucket}.s3.{region}.amazonaws.com`)
    /// are used. Custom endpoints use path style
    /// (`{endpoint}/{bucket}/{key}`).
    fn is_aws_endpoint(host: &str, region: &str) -> bool {
        host == format!("s3.{region}.amazonaws.com") || host == "s3.amazonaws.com"
    }
}

/// AWS S3 object storage with SigV4-signed requests.
///
/// Honest scope: core object operations only (PUT / GET / DELETE,
/// presigned GET URLs, and a bucket list probe for `test_connection`).
/// No bucket management, versioning, lifecycle, or multipart upload.
pub struct S3Provider {
    config: S3Config,
    client: reqwest::Client,
    endpoint_url: url::Url,
    virtual_host: bool,
}

impl S3Provider {
    pub fn new(config: S3Config, extra_hosts: &[String]) -> UnifiedResult<Self> {
        if config.region.trim().is_empty() {
            return Err(UnifiedError::bad_request("S3 region is required"));
        }
        if config.bucket.trim().is_empty() {
            return Err(UnifiedError::bad_request("S3 bucket is required"));
        }
        if config.access_key.trim().is_empty() || config.secret_key.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "S3 access key and secret key are required",
            ));
        }
        let endpoint_url = validate_endpoint_url(&config.endpoint, extra_hosts)?;
        let host = endpoint_url
            .host_str()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let virtual_host = S3Config::is_aws_endpoint(&host, config.region.trim());
        // Normalize the stored fields so signing never sees whitespace.
        let config = S3Config {
            endpoint: config.endpoint.trim().to_string(),
            region: config.region.trim().to_string(),
            bucket: config.bucket.trim().to_string(),
            access_key: config.access_key.trim().to_string(),
            secret_key: config.secret_key,
        };
        Ok(Self {
            config,
            client: build_http_client()?,
            endpoint_url,
            virtual_host,
        })
    }

    /// Base URL for one object key (no query string).
    fn object_url(&self, key: &str) -> UnifiedResult<url::Url> {
        let encoded = encode_key(key);
        let raw = if self.virtual_host {
            let host = self.endpoint_url.host_str().unwrap_or_default();
            format!(
                "https://{}.{host}/{encoded}",
                self.config.bucket,
                host = host
            )
        } else {
            let base = self.config.endpoint.trim_end_matches('/');
            format!("{base}/{}/{encoded}", self.config.bucket)
        };
        url::Url::parse(&raw).map_err(|_| {
            UnifiedError::bad_request("Cannot build the S3 object URL for the given key")
        })
    }

    /// Base URL for bucket-level operations (no key).
    fn bucket_url(&self) -> UnifiedResult<url::Url> {
        let raw = if self.virtual_host {
            let host = self.endpoint_url.host_str().unwrap_or_default();
            format!("https://{}.{host}/", self.config.bucket, host = host)
        } else {
            let base = self.config.endpoint.trim_end_matches('/');
            format!("{base}/{}/", self.config.bucket)
        };
        url::Url::parse(&raw)
            .map_err(|_| UnifiedError::internal("Cannot build the S3 bucket URL"))
    }

    /// Sign one request with SigV4. Returns the `Authorization` header
    /// value and the `x-amz-date` value; the caller also sends
    /// `x-amz-content-sha256` with `payload_hash`.
    fn sign(
        &self,
        method: &str,
        url: &url::Url,
        query: &[(String, String)],
        payload_hash: &str,
        now: DateTime<Utc>,
    ) -> UnifiedResult<(String, String)> {
        let date_stamp = now.format("%Y%m%d").to_string();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let mut sorted: Vec<(String, String)> = query.to_vec();
        sorted.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        let canonical_query = sorted
            .iter()
            .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let canonical_uri = if url.path().is_empty() {
            "/".to_string()
        } else {
            url.path().to_string()
        };
        let canonical_headers = format!(
            "host:{}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\n",
            host_header(url)
        );
        let signed_headers = "host;x-amz-content-sha256;x-amz-date";
        let canonical_request = format!(
            "{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
        );
        let scope = format!(
            "{date_stamp}/{}/s3/aws4_request",
            self.config.region
        );
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            sha256_hex(canonical_request.as_bytes())
        );
        let date_key = hmac_sha256(format!("AWS4{}", self.config.secret_key).as_bytes(), date_stamp.as_bytes())?;
        let region_key = hmac_sha256(&date_key, self.config.region.as_bytes())?;
        let service_key = hmac_sha256(&region_key, b"s3")?;
        let signing_key = hmac_sha256(&service_key, b"aws4_request")?;
        let signature = hex_encode(&hmac_sha256(&signing_key, string_to_sign.as_bytes())?);
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            self.config.access_key
        );
        Ok((authorization, amz_date))
    }

    /// Build a presigned GET URL for one key. The signature covers the
    /// canonical query string; the URL is valid for `expires_secs`.
    fn presigned_url(&self, key: &str, expires_secs: u64) -> UnifiedResult<String> {
        let url = self.object_url(key)?;
        let now = Utc::now();
        let date_stamp = now.format("%Y%m%d").to_string();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let scope = format!(
            "{date_stamp}/{}/s3/aws4_request",
            self.config.region
        );
        let credential = format!("{}/{}", self.config.access_key, scope);
        let mut query: Vec<(String, String)> = vec![
            ("X-Amz-Algorithm".to_string(), "AWS4-HMAC-SHA256".to_string()),
            ("X-Amz-Credential".to_string(), credential),
            ("X-Amz-Date".to_string(), amz_date.clone()),
            (
                "X-Amz-Expires".to_string(),
                expires_secs.to_string(),
            ),
            ("X-Amz-SignedHeaders".to_string(), "host".to_string()),
        ];
        query.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        let canonical_query = query
            .iter()
            .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let canonical_uri = url.path().to_string();
        let canonical_headers = format!("host:{}\n", host_header(&url));
        let canonical_request = format!(
            "GET\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\nhost\nUNSIGNED-PAYLOAD"
        );
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            sha256_hex(canonical_request.as_bytes())
        );
        let date_key = hmac_sha256(format!("AWS4{}", self.config.secret_key).as_bytes(), date_stamp.as_bytes())?;
        let region_key = hmac_sha256(&date_key, self.config.region.as_bytes())?;
        let service_key = hmac_sha256(&region_key, b"s3")?;
        let signing_key = hmac_sha256(&service_key, b"aws4_request")?;
        let signature = hex_encode(&hmac_sha256(&signing_key, string_to_sign.as_bytes())?);
        Ok(format!(
            "{}?{}&X-Amz-Signature={}",
            &url[..url::Position::BeforeQuery],
            canonical_query,
            signature
        ))
    }

    /// Send one signed request. `query` is included in both the URL
    /// and the signature; `body` is the request payload (empty for
    /// GET / DELETE). `headers` are sent unsigned: the SigV4
    /// signature covers only `host`, `x-amz-content-sha256` and
    /// `x-amz-date`, so an unsigned header such as `Range` (ranged
    /// GetObject) or `x-amz-copy-source` (CopyObject) does not
    /// invalidate it.
    async fn signed_request(
        &self,
        method: reqwest::Method,
        url: url::Url,
        query: &[(String, String)],
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> UnifiedResult<reqwest::Response> {
        let mut url = url;
        if !query.is_empty() {
            let query_string = query
                .iter()
                .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
                .collect::<Vec<_>>()
                .join("&");
            url.set_query(Some(&query_string));
        }
        let payload_hash = sha256_hex(&body);
        let (authorization, amz_date) = self.sign(
            method.as_str(),
            &url,
            query,
            &payload_hash,
            Utc::now(),
        )?;
        let mut request = self
            .client
            .request(method, url)
            .header(header::AUTHORIZATION, authorization)
            .header("x-amz-date", amz_date)
            .header("x-amz-content-sha256", payload_hash);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = request
            .body(body)
            .send()
            .await
            .map_err(|error| {
                UnifiedError::internal(format!("S3 request failed: {error}"))
            })?;
        Ok(response)
    }
}

#[async_trait]
impl StorageProvider for S3Provider {
    fn kind(&self) -> StorageKind {
        StorageKind::S3
    }

    async fn upload(&self, key: &str, data: Vec<u8>) -> Result<String, UnifiedError> {
        validate_key(key)?;
        let url = self.object_url(key)?;
        let response = self
            .signed_request(reqwest::Method::PUT, url, &[], &[], data)
            .await?;
        let status = response.status();
        if status.is_success() {
            Ok(key.to_string())
        } else {
            Err(map_provider_status("S3", "upload", status))
        }
    }

    async fn download(&self, key: &str) -> Result<Vec<u8>, UnifiedError> {
        validate_key(key)?;
        let url = self.object_url(key)?;
        let response = self
            .signed_request(reqwest::Method::GET, url, &[], &[], Vec::new())
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(map_provider_status("S3", "download", status));
        }
        response.bytes().await.map(|bytes| bytes.to_vec()).map_err(|error| {
            UnifiedError::internal(format!("Cannot read S3 response body: {error}"))
        })
    }

    async fn download_range(&self, key: &str, start: u64, end: u64) -> Result<Vec<u8>, UnifiedError> {
        validate_key(key)?;
        let url = self.object_url(key)?;
        let range = format!("bytes={start}-{end}");
        let response = self
            .signed_request(
                reqwest::Method::GET,
                url,
                &[],
                &[("Range", range.as_str())],
                Vec::new(),
            )
            .await?;
        let status = response.status();
        if status.as_u16() == 416 {
            return Err(UnifiedError::new(
                format!("Range {start}-{end} is not satisfiable for S3 object \"{key}\""),
                416,
            ));
        }
        if !status.is_success() {
            return Err(map_provider_status("S3", "download_range", status));
        }
        response.bytes().await.map(|bytes| bytes.to_vec()).map_err(|error| {
            UnifiedError::internal(format!("Cannot read S3 response body: {error}"))
        })
    }

    async fn object_size(&self, key: &str) -> Result<u64, UnifiedError> {
        validate_key(key)?;
        let url = self.object_url(key)?;
        let response = self
            .signed_request(reqwest::Method::HEAD, url, &[], &[], Vec::new())
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(map_provider_status("S3", "object_size", status));
        }
        response
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| {
                UnifiedError::internal(format!(
                    "S3 HeadObject for \"{key}\" returned no Content-Length"
                ))
            })
    }

    async fn move_object(&self, from_key: &str, to_key: &str) -> Result<(), UnifiedError> {
        if from_key == to_key {
            return Ok(());
        }
        validate_key(from_key)?;
        validate_key(to_key)?;
        // Server-side copy: no bytes cross the wire. The copy
        // source is the path-style `/{bucket}/{key}` form, which
        // S3 accepts regardless of the endpoint style.
        let url = self.object_url(to_key)?;
        let copy_source = format!("/{}/{}", self.config.bucket, from_key);
        let response = self
            .signed_request(
                reqwest::Method::PUT,
                url,
                &[],
                &[("x-amz-copy-source", copy_source.as_str())],
                Vec::new(),
            )
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(map_provider_status("S3", "move_object", status));
        }
        // The copy is in place; deleting the source is the only
        // remaining step. When it fails the copy is left behind
        // and the error is returned (orphan sweeper reclaims it).
        self.delete(from_key).await
    }

    async fn delete(&self, key: &str) -> Result<(), UnifiedError> {
        validate_key(key)?;
        let url = self.object_url(key)?;
        let response = self
            .signed_request(reqwest::Method::DELETE, url, &[], &[], Vec::new())
            .await?;
        let status = response.status();
        if status.is_success() || status.as_u16() == 404 {
            Ok(())
        } else {
            Err(map_provider_status("S3", "delete", status))
        }
    }

    async fn signed_url(&self, key: &str, expires_secs: u64) -> Result<String, UnifiedError> {
        validate_key(key)?;
        let expires = expires_secs.clamp(1, 604_800);
        self.presigned_url(key, expires)
    }

    async fn test_connection(&self) -> Result<(), UnifiedError> {
        let url = self.bucket_url()?;
        let query = vec![
            ("list-type".to_string(), "2".to_string()),
            ("max-keys".to_string(), "0".to_string()),
        ];
        let response = self
            .signed_request(reqwest::Method::GET, url, &query, &[], Vec::new())
            .await?;
        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(map_provider_status("S3", "connection test", status))
        }
    }
}

// R2 provider (Cloudflare R2, S3-compatible).

/// Configuration for `R2Provider`. The endpoint is the account endpoint,
/// for example `https://<account-id>.r2.cloudflarestorage.com`. The
/// region is `auto` for R2 unless a jurisdiction requires otherwise.
#[derive(Debug, Clone)]
pub struct R2Config {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key: String,
    /// Never logged, never returned by any API response.
    pub secret_key: String,
}

/// Cloudflare R2. R2 speaks the S3 API, so this provider delegates
/// every operation to an inner `S3Provider` built with the R2 endpoint;
/// only the reported kind differs.
pub struct R2Provider {
    inner: S3Provider,
}

impl R2Provider {
    pub fn new(config: R2Config, extra_hosts: &[String]) -> UnifiedResult<Self> {
        let region = if config.region.trim().is_empty() {
            "auto".to_string()
        } else {
            config.region.clone()
        };
        let inner = S3Provider::new(
            S3Config {
                endpoint: config.endpoint,
                region,
                bucket: config.bucket,
                access_key: config.access_key,
                secret_key: config.secret_key,
            },
            extra_hosts,
        )?;
        Ok(Self { inner })
    }
}

#[async_trait]
impl StorageProvider for R2Provider {
    fn kind(&self) -> StorageKind {
        StorageKind::R2
    }

    async fn upload(&self, key: &str, data: Vec<u8>) -> Result<String, UnifiedError> {
        self.inner.upload(key, data).await
    }

    async fn download(&self, key: &str) -> Result<Vec<u8>, UnifiedError> {
        self.inner.download(key).await
    }

    async fn download_range(&self, key: &str, start: u64, end: u64) -> Result<Vec<u8>, UnifiedError> {
        self.inner.download_range(key, start, end).await
    }

    async fn object_size(&self, key: &str) -> Result<u64, UnifiedError> {
        self.inner.object_size(key).await
    }

    async fn move_object(&self, from_key: &str, to_key: &str) -> Result<(), UnifiedError> {
        self.inner.move_object(from_key, to_key).await
    }

    async fn delete(&self, key: &str) -> Result<(), UnifiedError> {
        self.inner.delete(key).await
    }

    async fn signed_url(&self, key: &str, expires_secs: u64) -> Result<String, UnifiedError> {
        self.inner.signed_url(key, expires_secs).await
    }

    async fn test_connection(&self) -> Result<(), UnifiedError> {
        self.inner.test_connection().await
    }
}

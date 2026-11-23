// service_monitor module -- External library requirement checks and reports.

use unified_core::modules::{ExternalLibraryProbe, ModuleRegistry};
use chrono::{DateTime, Utc};
use regex::Regex;
use semver::Version;
use serde::Serialize;
use std::collections::HashMap;
use std::process::Command;
use std::sync::{LazyLock, Mutex};

use super::*;

// ---------------------------------------------------------------------------
// External library checks.
//
// Every module may declare the system libraries / binaries it needs
// through `Module::requires_external_libraries`. This section checks
// those declarations against the host the application runs on and, on
// request, against remote servers over SSH.
//
// Design notes:
//   - The checker never runs an arbitrary command. Local probes only
//     ever run a binary from the fixed allowlist in `probe_commands`
//     with a fixed version flag, and remote probes only ever run those
//     same commands through the system `ssh` client. A declared
//     library with no allowlisted probe is reported as unverifiable
//     instead of being probed some other way.
//   - No OS package management lives here: there is no install,
//     upgrade, or remove action anywhere in this section, and no
//     flow that needs root. The checker reports; the operator acts.
//   - Results are cached in memory. The status endpoint and the
//     Extensions Manager read the same cache, so the library data has
//     exactly one shape everywhere it appears.
// ---------------------------------------------------------------------------

/// Event emitted once per library that a check found missing or
/// outdated. Payload: `{"library", "required_version",
/// "found_version", "status", "target", "required_by", "reason",
/// "checked_at"}`. There was no pre-existing health/library event
/// from this module to reuse (it previously only emitted
/// `app.reload`), so this name follows the existing
/// `<domain>.<action>` naming (`cluster.health.changed`,
/// `module.enabled`) and lives next to the code that emits it, the
/// same way `MODULE_AUTO_ACTIVATED_EVENT` lives in
/// `unified_core::modules`.
pub const LIBRARY_CHECK_FAILED_EVENT: &str = "library.check.failed";

/// One declared external library requirement, aggregated across all
/// modules that declare it: the highest minimum version wins, and
/// every requiring module is listed so notices can name the owners.
#[derive(Debug, Clone)]
pub struct AggregatedRequirement {
    pub name: String,
    pub minimum_version: Option<String>,
    pub reasons: Vec<String>,
    pub required_by: Vec<String>,
    pub probe: Option<ExternalLibraryProbe>,
}

/// Result of checking one library on one target. `status` is exactly
/// one of `"ok"`, `"missing"`, or `"outdated"`: a library whose
/// binary runs but whose version cannot be parsed is reported as
/// `"outdated"` (the requirement cannot be proven met), and a
/// library with no registered probe is reported as `"missing"` with
/// a detail that says so, so neither case is ever silently "ok".
#[derive(Debug, Clone, Serialize)]
pub struct LibraryCheckResult {
    pub library: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub found_version: Option<String>,
    /// `"ok"`, `"missing"`, or `"outdated"`.
    pub status: String,
    /// Ids of the modules that require this library.
    pub required_by: Vec<String>,
    /// Why the library is needed (first declaring module's reason).
    pub reason: String,
    /// What the checker found, in plain language.
    pub detail: String,
    /// `"local"`, a registered SSH server id, or `ssh:user@host`.
    pub target: String,
    pub checked_at: DateTime<Utc>,
}

/// One completed check run against one target.
#[derive(Debug, Clone, Serialize)]
pub struct LibraryCheckReport {
    /// `"local"`, a registered SSH server id, or `ssh:user@host`.
    pub target: String,
    /// `"ok"` (nothing missing/outdated), `"warning"`, or `"error"`
    /// (the target itself could not be checked at all).
    pub status: String,
    pub checked_at: DateTime<Utc>,
    pub ok: usize,
    pub missing: usize,
    pub outdated: usize,
    /// Set only when `status` is `"error"`; then `results` is empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub results: Vec<LibraryCheckResult>,
}

/// Summary of the most recent local check, embedded in the status
/// response so health data and the library endpoints never diverge.
#[derive(Debug, Clone, Serialize)]
pub struct LibraryCheckSummary {
    /// False until the first check has run.
    pub checked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<DateTime<Utc>>,
    /// `"ok"`, `"warning"`, or `"unknown"` (never checked).
    pub status: String,
    pub total: usize,
    pub ok: usize,
    pub missing: usize,
    pub outdated: usize,
}

impl LibraryCheckSummary {
    pub(crate) fn unknown() -> Self {
        Self {
            checked: false,
            checked_at: None,
            status: "unknown".to_string(),
            total: 0,
            ok: 0,
            missing: 0,
            outdated: 0,
        }
    }

    pub(crate) fn from_report(report: &LibraryCheckReport) -> Self {
        Self {
            checked: true,
            checked_at: Some(report.checked_at),
            status: report.status.clone(),
            total: report.results.len(),
            ok: report.ok,
            missing: report.missing,
            outdated: report.outdated,
        }
    }
}

/// Fixed allowlist of version probes: (binary, version arguments).
/// ImageMagick has two candidates because version 7 renamed
/// `convert` to `magick`; the checker tries them in order.
pub(crate) fn probe_commands(probe: ExternalLibraryProbe) -> &'static [(&'static str, &'static [&'static str])]
{
    match probe {
        ExternalLibraryProbe::ImageMagick => {
            &[("magick", &["--version"]), ("convert", &["--version"])]
        }
        ExternalLibraryProbe::Ffmpeg => &[("ffmpeg", &["-version"])],
        ExternalLibraryProbe::Ghostscript => &[("gs", &["--version"])],
        ExternalLibraryProbe::ExifTool => &[("exiftool", &["-ver"])],
        ExternalLibraryProbe::Poppler => &[("pdftotext", &["-v"])],
        ExternalLibraryProbe::LibreOffice => &[("soffice", &["--version"])],
        ExternalLibraryProbe::Python3 => &[("python3", &["--version"])],
        ExternalLibraryProbe::Node => &[("node", &["--version"])],
        ExternalLibraryProbe::Git => &[("git", &["--version"])],
        ExternalLibraryProbe::Curl => &[("curl", &["--version"])],
        ExternalLibraryProbe::Wget => &[("wget", &["--version"])],
        ExternalLibraryProbe::Rsync => &[("rsync", &["--version"])],
        ExternalLibraryProbe::Tar => &[("tar", &["--version"])],
        ExternalLibraryProbe::Unzip => &[("unzip", &["-v"])],
        ExternalLibraryProbe::Zip => &[("zip", &["-v"])],
        ExternalLibraryProbe::Jpegoptim => &[("jpegoptim", &["--version"])],
        ExternalLibraryProbe::Pngquant => &[("pngquant", &["--version"])],
        ExternalLibraryProbe::Optipng => &[("optipng", &["--version"])],
        ExternalLibraryProbe::Cwebp => &[("cwebp", &["-version"])],
        ExternalLibraryProbe::Gifsicle => &[("gifsicle", &["--version"])],
        ExternalLibraryProbe::Sqlite3 => &[("sqlite3", &["--version"])],
        ExternalLibraryProbe::PostgresClient => &[("psql", &["--version"])],
        ExternalLibraryProbe::MysqlClient => &[("mysql", &["--version"])],
        ExternalLibraryProbe::RedisCli => &[("redis-cli", &["--version"])],
        ExternalLibraryProbe::Wkhtmltopdf => &[("wkhtmltopdf", &["--version"])],
        ExternalLibraryProbe::RsvgConvert => &[("rsvg-convert", &["--version"])],
        ExternalLibraryProbe::Vips => &[("vips", &["--version"])],
        ExternalLibraryProbe::Inkscape => &[("inkscape", &["--version"])],
    }
}

/// Name-based probe lookup for requirements declared without an
/// explicit probe. Only names in this table resolve; anything else
/// is unverifiable rather than guessed at.

pub(crate) fn probe_for_name(name: &str) -> Option<ExternalLibraryProbe> {
    match name.to_lowercase().as_str() {
        "imagemagick" | "magick" | "convert" => Some(ExternalLibraryProbe::ImageMagick),
        "ffmpeg" => Some(ExternalLibraryProbe::Ffmpeg),
        "ghostscript" | "gs" => Some(ExternalLibraryProbe::Ghostscript),
        "exiftool" => Some(ExternalLibraryProbe::ExifTool),
        "poppler" | "pdftotext" | "poppler-utils" => Some(ExternalLibraryProbe::Poppler),
        "libreoffice" | "soffice" => Some(ExternalLibraryProbe::LibreOffice),
        "python3" | "python" => Some(ExternalLibraryProbe::Python3),
        "node" | "nodejs" => Some(ExternalLibraryProbe::Node),
        "git" => Some(ExternalLibraryProbe::Git),
        "curl" => Some(ExternalLibraryProbe::Curl),
        "wget" => Some(ExternalLibraryProbe::Wget),
        "rsync" => Some(ExternalLibraryProbe::Rsync),
        "tar" => Some(ExternalLibraryProbe::Tar),
        "unzip" => Some(ExternalLibraryProbe::Unzip),
        "zip" => Some(ExternalLibraryProbe::Zip),
        "jpegoptim" => Some(ExternalLibraryProbe::Jpegoptim),
        "pngquant" => Some(ExternalLibraryProbe::Pngquant),
        "optipng" => Some(ExternalLibraryProbe::Optipng),
        "cwebp" | "libwebp" => Some(ExternalLibraryProbe::Cwebp),
        "gifsicle" => Some(ExternalLibraryProbe::Gifsicle),
        "sqlite3" | "sqlite" => Some(ExternalLibraryProbe::Sqlite3),
        "psql" | "postgresql" | "postgresql-client" => {
            Some(ExternalLibraryProbe::PostgresClient)
        }
        "mysql" | "mysql-client" => Some(ExternalLibraryProbe::MysqlClient),
        "redis-cli" | "redis" => Some(ExternalLibraryProbe::RedisCli),
        "wkhtmltopdf" => Some(ExternalLibraryProbe::Wkhtmltopdf),
        "rsvg-convert" | "librsvg" => Some(ExternalLibraryProbe::RsvgConvert),
        "vips" | "libvips" => Some(ExternalLibraryProbe::Vips),
        "inkscape" => Some(ExternalLibraryProbe::Inkscape),
        _ => None,
    }
}

/// Probe commands for one aggregated requirement: the explicit probe
/// first, else the name lookup, else nothing (unverifiable).
pub(crate) fn probes_for(
    requirement: &AggregatedRequirement,
) -> Vec<(&'static str, &'static [&'static str])> {
    let probe = requirement
        .probe
        .or_else(|| probe_for_name(&requirement.name));
    match probe {
        Some(probe) => probe_commands(probe).to_vec(),
        None => Vec::new(),
    }
}

/// First `major.minor[.patch]` found in probe output, as a semver
/// version plus the matched text (`"7.1.1"`, `"6.9"`). Tool output is
/// free-form (`"Version: ImageMagick 7.1.1-43 ..."`,
/// `"ffmpeg version 6.1.1 ..."`) so only the first version-shaped
/// token is trusted, never the whole line.

pub(crate) static VERSION_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\d+)\.(\d+)(?:\.(\d+))?").expect("version pattern is valid")
});

pub(crate) fn parse_loose_version(text: &str) -> Option<(Version, String)> {
    let captures = VERSION_PATTERN.captures(text)?;
    let major: u64 = captures[1].parse().ok()?;
    let minor: u64 = captures[2].parse().ok()?;
    let patch: u64 = captures
        .get(3)
        .map_or(0, |part| part.as_str().parse().unwrap_or(0));
    let display = if captures.get(3).is_some() {
        format!("{major}.{minor}.{patch}")
    } else {
        format!("{major}.{minor}")
    };
    Some((Version::new(major, minor, patch), display))
}

/// Parse a declared minimum version (`"7"`, `"7.1"`, `"7.1.0"`).
/// A bare major version means `major.0.0`.
pub(crate) fn parse_minimum_version(raw: &str) -> Option<Version> {
    let trimmed = raw.trim();
    let padded = if trimmed.chars().all(|c| c.is_ascii_digit()) {
        format!("{trimmed}.0")
    } else {
        trimmed.to_string()
    };
    parse_loose_version(&padded).map(|(version, _)| version)
}

/// Aggregate every module's external library declarations: one entry
/// per library (case-insensitive), highest minimum version wins,
/// every requiring module and reason collected.
pub fn aggregate_external_requirements(
    registry: &ModuleRegistry,
) -> Vec<AggregatedRequirement> {
    let mut out: Vec<AggregatedRequirement> = Vec::new();
    for module in registry.list() {
        for requirement in module.requires_external_libraries() {
            let key = requirement.name.to_lowercase();
            match out
                .iter()
                .position(|entry: &AggregatedRequirement| entry.name.to_lowercase() == key)
            {
                Some(index) => {
                    let entry = &mut out[index];
                    let module_id = module.id().to_string();
                    if !entry.required_by.contains(&module_id) {
                        entry.required_by.push(module_id);
                    }
                    let reason = requirement.reason.to_string();
                    if !entry.reasons.contains(&reason) {
                        entry.reasons.push(reason);
                    }
                    if entry.probe.is_none() {
                        entry.probe = requirement.probe;
                    }
                    if let Some(candidate) = requirement.minimum_version {
                        entry.minimum_version = Some(match &entry.minimum_version {
                            None => candidate.to_string(),
                            Some(current) => {
                                let current_version = parse_minimum_version(current);
                                let candidate_version = parse_minimum_version(candidate);
                                match (current_version, candidate_version) {
                                    (Some(a), Some(b)) if b > a => candidate.to_string(),
                                    // The stored minimum is unparseable, so
                                    // it proves nothing; a parseable
                                    // candidate replaces it instead of
                                    // being shadowed by the junk value.
                                    (None, Some(_)) => candidate.to_string(),
                                    _ => current.clone(),
                                }
                            }
                        });
                    }
                }
                None => out.push(AggregatedRequirement {
                    name: requirement.name.to_string(),
                    minimum_version: requirement.minimum_version.map(str::to_string),
                    reasons: vec![requirement.reason.to_string()],
                    required_by: vec![module.id().to_string()],
                    probe: requirement.probe,
                }),
            }
        }
    }
    for entry in &mut out {
        entry.required_by.sort();
        entry.reasons.sort();
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

/// Cached library check results: the latest local report plus one
/// report per remote target label. In-memory only; a restart clears
/// it and the next read runs a fresh local check.

pub(crate) struct LibraryCheckCache {
    pub(crate) local: Option<LibraryCheckReport>,
    pub(crate) remote: HashMap<String, LibraryCheckReport>,
}

pub(crate) static LIBRARY_CHECK_CACHE: LazyLock<Mutex<LibraryCheckCache>> =
    LazyLock::new(|| {
        Mutex::new(LibraryCheckCache {
            local: None,
            remote: HashMap::new(),
        })
    });


pub(crate) fn store_library_report(report: &LibraryCheckReport) {
    let mut cache = LIBRARY_CHECK_CACHE
        .lock()
        .expect("library check cache lock poisoned");
    if report.target == "local" {
        cache.local = Some(report.clone());
    } else {
        cache.remote.insert(report.target.clone(), report.clone());
    }
}

/// Latest local check results, or an empty list when no check has
/// run yet. Read by the Extensions Manager for per-module notices.
pub fn latest_library_results() -> Vec<LibraryCheckResult> {
    let cache = LIBRARY_CHECK_CACHE
        .lock()
        .expect("library check cache lock poisoned");
    cache
        .local
        .as_ref()
        .map(|report| report.results.clone())
        .unwrap_or_default()
}

/// Latest local check summary, for the status response.
pub fn latest_library_summary() -> LibraryCheckSummary {
    let cache = LIBRARY_CHECK_CACHE
        .lock()
        .expect("library check cache lock poisoned");
    cache
        .local
        .as_ref()
        .map(LibraryCheckSummary::from_report)
        .unwrap_or_else(LibraryCheckSummary::unknown)
}

/// Latest reports for every checked target, local first, then remote
/// targets sorted by label.
pub fn latest_library_reports() -> Vec<LibraryCheckReport> {
    let cache = LIBRARY_CHECK_CACHE
        .lock()
        .expect("library check cache lock poisoned");
    let mut reports: Vec<LibraryCheckReport> = Vec::new();
    if let Some(local) = &cache.local {
        reports.push(local.clone());
    }
    let mut remote: Vec<LibraryCheckReport> = cache.remote.values().cloned().collect();
    remote.sort_by(|a, b| a.target.cmp(&b.target));
    reports.extend(remote);
    reports
}

/// Admin notice text for a failing check result, for example
/// `"ImageMagick 7+ required, 6.9.11 found"`. Returns `None` for an
/// `"ok"` result.
pub fn library_notice_message(result: &LibraryCheckResult) -> Option<String> {
    match result.status.as_str() {
        "missing" => Some(match &result.required_version {
            Some(required) => format!("{} {required}+ required, not installed", result.library),
            None => format!("{} required, not installed", result.library),
        }),
        "outdated" => Some(match (&result.required_version, &result.found_version) {
            (Some(required), Some(found)) => {
                format!("{} {required}+ required, {found} found", result.library)
            }
            (Some(required), None) => format!(
                "{} {required}+ required, installed version could not be determined",
                result.library
            ),
            (None, _) => format!(
                "{} installed version could not be determined",
                result.library
            ),
        }),
        _ => None,
    }
}

/// Combine a finished process's stdout and stderr into one text.
/// Version output lands on either stream depending on the tool
/// (`pdftotext -v` writes to stderr), so both are read.
pub(crate) fn combined_output(output: &std::process::Output) -> String {
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&stderr);
    }
    text
}

pub(crate) fn truncate_text(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        trimmed.to_string()
    } else {
        trimmed.chars().take(max_chars).collect()
    }
}

pub(crate) fn missing_result(
    requirement: &AggregatedRequirement,
    target: &str,
    detail: &str,
    checked_at: DateTime<Utc>,
) -> LibraryCheckResult {
    LibraryCheckResult {
        library: requirement.name.clone(),
        required_version: requirement.minimum_version.clone(),
        found_version: None,
        status: "missing".to_string(),
        required_by: requirement.required_by.clone(),
        reason: requirement.reasons.first().cloned().unwrap_or_default(),
        target: target.to_string(),
        detail: detail.to_string(),
        checked_at,
    }
}

/// Evaluate probe output for one requirement: parse the installed
/// version and compare it with the declared minimum. A binary that
/// runs but reports no parseable version is `"outdated"` -- the
/// requirement cannot be proven met.

pub(crate) fn evaluate_found(
    requirement: &AggregatedRequirement,
    output: &str,
    target: &str,
    checked_at: DateTime<Utc>,
) -> LibraryCheckResult {
    let base = LibraryCheckResult {
        library: requirement.name.clone(),
        required_version: requirement.minimum_version.clone(),
        found_version: None,
        status: "ok".to_string(),
        required_by: requirement.required_by.clone(),
        reason: requirement.reasons.first().cloned().unwrap_or_default(),
        target: target.to_string(),
        detail: String::new(),
        checked_at,
    };
    let Some((found_version, found_text)) = parse_loose_version(output) else {
        return LibraryCheckResult {
            status: "outdated".to_string(),
            detail: "The library appears to be installed, but its version could not be parsed from the probe output.".to_string(),
            ..base
        };
    };
    let mut result = LibraryCheckResult {
        found_version: Some(found_text),
        ..base
    };
    match requirement
        .minimum_version
        .as_deref()
        .and_then(parse_minimum_version)
    {
        None => {
            result.detail = match &requirement.minimum_version {
                Some(minimum) => format!(
                    "Minimum version \"{minimum}\" could not be parsed, so only presence was checked."
                ),
                None => "Installed.".to_string(),
            };
        }
        Some(minimum) => {
            if found_version >= minimum {
                result.detail = "Installed version meets the minimum.".to_string();
            } else {
                result.status = "outdated".to_string();
                result.detail = "Installed version is older than the required minimum.".to_string();
            }
        }
    }
    result
}

/// Run one probe binary locally. Returns the combined output when
/// the binary ran (whatever its exit code), or `None` when the
/// binary is not installed or could not be started.

pub(crate) fn run_local_probe(binary: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(binary).args(args).output().ok()?;
    Some(combined_output(&output))
}

pub(crate) fn check_requirement_local(
    requirement: &AggregatedRequirement,
    checked_at: DateTime<Utc>,
) -> LibraryCheckResult {
    let probes = probes_for(requirement);
    if probes.is_empty() {
        return missing_result(
            requirement,
            "local",
            &format!(
                "No version probe is registered for \"{}\"; add one to the probe allowlist before this requirement can be verified.",
                requirement.name
            ),
            checked_at,
        );
    }
    let mut tried: Vec<&str> = Vec::new();
    for (binary, args) in probes {
        tried.push(binary);
        if let Some(output) = run_local_probe(binary, args) {
            return evaluate_found(requirement, &output, "local", checked_at);
        }
    }
    missing_result(
        requirement,
        "local",
        &format!(
            "Probe binaries ({}) were not found on PATH",
            tried.join(", ")
        ),
        checked_at,
    )
}


pub(crate) fn build_report(
    target: &str,
    checked_at: DateTime<Utc>,
    results: Vec<LibraryCheckResult>,
) -> LibraryCheckReport {
    let ok = results.iter().filter(|r| r.status == "ok").count();
    let missing = results.iter().filter(|r| r.status == "missing").count();
    let outdated = results.iter().filter(|r| r.status == "outdated").count();
    let status = if missing == 0 && outdated == 0 {
        "ok"
    } else {
        "warning"
    };
    LibraryCheckReport {
        target: target.to_string(),
        status: status.to_string(),
        checked_at,
        ok,
        missing,
        outdated,
        error: None,
        results,
    }
}


pub(crate) fn error_report(target: &str, detail: &str) -> LibraryCheckReport {
    LibraryCheckReport {
        target: target.to_string(),
        status: "error".to_string(),
        checked_at: Utc::now(),
        ok: 0,
        missing: 0,
        outdated: 0,
        error: Some(detail.to_string()),
        results: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::modules::{ExternalLibraryRequirement, Module};
    use std::sync::Arc;

    struct StubModule {
        id: &'static str,
        requirements: &'static [ExternalLibraryRequirement],
    }

    impl Module for StubModule {
        fn id(&self) -> &'static str {
            self.id
        }

        fn name(&self) -> &'static str {
            "Stub"
        }

        fn description(&self) -> &'static str {
            "Test stub module"
        }

        fn requires_external_libraries(&self) -> &[ExternalLibraryRequirement] {
            self.requirements
        }
    }

    fn aggregated_from(
        first: &'static [ExternalLibraryRequirement],
        second: &'static [ExternalLibraryRequirement],
    ) -> Vec<AggregatedRequirement> {
        let registry = ModuleRegistry::new();
        registry.register(Arc::new(StubModule {
            id: "first",
            requirements: first,
        }));
        registry.register(Arc::new(StubModule {
            id: "second",
            requirements: second,
        }));
        aggregate_external_requirements(&registry)
    }

    fn minimum_for(requirements: &[AggregatedRequirement], name: &str) -> Option<String> {
        requirements
            .iter()
            .find(|requirement| requirement.name == name)
            .and_then(|requirement| requirement.minimum_version.clone())
    }

    const GIT_UNPARSEABLE: &[ExternalLibraryRequirement] =
        &[ExternalLibraryRequirement::new("git", Some("not-a-version"), "needs git")];
    const GIT_ALSO_UNPARSEABLE: &[ExternalLibraryRequirement] =
        &[ExternalLibraryRequirement::new("git", Some("also-bad"), "needs git")];
    const GIT_V1: &[ExternalLibraryRequirement] =
        &[ExternalLibraryRequirement::new("git", Some("1.0"), "needs git")];
    const GIT_V2: &[ExternalLibraryRequirement] =
        &[ExternalLibraryRequirement::new("git", Some("2.0"), "needs git")];
    const GIT_NO_MINIMUM: &[ExternalLibraryRequirement] =
        &[ExternalLibraryRequirement::new("git", None, "needs git")];

    #[test]
    fn unparseable_minimum_is_replaced_by_parseable_candidate() {
        let aggregated = aggregated_from(GIT_UNPARSEABLE, GIT_V2);
        assert_eq!(minimum_for(&aggregated, "git"), Some("2.0".to_string()));
    }

    #[test]
    fn parseable_minimum_keeps_highest_candidate() {
        let aggregated = aggregated_from(GIT_V1, GIT_V2);
        assert_eq!(minimum_for(&aggregated, "git"), Some("2.0".to_string()));
    }

    #[test]
    fn parseable_minimum_keeps_current_over_lower_candidate() {
        let aggregated = aggregated_from(GIT_V2, GIT_V1);
        assert_eq!(minimum_for(&aggregated, "git"), Some("2.0".to_string()));
    }

    #[test]
    fn parseable_minimum_keeps_current_over_unparseable_candidate() {
        let aggregated = aggregated_from(GIT_V2, GIT_UNPARSEABLE);
        assert_eq!(minimum_for(&aggregated, "git"), Some("2.0".to_string()));
    }

    #[test]
    fn both_unparseable_minimums_keep_first() {
        let aggregated = aggregated_from(GIT_UNPARSEABLE, GIT_ALSO_UNPARSEABLE);
        assert_eq!(
            minimum_for(&aggregated, "git"),
            Some("not-a-version".to_string())
        );
    }

    #[test]
    fn missing_minimum_takes_candidate() {
        let aggregated = aggregated_from(GIT_NO_MINIMUM, GIT_V2);
        assert_eq!(minimum_for(&aggregated, "git"), Some("2.0".to_string()));
    }

    fn check_result(status: &str) -> LibraryCheckResult {
        LibraryCheckResult {
            library: "git".to_string(),
            required_version: None,
            found_version: None,
            status: status.to_string(),
            required_by: Vec::new(),
            reason: String::new(),
            detail: String::new(),
            target: "local".to_string(),
            checked_at: Utc::now(),
        }
    }

    #[test]
    fn parse_loose_version_reads_dotted_versions() {
        assert_eq!(
            parse_loose_version("1.2.3"),
            Some((Version::new(1, 2, 3), "1.2.3".to_string()))
        );
        assert_eq!(
            parse_loose_version("v2.0"),
            Some((Version::new(2, 0, 0), "2.0".to_string()))
        );
        assert_eq!(
            parse_loose_version("release 2.0.1-beta"),
            Some((Version::new(2, 0, 1), "2.0.1".to_string()))
        );
    }

    #[test]
    fn parse_loose_version_rejects_text_without_version() {
        assert_eq!(parse_loose_version("nothing here"), None);
        assert_eq!(parse_loose_version(""), None);
    }

    #[test]
    fn parse_minimum_version_pads_bare_major() {
        assert_eq!(parse_minimum_version("1"), Some(Version::new(1, 0, 0)));
        assert_eq!(parse_minimum_version("2.5"), Some(Version::new(2, 5, 0)));
        assert_eq!(parse_minimum_version(" 3 "), Some(Version::new(3, 0, 0)));
        assert_eq!(parse_minimum_version("abc"), None);
    }

    #[test]
    fn truncate_text_trims_and_clips() {
        assert_eq!(truncate_text("  hello world  ", 5), "hello");
        assert_eq!(truncate_text("hi", 10), "hi");
        assert_eq!(truncate_text("hello", 5), "hello");
    }

    #[test]
    fn library_notice_message_for_missing() {
        assert_eq!(
            library_notice_message(&check_result("missing")),
            Some("git required, not installed".to_string())
        );
        let mut result = check_result("missing");
        result.required_version = Some("2.0".to_string());
        assert_eq!(
            library_notice_message(&result),
            Some("git 2.0+ required, not installed".to_string())
        );
    }

    #[test]
    fn library_notice_message_for_outdated_and_ok() {
        let mut result = check_result("outdated");
        result.required_version = Some("2.0".to_string());
        result.found_version = Some("1.9".to_string());
        assert_eq!(
            library_notice_message(&result),
            Some("git 2.0+ required, 1.9 found".to_string())
        );
        assert_eq!(library_notice_message(&check_result("ok")), None);
    }
}

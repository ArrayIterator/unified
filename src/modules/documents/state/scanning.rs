//! Documents state.

use super::*;

impl DocumentsState {

    /// Run every registered scanner over `bytes` and combine the
    /// verdicts: any `Infected` wins, otherwise any `Error` wins,
    /// otherwise `Clean`. Scanners run in sorted scanner-name
    /// order (see `scanner_names`), so which threat is reported
    /// first is deterministic instead of depending on the
    /// `DashMap` iteration order. With no scanner registered the
    /// caller records `Skipped` (see `scan_document_bytes`).
    /// Returns the verdict together with the name(s) of the
    /// scanner(s) that produced it: the first infected scanner, the
    /// first erroring scanner, or every scanner when all are clean.
    /// Run every registered scanner through `scan_one` and combine
    /// the verdicts: any `Infected` wins, otherwise any `Error`
    /// wins, otherwise `Clean`. Scanners run in sorted scanner-name
    /// order (see `scanner_names`), so which threat is reported
    /// first is deterministic instead of depending on the
    /// `DashMap` iteration order. With no scanner registered the
    /// caller records `Skipped` (see `scan_document_bytes`).
    /// Returns the verdict together with the name(s) of the
    /// scanner(s) that produced it: the first infected scanner, the
    /// first erroring scanner, or every scanner when all are clean.
    fn run_scanners_with(
        &self,
        mut scan_one: impl FnMut(&Arc<dyn DocumentScanner>) -> ScanVerdict,
    ) -> Option<(ScanVerdict, Vec<String>)> {
        if self.scanners.is_empty() {
            return None;
        }
        let mut first_error: Option<(ScanVerdict, String)> = None;
        let mut clean: Vec<String> = Vec::new();
        for name in self.scanner_names() {
            let Some(scanner) = self.scanners.get(&name) else {
                continue;
            };
            match scan_one(&scanner) {
                verdict @ ScanVerdict::Infected { .. } => {
                    return Some((verdict, vec![name]));
                }
                verdict @ ScanVerdict::Error { .. } => {
                    if first_error.is_none() {
                        first_error = Some((verdict, name));
                    }
                }
                ScanVerdict::Clean => clean.push(name),
            }
        }
        if let Some((verdict, name)) = first_error {
            return Some((verdict, vec![name]));
        }
        Some((ScanVerdict::Clean, clean))
    }


    /// Run every registered scanner over `bytes` (see
    /// [`run_scanners_with`]).
    fn run_scanners(&self, bytes: &[u8]) -> Option<(ScanVerdict, Vec<String>)> {
        self.run_scanners_with(|scanner| scanner.scan(bytes))
    }


    /// Run every registered scanner over the file at `path`, opening
    /// it fresh for each scanner so a scanner that streams
    /// (`scan_reader`) never holds the whole file in RAM. A file
    /// that cannot be opened is an `Error` verdict for every
    /// scanner (fail closed). A scanner that does not stream
    /// (`streams_scan` is false) gets an `Error` verdict when the
    /// file is larger than `max_chunk_ram_bytes` instead of loading
    /// it - the cap is never exceeded on the scanner's behalf.
    fn run_scanners_file(&self, path: &Path, size: u64) -> Option<(ScanVerdict, Vec<String>)> {
        let cap = self.settings().max_chunk_ram_bytes;
        self.run_scanners_with(|scanner| {
            if size > cap && !scanner.streams_scan() {
                return ScanVerdict::Error {
                    reason: format!(
                        "File is {size} bytes, exceeding the {cap} byte in-RAM scan cap, and scanner \"{}\" does not stream",
                        scanner.name()
                    ),
                };
            }
            match std::fs::File::open(path) {
                Ok(mut file) => scanner.scan_reader(&mut file),
                Err(error) => ScanVerdict::Error {
                    reason: format!("Cannot open file for scanning: {error}"),
                },
            }
        })
    }


    /// Record one scan outcome: map the combined verdict to the
    /// stored status, emit `document.scan.completed`, and return the
    /// recorded status, threat label, and the name(s) of the
    /// scanner(s) that produced the verdict (`None` when no scanner
    /// is configured and the verdict is `Skipped`). The scanner is
    /// recorded by name - never the literal `"registered"` - so the
    /// audit trail shows which scanner actually ran.
    fn record_scan(
        &self,
        document_id: &str,
        version: u32,
        outcome: Option<(ScanVerdict, Vec<String>)>,
    ) -> (ScanStatus, Option<String>, Option<String>) {
        let (status, threat, scanner) = match outcome {
            None => (ScanStatus::Skipped, None, None),
            Some((ScanVerdict::Clean, names)) => (ScanStatus::Clean, None, Some(names.join(","))),
            Some((ScanVerdict::Infected { threat }, names)) => {
                (ScanStatus::Infected, Some(threat), Some(names.join(",")))
            }
            Some((ScanVerdict::Error { reason }, names)) => {
                (ScanStatus::Error, Some(reason), Some(names.join(",")))
            }
        };
        let verdict = match &status {
            ScanStatus::Clean => "clean",
            ScanStatus::Infected => "infected",
            ScanStatus::Error => "error",
            ScanStatus::Skipped => "skipped",
            ScanStatus::Pending => "pending",
        };
        self.emit(
            events::SCAN_COMPLETED,
            json!({
                "document_id": document_id,
                "version": version,
                "verdict": verdict,
                "threat": threat,
                "scanner": scanner.clone(),
            }),
        );
        (status, threat, scanner)
    }


    /// Scan `bytes` for a document version, emit
    /// `document.scan.completed`, and return the recorded status,
    /// threat label, and the name(s) of the scanner(s) that
    /// produced the verdict (`None` when no scanner is configured
    /// and the verdict is `Skipped`).
    pub(crate) fn scan_document_bytes(
        &self,
        document_id: &str,
        version: u32,
        bytes: &[u8],
    ) -> (ScanStatus, Option<String>, Option<String>) {
        let outcome = self.run_scanners(bytes);
        self.record_scan(document_id, version, outcome)
    }


    /// Scan the file at `path` for a document version, without
    /// loading it into RAM (see [`run_scanners_file`]). With no
    /// scanner configured the verdict is `Skipped` and the file is
    /// never opened. Same event and return shape as
    /// [`scan_document_bytes`].
    pub(crate) fn scan_document_file(
        &self,
        document_id: &str,
        version: u32,
        path: &Path,
        size: u64,
    ) -> (ScanStatus, Option<String>, Option<String>) {
        let outcome = self.run_scanners_file(path, size);
        self.record_scan(document_id, version, outcome)
    }
}

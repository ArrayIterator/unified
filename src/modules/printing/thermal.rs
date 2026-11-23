//! printing - thermal.

use unified_core::error::{UnifiedError, UnifiedResult};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};
use super::{events::*, paper::*, profiles::*, jobs::*, state::*, auth::*, hooks::*, handlers_paper::*, handlers_profiles::*, handlers_jobs::*, module::*};



// ---------------------------------------------------------------------------
// Thermal (ESC/POS) rendering
// ---------------------------------------------------------------------------
/// ESC/POS control bytes. These are the published ESC/POS command
/// set, not application behaviour: `ESC @` initialise, `ESC a n`
/// alignment, `ESC E n` bold, `GS ! n` character size, `ESC t n`
/// code page, `ESC p m t1 t2` drawer kick, `GS V m n` cut.
pub(crate) const ESC: u8 = 0x1B;

pub(crate) const GS: u8 = 0x1D;


/// One line of a thermal document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThermalLine {
    pub text: String,
    /// `left`, `center`, or `right`. Defaults to `left`.
    #[serde(default = "default_thermal_align")]
    pub align: String,
    #[serde(default)]
    pub bold: bool,
    /// Double width and double height.
    #[serde(default)]
    pub double_size: bool,
}


pub(crate) fn default_thermal_align() -> String {
    "left".to_string()
}


/// A whole thermal document: lines plus the page-level options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThermalDocument {
    pub lines: Vec<ThermalLine>,
    pub width_columns: u32,
    #[serde(default)]
    pub cut: bool,
    #[serde(default)]
    pub drawer_kick: bool,
}


/// Validate one thermal line: text at most 1000 chars, alignment
/// from the closed vocabulary.
pub(crate) fn validate_thermal_line(line: &ThermalLine) -> UnifiedResult<()> {
    if line.text.chars().count() > 1000 {
        return Err(UnifiedError::bad_request(
            "A thermal line must be at most 1000 characters",
        ));
    }
    if !["left", "center", "right"].contains(&line.align.as_str()) {
        return Err(UnifiedError::bad_request(
            "Thermal line alignment must be left, center, or right",
        ));
    }
    Ok(())
}


/// Wrap `text` to `width` columns, splitting over-long words.
/// Character-count based: ESC/POS printers are monospace, so one
/// character is one column for the Latin scripts these printers
/// target.
pub(crate) fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    for raw in text.split('\n') {
        if raw.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in raw.split(' ') {
            let word_len = word.chars().count();
            if word_len > width {
                if !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                }
                let mut rest = word;
                while rest.chars().count() > width {
                    let cut: String = rest.chars().take(width).collect();
                    lines.push(cut.clone());
                    rest = &rest[cut.len()..];
                }
                current = rest.to_string();
                continue;
            }
            if current.is_empty() {
                current = word.to_string();
            } else if current.chars().count() + 1 + word_len <= width {
                current.push(' ');
                current.push_str(word);
            } else {
                lines.push(std::mem::take(&mut current));
                current = word.to_string();
            }
        }
        lines.push(current);
    }
    lines
}


/// Pad one already-wrapped line to `width` columns per its
/// alignment. Lines at or over the width are returned unchanged.
pub(crate) fn align_text(text: &str, width: usize, align: &str) -> String {
    let len = text.chars().count();
    if len >= width {
        return text.to_string();
    }
    match align {
        "center" => {
            let left = (width - len) / 2;
            format!("{}{}", " ".repeat(left), text)
        }
        "right" => format!("{}{}", " ".repeat(width - len), text),
        _ => text.to_string(),
    }
}


/// Render a thermal document as human-readable text: the preview
/// shown before printing, with alignment applied and no control
/// bytes.
pub fn render_thermal_preview(doc: &ThermalDocument) -> String {
    let width = doc.width_columns.max(16) as usize;
    let mut out = String::new();
    for line in &doc.lines {
        for wrapped in wrap_text(&line.text, width) {
            out.push_str(&align_text(&wrapped, width, &line.align));
            out.push('\n');
        }
    }
    out
}


/// Generate the ESC/POS byte stream for a thermal document.
/// The stream is what a printer consumes; the caller decides how
/// it reaches the printer (see the module docs).
pub fn escpos_bytes(doc: &ThermalDocument, code_page: &str) -> Vec<u8> {
    let mut out = vec![ESC, b'@', ESC, b't', code_page_value(code_page)];
    let width = doc.width_columns.max(16) as usize;
    for line in &doc.lines {
        let align_byte = match line.align.as_str() {
            "center" => 1,
            "right" => 2,
            _ => 0,
        };
        out.extend_from_slice(&[ESC, b'a', align_byte]);
        out.extend_from_slice(&[ESC, b'E', u8::from(line.bold)]);
        out.extend_from_slice(&[GS, b'!', if line.double_size { 0x11 } else { 0x00 }]);
        for wrapped in wrap_text(&line.text, width) {
            out.extend_from_slice(wrapped.as_bytes());
            out.push(b'\n');
        }
        out.extend_from_slice(&[ESC, b'E', 0, GS, b'!', 0x00]);
    }
    out.extend_from_slice(&[ESC, b'a', 0]);
    if doc.drawer_kick {
        out.extend_from_slice(&[ESC, b'p', 0, 25, 250]);
    }
    if doc.cut {
        out.extend_from_slice(&[GS, b'V', 66, 0]);
    }
    out
}


/// Render one thermal document both ways: the ESC/POS stream
/// (base64) and its text preview.
pub fn render_thermal(doc: &ThermalDocument, code_page: &str) -> ThermalRender {
    ThermalRender {
        escpos_base64: BASE64.encode(escpos_bytes(doc, code_page)),
        preview: render_thermal_preview(doc),
    }
}


/// The two renderings of a thermal document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThermalRender {
    pub escpos_base64: String,
    pub preview: String,
}


/// Input for the stateless thermal preview endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThermalPreviewRequest {
    pub lines: Vec<ThermalLine>,
    #[serde(default = "default_preview_width")]
    pub width_columns: u32,
    #[serde(default)]
    pub code_page: Option<String>,
    #[serde(default)]
    pub cut: bool,
    #[serde(default)]
    pub drawer_kick: bool,
}


pub(crate) fn default_preview_width() -> u32 {
    42
}
#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, align: &str) -> ThermalLine {
        ThermalLine {
            text: text.to_string(),
            align: align.to_string(),
            bold: false,
            double_size: false,
        }
    }

    #[test]
    fn wrap_text_breaks_on_word_boundaries() {
        assert_eq!(
            wrap_text("hello world", 5),
            vec!["hello".to_string(), "world".to_string()]
        );
        assert_eq!(
            wrap_text("one two three", 7),
            vec!["one two".to_string(), "three".to_string()]
        );
    }

    #[test]
    fn wrap_text_splits_long_words_and_keeps_empty_lines() {
        assert_eq!(
            wrap_text("abcdefghij", 4),
            vec!["abcd".to_string(), "efgh".to_string(), "ij".to_string()]
        );
        assert_eq!(wrap_text("a\nb", 10), vec!["a".to_string(), "b".to_string()]);
        assert_eq!(wrap_text("", 10), vec![String::new()]);
        assert_eq!(wrap_text("hello", 0), vec!["hello".to_string()]);
    }

    #[test]
    fn align_text_pads_left_center_and_right() {
        assert_eq!(align_text("hi", 6, "left").as_str(), "hi");
        assert_eq!(align_text("hi", 6, "center").as_str(), "  hi");
        assert_eq!(align_text("hi", 6, "right").as_str(), "    hi");
        assert_eq!(align_text("toolongtext", 4, "right").as_str(), "toolongtext");
    }

    #[test]
    fn validate_thermal_line_accepts_known_alignments() {
        assert!(validate_thermal_line(&line("hello", "left")).is_ok());
        assert!(validate_thermal_line(&line("hello", "center")).is_ok());
        assert!(validate_thermal_line(&line("hello", "right")).is_ok());
        assert!(validate_thermal_line(&line("hello", "middle")).is_err());
        assert!(validate_thermal_line(&line(&"x".repeat(1001), "left")).is_err());
    }

    #[test]
    fn render_thermal_preview_applies_alignment() {
        let doc = ThermalDocument {
            lines: vec![line("hi", "left"), line("hi", "center")],
            width_columns: 16,
            cut: false,
            drawer_kick: false,
        };
        assert_eq!(render_thermal_preview(&doc).as_str(), "hi\n       hi\n");
    }

    #[test]
    fn escpos_bytes_starts_with_init_sequence_and_cuts() {
        let doc = ThermalDocument {
            lines: vec![line("hi", "left")],
            width_columns: 32,
            cut: true,
            drawer_kick: false,
        };
        let bytes = escpos_bytes(&doc, "cp437");
        assert_eq!(&bytes[..2], &[0x1B, b'@']);
        assert!(bytes.windows(3).any(|w| w == [0x1D, b'V', 66]));
    }
}

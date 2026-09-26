//! Writing the collected buffer out to a file.
//!
//! Formatting happens in the frontend, which already holds the rows and knows
//! what the user is looking at; this module owns the part the frontend must not
//! own — where the file lands, what it may be called, and the fact that a CSV is
//! written with a byte-order mark so Excel reads Chinese instead of mojibake.

use std::path::{Path, PathBuf};

use crate::error::{DroidLogError, Result};

/// Longest file name accepted, before the format's extension.
pub const MAX_STEM: usize = 96;

/// Where exports land inside the user's download directory.
pub const EXPORT_FOLDER: &str = "Droidlog";

/// The formats the UI offers.
///
/// The extension is decided here rather than by the caller: a name arriving from
/// the frontend is data, not a decision about file types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExportFormat {
    /// Human-readable log lines.
    Log,
    /// Spreadsheet-friendly, comma separated, UTF-8 **with** a BOM.
    Csv,
    /// One JSON object per line inside an array.
    Json,
}

impl ExportFormat {
    /// File extension without the dot.
    #[must_use]
    pub fn extension(self) -> &'static str {
        match self {
            Self::Log => "log",
            Self::Csv => "csv",
            Self::Json => "json",
        }
    }

    /// Parses the name the frontend sent.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "log" => Some(Self::Log),
            "csv" => Some(Self::Csv),
            "json" => Some(Self::Json),
            _ => None,
        }
    }

    /// Whether the file needs a UTF-8 byte-order mark.
    ///
    /// Excel is the reason: without it a Chinese CSV opens as mojibake, and a
    /// BOM is harmless everywhere else that reads CSV.
    #[must_use]
    pub fn wants_bom(self) -> bool {
        matches!(self, Self::Csv)
    }
}

/// What was written, for the UI to report.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOutcome {
    /// Absolute path of the written file.
    pub path: String,
    /// Bytes written, including any BOM.
    pub bytes: usize,
    /// Format actually used.
    pub format: ExportFormat,
}

/// The directory exports are written to: `<downloads>/Droidlog`.
///
/// Falls back to the working directory when the platform cannot name a download
/// folder, because refusing to export is worse than exporting somewhere obvious.
#[must_use]
pub fn export_dir(downloads: Option<PathBuf>) -> PathBuf {
    match downloads {
        Some(dir) => dir.join(EXPORT_FOLDER),
        None => PathBuf::from(EXPORT_FOLDER),
    }
}

/// Turns whatever the frontend suggested into a name that is safe to create.
///
/// Everything outside `[A-Za-z0-9._-]` is dropped, leading dots are removed (so
/// the file cannot become a hidden one), the stem is capped, and the format's
/// extension is forced. `../evil.log` therefore becomes `evil.log`, and an empty
/// result falls back to a fixed name.
#[must_use]
pub fn sanitize_file_name(requested: &str, format: ExportFormat) -> String {
    // Only the last path component counts: the rest is a traversal attempt.
    let last = requested
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(requested)
        .trim();

    let stem = last
        .rsplit_once('.')
        .map_or(last, |(stem, _)| stem)
        .trim_start_matches('.')
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(MAX_STEM)
        .collect::<String>();

    let stem = stem.trim_matches(['-', '_', '.']);
    let stem = if stem.is_empty() { "droidlog" } else { stem };
    format!("{stem}.{}", format.extension())
}

/// Writes `content` into `dir`, creating the directory when needed.
///
/// # Errors
///
/// Returns [`DroidLogError::InvalidInput`] when `format` and `file_name`
/// disagree, and an I/O error (wrapped) when the directory or file cannot be
/// written.
pub fn write(
    dir: &Path,
    file_name: &str,
    content: &str,
    format: ExportFormat,
) -> Result<ExportOutcome> {
    let expected = format.extension();
    if !file_name.ends_with(&format!(".{expected}")) {
        return Err(DroidLogError::InvalidInput(format!(
            "导出文件名与格式不符：{file_name} 不是 .{expected}"
        )));
    }

    std::fs::create_dir_all(dir).map_err(|err| {
        DroidLogError::InvalidInput(format!("无法创建导出目录 {}：{err}", dir.display()))
    })?;

    let path = dir.join(file_name);
    let mut bytes = Vec::with_capacity(content.len() + 3);
    if format.wants_bom() {
        bytes.extend_from_slice("\u{FEFF}".as_bytes());
    }
    bytes.extend_from_slice(content.as_bytes());

    std::fs::write(&path, &bytes).map_err(|err| {
        DroidLogError::InvalidInput(format!("无法写入 {}：{err}", path.display()))
    })?;

    Ok(ExportOutcome {
        path: path.to_string_lossy().into_owned(),
        bytes: bytes.len(),
        format,
    })
}

/// Whether `path` sits inside `dir`.
///
/// Both are expected to be canonical; this is the check that keeps
/// [`reveal`] from handing an arbitrary path to an external program.
#[must_use]
pub fn is_inside(dir: &Path, path: &Path) -> bool {
    path.starts_with(dir)
}

/// Opens the system file manager with `path` selected.
///
/// # Errors
///
/// Returns [`DroidLogError::InvalidInput`] when the file does not exist or lives
/// outside `dir`, and a spawn error when the file manager cannot be started.
pub fn reveal(dir: &Path, path: &Path) -> Result<()> {
    let canonical = path.canonicalize().map_err(|err| {
        DroidLogError::InvalidInput(format!("导出文件不存在：{}（{err}）", path.display()))
    })?;
    let root = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    if !is_inside(&root, &canonical) {
        return Err(DroidLogError::InvalidInput(format!(
            "只能打开导出目录内的文件：{}",
            canonical.display()
        )));
    }

    // `explorer /select,<file>` opens the containing folder with the file
    // highlighted; passing it as one argument keeps the comma and any spaces
    // inside the path intact.
    crate::executor::spawn_detached(
        "explorer.exe",
        vec![format!("/select,{}", canonical.display())],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_map_to_formats() {
        assert_eq!(ExportFormat::Log.extension(), "log");
        assert_eq!(ExportFormat::Csv.extension(), "csv");
        assert_eq!(ExportFormat::Json.extension(), "json");
        assert_eq!(ExportFormat::from_name("csv"), Some(ExportFormat::Csv));
        assert_eq!(ExportFormat::from_name("txt"), None);
        assert!(ExportFormat::Csv.wants_bom());
        assert!(!ExportFormat::Log.wants_bom());
    }

    #[test]
    fn names_are_sanitised() {
        let log = ExportFormat::Log;
        assert_eq!(sanitize_file_name("droidlog-1.log", log), "droidlog-1.log");
        // The extension is the format's, whatever was suggested.
        assert_eq!(sanitize_file_name("report.csv", log), "report.log");
        // Path traversal cannot survive: only the last component is kept.
        assert_eq!(sanitize_file_name("../../evil.log", log), "evil.log");
        assert_eq!(sanitize_file_name("..\\..\\evil.log", log), "evil.log");
        // Unsupported characters are dropped, not escaped.
        assert_eq!(sanitize_file_name("日志 导出.log", log), "droidlog.log");
        assert_eq!(sanitize_file_name("a b:c*?.log", log), "abc.log");
        // A leading dot would hide the file.
        assert_eq!(sanitize_file_name(".hidden.log", log), "hidden.log");
        // Empty or punctuation-only names fall back.
        assert_eq!(sanitize_file_name("", log), "droidlog.log");
        assert_eq!(sanitize_file_name("...", log), "droidlog.log");
    }

    #[test]
    fn long_names_are_capped() {
        let long = "x".repeat(300);
        let name = sanitize_file_name(&format!("{long}.log"), ExportFormat::Log);
        assert!(name.len() <= MAX_STEM + 4, "{}", name.len());
        assert!(name.ends_with(".log"));
    }

    #[test]
    fn a_mismatched_name_is_refused() {
        let dir = std::env::temp_dir();
        let err = write(&dir, "report.csv", "a", ExportFormat::Log);
        assert!(err.is_err());
    }

    #[test]
    fn csv_gets_a_bom_and_log_does_not() {
        let dir = std::env::temp_dir().join(format!("droidlog-test-{}", std::process::id()));
        let csv = write(&dir, "a.csv", "seq,msg\n1,hi\n", ExportFormat::Csv);
        let written = csv.expect("csv write");
        let bytes = std::fs::read(&written.path).expect("read csv");
        assert!(bytes.starts_with(&[0xEF, 0xBB, 0xBF]), "csv must start with a BOM");
        assert_eq!(written.bytes, bytes.len());

        let log = write(&dir, "b.log", "one\ntwo\n", ExportFormat::Log).expect("log write");
        let bytes = std::fs::read(&log.path).expect("read log");
        assert!(!bytes.starts_with(&[0xEF, 0xBB, 0xBF]), "log must not have a BOM");
        assert_eq!(std::fs::read_to_string(&log.path).unwrap_or_default(), "one\ntwo\n");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_dir_nests_under_downloads() {
        let dir = export_dir(Some(PathBuf::from("C:/Users/x/Downloads")));
        assert!(dir.ends_with(EXPORT_FOLDER));
        assert!(dir.to_string_lossy().contains("Downloads"));
        // No download directory known: still a usable relative folder.
        assert_eq!(export_dir(None), PathBuf::from(EXPORT_FOLDER));
    }

    #[test]
    fn containment_is_checked_by_prefix() {
        let dir = Path::new("C:/Users/x/Downloads/Droidlog");
        assert!(is_inside(dir, &dir.join("a.log")));
        assert!(is_inside(dir, &dir.join("nested/a.log")));
        assert!(!is_inside(dir, Path::new("C:/Users/x/Downloads/other/a.log")));
        // A sibling whose name merely starts the same must not pass.
        assert!(!is_inside(dir, Path::new("C:/Users/x/Downloads/Droidlog2/a.log")));
    }

    #[test]
    fn revealing_a_missing_file_is_an_error() {
        let dir = std::env::temp_dir();
        let missing = dir.join("droidlog-does-not-exist-12345.log");
        assert!(reveal(&dir, &missing).is_err());
    }
}

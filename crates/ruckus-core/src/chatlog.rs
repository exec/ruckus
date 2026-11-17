//! Append-only per-network, per-target plain text logs.
//!
//! Adapted from Lurk `chatlog/`, Copyright (c) 2026 exec, MIT (see LICENSE).
//! Unlike Lurk's lossy filename sanitizer, byte-hex filenames keep distinct
//! targets distinct and prevent traversal, separators and reserved device names.
//! No handles are cached, so remote target names cannot exhaust descriptors.
//! Use a trusted private root: symlink checks cannot defeat concurrent changes
//! by another process with write access to the parent tree.
use std::{
    fmt, fs,
    io::{self, Write},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

/// Maximum UTF-8 bytes in a network or target identifier.
pub const MAX_NAME_BYTES: usize = 100;
/// Maximum UTF-8 bytes in a single input log entry.
pub const MAX_LINE_BYTES: usize = 16 * 1024;

/// Redacted log error; no paths or message contents are retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    /// A name or entry exceeds its bound.
    TooLarge,
    /// Path contains symlinks, parent traversal, or a non-regular log file.
    UnsafePath,
    /// Existing Unix log file permits group/other access.
    InsecurePermissions,
    /// Timestamp predates the Unix epoch.
    InvalidTime,
    /// Filesystem error category.
    Io(io::ErrorKind),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TooLarge => "log input exceeds size limit",
            Self::UnsafePath => "unsafe log path",
            Self::InsecurePermissions => "insecure log file permissions",
            Self::InvalidTime => "invalid log timestamp",
            Self::Io(_) => "log I/O failure",
        })
    }
}
impl std::error::Error for Error {}
impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error.kind())
    }
}

/// Reversible, case-sensitive filesystem component, always beginning `n-`.
/// Empty names are supported. No IRC casemapping is guessed by this layer.
pub fn safe_name(name: &str) -> Result<String, Error> {
    if name.len() > MAX_NAME_BYTES {
        return Err(Error::TooLarge);
    }
    let mut result = String::with_capacity(2 + name.len() * 2);
    result.push_str("n-");
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in name.bytes() {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 15) as usize] as char);
    }
    Ok(result)
}

fn check_path(path: &Path) -> Result<(), Error> {
    let mut prefix = PathBuf::new();
    for component in path.components() {
        if component == Component::ParentDir {
            return Err(Error::UnsafePath);
        }
        prefix.push(component);
        match fs::symlink_metadata(&prefix) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(Error::UnsafePath),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Synchronous logger. Errors are returned so the frontend can decide how to
/// report or ignore them. `&mut self` serializes calls through one instance;
/// separate instances/processes are not a transactional multi-writer protocol.
pub struct Logger {
    root: PathBuf,
}
impl fmt::Debug for Logger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Logger { [REDACTED] }")
    }
}
impl Logger {
    /// Configure a root without performing I/O; directories are created lazily.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Compute the safe log path without accessing disk.
    pub fn path_for(&self, scope: &str, target: &str) -> Result<PathBuf, Error> {
        Ok(self
            .root
            .join(safe_name(scope)?)
            .join(format!("{}.log", safe_name(target)?)))
    }

    /// Append using the current clock. See `log_at` for format and bounds.
    pub fn log(&mut self, scope: &str, target: &str, line: &str) -> Result<(), Error> {
        self.log_at(scope, target, SystemTime::now(), line)
    }

    /// Append `<unix-seconds> <escaped-entry>\n`, with a deterministic clock.
    ///
    /// Controls (including CR/LF, ESC, C1 and bidi formatting) are escaped as
    /// literal `\u{hex}` sequences; backslashes are doubled for unambiguous
    /// records. Thus an entry cannot forge another physical log line.
    /// The prefix uses Unix seconds instead of local dates to avoid timezone
    /// dependencies. Renderers must still treat all log text as untrusted.
    ///
    /// Newly created directories/files use Unix 0700/0600. Existing broadly
    /// readable Unix log files are rejected, not silently chmodded.
    /// Non-Unix platforms use default ACLs, with no owner-only guarantee.
    /// Entries are flushed to the OS, not fsynced; partial writes are possible
    /// on I/O failure. The caller controls log retention and total disk use.
    pub fn log_at(
        &mut self,
        scope: &str,
        target: &str,
        time: SystemTime,
        line: &str,
    ) -> Result<(), Error> {
        if line.len() > MAX_LINE_BYTES {
            return Err(Error::TooLarge);
        }
        let seconds = time
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::InvalidTime)?
            .as_secs();
        let path = self.path_for(scope, target)?;
        check_path(&path)?;
        let dir = path.parent().ok_or(Error::UnsafePath)?;
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(dir)?;
        match fs::symlink_metadata(&path) {
            Ok(meta) => {
                if !meta.is_file() {
                    return Err(Error::UnsafePath);
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if meta.permissions().mode() & 0o077 != 0 {
                        return Err(Error::InsecurePermissions);
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let mut options = fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut output = options.open(path)?;
        let mut record = format!("{seconds} ");
        for c in line.chars() {
            if c == '\\' {
                record.push_str("\\\\");
            } else if c.is_control()
                || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            {
                use fmt::Write as _;
                // Writing to String cannot fail.
                let _ = write!(record, "\\u{{{:x}}}", c as u32);
            } else {
                record.push(c);
            }
        }
        record.push('\n');
        output.write_all(record.as_bytes())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static ID: AtomicU64 = AtomicU64::new(0);
    fn temp() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ruckus-chatlog-test-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        path.canonicalize().unwrap()
    }
    #[test]
    fn names_are_safe_distinct_and_bounded() {
        let inputs = [
            "",
            ".",
            "..",
            "../../evil",
            "/",
            "\\",
            "CON",
            "a/b",
            "a_b",
            "A",
            "a",
            "#é",
            "\0",
        ];
        let mut seen = std::collections::HashSet::new();
        for input in inputs {
            let name = safe_name(input).unwrap();
            assert!(name.starts_with("n-"));
            assert!(!name.contains(['/', '\\', '.']));
            assert!(seen.insert(name));
        }
        assert!(safe_name(&"x".repeat(MAX_NAME_BYTES)).is_ok());
        assert_eq!(
            safe_name(&"x".repeat(MAX_NAME_BYTES + 1)),
            Err(Error::TooLarge)
        );
    }
    #[test]
    fn deterministic_append_and_escaping() {
        let root = temp();
        let mut logger = Logger::new(root.join("logs"));
        let time = UNIX_EPOCH + std::time::Duration::from_secs(42);
        logger
            .log_at(
                "../network",
                "../../target",
                time,
                "hello\n\r\u{1b}[31m\u{202e}\\",
            )
            .unwrap();
        logger
            .log_at("../network", "../../target", time, "世界")
            .unwrap();
        let path = logger.path_for("../network", "../../target").unwrap();
        assert!(path.starts_with(root.join("logs")));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "42 hello\\u{a}\\u{d}\\u{1b}[31m\\u{202e}\\\\\n42 世界\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(
                logger.log_at("../network", "../../target", time, "no"),
                Err(Error::InsecurePermissions)
            );
        }
        assert_eq!(
            logger.log_at("n", "t", time, &"a".repeat(MAX_LINE_BYTES + 1)),
            Err(Error::TooLarge)
        );
        assert_eq!(
            logger.log_at(
                "n",
                "t",
                UNIX_EPOCH - std::time::Duration::from_secs(1),
                "no"
            ),
            Err(Error::InvalidTime)
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn rejects_symlink_files_and_directories() {
        use std::os::unix::fs::symlink;
        let root = temp();
        let mut logger = Logger::new(root.join("logs"));
        logger.log_at("n", "t", UNIX_EPOCH, "first").unwrap();
        let path = logger.path_for("n", "t").unwrap();
        fs::remove_file(&path).unwrap();
        let outside = root.join("outside");
        fs::write(&outside, "unchanged").unwrap();
        symlink(&outside, &path).unwrap();
        assert_eq!(
            logger.log_at("n", "t", UNIX_EPOCH, "no"),
            Err(Error::UnsafePath)
        );
        assert_eq!(fs::read_to_string(&outside).unwrap(), "unchanged");
        symlink(&root, root.join("link")).unwrap();
        let mut logger = Logger::new(root.join("link"));
        assert_eq!(
            logger.log_at("n", "t", UNIX_EPOCH, "no"),
            Err(Error::UnsafePath)
        );
        fs::remove_dir_all(root).unwrap();
    }
}

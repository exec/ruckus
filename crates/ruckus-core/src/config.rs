//! JSON configuration and XDG paths, independent of the client and UI.
//!
//! Adapted from Lurk `config/`, Copyright (c) 2026 exec, MIT (see LICENSE).
//! Credentials are stored in cleartext. Save uses a same-directory 0600
//! temporary file, file sync, atomic rename, and (on Unix) directory sync.
//! Existing directories are not chmodded. Callers must keep the parent tree
//! trusted: portable std APIs cannot prevent concurrent symlink replacement.
//! TLS and credential transport policy remain the client's responsibility.

use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    ffi::OsStr,
    fmt, fs,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// Maximum JSON input size, bounding accidental or hostile configuration reads.
pub const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// Configuration error that never includes paths, JSON content or credentials.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    /// Filesystem failure, with only its non-sensitive category retained.
    Io(io::ErrorKind),
    /// Invalid JSON or data types.
    InvalidJson,
    /// Configuration exceeds the size bound.
    TooLarge,
    /// No absolute home directory is available.
    MissingHome,
    /// An existing path component is a symlink or has the wrong type.
    UnsafePath,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(kind) => write!(f, "configuration I/O error ({kind:?})"),
            Self::InvalidJson => f.write_str("invalid configuration JSON"),
            Self::TooLarge => f.write_str("configuration exceeds size limit"),
            Self::MissingHome => f.write_str("absolute home directory unavailable"),
            Self::UnsafePath => f.write_str("unsafe configuration path"),
        }
    }
}
impl std::error::Error for Error {}
impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error.kind())
    }
}

/// SASL settings; an empty mechanism disables SASL.
#[derive(Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sasl {
    /// PLAIN or EXTERNAL; checked case-insensitively by validation.
    pub mechanism: String,
    /// Authentication identity.
    pub username: String,
    /// Cleartext password.
    pub password: String,
}
/// Default identity, used by a client when per-network fields are blank.
#[derive(Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Identity {
    /// Preferred nickname.
    pub nick: String,
    /// IRC USER username.
    pub user: String,
    /// IRC real name.
    pub realname: String,
}
/// Saved direct connection. No connection or TLS policy is enforced here.
#[derive(Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Network {
    /// Unique display name.
    pub name: String,
    /// Host and port, with brackets around IPv6 literals.
    pub addr: String,
    /// Request TLS.
    pub tls: bool,
    /// Explicitly disable certificate verification (dangerous).
    pub insecure: bool,
    /// Explicit client opt-in to plaintext credentials (dangerous).
    pub allow_insecure_auth: bool,
    /// Nickname override.
    pub nick: String,
    /// USER username override.
    pub user: String,
    /// Real name override.
    pub realname: String,
    /// Server PASS secret.
    pub pass: String,
    /// SASL credentials.
    pub sasl: Sasl,
    /// Channels to join after registration.
    pub channels: Vec<String>,
    /// Additional highlight words.
    pub highlights: Vec<String>,
    /// Ignored nicknames.
    pub ignores: Vec<String>,
}
/// On-disk document. Unknown fields are tolerated for forward compatibility.
/// Deserialization does not validate or resolve identity defaults.
#[derive(Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct File {
    /// Identity defaults.
    pub defaults: Identity,
    /// Ordered saved networks.
    pub networks: Vec<Network>,
    /// Frontend preference to hide IRC formatting.
    pub disable_formatting: bool,
}
macro_rules! redacted_debug {
    ($($ty:ty),*) => {$(
        impl fmt::Debug for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($ty), " { [REDACTED] }"))
            }
        }
    )*};
}
redacted_debug!(Sasl, Identity, Network, File);

/// Paths resolved from process environment, or explicit inputs in tests.
#[derive(Clone, Eq, PartialEq)]
pub struct Paths {
    /// JSON document location.
    pub config: PathBuf,
    /// Chat-log root.
    pub logs: PathBuf,
}
redacted_debug!(Paths);
impl Paths {
    /// Resolve RUCKUS_CONFIG/RUCKUS_LOG_DIR overrides, then absolute XDG homes,
    /// then HOME-based defaults. Relative XDG values are ignored per XDG.
    /// Explicit Ruckus overrides may be relative. Empty values count as unset.
    pub fn resolve(
        config_override: Option<&OsStr>,
        log_override: Option<&OsStr>,
        xdg_config: Option<&OsStr>,
        xdg_data: Option<&OsStr>,
        home: Option<&OsStr>,
    ) -> Result<Self, Error> {
        let nonempty = |v: Option<&OsStr>| v.filter(|s| !s.is_empty()).map(PathBuf::from);
        let home = nonempty(home).filter(|p| p.is_absolute());
        let base = |xdg: Option<&OsStr>, fallback: &str| -> Result<PathBuf, Error> {
            nonempty(xdg)
                .filter(|p| p.is_absolute())
                .or_else(|| home.as_ref().map(|h| h.join(fallback)))
                .ok_or(Error::MissingHome)
        };
        let config = match nonempty(config_override) {
            Some(p) => p,
            None => base(xdg_config, ".config")?.join("ruckus/config.json"),
        };
        let logs = match nonempty(log_override) {
            Some(p) => p,
            None => base(xdg_data, ".local/share")?.join("ruckus/logs"),
        };
        Ok(Self { config, logs })
    }
    /// Resolve from the current process environment without changing it.
    pub fn from_env() -> Result<Self, Error> {
        let values = [
            "RUCKUS_CONFIG",
            "RUCKUS_LOG_DIR",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "HOME",
        ]
        .map(std::env::var_os);
        Self::resolve(
            values[0].as_deref(),
            values[1].as_deref(),
            values[2].as_deref(),
            values[3].as_deref(),
            values[4].as_deref(),
        )
    }
}

/// One validation problem, indexed without echoing user-controlled content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ValidationError {
    /// Zero-based index into `File::networks`.
    pub network: usize,
    /// Static field name.
    pub field: &'static str,
    /// Static explanation.
    pub problem: &'static str,
}
impl File {
    /// Find a network by ASCII-case-insensitive display name.
    pub fn get(&self, name: &str) -> Option<&Network> {
        self.networks
            .iter()
            .find(|n| n.name.eq_ignore_ascii_case(name))
    }
    /// Replace a network in place (using its prior name for renames), or append.
    pub fn upsert(&mut self, network: Network, prior_name: Option<&str>) {
        let key = prior_name.unwrap_or(&network.name);
        if let Some(index) = self
            .networks
            .iter()
            .position(|n| n.name.eq_ignore_ascii_case(key))
        {
            self.networks[index] = network;
        } else {
            self.networks.push(network);
        }
    }
    /// Remove a network by ASCII-case-insensitive display name.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.networks.len();
        self.networks.retain(|n| !n.name.eq_ignore_ascii_case(name));
        before != self.networks.len()
    }
    /// Check required names, duplicates, host:port syntax and supported SASL.
    /// This is not wire validation; the client must validate all outgoing fields.
    pub fn validate(&self) -> Vec<ValidationError> {
        let mut seen = HashSet::new();
        let mut errors = Vec::new();
        for (index, n) in self.networks.iter().enumerate() {
            let mut add = |field, problem| {
                errors.push(ValidationError {
                    network: index,
                    field,
                    problem,
                })
            };
            if n.name.trim().is_empty() {
                add("name", "must not be empty");
            } else if !seen.insert(n.name.to_ascii_lowercase()) {
                add("name", "duplicate name");
            }
            let address_ok = n.addr.rsplit_once(':').is_some_and(|(host, port)| {
                !host.is_empty()
                    && !host.chars().any(char::is_whitespace)
                    && (!host.contains(':') || (host.starts_with('[') && host.ends_with(']')))
                    && port.parse::<u16>().is_ok_and(|p| p != 0)
                    && !host.chars().any(char::is_control)
            });
            if !address_ok {
                add("addr", "must be host:port with a nonzero port");
            }
            if !matches!(
                n.sasl.mechanism.to_ascii_uppercase().as_str(),
                "" | "PLAIN" | "EXTERNAL"
            ) {
                add("sasl.mechanism", "unsupported SASL mechanism");
            }
        }
        errors
    }
}

// Reject symlinks in existing components, including the final file. This is
// defense in depth, not a substitute for an attacker-inaccessible parent tree.
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
fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

/// Load from an explicit path; a missing file yields an empty document.
/// Symlinks, non-regular files and oversized input are rejected.
pub fn load_from(path: impl AsRef<Path>) -> Result<File, Error> {
    let path = path.as_ref();
    check_path(path)?;
    // Inspect before opening so a pre-existing FIFO/device cannot block a load.
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_file() => return Err(Error::UnsafePath),
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(File::default()),
        Err(e) => return Err(e.into()),
    }
    let input = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(File::default()),
        Err(e) => return Err(e.into()),
    };
    if !input.metadata()?.is_file() {
        return Err(Error::UnsafePath);
    }
    let mut data = Vec::new();
    input.take(MAX_CONFIG_BYTES + 1).read_to_end(&mut data)?;
    if data.len() as u64 > MAX_CONFIG_BYTES {
        return Err(Error::TooLarge);
    }
    serde_json::from_slice(&data).map_err(|_| Error::InvalidJson)
}

/// Atomically save JSON, making newly created Unix directories 0700 and the
/// replacement file 0600. Existing file permissions do not carry over.
/// On non-Unix, default OS ACLs apply; this API does not promise owner-only ACLs.
/// A directory-sync failure can be reported after the replacement has occurred.
pub fn save(document: &File, path: impl AsRef<Path>) -> Result<(), Error> {
    let path = path.as_ref();
    check_path(path)?;
    let mut data = serde_json::to_vec_pretty(document).map_err(|_| Error::InvalidJson)?;
    data.push(b'\n');
    if data.len() as u64 > MAX_CONFIG_BYTES {
        return Err(Error::TooLarge);
    }
    let dir = parent(path);
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)?;
    let mut temporary = None;
    for _ in 0..128 {
        let candidate = dir.join(format!(
            ".ruckus-config-{}-{}.tmp",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&candidate) {
            Ok(f) => {
                temporary = Some((candidate, f));
                break;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    let (temporary_path, mut output) = temporary.ok_or(Error::Io(io::ErrorKind::AlreadyExists))?;
    let result = (|| {
        output.write_all(&data)?;
        output.sync_all()?;
        drop(output);
        fs::rename(&temporary_path, path)?;
        #[cfg(unix)]
        fs::File::open(dir)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temp() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "ruckus-config-test-{}-{}",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        // macOS commonly has /tmp as a symlink; use the real root in tests.
        p.canonicalize().unwrap()
    }
    #[test]
    fn paths_without_environment_mutation() {
        let paths = Paths::resolve(
            None,
            None,
            Some(OsStr::new("relative")),
            None,
            Some(OsStr::new("/home/test")),
        )
        .unwrap();
        assert_eq!(
            paths.config,
            Path::new("/home/test/.config/ruckus/config.json")
        );
        assert_eq!(paths.logs, Path::new("/home/test/.local/share/ruckus/logs"));
        let paths = Paths::resolve(
            Some(OsStr::new("custom")),
            Some(OsStr::new("logs")),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(paths.config, Path::new("custom"));
        assert!(Paths::resolve(None, None, None, None, None).is_err());
        let paths = Paths::resolve(
            None,
            None,
            Some(OsStr::new("/cfg")),
            Some(OsStr::new("/data")),
            None,
        )
        .unwrap();
        assert_eq!(paths.config, Path::new("/cfg/ruckus/config.json"));
        assert_eq!(paths.logs, Path::new("/data/ruckus/logs"));
    }
    #[test]
    fn save_load_permissions_and_errors() {
        let root = temp();
        let path = root.join("private/config.json");
        assert_eq!(load_from(&path).unwrap(), File::default());
        let mut doc = File::default();
        doc.networks.push(Network {
            name: "test".into(),
            addr: "[::1]:6697".into(),
            pass: "secret".into(),
            ..Network::default()
        });
        save(&doc, &path).unwrap();
        assert_eq!(load_from(&path).unwrap(), doc);
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
            save(&doc, &path).unwrap();
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::write(&path, br#"{"networks":"secret"}"#).unwrap();
        assert_eq!(load_from(&path), Err(Error::InvalidJson));
        assert!(!format!("{doc:?}").contains("secret"));
        fs::write(&path, vec![b' '; MAX_CONFIG_BYTES as usize + 1]).unwrap();
        assert_eq!(load_from(&path), Err(Error::TooLarge));
        // A failed replacement leaves no secret-bearing temporary files.
        let target_dir = root.join("target");
        fs::create_dir(&target_dir).unwrap();
        assert_eq!(load_from(&target_dir), Err(Error::UnsafePath));
        assert!(save(&doc, &target_dir).is_err());
        assert!(
            fs::read_dir(&root).unwrap().all(|p| !p
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp"))
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn validation_and_edits() {
        let mut doc = File::default();
        doc.upsert(
            Network {
                name: "Test".into(),
                addr: "localhost:6697".into(),
                ..Network::default()
            },
            None,
        );
        assert!(doc.validate().is_empty());
        doc.upsert(
            Network {
                name: "Renamed".into(),
                addr: "bad".into(),
                ..Network::default()
            },
            Some("test"),
        );
        assert_eq!(doc.networks.len(), 1);
        assert!(doc.get("RENAMED").is_some());
        doc.networks.push(doc.networks[0].clone());
        assert_eq!(doc.validate().len(), 3);
        assert!(doc.remove("renamed"));
        assert!(!doc.remove("renamed"));
    }
    #[test]
    fn json_defaults_unknown_fields_and_validation() {
        let doc: File = serde_json::from_str(
            r#"{"future":true,"networks":[{"name":"a","addr":"[::1]:6697","sasl":{"mechanism":"plain","password":"secret"}}]}"#
        ).unwrap();
        assert!(doc.validate().is_empty());
        assert_eq!(doc.networks[0].sasl.password, "secret");
        assert!(!format!("{:?}", doc.networks[0]).contains("secret"));
        assert!(!format!("{:?}", doc.networks[0].sasl).contains("secret"));
        for addr in [
            "host",
            ":6697",
            "host:0",
            "host:65536",
            "::1:6697",
            "host\n:6697",
        ] {
            let mut invalid = doc.clone();
            invalid.networks[0].addr = addr.into();
            assert_eq!(invalid.validate()[0].field, "addr");
        }
        let mut invalid = doc;
        invalid.networks[0].sasl.mechanism = "secret".into();
        assert_eq!(invalid.validate()[0].field, "sasl.mechanism");
        assert!(!format!("{:?}", invalid.validate()).contains("secret"));
    }
    #[cfg(unix)]
    #[test]
    fn reject_symlinks() {
        use std::os::unix::fs::symlink;
        let root = temp();
        let actual = root.join("actual");
        fs::write(&actual, "{}").unwrap();
        let link = root.join("link");
        symlink(&actual, &link).unwrap();
        assert_eq!(load_from(&link), Err(Error::UnsafePath));
        assert_eq!(save(&File::default(), &link), Err(Error::UnsafePath));
        symlink(&root, root.join("directory-link")).unwrap();
        assert_eq!(
            save(&File::default(), root.join("directory-link/config")),
            Err(Error::UnsafePath)
        );
        fs::remove_dir_all(root).unwrap();
    }
}

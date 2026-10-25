//! Where mejl keeps its files.
//!
//! The four base directories come from `etcetera`'s native strategy: XDG on
//! Linux and the BSDs (honouring `XDG_CONFIG_HOME` and friends), Apple's
//! `Library` folders on macOS and the known folders on Windows. Tests and
//! demo mode use [`Paths::under`] with a temporary base instead, so they
//! never read or write the user's real directories.
//!
//! Layout:
//!
//! | Path | Contents |
//! |---|---|
//! | `<config>/config.toml` | The config file, unless `--config` names another |
//! | `<data>/accounts/<name>/` | One account: `cache.db`, `state.db` and the instance lock. Mode 0700 |
//! | `<cache>/caps.toml` | Cached terminal capabilities (safe to delete) |
//! | `<state>/log/` | Rolling log files |

use std::io;
use std::path::{Path, PathBuf};

use etcetera::app_strategy::{AppStrategy, AppStrategyArgs, choose_native_strategy};

/// The config file name inside the config directory.
pub const CONFIG_FILE: &str = "config.toml";
/// The directory under the data directory that holds one folder per account.
pub const ACCOUNTS_DIR: &str = "accounts";
/// The log directory under the state directory.
pub const LOG_DIR: &str = "log";
/// The capability cache file in the cache directory.
pub const CAPS_FILE: &str = "caps.toml";

/// The longest account name accepted, so directory names stay short.
pub const MAX_ACCOUNT_NAME: usize = 64;

/// Failure to find the user's home directory.
#[derive(Debug, thiserror::Error)]
#[error("cannot find your home directory, so mejl does not know where to keep its files")]
pub struct PathsError;

/// mejl's directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// The config file to read.
    pub config_file: PathBuf,
    /// The config directory.
    pub config_dir: PathBuf,
    /// Durable data: account stores.
    pub data_dir: PathBuf,
    /// Data mejl can rebuild, such as the capability cache.
    pub cache_dir: PathBuf,
    /// Logs and other state that is neither config nor data.
    pub state_dir: PathBuf,
}

impl Paths {
    /// The platform's directories for mejl. Nothing is created.
    pub fn from_system() -> Result<Paths, PathsError> {
        let strategy = choose_native_strategy(AppStrategyArgs {
            top_level_domain: "org".into(),
            author: "mejl".into(),
            app_name: "mejl".into(),
        })
        .map_err(|_| PathsError)?;
        let config_dir = strategy.config_dir();
        let data_dir = strategy.data_dir();
        // Only XDG has a state directory. Elsewhere logs go next to the data.
        let state_dir = strategy
            .state_dir()
            .unwrap_or_else(|| data_dir.join("state"));
        Ok(Paths {
            config_file: config_dir.join(CONFIG_FILE),
            config_dir,
            data_dir,
            cache_dir: strategy.cache_dir(),
            state_dir,
        })
    }

    /// Directories under one base: `<base>/config`, `<base>/data`,
    /// `<base>/cache` and `<base>/state`. Used by tests and demo mode.
    pub fn under(base: &Path) -> Paths {
        let config_dir = base.join("config");
        Paths {
            config_file: config_dir.join(CONFIG_FILE),
            config_dir,
            data_dir: base.join("data"),
            cache_dir: base.join("cache"),
            state_dir: base.join("state"),
        }
    }

    /// Uses another config file, as `--config` asks.
    pub fn with_config_file(mut self, file: PathBuf) -> Paths {
        self.config_file = file;
        self
    }

    /// The folder for one account's stores. The name must already be valid
    /// (see [`validate_account_name`]).
    pub fn account_dir(&self, account: &str) -> PathBuf {
        self.data_dir.join(ACCOUNTS_DIR).join(account)
    }

    /// Creates the account folder with mode 0700 and returns it.
    pub fn ensure_account_dir(&self, account: &str) -> io::Result<PathBuf> {
        validate_account_name(account)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let dir = self.account_dir(account);
        create_private_dir(&dir)?;
        Ok(dir)
    }

    /// Where log files go.
    pub fn log_dir(&self) -> PathBuf {
        self.state_dir.join(LOG_DIR)
    }

    /// The cached terminal capabilities.
    pub fn caps_file(&self) -> PathBuf {
        self.cache_dir.join(CAPS_FILE)
    }
}

/// Checks that an account name is safe to use as a folder name and as a
/// keyring key: 1 to 64 characters of ASCII letters, digits, `-`, `_` and
/// `.`, not starting with `.`.
pub fn validate_account_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("an account name cannot be empty".into());
    }
    if name.len() > MAX_ACCOUNT_NAME {
        return Err(format!(
            "account name {name:?} is longer than {MAX_ACCOUNT_NAME} characters"
        ));
    }
    if name.starts_with('.') {
        return Err(format!("account name {name:?} cannot start with a dot"));
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
    {
        return Err(format!(
            "account name {name:?} has {bad:?}; use letters, digits, '-', '_' or '.'"
        ));
    }
    Ok(())
}

/// Creates a directory and its parents. The last component gets mode 0700
/// on Unix, because it holds mail and tokens must never sit next to
/// world-readable files. An existing directory is tightened to 0700 too.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match std::fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && dir.is_dir() => {
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            }
            Err(e) => Err(e),
        }
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_puts_everything_below_the_base() {
        let base = tempfile::tempdir().unwrap();
        let p = Paths::under(base.path());
        for dir in [&p.config_dir, &p.data_dir, &p.cache_dir, &p.state_dir] {
            assert!(dir.starts_with(base.path()), "{dir:?}");
        }
        assert_eq!(
            p.config_file,
            base.path().join("config").join("config.toml")
        );
        assert_eq!(
            p.account_dir("work"),
            base.path().join("data").join("accounts").join("work")
        );
        assert_eq!(p.log_dir(), base.path().join("state").join("log"));
        assert_eq!(p.caps_file(), base.path().join("cache").join("caps.toml"));
    }

    #[test]
    fn config_override_keeps_other_dirs() {
        let base = tempfile::tempdir().unwrap();
        let p = Paths::under(base.path()).with_config_file(base.path().join("other.toml"));
        assert_eq!(p.config_file, base.path().join("other.toml"));
        assert_eq!(p.data_dir, base.path().join("data"));
    }

    #[test]
    fn account_dir_is_private() {
        let base = tempfile::tempdir().unwrap();
        let p = Paths::under(base.path());
        let dir = p.ensure_account_dir("personal").unwrap();
        assert!(dir.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        // A second call is fine and tightens loose permissions.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            p.ensure_account_dir("personal").unwrap();
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
    }

    #[test]
    fn bad_account_names_are_refused() {
        for bad in [
            "",
            ".hidden",
            "a/b",
            "../up",
            "space name",
            "ä",
            &"x".repeat(65),
        ] {
            assert!(validate_account_name(bad).is_err(), "{bad:?}");
        }
        for good in ["default", "work.2", "a-b_c", &"x".repeat(64)] {
            assert!(validate_account_name(good).is_ok(), "{good:?}");
        }
        let base = tempfile::tempdir().unwrap();
        let err = Paths::under(base.path())
            .ensure_account_dir("../escape")
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(!base.path().join("data").join("escape").exists());
    }

    #[test]
    fn create_private_dir_refuses_a_file_in_the_way() {
        let base = tempfile::tempdir().unwrap();
        let file = base.path().join("taken");
        std::fs::write(&file, b"x").unwrap();
        assert!(create_private_dir(&file).is_err());
    }
}

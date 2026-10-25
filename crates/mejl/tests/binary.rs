//! Runs the real `mejl` executable.
//!
//! Every run gets a temporary HOME and XDG directories, so it never reads
//! or writes the user's real config, data, cache or state. Runs that would
//! reach the keyring set MEJL_TOKEN instead, so the OS keyring is never
//! touched. Unix only: on Windows the directories come from the known
//! folders API, which ignores the environment.

#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Sandbox {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self, sub: &str) -> std::path::PathBuf {
        self.dir.path().join(sub)
    }

    fn config_file(&self) -> std::path::PathBuf {
        self.path("xdg-config").join("mejl").join("config.toml")
    }

    fn write_config(&self, text: &str) {
        let file = self.config_file();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }

    fn mejl(&self, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_mejl"));
        cmd.args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.dir.path())
            .env("XDG_CONFIG_HOME", self.path("xdg-config"))
            .env("XDG_DATA_HOME", self.path("xdg-data"))
            .env("XDG_CACHE_HOME", self.path("xdg-cache"))
            .env("XDG_STATE_HOME", self.path("xdg-state"))
            .stdin(std::process::Stdio::null());
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn only_under(base: &Path, path: &Path) {
    assert!(
        path.starts_with(base),
        "{} escaped the sandbox",
        path.display()
    );
}

#[test]
fn version_and_help() {
    let s = Sandbox::new();
    let o = s.mejl(&["--version"], &[]);
    assert!(o.status.success());
    assert_eq!(
        stdout(&o).trim(),
        format!("mejl {}", env!("CARGO_PKG_VERSION"))
    );
    let o = s.mejl(&["--help"], &[]);
    assert!(o.status.success());
    assert!(stdout(&o).contains("Exit codes"));
}

#[test]
fn usage_error_exits_2() {
    let s = Sandbox::new();
    let o = s.mejl(&["frobnicate"], &[]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn print_config_works_and_logs_stay_in_the_sandbox() {
    let s = Sandbox::new();
    let o = s.mejl(&["doctor", "--print-config"], &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stdout(&o).contains("[sync]"));
    let log_dir = s.path("xdg-state").join("mejl").join("log");
    assert!(log_dir.is_dir(), "log dir missing at {}", log_dir.display());
    only_under(s.dir.path(), &log_dir);
}

#[test]
fn config_error_exits_3() {
    let s = Sandbox::new();
    s.write_config("[ui]\nmouse = \"maybe\"\n");
    let o = s.mejl(&["keys", "--check"], &[]);
    assert_eq!(o.status.code(), Some(3), "{}", stderr(&o));
    assert!(stderr(&o).contains("line 2"), "{}", stderr(&o));
}

#[test]
fn doctor_without_a_terminal_reports_and_hides_the_token() {
    let s = Sandbox::new();
    let token = "fmu1-binary-test-secret-000000000";
    let o = s.mejl(&["doctor"], &[("MEJL_TOKEN", token), ("TERM", "dumb")]);
    assert!(o.status.success(), "{}", stderr(&o));
    let out = stdout(&o);
    assert!(out.contains("from MEJL_TOKEN"), "{out}");
    assert!(out.contains("not run"), "{out}");
    assert!(!out.contains(token));
    assert!(!stderr(&o).contains(token));
    // Nor in the log file.
    let log_dir = s.path("xdg-state").join("mejl").join("log");
    for entry in std::fs::read_dir(log_dir).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        assert!(!text.contains(token), "{text}");
    }
}

#[test]
fn run_with_env_token_checks_the_setup() {
    let s = Sandbox::new();
    // Piped stdio is not a terminal, so mejl checks the setup and stops
    // before it builds a client; nothing is contacted.
    let o = s.mejl(&[], &[("MEJL_TOKEN", "fmu1-binary-test-token-11111111")]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(
        stderr(&o).contains("interactive terminal"),
        "{}",
        stderr(&o)
    );
    assert!(!stderr(&o).contains("fmu1-binary"));
    let account = s
        .path("xdg-data")
        .join("mejl")
        .join("accounts")
        .join("default");
    assert!(account.is_dir());
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&account).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
}

#[test]
fn demo_without_a_terminal_says_so() {
    let s = Sandbox::new();
    let o = s.mejl(&["--demo"], &[]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(
        stderr(&o).contains("interactive terminal"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn login_with_piped_empty_stdin_fails_cleanly() {
    let s = Sandbox::new();
    let o = s.mejl(&["login"], &[]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(stderr(&o).contains("cancelled"), "{}", stderr(&o));
}

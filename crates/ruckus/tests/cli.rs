//! Smoke tests for the executable's public command-line interface.

use std::process::Command;

#[cfg(not(feature = "gui"))]
#[test]
fn disabled_desktop_does_not_pretend_to_open_a_window() {
    let output = Command::new(env!("CARGO_BIN_EXE_ruckus"))
        .output()
        .expect("run headless ruckus");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("--features gui")
    );
}

#[test]
fn help_explains_desktop_startup() {
    for args in [vec!["--help"], vec!["-h"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_ruckus"))
            .args(args)
            .output()
            .expect("run ruckus");
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let stdout = String::from_utf8(output.stdout).expect("UTF-8 help");
        assert!(stdout.contains("Usage: ruckus"));
        assert!(stdout.contains("GPUI desktop"));
    }
}

#[test]
fn version_matches_package_metadata() {
    for arg in ["--version", "-V"] {
        let output = Command::new(env!("CARGO_BIN_EXE_ruckus"))
            .arg(arg)
            .output()
            .expect("run ruckus");
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert_eq!(
            String::from_utf8(output.stdout).expect("UTF-8 version"),
            format!("ruckus {}\n", env!("CARGO_PKG_VERSION"))
        );
    }
}

#[test]
fn unsupported_arguments_fail_without_echoing_input() {
    for args in [
        vec!["--unknown"],
        vec!["--help", "unexpected-secret"],
        vec!["--version", "--help"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_ruckus"))
            .args(&args)
            .output()
            .expect("run ruckus");
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).expect("UTF-8 error");
        assert!(stderr.contains("unsupported arguments"));
        for arg in args {
            // The usage hint may mention --help, but no other input is echoed.
            if arg != "--help" {
                assert!(!stderr.contains(arg));
            }
        }
    }
}

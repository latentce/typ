use std::process::Command;

use typ_rs_core::corpus::CORPUS_VERSION;

struct Run {
    ok: bool,
    stdout: String,
    stderr: String,
}

/// Runs `typ` with stdin closed and stdout captured, so it is not attached
/// to a terminal.
fn typ(args: &[&str]) -> Run {
    let output = Command::new(env!("CARGO_BIN_EXE_typ"))
        .args(args)
        .output()
        .expect("typ binary runs");
    Run {
        ok: output.status.success(),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

#[test]
fn version_shows_the_licence_and_the_corpus_attribution_under_both_flags() {
    for flag in ["--version", "-V"] {
        let run = typ(&[flag]);
        let stdout = &run.stdout;

        assert!(run.ok);
        assert!(
            stdout.starts_with(&format!("typ {}\n", env!("CARGO_PKG_VERSION"))),
            "{flag}: {stdout}"
        );
        assert!(stdout.contains("Licence: MIT"), "{flag}: {stdout}");
        assert!(stdout.contains("Google Books"), "{flag}: {stdout}");
        assert!(stdout.contains("CC BY 3.0"), "{flag}: {stdout}");
        assert!(
            stdout.contains(&format!("corpus version {CORPUS_VERSION}")),
            "{flag}: {stdout}"
        );
    }
}

#[test]
fn a_session_refuses_to_start_without_an_interactive_terminal() {
    let run = typ(&[]);

    assert!(!run.ok);
    assert_eq!(run.stdout, "");
    assert_eq!(run.stderr.lines().count(), 1, "{:?}", run.stderr);
    assert!(
        run.stderr.contains("interactive terminal"),
        "{:?}",
        run.stderr
    );
}

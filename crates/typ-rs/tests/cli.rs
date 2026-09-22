use std::collections::HashSet;
use std::process::Command;

use typ_rs_core::corpus::{CORPUS_VERSION, Corpus};

struct Run {
    ok: bool,
    stdout: String,
    stderr: String,
}

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
fn a_bare_run_prints_fifty_corpus_words_on_one_line() {
    let run = typ(&[]);

    assert!(run.ok, "{}", run.stderr);
    assert_eq!(run.stdout.lines().count(), 1, "{:?}", run.stdout);
    let words: Vec<&str> = run.stdout.trim_end().split(' ').collect();
    assert_eq!(words.len(), 50, "{}", run.stdout);

    let known: HashSet<&str> = Corpus::bundled().words().iter().map(|w| &*w.text).collect();
    for word in &words {
        assert!(known.contains(word), "{word:?} is not a corpus word");
    }
}

#[test]
fn each_run_draws_a_fresh_prompt() {
    assert_ne!(typ(&[]).stdout, typ(&[]).stdout);
}

//! `typ-rs-core` must stay free of terminal and database crates so that every
//! session can be reproduced from its events without either. Rather than
//! naming crates to forbid, this pins the complete set of crates allowed in
//! its normal dependency tree; adding a dependency means extending the list
//! deliberately.

use std::collections::BTreeSet;
use std::process::Command;

const ALLOWED: &[&str] = &[
    "typ-rs-core",
    "rand_core",
    "rand_chacha",
    "ppv-lite86",
    "zerocopy",
    "unicode-width",
];

#[test]
fn core_depends_only_on_the_allowed_crates() {
    let output = Command::new(env!("CARGO"))
        .args([
            "tree",
            "--package",
            "typ-rs-core",
            "--edges",
            "normal",
            "--prefix",
            "none",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo tree runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let tree = String::from_utf8(output.stdout).unwrap();
    let crates: BTreeSet<&str> = tree
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    assert!(crates.contains("typ-rs-core"), "{tree}");

    let unexpected: Vec<&str> = crates
        .iter()
        .copied()
        .filter(|c| !ALLOWED.contains(c))
        .collect();
    assert!(
        unexpected.is_empty(),
        "typ-rs-core pulled in crates outside its allowlist: {unexpected:?}\n{tree}"
    );
}

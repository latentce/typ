//! Simulator binary. Synthetic learners and baseline schedulers arrive with
//! the session pipeline; for now the binary only proves the crate wiring.

fn main() {
    println!(
        "typ-sim: no learners yet (corpus version {})",
        typ_rs_core::corpus::CORPUS_VERSION
    );
}

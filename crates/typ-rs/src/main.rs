use std::sync::LazyLock;

use clap::Parser;
use typ_rs_core::corpus::{CORPUS_VERSION, Corpus, ReferenceDistribution};

const PROMPT_WORDS: usize = 50;

static VERSION: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{}\n\
         Licence: MIT\n\
         Corpus: English words (corpus version {CORPUS_VERSION}) derived from the\n\
         Google Books Ngram Viewer Exports v3 via orgtre/google-books-ngram-frequency,\n\
         licensed CC BY 3.0 <https://creativecommons.org/licenses/by/3.0/>.",
        env!("CARGO_PKG_VERSION")
    )
});

/// A local terminal typing trainer that targets the character patterns you are weak on.
#[derive(Parser)]
#[command(name = "typ", version = VERSION.as_str())]
struct Cli {}

fn main() {
    let Cli {} = Cli::parse();

    let seed = getrandom::u64().expect("operating system randomness");
    let corpus = Corpus::bundled();
    let prompt: Vec<&str> = ReferenceDistribution::new(corpus)
        .seeded_sampler(seed)
        .take(PROMPT_WORDS)
        .map(|id| corpus.text(id))
        .collect();
    println!("{}", prompt.join(" "));
}

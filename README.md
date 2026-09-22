# typ

A local terminal typing trainer. It measures which character patterns you are
weak on (characters, bigrams, and trigrams, with space as an ordinary
character) and composes practice prompts of real English words to improve
them, while measuring whether the practice transfers to ordinary typing.

Everything is local: no account, no server, no telemetry.

## Status

Early. `typ` currently runs one 50-word session: the prompt appears inline
below the command, you type it, and it reports gross WPM and final accuracy.
Nothing is saved yet; statistics and the scheduler are being built on top of
it.

```
$ typ --version
$ typ
```

Set `NO_COLOR` to get bold and underline instead of colour. `Ctrl-C` or `Esc`
ends a session early.

## Installing

Not yet on crates.io. Until it is, install from a checkout:

```
cargo install --path crates/typ-rs
```

Once published, `cargo install typ-rs` will install and update the `typ`
command.

## Developing

With [`just`](https://github.com/casey/just) installed, `just` lists the
common tasks:

```
just run              # build and run typ from source
just run --version
just test
just check            # fmt, clippy, tests: run before committing
```

Without it, the equivalents are `cargo build -p typ-rs && ./target/debug/typ`,
`cargo test --workspace`, and so on.

The terminal loop itself is checked by hand; see
[`docs/smoke-test.md`](docs/smoke-test.md).

The workspace has four crates:

- `crates/typ-rs-core`: session state machine, analysis, model, scheduler, word
  selection, and the corpus. No terminal or database dependency.
- `crates/typ-rs-store`: SQLite persistence, migrations, and rebuild.
- `crates/typ-rs`: the `typ` binary (terminal and CLI).
- `crates/typ-sim`: a simulator that drives the same pipeline with synthetic
  learners.

The corpus is compiled in from `crates/typ-rs-core/corpus/1grams_english.csv` and derived at
startup; editing that file or the filter rules in `typ-rs-core` is a corpus
version bump.

## Licence

Licensed under the [MIT licence](LICENSE-MIT).

### Corpus attribution

The bundled corpus is derived from the English unigram list in
[orgtre/google-books-ngram-frequency](https://github.com/orgtre/google-books-ngram-frequency),
itself computed from the Google Books Ngram Viewer Exports (version 3). Both
are licensed under the
[Creative Commons Attribution 3.0 Unported License](https://creativecommons.org/licenses/by/3.0/).
The unmodified source list and the licence text are in [`crates/typ-rs-core/corpus/`](crates/typ-rs-core/corpus/).

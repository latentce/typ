# typ

A local terminal typing trainer. It measures which character patterns you are
weak on (characters, bigrams, and trigrams, with space as an ordinary
character) and composes practice prompts of real English words to improve
them, while measuring whether the practice transfers to ordinary typing.

Everything is local: no account, no server, no telemetry.

## Status

Early. `typ` runs one 50-word session: the prompt appears inline below the
command, you type it, and it reports gross WPM, raw (first-attempt) accuracy,
final accuracy, and consistency. Every session is saved, including
interrupted ones, and the prompt for your next session is composed as soon as
the current one ends.

`typ config words 30` changes the session length (10 to 200 words) for every
run to come; `typ --words 30` changes it for one run. Settings take effect on
the very next run: a prompt composed ahead for other settings is replaced on
the spot. Statistics live in profiles, one per typing condition, so a second
keyboard layout gets its own history: `typ config profile laptop` switches to
a profile (creating it on first use), `typ --profile laptop` uses one for a
single run, and `typ config layout` shows or sets a profile's layout while
nothing has been typed on it yet. Only `qwerty` exists so far.

After each session `typ` updates decaying statistics for every character,
bigram, and trigram you typed (space counts as an ordinary character, so the
start and end of words are patterns too), normalised for your usual speed and
for how fast you were going that day. How much a session's speed counts
depends on its raw accuracy: nothing at or below 90%, fully from 98%, so speed
bought by accepting errors is not rewarded. From your fifth completed session
on, and every five after that, `typ` also fits a small model of how much of a
pattern's slowness is explained by its context: where it sits in its words,
how long and how common they are, and what your fingers have to do to reach
it on your layout (same finger, same hand, row change, key distance). That
separates a pattern that is merely awkward on the keyboard from one you are
personally weak on. `typ stats` lists your recent completed
sessions with their ids, then the ten patterns you are slowest on relative to
your baseline and the ten you most often get wrong, each with how much
evidence is behind it. Every statistic is a cache: `typ rebuild` recomputes
all of them from your stored sessions, and an upgrade that changes the
algorithm does so automatically. `typ replay <id>` shows how a stored session
was interpreted: each word's first attempt, its own raw accuracy, and the
patterns its errors count against, and which keystroke intervals count as
clean motor evidence. The scheduler that turns these statistics into targeted
prompts is being built on top of this.

```
$ typ --version
$ typ
$ typ --words 30 --profile laptop
$ typ config words 30
$ typ config profile laptop
$ typ stats
$ typ rebuild
$ typ replay 12
```

Set `NO_COLOR` to get bold and underline instead of colour. `Ctrl-C` or `Esc`
ends a session early.

Data lives in a single SQLite file under your platform's data directory:
`~/.local/share/typ/typ.db` on Linux, `~/Library/Application Support/typ/` on
macOS, `%APPDATA%\typ\` on Windows. Nothing leaves your machine.

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
[`docs/smoke-test.md`](docs/smoke-test.md). Set `TYP_DATA_DIR` to point a run
at a different database directory, so that experiments do not touch your own
history.

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

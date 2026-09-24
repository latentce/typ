# typ

A typing trainer for the terminal that works out which letter combinations
slow you down or trip you up, and builds each practice prompt around them.

```
$ typ
the quick brown fox jumps over the lazy dog ...

68 wpm  96.2% raw  99.4% final  71% consistency
74 wpm on standard text  +3 vs recent
next: th, ou␣, ing (exploring ␣wr)
```

- Prompts are real English words, not random letter soup.
- Everything runs locally in a single SQLite file. No account, no network.
- It tracks characters, bigrams, and trigrams (spaces included), so "the end
  of words that finish in `ou`" is a thing it can notice and practise.
- It measures whether the practice actually transfers to ordinary text, and
  is honest about the difference between a real gain and noise.

How the adaptation works is described in
[`docs/how-it-works.md`](docs/how-it-works.md).

## Status

Early. The core loop (type, get measured, get a targeted prompt) is done and
stable enough to use daily. Only the QWERTY layout exists. Not yet on
crates.io.

## Install

From a checkout:

```
cargo install --path crates/typ-rs
```

This puts a `typ` binary in `~/.cargo/bin`.

## Using it

### A session

Run `typ`. The prompt appears below your shell prompt; start typing. Mistakes
show in red, backspace fixes them. `Ctrl-C` or `Esc` ends early.

When you finish, three lines print:

| Line | Meaning |
| --- | --- |
| `68 wpm  96.2% raw  99.4% final  71% consistency` | Gross WPM; accuracy on your first try at each character; accuracy of what you submitted after corrections; how even your keystroke timing was. |
| `74 wpm on standard text  +3 vs recent` | Your speed translated to ordinary text. Targeted prompts are deliberately harder, so raw WPM drops when targeting kicks in; this figure corrects for that and is the one to watch. `baseline recorded` on your first session. |
| `next: th, ou␣, ing (exploring ␣wr)` | The patterns the next prompt will practise, weakest first, plus one the model is merely unsure about. `␣` is a space, so `ou␣` means "words ending in ou". |

Your first session is a plain sample of common words. From the second session
on, part of the prompt targets your weaknesses; that share ramps from 30% to
80% over your first few sessions.

Interrupted sessions are saved too. They report how many words you got
through and no speed.

### Settings and profiles

```
typ --words 30               # 30 words, this run only
typ config words 30          # 30 words from now on (10 to 200)
typ config words             # show the current setting

typ --profile laptop         # use a profile for this run (created if new)
typ config profile laptop    # switch to it permanently
typ config layout            # show this profile's layout (only qwerty exists)
```

A profile is an isolated history. Use one per physically different setup: a
different keyboard, a different layout. Stats never mix across profiles.

### Looking at your history

```
typ stats
```

Shows your last ten sessions, then:

- **probes**: speed and accuracy over your last hundred "probe" words (words
  chosen at random, not for practice), marked `sustained improvement` or
  `sustained decline` only when the change is statistically separable from
  the hundred before.
- **word initiation**: median time to start each word, per session.
- **transfer to untargeted words**: for each recently practised pattern, how
  you type it in words used for drilling versus words that were not.
- **slowest / most error-prone / weakest patterns**, each with how much
  evidence backs the estimate.
- **deferred candidates**: patterns being held back as controls.

```
typ replay 12          # how session 12 was interpreted, word by word
typ replay 12 --diff   # what the current algorithm would say differently
typ rebuild            # recompute every statistic from stored sessions
```

Every statistic is a cache over the raw keystroke log. `rebuild` is safe to
run any time, and runs automatically after an upgrade that changes the
algorithm.

### Environment

| Variable | Effect |
| --- | --- |
| `NO_COLOR` | Bold and underline instead of colour. |
| `TYP_DATA_DIR` | Use this directory for the database instead of the platform default. |

Data lives in one file: `~/.local/share/typ/typ.db` on Linux,
`~/Library/Application Support/typ/typ.db` on macOS, `%APPDATA%\typ\typ.db`
on Windows.

## Developing

You need a Rust toolchain and [`just`](https://github.com/casey/just).
`just` on its own lists every recipe.

| Recipe | What it does |
| --- | --- |
| `just run [args]` | Build debug and run `typ`. `just run stats`, `just run --words 20`, etc. |
| `just build` / `just release` | Build the `typ` binary, debug or release. |
| `just test` | `cargo test --workspace`. |
| `just lint` | Clippy with warnings as errors. |
| `just fmt` | `cargo fmt --all`. |
| `just check` | Format check, lint, and tests. Run before committing. |
| `just sim [args]` | Run the simulator against synthetic typists (release build). |
| `just sim-gate` | The simulator's integration tests with optimisations on. |
| `just doc` | Build and open rustdoc for the workspace. |
| `just install` / `just uninstall` | Install `typ` from this checkout into `~/.cargo/bin`, or remove it. |

Aliases: `just r`, `b`, `t`, `c` for run, build, test, check.

Point `TYP_DATA_DIR` at a scratch directory while developing so experiments
stay out of your own history:

```
TYP_DATA_DIR=/tmp/typ-dev just run
```

### Where things live

```
crates/
  typ-rs-core/    session state machine, analysis, model, scheduler, composer, corpus
  typ-rs-store/   SQLite persistence, migrations, rebuild
  typ-rs/         the `typ` binary: terminal loop, CLI, reports
  typ-sim/        simulator: drives the real pipeline with synthetic learners
docs/
  how-it-works.md   how a session becomes the next prompt
  architecture.md   crates, data flow, persistence, versioning
  simulator.md      what the simulator is for and what it has shown
  smoke-test.md     manual checklist for the terminal loop
```

The terminal loop is checked by hand; run through
[`docs/smoke-test.md`](docs/smoke-test.md) after touching `crates/typ-rs/src/`.

## Licence

[MIT](LICENSE-MIT).

The bundled word list is derived from the English unigram list in
[orgtre/google-books-ngram-frequency](https://github.com/orgtre/google-books-ngram-frequency),
computed from the Google Books Ngram Viewer Exports v3, both under
[CC BY 3.0](https://creativecommons.org/licenses/by/3.0/). The unmodified
source and licence text are in
[`crates/typ-rs-core/corpus/`](crates/typ-rs-core/corpus/).

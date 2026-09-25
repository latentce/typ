# typ

A typing trainer for the terminal that works out which letter combinations
slow you down or trip you up, and builds each practice prompt around them.

```
$ typ
the quick brown fox jumps over the lazy dog ...

68 wpm  96.2% accuracy  99.4% after corrections  71% consistency
74 wpm on standard text  +3 vs recent
next: th, ou␣, ing (exploring ␣wr)
```

- Prompts are real English words, not random letter soup.
- Everything runs locally in a single SQLite file. No account, no network.
- It tracks characters, bigrams, and trigrams (spaces included), so "the end
  of words that finish in `ou`" is a thing it can notice and practice.
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

Run `typ`. The prompt appears below your shell prompt in gray; start typing.
Correct letters take your normal text color, mistakes show the expected
letter in red, and anything typed past the end of a word is added in a
darker red. Space moves to the next word whether or not the word is right;
a word left wrong is underlined. Backspace fixes mistakes, and steps back
into the previous word only if that word was left wrong. `Shift-Tab` throws
the prompt away and shows a fresh one in its place, as often as you like.
`Ctrl-C` or `Esc` ends early.

When you finish, three lines print:

| Line | Meaning |
| --- | --- |
| `68 wpm  96.2% accuracy  99.4% after corrections  71% consistency` | Gross WPM; accuracy on your first try at each character; accuracy of what you submitted after corrections; how even your keystroke timing was. |
| `74 wpm on standard text  +3 vs recent` | Your speed translated to ordinary text. Targeted prompts are deliberately harder, so raw WPM drops when targeting kicks in; this figure corrects for that and is the one to watch. `baseline recorded` on your first session. |
| `next: th, ou␣, ing (exploring ␣wr)` | The patterns the next prompt will practice, weakest first, plus one the model is merely unsure about. `␣` is a space, so `ou␣` means "words ending in ou". |

Your first session is a plain sample of common words. From the second session
on, part of the prompt targets your weaknesses; that share ramps from 30% to
80% over your first few sessions.

Interrupted sessions are saved too. They report how many words you got
through and no speed. Nothing is saved, though, if you leave or restart
before typing anything: a prompt you walk away from is simply shown again
next time.

### Settings and profiles

```
typ --words 30               # 30 words, this run only
typ config words 30          # 30 words from now on (10 to 200)
typ config words             # show the current setting

typ --profile laptop         # use a profile for this run (created if new)
typ config profile laptop    # switch to it permanently
typ config layout            # show this profile's layout (only qwerty exists)

typ config cursor-shape block    # block, beam (the default), or underline
typ config cursor-blink on       # on or off (the default)
```

A profile is an isolated history. Use one per physically different setup: a
different keyboard, a different layout. Stats never mix across profiles.

The cursor settings are yours, not a profile's: they apply to every profile.
While a session runs, `typ` gives the terminal's cursor the shape and blink
you chose, and hands the terminal its own cursor back when the session ends.

### Looking at your history

```
typ stats
```

Your progress view. It opens with a headline: your speed on standard text
and your accuracy, each with how it has moved (`▲ +6` in green, `▼ -2` in
red) against your recent sessions, and whether your probe words show a
`sustained improvement` or `sustained decline`, a marker given only when
the change is statistically separable from the hundred probe words before.
Speed is the recent trend of your speed on standard text, not the raw WPM of
the last prompt, so a hard practice prompt never makes you look slower than
you are.

Below that, once you have two sessions, two charts: your speed on standard
text, then your accuracy, over your recent sessions, oldest on the left.
Each is a framed box with a legend: every session is a `•`, and the trend is
the line drawn through them, so a single noisy session stands apart from
where you are heading: the speed line is your recent series, the accuracy
line the mean of your last five sessions. The two charts share one session
axis, each session in the same column of both, so a session's speed and
accuracy sit one above the other; the footer names the first and last
sessions shown as `#id · date`. The speed chart plots speed on standard
text rather than the WPM of each prompt, for the same reason as the
headline: practice prompts differ in difficulty from session to session, so
the prompt's own WPM would draw a hard prompt as a slow day. (A session
whose speed on standard text is not yet known leaves a gap in that chart,
which the line runs across, rather than being drawn at zero.) The boxes
grow with the number of sessions up to a fixed size and shrink only in a
terminal narrower than that; a wider terminal does not stretch them. The
accuracy axis runs from just below your lowest recent accuracy to 100, so
small differences stay visible instead of being squashed into the top of
the box. The trend line is green where color is available and the charts
read the same without it.

Then a table of your last ten sessions (id, when, words, wpm, speed on
standard text, accuracy, and accuracy after corrections), and a **focus**
block naming the five patterns the trainer is working on, each tagged
`slow`, `error-prone`, or `both`, and the patterns your next session will
practice.

The session ids are what `typ replay` takes.

```
typ inspect            # the model's view: what it believes about your patterns, with the evidence
typ replay 12          # how session 12 was interpreted, word by word
typ replay 12 --diff   # what the current algorithm would say differently
typ rebuild            # recompute every statistic from stored sessions
```

Every statistic is a cache over the raw keystroke log. `rebuild` is safe to
run any time, and runs automatically after an upgrade that changes the
algorithm. `inspect` prints what the model believes in full: the probe
windows, word-initiation medians per session, transfer from drilled words to
untargeted ones, the slowest, most error-prone, and weakest patterns with the
evidence behind each estimate, and the candidates being held back as
controls.

### Environment

| Variable | Effect |
| --- | --- |
| `NO_COLOR` | Bold and underline instead of color while typing; no color in `typ stats`. |
| `TYP_DATA_DIR` | Use this directory for the database instead of the default. |

Data lives in one file: `~/.local/share/typ/typ.db` on Linux,
`~/Library/Application Support/typ/typ.db` on macOS, `%APPDATA%\typ\typ.db`
on Windows. That is for an installed `typ`; a debug build keeps its own file
under `target/typ-data` in its source tree (see [Developing](#developing)).

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
| `just sim-gate` | The simulator's integration tests with optimizations on. |
| `just doc` | Build and open rustdoc for the workspace. |
| `just install` / `just uninstall` | Install `typ` from this checkout into `~/.cargo/bin`, or remove it. |

Aliases: `just r`, `b`, `t`, `c` for run, build, test, check.

A debug build never touches your own history: unless `TYP_DATA_DIR` is set,
it keeps its database at `target/typ-data/typ.db` in the checkout, separate
from the installed `typ`'s. `cargo clean` removes it. A release build
(`just release`, `./target/release/typ`) uses the same directory as an
installed `typ`, so point it elsewhere if you run one by hand:

```
TYP_DATA_DIR=/tmp/typ-dev ./target/release/typ
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

## License

[MIT](LICENSE-MIT).

The bundled word list is derived from the English unigram list in
[orgtre/google-books-ngram-frequency](https://github.com/orgtre/google-books-ngram-frequency),
computed from the Google Books Ngram Viewer Exports v3, both under
[CC BY 3.0](https://creativecommons.org/licenses/by/3.0/). The unmodified
source and license text are in
[`crates/typ-rs-core/corpus/`](crates/typ-rs-core/corpus/).

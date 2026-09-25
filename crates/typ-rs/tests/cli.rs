use std::path::Path;
use std::process::Command;

use typ_rs_core::compose::{ComposedPrompt, ComposedWord, WordRole};
use typ_rs_core::corpus::{CORPUS_VERSION, Corpus};
use typ_rs_core::metrics::summarize;
use typ_rs_core::prompt::Prompt;
use typ_rs_core::scheduler::{SelectedTarget, TargetRole, achieved_doses};
use typ_rs_core::session::{EndCondition, Input, Key, SessionState};
use typ_rs_store::{DEFAULT_PROFILE, SessionEnd, SessionStart, Store};

struct Run {
    ok: bool,
    stdout: String,
    stderr: String,
}

/// Runs `typ` with stdin closed and stdout captured, so it is not attached
/// to a terminal, against a database under `data_dir`. The time zone is fixed
/// so that listed times are predictable.
fn typ_in(data_dir: &Path, args: &[&str]) -> Run {
    let output = Command::new(env!("CARGO_BIN_EXE_typ"))
        .args(args)
        .env("TYP_DATA_DIR", data_dir)
        .env("TZ", "UTC")
        .output()
        .expect("typ binary runs");
    Run {
        ok: output.status.success(),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

fn typ(args: &[&str]) -> Run {
    let dir = tempfile::tempdir().unwrap();
    typ_in(dir.path(), args)
}

/// Types `script` against `prompt`, one key per 100 ms, and stores the
/// session as started at `started_at`, applied to the profile's statistics
/// and summarized the way a live session is. Whatever prompt is waiting is
/// replaced by `prompt` again, as probes only, so every session in a test
/// types `prompt`. `⌫` is backspace and `⎋` an interrupt.
fn store_session(store: &mut Store, started_at: i64, prompt: &str, script: &str) {
    store_session_with(
        store,
        started_at,
        prompt,
        script,
        100_000,
        Vec::new(),
        Vec::new(),
    );
}

/// As [`store_session`], typed at one key per `pace_micros`.
fn store_session_at_pace(
    store: &mut Store,
    started_at: i64,
    prompt: &str,
    script: &str,
    pace_micros: u64,
) {
    store_session_with(
        store,
        started_at,
        prompt,
        script,
        pace_micros,
        Vec::new(),
        Vec::new(),
    );
}

/// As [`store_session`], typed at one key per `pace_micros`, with the
/// given patterns selected for the prompt waiting afterward and the given
/// words of it shown as targeted.
fn store_session_with(
    store: &mut Store,
    started_at: i64,
    prompt: &str,
    script: &str,
    pace_micros: u64,
    next_targets: Vec<SelectedTarget>,
    next_targeted_words: Vec<&str>,
) {
    let profile = store.profile(DEFAULT_PROFILE).unwrap();
    let words = || Prompt::new(prompt.split(' '));
    let started = store
        .start_session(
            &profile,
            SessionStart {
                started_at,
                seed: 1,
                word_count: words().word_count(),
            },
            || ComposedPrompt::probes(words()),
        )
        .unwrap();
    assert_eq!(started.prompt, words());
    let mut state = SessionState::new(
        started.prompt.clone(),
        EndCondition::AfterWords(started.prompt.word_count()),
    );
    for (i, c) in script.chars().enumerate() {
        let key = match c {
            '⎋' => Key::Interrupt,
            '⌫' => Key::Backspace,
            c => Key::Char(c),
        };
        state.apply_event(Input::new(i as u64 * pace_micros, key));
    }
    let mut model = store.model(&profile).unwrap();
    let update = model.apply_session(&state, started_at, Corpus::bundled(), store.config());
    let events = achieved_doses(&state, &started.targets);
    let recent = store.recent_series(&profile).unwrap();
    let summary = summarize(
        &state,
        &update,
        &started.words,
        recent.as_ref(),
        store.config(),
    );
    let mut next = ComposedPrompt::probes(words());
    next.targets = next_targets;
    for (word, meta) in next.prompt.words().iter().zip(&mut next.words) {
        if next_targeted_words.contains(&word.as_ref()) {
            *meta = ComposedWord {
                role: WordRole::Targeted,
                exposed_targets: Vec::new(),
                selection_score: Some(0.0),
                contamination: None,
            };
        }
    }
    store
        .finish_session(
            started.id,
            &SessionEnd {
                state: &state,
                model: &model,
                events: &events,
                summary: summary.as_ref(),
                next_prompt: &next,
                ended_at: started_at + 60,
            },
        )
        .unwrap();
}

fn target(pattern: &str, role: TargetRole) -> SelectedTarget {
    SelectedTarget {
        pattern: pattern.into(),
        role,
        weakness_mean: 0.5,
        weakness_sd: 0.2,
        priority: 0.1,
        planned_dose: 6,
    }
}

/// How many rows of braille a trend chart's body has.
const CHART_ROWS: usize = 13;

/// A trend chart taken apart: its header lines, the body rows between the
/// two `│` with the whole-number label after each, and the footer.
struct Chart<'a> {
    header: Vec<&'a str>,
    rows: Vec<&'a str>,
    labels: Vec<Option<u32>>,
    footer: &'a str,
}

/// A trend chart as `stats` prints it to a pipe: `title` over the header,
/// a box 32 columns wide holding thirteen rows of braille with a label on
/// every third row, and a footer naming the first and last sessions across
/// the box's 34 columns.
fn assert_chart<'a>(chart: &'a str, title: &str, first: &str, last: &str) -> Chart<'a> {
    let lines: Vec<&str> = chart.lines().collect();
    let top = lines
        .iter()
        .position(|l| l.starts_with('┌'))
        .unwrap_or_else(|| panic!("{chart}"));
    assert!(lines[0].starts_with(title), "{chart}");
    assert_eq!(lines[top], format!("┌{}┐", "─".repeat(32)), "{chart}");
    let bottom = top + 1 + CHART_ROWS;
    assert_eq!(lines[bottom], format!("└{}┘", "─".repeat(32)), "{chart}");
    assert_eq!(lines.len(), bottom + 2, "{chart}");
    let mut rows = Vec::new();
    let mut labels = Vec::new();
    for line in &lines[top + 1..bottom] {
        let (row, rest) = line
            .strip_prefix('│')
            .and_then(|l| l.split_once('│'))
            .unwrap_or_else(|| panic!("{line:?}"));
        assert_eq!(row.chars().count(), 32, "{line:?}");
        rows.push(row);
        labels.push(
            rest.strip_prefix(' ')
                .map(|l| l.parse().unwrap_or_else(|_| panic!("{line:?}"))),
        );
    }
    assert!(
        rows.iter()
            .flat_map(|row| row.chars())
            .any(|c| ('\u{2801}'..='\u{28ff}').contains(&c)),
        "{chart}"
    );
    assert!(
        labels
            .iter()
            .enumerate()
            .all(|(r, l)| l.is_some() == (r % 3 == 0)),
        "{chart}"
    );
    let footer = lines[bottom + 1];
    assert!(footer.starts_with(&format!(" {first} ")), "{footer:?}");
    assert!(footer.ends_with(last), "{footer:?}");
    assert_eq!(footer.chars().count(), 34, "{footer:?}");
    Chart {
        header: lines[..top].to_vec(),
        rows,
        labels,
        footer,
    }
}

impl Chart<'_> {
    /// The labels on rows 0, 3, 6, 9, and 12, top first.
    fn labels(&self) -> Vec<u32> {
        self.labels.iter().flatten().copied().collect()
    }

    /// The column of every `•` in the body, in column order.
    fn markers(&self) -> Vec<usize> {
        let mut columns: Vec<usize> = self
            .rows
            .iter()
            .flat_map(|row| {
                row.chars()
                    .enumerate()
                    .filter(|(_, c)| *c == '•')
                    .map(|(i, _)| i)
            })
            .collect();
        columns.sort_unstable();
        columns
    }

    /// Whether any row has a braille cell (not blank) at `column`.
    fn braille_at(&self, column: usize) -> bool {
        self.rows.iter().any(|row| {
            row.chars()
                .nth(column)
                .is_some_and(|c| ('\u{2801}'..='\u{28ff}').contains(&c))
        })
    }
}

#[test]
fn version_shows_the_license_and_the_corpus_attribution_under_both_flags() {
    for flag in ["--version", "-V"] {
        let run = typ(&[flag]);
        let stdout = &run.stdout;

        assert!(run.ok);
        assert!(
            stdout.starts_with(&format!("typ {}\n", env!("CARGO_PKG_VERSION"))),
            "{flag}: {stdout}"
        );
        assert!(stdout.contains("License: MIT"), "{flag}: {stdout}");
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
    let dir = tempfile::tempdir().unwrap();
    let run = typ_in(dir.path(), &[]);

    assert!(!run.ok);
    assert_eq!(run.stdout, "");
    assert_eq!(run.stderr.lines().count(), 1, "{:?}", run.stderr);
    assert!(
        run.stderr.contains("interactive terminal"),
        "{:?}",
        run.stderr
    );
    assert!(!dir.path().join("typ.db").exists());
}

#[test]
fn inspect_and_stats_on_a_fresh_data_directory_create_the_database_and_list_nothing() {
    for command in ["inspect", "stats"] {
        let dir = tempfile::tempdir().unwrap();
        let run = typ_in(dir.path(), &[command]);

        assert!(run.ok, "{command}: {}", run.stderr);
        assert_eq!(run.stdout, "no completed sessions yet\n", "{command}");
        assert_eq!(run.stderr, "", "{command}");
        assert!(dir.path().join("typ.db").is_file());
    }
}

#[test]
fn inspect_lists_completed_sessions_most_recent_first_and_skips_interrupted_ones() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = Store::open(&dir.path().join("typ.db")).unwrap();
        // 2024-01-15 10:30:00 UTC: "cat dg" is 6 final characters over 0.6 s.
        store_session(&mut store, 1_705_314_600, "cat dog", "cat dg ");
        // 2024-01-16 08:00:00 UTC: 7 characters over 0.6 s, all correct.
        store_session(&mut store, 1_705_392_000, "cat dog", "cat dog");
        store_session(&mut store, 1_705_400_000, "cat dog", "ca⎋");
    }

    let inspect = typ_in(dir.path(), &["inspect"]);
    assert!(inspect.ok, "{}", inspect.stderr);

    let mut sections = inspect.stdout.split("\n\n");
    // Every keystroke took 100 ms, so each session's speed on standard
    // text is 120 wpm: the pace of its clean intervals, whatever the
    // prompt. Gross WPM counts one character more than there are
    // intervals, which shows on a two-word prompt.
    assert_eq!(
        sections.next().unwrap(),
        "  id  when                  words      wpm      on standard text         raw       consistency\n\
         \x20  2  2024-01-16 08:00    2 words  140 wpm  120 on standard text  100.0% raw  100% consistency\n\
         \x20  1  2024-01-15 10:30    2 words  120 wpm  120 on standard text   83.3% raw  100% consistency"
    );
    // Four probe words: 15 characters (the last word of the second session
    // ended without a space) over 1.2 s, with 11 of 12 target characters
    // right the first time. Too few for a previous window, so no marker.
    assert_eq!(
        sections.next().unwrap(),
        "probes\n  last   4 uncontaminated  150 wpm   91.7% raw"
    );
    assert_eq!(
        sections.next().unwrap(),
        "word initiation (median ms, oldest first)\n  100  100"
    );
    let slowest = sections.next().unwrap();
    assert!(slowest.starts_with("slowest patterns\n  "), "{slowest}");
    // Nothing is slower than the baseline.
    assert!(
        slowest
            .lines()
            .skip(1)
            .all(|l| l.contains("   +0%  n_eff ")),
        "{slowest}"
    );
    let errors = sections.next().unwrap();
    // `dg` for `dog` omitted the `o`: the pattern ending there heads the
    // list, with the whole chain behind it.
    assert!(
        errors.starts_with("most error-prone patterns\n  ␣do  "),
        "{errors}"
    );
    assert!(
        errors.lines().nth(2).unwrap().starts_with("  do  "),
        "{errors}"
    );
    assert!(
        errors.lines().nth(3).unwrap().starts_with("  o   "),
        "{errors}"
    );
    let weakest = sections.next().unwrap();
    assert!(
        weakest.starts_with("weakest patterns\n  ␣do  +"),
        "{weakest}"
    );
    assert!(weakest.lines().nth(1).unwrap().contains(" ± "), "{weakest}");
    assert_eq!(sections.next(), None);
}

#[test]
fn inspect_shows_transfer_of_recently_practiced_patterns_to_untargeted_words() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = Store::open(&dir.path().join("typ.db")).unwrap();
        // The second prompt targets `at` through "cat"; "hat" is a probe
        // in both sessions and never targeted.
        store_session_with(
            &mut store,
            1_705_314_600,
            "cat hat",
            "cat hat",
            100_000,
            vec![target("at", TargetRole::Target)],
            vec!["cat"],
        );
        store_session(&mut store, 1_705_392_000, "cat hat", "cat hat");
    }

    let run = typ_in(dir.path(), &["inspect"]);
    assert!(run.ok, "{}", run.stderr);
    let transfer = run
        .stdout
        .split("\n\n")
        .find(|s| s.starts_with("transfer to untargeted words"))
        .unwrap_or_else(|| panic!("{}", run.stdout));
    // "cat" was targeted in the last ten sessions, so both its `t` slots
    // are on the targeted side; both of "hat" are on the other.
    assert_eq!(
        transfer,
        "transfer to untargeted words\n\
         \x20 at   targeted  100 ms 100.0% raw  n   2  untargeted  100 ms 100.0% raw  n   2"
    );
}

#[test]
fn inspect_lines_up_a_transfer_row_without_targeted_slots_with_one_that_has_them() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = Store::open(&dir.path().join("typ.db")).unwrap();
        // The second prompt targets `at` and `og` but only "cat" is shown
        // as targeted, so `og` is met in "dog" on the untargeted side alone.
        store_session_with(
            &mut store,
            1_705_314_600,
            "cat hat dog",
            "cat hat dog",
            100_000,
            vec![
                target("at", TargetRole::Target),
                target("og", TargetRole::Target),
            ],
            vec!["cat"],
        );
        store_session(&mut store, 1_705_392_000, "cat hat dog", "cat hat dog");
    }

    let run = typ_in(dir.path(), &["inspect"]);
    assert!(run.ok, "{}", run.stderr);
    let transfer = run
        .stdout
        .split("\n\n")
        .find(|s| s.starts_with("transfer to untargeted words"))
        .unwrap_or_else(|| panic!("{}", run.stdout));
    let rows: Vec<&str> = transfer.lines().skip(1).collect();
    assert_eq!(
        rows,
        [
            "  at   targeted  100 ms 100.0% raw  n   2  untargeted  100 ms 100.0% raw  n   2",
            "  og   targeted   -- ms     -- raw  n   0  untargeted  100 ms 100.0% raw  n   2",
        ]
    );
    assert_eq!(rows[0].len(), rows[1].len());
}

#[test]
fn inspect_shows_the_deferred_candidates_with_their_windows() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = Store::open(&dir.path().join("typ.db")).unwrap();
        store_session(&mut store, 1_705_314_600, "cat dog", "cat dog");
        // The prompt composed ahead defers `og` and targets `at`.
        store_session_with(
            &mut store,
            1_705_392_000,
            "cat dog",
            "cat dog",
            100_000,
            vec![
                target("at", TargetRole::Target),
                target("og", TargetRole::Deferred),
            ],
            vec![],
        );
    }

    let run = typ_in(dir.path(), &["inspect"]);
    assert!(run.ok, "{}", run.stderr);
    let deferred = run.stdout.split("\n\n").last().unwrap();
    assert_eq!(
        deferred,
        "deferred candidates\n  og   2 sessions remaining\n"
    );
}

#[test]
fn the_inspect_listing_header_lines_up_with_its_rows_whether_or_not_a_figure_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = Store::open(&dir.path().join("typ.db")).unwrap();
        store_session(&mut store, 1_705_314_600, "cat dog", "cat dog");
        store_session(&mut store, 1_705_392_000, "cat dog", "cat dog");
    }
    // A session applied to the model but not yet summarized has no speed
    // on standard text to show.
    rusqlite::Connection::open(dir.path().join("typ.db"))
        .unwrap()
        .execute_batch("DELETE FROM session_metrics WHERE session_id = 1")
        .unwrap();

    let run = typ_in(dir.path(), &["inspect"]);
    assert!(run.ok, "{}", run.stderr);
    let listing: Vec<&str> = run.stdout.split("\n\n").next().unwrap().lines().collect();
    let [header, full, sparse] = listing[..] else {
        panic!("{}", run.stdout);
    };
    assert_eq!(
        header,
        "  id  when                  words      wpm      on standard text         raw       consistency"
    );
    assert_eq!(
        full,
        "   2  2024-01-16 08:00    2 words  140 wpm  120 on standard text  100.0% raw  100% consistency"
    );
    assert_eq!(
        sparse,
        "   1  2024-01-15 10:30    2 words  140 wpm   -- on standard text  100.0% raw  100% consistency"
    );
    assert_eq!(header.len(), full.len());
    assert_eq!(full.len(), sparse.len());
}

// --- Progress --------------------------------------------------------------

#[test]
fn stats_with_one_session_shows_the_headline_without_changes_and_asks_for_one_more() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = Store::open(&dir.path().join("typ.db")).unwrap();
        // 2024-01-16 08:00:00 UTC: 7 characters over 0.6 s, all correct, at
        // 100 ms a keystroke, which is 120 wpm on standard text. The prompt
        // composed ahead targets `at` and explores `og`.
        store_session_with(
            &mut store,
            1_705_392_000,
            "cat dog",
            "cat dog",
            100_000,
            vec![
                target("at", TargetRole::Target),
                target("og", TargetRole::Explore),
            ],
            vec![],
        );
    }

    let run = typ_in(dir.path(), &["stats"]);
    assert!(run.ok, "{}", run.stderr);
    assert_eq!(run.stderr, "");
    let mut blocks = run.stdout.split("\n\n");
    assert_eq!(
        blocks.next().unwrap(),
        "120 wpm on standard text\n\
         100.0% accuracy\n\
         not enough probes yet to call a trend"
    );
    assert_eq!(
        blocks.next().unwrap(),
        "complete 1 more session to see your trend"
    );
    // At 80 columns the table has just the room its figures need, so the
    // two long headers wrap while every figure stays on one line.
    assert_eq!(
        blocks.next().unwrap(),
        "┌───┬──────────────────┬───────┬─────┬───────────────┬──────────┬──────────────┐\n\
         │ # ┆ when             ┆ words ┆ wpm ┆   on standard ┆ accuracy ┆        after │\n\
         │   ┆                  ┆       ┆     ┆          text ┆          ┆  corrections │\n\
         ╞═══╪══════════════════╪═══════╪═════╪═══════════════╪══════════╪══════════════╡\n\
         │ 1 ┆ 2024-01-16 08:00 ┆     2 ┆ 140 ┆           120 ┆   100.0% ┆       100.0% │\n\
         └───┴──────────────────┴───────┴─────┴───────────────┴──────────┴──────────────┘"
    );
    let focus = blocks.next().unwrap();
    let lines: Vec<&str> = focus.lines().collect();
    assert_eq!(lines[0], "focus", "{focus}");
    assert_eq!(lines[2], "│ pattern ┆ why  │", "{focus}");
    // Nothing was slow or wrong, so every pattern is tagged `slow` by
    // default: five of them, each on its own row between rules.
    assert!(
        lines[4..lines.len() - 1]
            .iter()
            .step_by(2)
            .all(|l| l.ends_with(" ┆ slow │")),
        "{focus}"
    );
    assert_eq!(
        lines.last().unwrap(),
        &"next session practices: at (exploring og)",
        "{focus}"
    );
    assert_eq!(lines.len(), 2 + 2 + 5 * 2 + 1, "{focus}");
    assert_eq!(blocks.next(), None);
    // No chart: no line carries the legend.
    assert!(!run.stdout.contains("⠒ trend"), "{}", run.stdout);
}

#[test]
fn stats_with_rising_speed_shows_the_change_charts_the_trends_and_tables_the_sessions() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = Store::open(&dir.path().join("typ.db")).unwrap();
        // Each session faster than the one before: 100, 80, then 60 ms a
        // keystroke; an interrupted one among them.
        store_session_at_pace(&mut store, 1_705_314_600, "cat dog", "cat dog", 100_000);
        store_session_at_pace(&mut store, 1_705_392_000, "cat dog", "cat dg ", 80_000);
        store_session(&mut store, 1_705_400_000, "cat dog", "ca⎋");
        store_session_at_pace(&mut store, 1_705_478_400, "cat dog", "cat dog", 60_000);
    }
    // A session applied to the model but not yet summarized has no speed
    // on standard text, and its recent series is unknown. It is the middle
    // one, so the gap falls inside the charts and the oldest session, which
    // the headline compares against, keeps its figures.
    rusqlite::Connection::open(dir.path().join("typ.db"))
        .unwrap()
        .execute_batch("DELETE FROM session_metrics WHERE session_id = 2")
        .unwrap();

    let run = typ_in(dir.path(), &["stats"]);
    assert!(run.ok, "{}", run.stderr);
    let mut blocks = run.stdout.split("\n\n");
    let headline: Vec<&str> = blocks.next().unwrap().lines().collect();
    let speed = headline[0];
    let (level, change) = speed.split_once("  ").unwrap_or_else(|| panic!("{speed}"));
    let level = level
        .strip_suffix(" wpm on standard text")
        .unwrap_or_else(|| panic!("{speed}"));
    assert!(level.parse::<u32>().unwrap() > 120, "{speed}");
    let change = change
        .strip_prefix("▲ +")
        .unwrap_or_else(|| panic!("{speed}"));
    assert!(change.parse::<u32>().unwrap() > 0, "{speed}");
    // The mean over the three sessions, the second analyzed on the spot;
    // nothing to compare it with yet.
    assert_eq!(headline[1], "94.4% accuracy");
    assert_eq!(headline[2], "not enough probes yet to call a trend");

    // Both charts cover the same three sessions in a box 32 columns wide,
    // each session in the same column of both. The speed chart notes the
    // session without a speed on standard text and leaves its column
    // empty but for the trend line running across it. Stdout is not a
    // terminal, so no color.
    let speed = assert_chart(
        blocks.next().unwrap(),
        "speed on standard text · 3 sessions",
        "#1 · 2024-01-15",
        "#4 · 2024-01-17",
    );
    assert_eq!(
        speed.header,
        [
            "speed on standard text · 3 sessions   1 without a speed on standard text",
            " • session  ⠒ trend",
        ]
    );
    let labels = speed.labels();
    assert!(labels.iter().all(|l| l.is_multiple_of(5)), "{labels:?}");
    assert!(labels.windows(2).all(|w| w[0] > w[1]), "{labels:?}");
    assert!((labels[0] - labels[4]).is_multiple_of(20), "{labels:?}");
    assert_eq!(speed.markers(), [0, 31]);
    assert!(speed.braille_at(16), "{}", speed.rows.join("\n"));

    let accuracy = assert_chart(
        blocks.next().unwrap(),
        "accuracy · 3 sessions",
        "#1 · 2024-01-15",
        "#4 · 2024-01-17",
    );
    assert_eq!(
        accuracy.header,
        ["accuracy · 3 sessions   • session  ⠒ trend"]
    );
    // The sessions' accuracies are 100%, 83.3%, and 100%.
    assert_eq!(accuracy.labels(), [100, 95, 90, 85, 80]);
    assert_eq!(accuracy.markers(), [0, 16, 31]);
    assert_eq!(speed.footer, accuracy.footer);
    assert!(!run.stdout.contains('\x1b'), "{}", run.stdout);

    let table: Vec<&str> = blocks.next().unwrap().lines().collect();
    assert_eq!(
        table[1..3],
        [
            "│ # ┆ when             ┆ words ┆ wpm ┆   on standard ┆ accuracy ┆        after │",
            "│   ┆                  ┆       ┆     ┆          text ┆          ┆  corrections │",
        ]
    );
    // 7 characters over 0.36 s; 6 final characters over 0.48 s, with 5 of 6
    // target characters right the first time; 7 characters over 0.6 s.
    assert!(
        table[4].starts_with("│ 4 ┆ 2024-01-17 08:00 ┆     2 ┆ 233 ┆ "),
        "{}",
        table[4]
    );
    assert!(
        table[4].ends_with(" ┆   100.0% ┆       100.0% │"),
        "{}",
        table[4]
    );
    assert_eq!(
        table[6],
        "│ 2 ┆ 2024-01-16 08:00 ┆     2 ┆ 150 ┆            -- ┆    83.3% ┆        66.7% │"
    );
    assert_eq!(
        table[8],
        "│ 1 ┆ 2024-01-15 10:30 ┆     2 ┆ 140 ┆           120 ┆   100.0% ┆       100.0% │"
    );
    assert_eq!(table.len(), 10, "{}", table.join("\n"));
    assert!(!run.stdout.contains("│ 3 ┆"), "{}", run.stdout);
    assert!(!run.stdout.contains("complete 1 more"), "{}", run.stdout);
    assert!(blocks.next().unwrap().starts_with("focus\n"));
}

#[test]
fn rebuild_reports_how_many_sessions_it_reapplied_and_changes_nothing_visible() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = Store::open(&dir.path().join("typ.db")).unwrap();
        store_session(&mut store, 1_705_314_600, "cat dog", "cat dg ");
        store_session(&mut store, 1_705_392_000, "cat dog", "cat dog");
        store_session(&mut store, 1_705_400_000, "cat dog", "ca⎋");
    }
    let before = typ_in(dir.path(), &["inspect"]);
    let progress_before = typ_in(dir.path(), &["stats"]);

    let run = typ_in(dir.path(), &["rebuild"]);
    assert!(run.ok, "{}", run.stderr);
    assert_eq!(run.stdout, "rebuilt the statistics from 3 sessions\n");
    assert_eq!(run.stderr, "");

    let after = typ_in(dir.path(), &["inspect"]);
    assert_eq!(after.stdout, before.stdout);
    assert_eq!(
        typ_in(dir.path(), &["stats"]).stdout,
        progress_before.stdout
    );

    let run = typ(&["rebuild"]);
    assert!(run.ok, "{}", run.stderr);
    assert_eq!(run.stdout, "rebuilt the statistics from 0 sessions\n");
}

#[test]
fn replay_shows_how_every_word_and_interval_of_a_stored_session_was_interpreted() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = Store::open(&dir.path().join("typ.db")).unwrap();
        store_session(&mut store, 1_705_314_600, "cat dog", "cxt⌫⌫at dog");
    }

    let run = typ_in(dir.path(), &["replay", "1"]);
    assert!(run.ok, "{}", run.stderr);
    assert_eq!(run.stderr, "");
    assert_eq!(
        run.stdout,
        "session 1  2024-01-15 10:30  completed  2 words\n\
         84 wpm  83.3% accuracy  100.0% after corrections  100% consistency\n\
         4 corrections  0 uncorrected  error latency 300 ms  5 clean intervals\n\
         \n\
         words\n\
         \x20 0 cat  first attempt \"cxt\"  history \"cxtat\"  67% raw\n\
         \x20     substitution x at 1 → \" ca\"\n\
         \x20 1 dog  first attempt \"dog\"  100% raw\n\
         \n\
         intervals\n\
         \x20seq       at  latency  slot  key  class\n\
         \x20  0        0        -   0:0  c    excluded: first_of_session\n\
         \x20  1      100      100   0:1  x    clean\n\
         \x20  2      200      100   0:2  t    clean\n\
         \x20  3      300      100   0:3  ⌫    excluded: backspace\n\
         \x20  4      400      100   0:2  ⌫    excluded: backspace, after_correction\n\
         \x20  5      500      100   0:1  a    excluded: replacement, after_correction\n\
         \x20  6      600      100   0:2  t    excluded: replacement, after_correction\n\
         \x20  7      700      100   0:3  ␣    excluded: after_correction\n\
         \x20  8      800      100   1:0  d    clean\n\
         \x20  9      900      100   1:1  o    clean\n\
         \x20 10     1000      100   1:2  g    clean\n"
    );
}

#[test]
fn replay_of_an_interrupted_session_marks_the_unsubmitted_word_and_shows_no_speed() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = Store::open(&dir.path().join("typ.db")).unwrap();
        store_session(&mut store, 1_705_314_600, "cat dog fox", "cat do⎋");
    }

    let run = typ_in(dir.path(), &["replay", "1"]);
    assert!(run.ok, "{}", run.stderr);
    let lines: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(
        lines[0],
        "session 1  2024-01-15 10:30  interrupted  3 words"
    );
    assert_eq!(lines[1], "interrupted after 1 word");
    assert_eq!(lines[6], "  1 dog  first attempt \"do\"  (not submitted)");
    assert!(!run.stdout.contains("fox"), "{}", run.stdout);
}

#[test]
fn replay_of_an_unknown_session_fails_with_one_line() {
    let run = typ(&["replay", "7"]);
    assert!(!run.ok);
    assert_eq!(run.stdout, "");
    assert_eq!(run.stderr, "typ: no session 7\n");
}

#[test]
fn replay_diff_compares_the_current_pipeline_with_the_stored_summary() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = Store::open(&dir.path().join("typ.db")).unwrap();
        store_session(&mut store, 1_705_314_600, "cat dog", "cat dg ");
        store_session(&mut store, 1_705_392_000, "cat dog", "cat dog");
        store_session(&mut store, 1_705_400_000, "cat dog", "ca⎋");
    }
    for id in ["1", "2"] {
        let run = typ_in(dir.path(), &["replay", id, "--diff"]);
        assert!(run.ok, "{}", run.stderr);
        assert_eq!(
            run.stdout,
            "no differences between the stored summary and the current pipeline\n"
        );
    }
    let run = typ_in(dir.path(), &["replay", "3", "--diff"]);
    assert!(run.ok, "{}", run.stderr);
    assert_eq!(run.stdout, "no summary: the session was interrupted\n");

    // A stored figure that the pipeline no longer produces shows up.
    rusqlite::Connection::open(dir.path().join("typ.db"))
        .unwrap()
        .execute_batch("UPDATE session_metrics SET gross_wpm = 99 WHERE session_id = 2")
        .unwrap();
    let run = typ_in(dir.path(), &["replay", "2", "--diff"]);
    assert!(run.ok, "{}", run.stderr);
    assert_eq!(
        run.stdout,
        "differences from the stored summary\n\
         \x20 gross_wpm                        stored    99.0000  current   140.0000\n"
    );
}

// --- Configuration -----------------------------------------------------------

/// Runs `typ` expecting success with nothing on stderr; returns stdout.
fn ok(data_dir: &Path, args: &[&str]) -> String {
    let run = typ_in(data_dir, args);
    assert!(run.ok, "{args:?}: {}", run.stderr);
    assert_eq!(run.stderr, "", "{args:?}");
    run.stdout
}

/// Runs `typ` expecting failure with one line on stderr and nothing on
/// stdout; returns that line.
fn one_line_error(data_dir: &Path, args: &[&str]) -> String {
    let run = typ_in(data_dir, args);
    assert!(!run.ok, "{args:?}: {}", run.stdout);
    assert_eq!(run.stdout, "", "{args:?}");
    assert_eq!(run.stderr.lines().count(), 1, "{args:?}: {:?}", run.stderr);
    run.stderr
}

#[test]
fn config_shows_the_defaults_on_a_fresh_data_directory() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(ok(dir.path(), &["config", "words"]), "50\n");
    assert_eq!(ok(dir.path(), &["config", "layout"]), "qwerty\n");
    assert_eq!(ok(dir.path(), &["config", "profile"]), "default\n");
    assert_eq!(ok(dir.path(), &["config", "cursor-shape"]), "beam\n");
    assert_eq!(ok(dir.path(), &["config", "cursor-blink"]), "off\n");
}

#[test]
fn config_sets_a_value_silently_and_reads_it_back() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(ok(dir.path(), &["config", "words", "30"]), "");
    assert_eq!(ok(dir.path(), &["config", "words"]), "30\n");
    assert_eq!(ok(dir.path(), &["config", "layout", "qwerty"]), "");
    assert_eq!(ok(dir.path(), &["config", "layout"]), "qwerty\n");
    assert_eq!(ok(dir.path(), &["config", "profile", "alt"]), "");
    assert_eq!(ok(dir.path(), &["config", "profile"]), "alt\n");
    assert_eq!(ok(dir.path(), &["config", "cursor-shape", "block"]), "");
    assert_eq!(ok(dir.path(), &["config", "cursor-shape"]), "block\n");
    assert_eq!(ok(dir.path(), &["config", "cursor-blink", "on"]), "");
    assert_eq!(ok(dir.path(), &["config", "cursor-blink"]), "on\n");
}

#[test]
fn the_cursor_settings_are_shared_by_every_profile() {
    let dir = tempfile::tempdir().unwrap();
    ok(dir.path(), &["config", "cursor-shape", "underline"]);
    assert_eq!(
        ok(dir.path(), &["config", "cursor-shape", "--profile", "alt"]),
        "underline\n"
    );
    ok(
        dir.path(),
        &["config", "cursor-blink", "on", "--profile", "alt"],
    );
    assert_eq!(ok(dir.path(), &["config", "cursor-blink"]), "on\n");
}

#[test]
fn config_rejects_invalid_values_with_one_line_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    ok(dir.path(), &["config", "words", "30"]);

    for bad in ["5", "201", "abc", "30.0"] {
        let line = one_line_error(dir.path(), &["config", "words", bad]);
        assert!(line.starts_with("typ: "), "{line}");
        assert!(line.contains("10") && line.contains("200"), "{line}");
        assert!(line.contains(bad), "{line}");
    }
    let line = one_line_error(dir.path(), &["config", "layout", "dvorak"]);
    assert!(line.contains("dvorak") && line.contains("qwerty"), "{line}");
    let line = one_line_error(dir.path(), &["config", "profile", "my profile"]);
    assert!(line.contains("my profile"), "{line}");
    let line = one_line_error(dir.path(), &["config", "cursor-shape", "bar"]);
    assert!(
        line.contains("bar") && line.contains("block, beam, underline"),
        "{line}"
    );
    let line = one_line_error(dir.path(), &["config", "cursor-blink", "yes"]);
    assert!(line.contains("yes") && line.contains("on or off"), "{line}");

    assert_eq!(ok(dir.path(), &["config", "words"]), "30\n");
    assert_eq!(ok(dir.path(), &["config", "layout"]), "qwerty\n");
    assert_eq!(ok(dir.path(), &["config", "profile"]), "default\n");
    assert_eq!(ok(dir.path(), &["config", "cursor-shape"]), "beam\n");
    assert_eq!(ok(dir.path(), &["config", "cursor-blink"]), "off\n");
}

#[test]
fn an_unknown_setting_fails() {
    let run = typ(&["config", "color"]);
    assert!(!run.ok);
    assert!(run.stderr.contains("color"), "{}", run.stderr);
}

/// Whether `command`'s output shows the session started at `when` (a
/// local `YYYY-MM-DD HH:MM`): in `inspect`'s listing, or `stats`'s table.
fn shows_session(data_dir: &Path, command: &str, when: &str) -> bool {
    let stdout = ok(data_dir, &[command]);
    match command {
        "inspect" => stdout
            .lines()
            .skip(1)
            .any(|line| line.contains(&format!("  {when}  "))),
        _ => stdout.contains(&format!(" ┆ {when} ┆ ")),
    }
}

#[test]
fn switching_profile_switches_whose_settings_and_sessions_are_shown() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = Store::open(&dir.path().join("typ.db")).unwrap();
        store_session(&mut store, 1_705_392_000, "cat dog", "cat dog");
    }
    ok(dir.path(), &["config", "words", "30"]);
    for command in ["inspect", "stats"] {
        assert!(shows_session(dir.path(), command, "2024-01-16 08:00"));
    }

    ok(dir.path(), &["config", "profile", "alt"]);
    assert_eq!(ok(dir.path(), &["config", "words"]), "50\n");
    for command in ["inspect", "stats"] {
        assert_eq!(ok(dir.path(), &[command]), "no completed sessions yet\n");
    }

    ok(dir.path(), &["config", "profile", "default"]);
    assert_eq!(ok(dir.path(), &["config", "words"]), "30\n");
    for command in ["inspect", "stats"] {
        assert!(shows_session(dir.path(), command, "2024-01-16 08:00"));
    }
}

#[test]
fn the_profile_flag_applies_to_one_run_and_does_not_change_the_active_profile() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut store = Store::open(&dir.path().join("typ.db")).unwrap();
        store_session(&mut store, 1_705_392_000, "cat dog", "cat dog");
    }
    ok(dir.path(), &["--profile", "alt", "config", "words", "30"]);
    assert_eq!(ok(dir.path(), &["config", "profile"]), "default\n");
    assert_eq!(ok(dir.path(), &["config", "words"]), "50\n");
    assert_eq!(
        ok(dir.path(), &["config", "words", "--profile", "alt"]),
        "30\n"
    );
    for command in ["inspect", "stats"] {
        assert_eq!(
            ok(dir.path(), &[command, "--profile", "alt"]),
            "no completed sessions yet\n"
        );
        assert!(shows_session(dir.path(), command, "2024-01-16 08:00"));
    }
}

#[test]
fn the_words_flag_is_validated_and_only_applies_to_a_session() {
    let dir = tempfile::tempdir().unwrap();
    for bad in ["5", "abc"] {
        let line = one_line_error(dir.path(), &["--words", bad]);
        assert!(line.contains("10") && line.contains("200"), "{line}");
        assert!(line.contains(bad), "{line}");
    }
    for command in ["inspect", "stats"] {
        let line = one_line_error(dir.path(), &["--words", "20", command]);
        assert!(line.contains("--words"), "{command}: {line}");
    }
    assert!(!dir.path().join("typ.db").exists());
}

#[test]
fn the_profile_flag_is_rejected_where_it_would_have_no_effect() {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        &["rebuild", "--profile", "alt"][..],
        &["--profile", "alt", "replay", "1"][..],
    ] {
        let line = one_line_error(dir.path(), args);
        assert!(line.contains("--profile"), "{args:?}: {line}");
        assert!(
            line.contains("stats") && line.contains("inspect"),
            "{args:?}: {line}"
        );
    }
    assert!(!dir.path().join("typ.db").exists());
}

#[test]
fn a_profile_is_created_the_first_time_it_is_named_whatever_the_command() {
    let dir = tempfile::tempdir().unwrap();
    ok(dir.path(), &["--profile", "a", "config", "profile"]);
    ok(dir.path(), &["--profile", "b", "stats"]);
    ok(dir.path(), &["--profile", "c", "config", "layout"]);
    ok(dir.path(), &["--profile", "d", "inspect"]);
    let db = Store::open(&dir.path().join("typ.db")).unwrap();
    for name in ["a", "b", "c", "d"] {
        assert_eq!(db.profile(name).unwrap().layout, "qwerty", "{name}");
    }
}

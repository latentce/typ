# Manual smoke test: the terminal session

The session loop, raw mode, and terminal restoration cannot be covered by
automated tests. Run through this by hand after touching anything under
`crates/typ-rs/src/`.

Every step must leave a usable shell: cursor visible, typed text echoed,
Enter runs commands. If a step leaves the terminal broken, `reset` recovers
it, and the bug is in the raw-mode guard.

Setup:

```
just build
```

The steps use `just run`, which runs the debug binary. A debug build keeps
its database at `target/typ-data/typ.db`, so these sessions never enter your
own history. To start from nothing, `rm -r target/typ-data` first.

1. **Start.** `just run`. The line `— wpm   0:00` appears in muted gray
   directly below the command, and the prompt in muted gray below it with
   the terminal's cursor, now a steady thin bar, before its first character.
   There is no countdown, menu, or cleared screen.
2. **Type.** Type a few words with some mistakes. Correct characters take the
   normal foreground, mistakes are red, extras past the end of a word appear
   in a darker red after it and push the following text right. Press space
   on a word with a mistake or a missing letter: the whole word is underlined
   and its missing letters stay gray; the space after it is not underlined.
   Backspace removes characters and extras; backspace at the start of a word
   only re-enters the previous word if it was left with an error, and the
   underline goes away while you are in it. Only the cells that changed are
   repainted, and the cursor does not flicker or visibly jump.
3. **Header.** On your first keystroke the header loses its gray but still
   reads `— wpm   0:00`; one second later it shows a speed and `0:01`, and
   from then on both change once a second, on the second, never with a
   keystroke. Stop typing for a few seconds: the clock keeps counting and
   the speed falls with every tick. Type a burst: the prompt updates at once
   but the header waits for the next tick. On a fresh database the speed is
   in the terminal's own color however fast you type; once a session has
   completed (step 6), type well above your usual speed and it turns green,
   dawdle and it turns red, and near your average it is uncolored.
4. **Resize.** Drag the terminal narrower and wider while typing, with the
   cursor on the second line or later. The header stays on its own row above
   the prompt; the prompt re-wraps at word boundaries and repaints in full;
   the cursor stays on the next expected character; nothing is left behind
   above or below. (The repaint assumes the terminal re-wraps long lines
   when narrowed, as kitty and most others do; one that clips them instead
   may repaint too high after a narrowing.) Shrink the terminal until it is
   shorter than the prompt: the prompt scrolls to keep the cursor visible,
   and the header stays put.
5. **Paste.** Paste a few words. Nothing is typed and the caret does not
   move.
6. **Complete.** Type the prompt to the end. On the final character (or a
   space after the final word) the header stops, keeping its color, and a
   line with gross WPM, accuracy, accuracy after corrections, and
   consistency appears below the prompt, then a `next:` line naming the
   patterns the next prompt will practice (`next: none yet` only if no
   session has completed yet, since the first prompt is a baseline), and
   the shell prompt returns. The WPM in the header and the WPM on the
   results line are the same number. The header, the whole prompt, and the
   results stay in scrollback. Run `just run` again: a few words of the new
   prompt contain the patterns named, and their share grows over the next
   few sessions.
7. **`Ctrl-C`.** `just run`, type a word or two, press `Ctrl-C`. The header
   freezes in muted gray at the moment of the interrupt, the session ends
   with `interrupted after N words` and the shell is back.
8. **`Esc`.** Same as the previous step with `Esc`.
9. **Restart.** `just run` and, before typing, press `Shift-Tab`: a
   different prompt is painted where the first one was, with the cursor
   before its first character and nothing left over above or below. Type
   half a word with a mistake, wait for the header to show a speed, and
   press `Shift-Tab` again: the typed characters and their colors are gone
   with the prompt, and the header is back to `— wpm   0:00` in gray until
   your first keystroke into the new prompt, when its clock starts again
   from `0:00`. Type this prompt to the end: the results print as usual,
   and `just run stats` lists it once, with no trace of the two prompts
   that were thrown away.
10. **`Esc` untyped.** `just run`, note the first few words, and press `Esc`
    without typing. The line `nothing typed` prints below the prompt, with
    the gray `— wpm   0:00` still above it, no results and no
    `next:` line, and the shell is back. `just run` again: the same prompt
    is shown. Press `Shift-Tab`, then `Esc`: `nothing typed` again, and the
    next `just run` shows the restarted prompt. `just run stats` lists no
    new session.
11. **Induced panic.** `TYP_PANIC_AFTER=3 just run`, then type three
    characters. The panic message prints below the prompt on a restored
    terminal and the shell is usable. (The variable is honored only by debug
    builds.)
12. **`NO_COLOR`.** `NO_COLOR=1 just run`. Untyped text is dim, mistakes are
    bold and underlined, nothing is colored. The header is dim until the
    first keystroke and plain text after, whatever the speed: no bold, no
    underline.
13. **Cursor.** `just run config cursor-shape block` then `just run`: the
    cursor is a block. `just run config cursor-blink on` then `just run`: it
    blinks. `just run config cursor-shape underline`: an underline. After
    each session ends or is interrupted, the shell's cursor is back to what
    it was before. `just run config cursor-shape beam` and `just run config
    cursor-blink off` restore the defaults. `just run config cursor-shape
    bar` refuses on one line.
14. **Narrow terminal.** Make the terminal narrower than 20 columns and run
    `just run`. It refuses with a one-line message and exits without touching
    the terminal.
15. **Not a terminal.** `just run < /dev/null` refuses with a one-line
    message.
16. **Stats during a session.** `just run` in one terminal and, while it is
    waiting for input, `just run stats` in another (from the same
    checkout). The headline gives your speed on standard text and accuracy
    with their changes as colored `▲`/`▼` arrows (or without a change after
    the first session, with `complete 1 more session to see your trend`
    beneath), the table lists the sessions completed above, most recent
    first, as wide as the terminal, and the `focus` block tags the five
    weakest patterns and names what the next session practices; neither
    command disturbs the other. `just run inspect` shows the same sessions
    in plain text under a header row that lines up with them, the slowest,
    most error-prone, and weakest patterns so far (the last as `mean ± sd`)
    and, once the scheduler has held some candidates back, `deferred
    candidates` with the sessions remaining. Finish or interrupt the
    session: the next `just run` shows a different prompt.
17. **Charts.** With two or more sessions completed, `just run stats` shows
    the headline with a change, then two charts between it and the table:
    `speed on standard text · N sessions` and `accuracy · N sessions`, each
    a framed box of thirteen rows with the legend `• session  ⠒ recent
    average` on its title line, every session a `•` in the terminal's own
    color, a green line drawn through them, five whole-number labels down
    the right of the frame (`100` topping the accuracy chart), and a footer
    naming the first and last sessions as `#id · date`. Under the second
    chart, one line: `recent average: newer sessions count more, a session
    5 back half as much`. The two charts line up session for session: each
    `•` in the speed chart has one in the same column of the accuracy
    chart. Widen the terminal: the boxes stay the same size; narrow it
    below the box's width and they shrink to fit. `NO_COLOR=1 just run
    stats` shows the same charts with the line uncolored, and the arrows
    plain.
18. **Replay.** `just run replay N` with an id from the table. The first
    lines repeat the results the session printed; below them every word
    shows its first attempt, its own raw accuracy, and the errors attributed
    to patterns, and every
    keystroke its interval class. The corrections you typed in step 2 show
    as `excluded: backspace` / `replacement`, the resize as `after_resize`,
    the paste as `in_paste`, and any long pause as a hesitation.
19. **Rebuild.** `just run stats > /tmp/before`, then `just run rebuild`
    (it reports how many sessions it reapplied) and `just run stats` again:
    the output is identical to `/tmp/before`. On a database from before
    this version, `stats` first shows `-- accuracy` and a line ending `run
    typ rebuild to fill it in` under the charts; the rebuild fills the
    recent accuracy in for every older session and both go away.
20. **Settings.** `just run --words 12`: the prompt has 12 words; interrupt
    it. `just run config words 15`, then `just run`: the prompt has 15
    words even though one was composed ahead at 50; interrupt it. `just run
    config words 5` refuses on one line and `just run config words` still
    prints `15`. `just run --profile smoke`: a 50-word prompt (the new
    profile's default); interrupt it, then `just run stats --profile smoke`
    shows nothing while `just run stats` still lists your sessions, and
    `just run config profile` still prints `default`.

Setting `TYP_DIAGNOSTICS=1` prints the render timing per input batch (count,
mean, max) to stderr after the results, for checking that painting stays well
under a millisecond.

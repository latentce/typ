# Manual smoke test: the terminal session

The session loop, raw mode, and terminal restoration cannot be covered by
automated tests, so run through this by hand after touching anything under
`crates/typ-rs/src/`. Every step must leave a usable shell: cursor visible,
typed text echoed, Enter runs commands. If a step leaves the terminal broken,
`reset` recovers it; the bug is in the guard.

Build first with `just build` (or `cargo build -p typ-rs`); the commands below
use `just run`, which runs the debug binary. Export
`TYP_DATA_DIR=/tmp/typ-smoke` first so the sessions typed here are kept
apart from your own history.

1. **Start.** `just run`. The prompt appears in muted gray directly below the
   command with a reverse-video caret on its first character. There is no
   countdown, menu, or cleared screen.
2. **Type.** Type a few words with some mistakes. Correct characters take the
   normal foreground, mistakes are red, extras past the end of a word appear
   in red after it and push the following text right. Backspace removes
   characters and extras; backspace at the start of a word only re-enters the
   previous word if it was left with an error. Only the cells that changed
   are repainted: no flicker.
3. **Resize.** Drag the terminal narrower and wider while typing. The prompt
   re-wraps at word boundaries and repaints in full; the caret stays on the
   next expected character; nothing is left behind. Shrink the terminal
   until it is shorter than the prompt: the prompt scrolls to keep the caret
   visible.
4. **Paste.** Paste a few words. Nothing is typed and the caret does not
   move.
5. **Complete.** Type the prompt to the end. On the final character (or a
   space after the final word) a line with gross WPM and final accuracy
   appears below the prompt and the shell prompt returns. The whole prompt
   and the results stay in scrollback.
6. **`Ctrl-C`.** `just run`, type a word or two, press `Ctrl-C`. The session
   ends with `interrupted after N words` and the shell is back.
7. **`Esc`.** Same as the previous step with `Esc`.
8. **Induced panic.** `TYP_PANIC_AFTER=3 just run`, then type three
   characters. The panic message prints below the prompt on a restored
   terminal and the shell is usable. (The variable is honoured only by debug
   builds.)
9. **`NO_COLOR`.** `NO_COLOR=1 just run`. Untyped text is dim, mistakes are
   bold and underlined, nothing is coloured.
10. **Narrow terminal.** Make the terminal narrower than 20 columns and run
    `just run`. It refuses with a one-line message and exits without touching
    the terminal.
11. **Not a terminal.** `just run < /dev/null` refuses with a one-line
    message.
12. **Stats during a session.** `just run` in one terminal and, while it is
    waiting for input, `just run stats` in another (with the same
    `TYP_DATA_DIR`). The listing shows the sessions completed above, most
    recent first, and neither command disturbs the other. Finish or
    interrupt the session: the next `just run` shows a different prompt.

Setting `TYP_DIAGNOSTICS=1` prints the render timing per input batch (count,
mean, max) to stderr after the results, for checking that painting stays well
under a millisecond.

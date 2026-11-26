# Measuring scrolling

How closely the page follows a finger on a trackpad, measured the same way
each time so that a change can be compared with what came before. Use a
long page that you scroll for a while, and the same one each time. The
scripts need Python 3.9 or later and nothing else; they are tools for
working on kb, not part of it, and CI doesn't run them. See
"Scrolling" in [docs/ARCHITECTURE.md](../../docs/ARCHITECTURE.md) for what
was found.

1. **Record a scroll.** Run kb with `KB_TRACE` and scroll by hand:

   ```sh
   KB_TRACE=recorded.tsv kb https://example.com
   awk -F'\t' '$2 == "tick"' recorded.tsv > ticks.tsv
   ```

2. **Replay it** into a kb in a new pane, typed in as the terminal would
   send the wheel, and note when each frame reached the terminal:

   ```sh
   REPLAY=ticks.tsv INJECT=run.inject KB_TRACE=run.tsv \
     python3 tools/scroll/relay.py run.log -- kb https://example.com
   ```

   Wait for the page to load, put it back at the top
   (`kb ctl scroll top`), then `touch ticks.tsv.go` to start. The replay is
   over when `run.inject` ends with `done`.

   To measure over ssh, relay `ssh -t host 'KB_TRACE=run.tsv kb URL'`
   instead, and copy `run.tsv` back afterwards.

3. **Score it:**

   ```sh
   python3 tools/scroll/score.py run.tsv run.log 0 run.inject
   ```

   The `0` is the difference between the two machines' clocks in
   milliseconds, when kb ran on another one (its clock less this one's).

Run each case at least twice: a busy terminal or network can stall a run
for a second or more, and the page's own animations differ between runs.

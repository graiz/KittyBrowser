---
name: kb
description: "A real browser in a terminal pane beside you, driven with `kb ctl`. The user watches each page and click as you work. Prefer it to a browser extension or Playwright when working with someone in the terminal: they see what you do, their sign-ins stay, and sites work as in any browser (Reddit and LinkedIn too). Use it to preview and test web pages and local dev servers, check for errors, try phone layouts, run QA and test scripts, and look things up, showing the pages you cite. Also use it when the user asks to open, show or check a page."
---
<!-- Installed by "kb skill install". Remove it with "kb skill remove". -->

# kb

kb is a browser in a terminal pane. You open it beside your pane, the user
watches the page there, and you drive it with `kb ctl`. Use it to show your
work as well as to do it.

## Why kb

- **The user sees it.** They watch you work, can stop you, and can take over
  with the mouse. Playwright and other headless tools show nothing until you
  report back.
- **Sign-ins stay.** kb has its own profile, apart from the user's Chrome:
  they sign in once and stay signed in, popups like "Continue with Google"
  included. Only the first kb running has it; one opened beside it starts
  fresh, so to use their sign-ins, reuse their kb if they say so, or ask them
  to close it. A kb the user named (`kb --name work`) always has its own
  profile; reach it with `-i work` when they say to.
- **Sites work as in any browser,** including ones some extensions don't
  support, such as Reddit or LinkedIn. kb gets around nothing: a site's terms
  apply, and you act for the user, at a person's pace.

When nobody needs to watch (a CI job, say), a headless tool is fine.

`kb ctl --help` lists every command; `kb ctl <command> --help` explains one.

## Your kb, in a pane beside yours

Several kbs can run at once. Each has a number and an id (`3-a7f2`); always
pass your kb's id with `-i`. Without `-i`, commands go to a default kb, which
may be someone else's. Once a kb closes, its number passes to the next kb to
start, while its id just stops working. Never drive a kb you didn't open. If
one you opened earlier in this conversation is still in `kb list`, reuse it.

To open one in Rex (`$REX_SESSION` is set; Rex calls a pane a block), split
a pane and find your kb by that pane, since others may be starting kbs at the same moment. Run it as
one shell command:

```sh
url=https://example.com   # the page to open
pane=$(rex split -s "$REX_SESSION" -b "$REX_BLOCK" --split=right --focus=false --label kb --json -- kb "$url" | jq -r '.block_ids[0]')
for _ in $(seq 1 40); do
  id=$(kb list 2>/dev/null | awk -F'\t' -v p="$pane" '$3 == p {print $4}')
  [ -n "$id" ] && break
  sleep 0.25
done
echo "kb $id in $pane"
```

In another terminal, open a pane beside yours with the terminal's own
command, without taking the user's focus (herdr: `herdr pane split`, kitty:
`kitten @ launch`, WezTerm: `wezterm cli split-pane`), and run `kb "$url"` in
it. If it has no such command, ask the user to open a pane and run `kb URL`.
Then take the id of the kb `kb list` didn't show before, and check
`kb ctl -i ID status` shows your page, in case someone else's started at the
same time. `kb list` prints one kb a line, tab-separated: number, page,
pane, id. (tmux, screen and Zellij can't show kb's images: open it in a pane
of the terminal itself.)

Each shell command starts afresh, so write the id (and pane) into every
later command: `kb ctl -i 3-a7f2 open URL`. Below, `ID` stands for it, and
outside code blocks a command is named alone: `box` means `kb ctl -i ID box`.

A new pane is often narrow, so sites show their phone layout. For reading,
or to see the desktop layout, run `viewport 1280` first; given only a width,
kb makes the page as tall as the pane.

Leave your pane open unless asked to close it: the user may want to keep
looking. Close it with `rex block close "$pane"` in Rex, or the terminal's
own command; that quits the kb. If it's already gone, there's nothing to
close.

Lost your id? Find it in `kb list` by your pane, or stop and ask. If kb
won't start, or its pane stays blank, `kb doctor` run in that pane says why.

### When the user closes your pane

If your kb is gone from `kb list`, or `kb ctl -i ID` says it isn't running,
the user closed it; it didn't crash. They most likely wanted it out of the
way, which overrides an earlier ask to show it in a pane, so don't open a new
pane for it. If
the task can finish without them watching, carry on headless; if you can't
tell whether they want you to go on, ask, or carry on if there's no one to
ask (as when you're a subagent). `kb ctl` may say "kb closed the connection"
for a command that was running as the pane closed.

`kb --headless URL` runs a kb in the background, with the same commands, and
prints its id first (`kb 3-a7f2, headless`): that's yours. Like any kb, it
has the signed-in profile only if no other kb is running; `--name NAME`
gives it a profile of its own, kept for next time. It has a desktop
viewport (1280x800) and no pane. Stop it with `kb ctl -i ID quit` when
you're done; one left behind quits after ten minutes without a command. `kb
list` marks headless kbs, and shows "(not answering)" for a moment while a
kb starts or quits.

## Selectors

CSS (the first visible match; it reaches into shadow DOM, and `a >>> b`
picks `b` inside `a`'s shadow root, or inside the page in iframe `a`),
`text=Some words` (the element showing them), `@N` (a number from `links`),
or `:focus`.

## Asking the user to point

When the user means something on the page ("this button", "that box is
off", "the second card"), don't guess a selector from their words: ask them
to show you. Say "click it in kb", then run `kb ctl pick`. kb goes into
inspect mode in the pane, outlining what the mouse is over; the user's
click picks the element (the page gets no click), and `pick` answers with
its `selector` line, ready for `click`, `text`, `box`, `style` and the
rest, with its size, styles and text (and a `link` line for a link). To
see exactly what they pointed at, run `kb ctl screenshot --selector` with
that selector. It waits up to a minute; if the user
presses Esc, or nothing is picked, it fails and says so. It needs kb in a
pane the user can see; a headless kb can't pick.

## Testing your own pages

After each change:

```sh
kb ctl -i ID reload                               # or open URL; waits for the load, reports errors
kb ctl -i ID check                                # exit 0 if healthy, 1 on console errors, failed requests or an error page
kb ctl -i ID screenshot --out "$TMPDIR/page.png"  # then read the image
```

Only `check` sets the exit status; the other commands report what they find.

Then try what you changed. Read the page's HTML for selectors rather than
guessing: `html form` for a form's fields, `html body` for the whole page,
including where a result will appear. HTML includes hidden elements; `text`
shows only what's visible:

```sh
kb ctl -i ID type '#email' 'a@b.co' --clear
kb ctl -i ID select '#plan' Pro  # by value or label
kb ctl -i ID upload '#avatar' me.png # the file field, its label, or the button that opens its chooser
kb ctl -i ID click 'text=Save'   # fails if hidden, off screen or covered
kb ctl -i ID wait '#result' --text Saved # the element the click should change, and words you expect
kb ctl -i ID console --errors    # this page's; --all adds earlier pages'
kb ctl -i ID network --failed
```

`click`, `type`, `press` and `select` answer once the page settles, with the
errors, dialogs and downloads they caused. What comes later (after a timer or a
request) isn't there yet, so please `wait` for it before reading.

- `wait SELECTOR` alone passes as soon as the element exists, even empty;
  add `--text` or `--visible` to wait for what it shows.
- `wait --text` matches visible text, case and all. If you don't know the
  words, `wait SELECTOR --visible`, then `text SELECTOR`. It also takes
  `--count N`, `--gone` and `--timeout` (10 s by default).
- A typed value shows when you read that field (`text '#name'`), not in the
  whole page's `text`.

When something looks wrong or does nothing:

| Command | Shows |
|---|---|
| `box SELECTOR` | where it is, whether it's rendered, what covers it |
| `style SELECTOR [PROPERTY] [--why]` | computed CSS; `--why` lists the rules that set it, winner first, with file and line |
| `listeners SELECTOR` | its event listeners; none may mean a parent handles the event |
| `storage` | localStorage, sessionStorage and cookie names |
| `clear-storage` | clears the site's cookies and storage (signing it out); then `reload` tests a first visit |
| `html SELECTOR`, `eval 'JS'` | anything else; wrap declarations in `(() => { ... })()` |

Also:

- **Dark mode:** `scheme dark` (or `light`, or `auto` to follow the terminal).
- **Slow network:** `throttle slow` (or `fast`, `offline`), then `throttle off`.
- **Phone layout:** `status` shows the viewport and whether it was set;
  `viewport 375x800`, look (`text` or a screenshot), then
  `viewport reset`, which drops any size you set: the kb follows its pane
  again (a headless kb goes back to 1280x800). A viewport stays set across
  pages until reset. A control hidden at that width makes `click` fail with
  "the element has no size".
- **Dev servers:** use the address the server printed, not a guessed port.
  Local pages (files, localhost, private addresses, `.local`, `.test`,
  `.vibe`) are never cached, so an edit always shows. Relative paths for
  `open` and `screenshot --out` start from your working directory.

## QA and test scripts

A script is one shell, so a helper function works there. Assert with exit
codes:

```sh
set -e
id=3-a7f2                                        # your kb's id, from kb list
k() { kb ctl -i "$id" "$@"; }
k open "$base/login"
k type '#user' tomsmith; k type '#pass' 'SuperSecretPassword!'
k click 'button[type=submit]'
k wait 'text=You logged into a secure area'      # fails after 10 s (--timeout)
[ "$(k text '#count')" = "3" ] || { echo "FAIL: expected 3 items"; exit 1; }
k check --ignore optimizely.com,doubleclick.net  # third-party noise doesn't count
```

- `text SELECTOR` prefixes a hidden element's text with `(hidden: …)`.
- `click --double` double-clicks; `click SELECTOR --at X,Y` clicks a point
  in an element (a canvas, a map). If the mouse click doesn't reach the
  page, `click` clicks by script instead and says so.
- `alert`, `confirm` and `prompt` that your commands set off are accepted
  and reported by the command that caused them; `dialog dismiss` or
  `dialog accept --text TEXT` changes that for later ones. Ones the user
  sets off are asked in the pane; while one waits, commands to its page
  fail saying so, and `status` names it.
  Leave it to the user, or answer it with `dialog accept` or `dismiss`.
- A download is reported by the command that caused it, with the path it
  was saved to (Chrome's downloads folder, numbered when a name repeats):
  read the file there. One still running when the command answers shows in
  `status`, and `console` records where it was saved when it finishes.
- `status` saying `responding: no` means a script on the page runs without
  a pause, and commands that need the page will wait on it. `reload` or
  `open` stops the script and leaves the page.
- `eval` reads what no command does (an order, a count, one value).

## Reading and research

Read with text, not screenshots; it's far cheaper:

```sh
kb ctl -i ID viewport 1280
kb ctl -i ID open 'https://html.duckduckgo.com/html/?q=sqlite+fts5+bm25'
kb ctl -i ID links --match sqlite.org       # links and buttons (not plain text), with real addresses
kb ctl -i ID click @3
kb ctl -i ID text                           # the page's main content
kb ctl -i ID outline                        # title and headings (there's no title command)
kb ctl -i ID text --section 'Ranking'       # one section, by its heading's words; scrolls to it
kb ctl -i ID find 'bm25'                    # where it appears, with its section (first 10; --limit N)
kb ctl -i ID tables; kb ctl -i ID tables 2  # list tables; print one as tab-separated rows
kb ctl -i ID images                         # alt text, size, caption and source
```

- When you cite a page, leave the passage on screen (`find` and
  `text --section` scroll to it) so the user sees it; with no pane, skip
  this.
- Console errors on other people's sites are their noise, not yours to fix.
- A bot check ("Just a moment", a CAPTCHA) is reported as one; kb can't pass
  it, so try another source. Searching many times in a row brings one on:
  search once, then read the results.
- Don't log in, post, or submit forms other than searches unless the user
  asks.

## Things to know

- One page per kb, no tabs; open a second pane to compare two pages.
- A window the page opens (a sign-in popup, a new tab) is shown in its
  place; click and type in it as on any page. `click` says when one opens
  or closes; `window` lists them, and `window N` shows another.
- A popup that finishes closes itself. To dismiss one, run `back` while it
  shows the page it opened with; that closes it.
- Screenshots are one image pixel per CSS pixel. `--size small` halves
  that, `--size sharp` gives device pixels, `--size 800` a set width.
  Prefer `--selector CSS` for one element; `--full` pages stop at 8000
  pixels tall.
- Commands look inside an iframe only when asked: `iframe#pay >>> #card`.
  A frame from another site can't be reached; open its address instead.

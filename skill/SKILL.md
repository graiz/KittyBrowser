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
- **Sign-ins stay.** kb keeps its own profile (`~/.kittybrowser/profile`),
  apart from the user's everyday Chrome. They sign in to a site once and stay
  signed in; sign-in popups ("Continue with Google") work. Only the first kb
  running uses that profile: one opened beside it gets a fresh profile, so to
  use their sign-ins, reuse their kb if they say so, or ask them to close it.
- **Sites work as in any browser,** including ones some extensions don't
  support, such as Reddit or LinkedIn. kb gets around nothing: a site's terms
  apply, and you act for the user, at a person's pace.

When nobody needs to watch (a CI job, say), a headless tool is fine.

`kb ctl --help` lists every command; `kb ctl <command> --help` explains one.

## Your kb, in a pane beside yours

Several kbs can run at once, each with a number. Always pass yours with
`-i N`: without it, commands go to the lowest-numbered kb, which may be
someone else's. Never drive a kb you didn't open. If one you opened earlier
in this conversation is still in `kb list`, reuse it.

To open one in Rex (`$REX_SESSION` is set), split a pane and find your kb
by that pane's id. Others may be starting kbs at the same moment, so the
pane is the only sure way to tell yours. Run it as one shell command:

```sh
pane=$(rex split -s "$REX_SESSION" -b "$REX_BLOCK" --split=right --focus=false --label kb --json -- kb "$url" | jq -r '.block_ids[0]')
for _ in $(seq 1 40); do
  n=$(kb list 2>/dev/null | awk -F'\t' -v p="$pane" '$3 == p {print $1}')
  [ -n "$n" ] && break
  sleep 0.25
done
echo "kb $n in $pane"
```

In another terminal, start `kb "$url"` in a new pane (below) and take the
number `kb list` didn't show before; then check `kb ctl -i N status` shows
your page, in case someone else's kb started at the same time.

Each shell command starts afresh, so write the number into every command:
`kb ctl -i 3 open URL`. Below, `N` stands for it, and outside code blocks a
command is named alone: `box` means `kb ctl -i N box`.

Opening a pane elsewhere, without taking the user's focus:

| Terminal | Command |
|---|---|
| herdr (`HERDR_ENV=1`) | `herdr pane split <your pane> --direction right --no-focus`, then `herdr pane run <new pane> "kb $url"` |
| kitty | `kitten @ launch --location=vsplit --keep-focus kb "$url"` (needs remote control) |
| WezTerm | `wezterm cli split-pane --right -- kb "$url"` |
| Ghostty, cmux, others | the terminal's own command or scripting, if any; or ask the user to split a pane (Cmd+D in Ghostty) and run `kb URL` |
| tmux, screen, Zellij | can't show kb's images: open it in a pane of the terminal itself |

Close your pane with `rex block close "$pane"` in Rex, or the terminal's
own command elsewhere.
Closing the pane quits that kb. Leave it open unless asked to close it: the
user may want to keep looking.

Numbers are reused once a kb closes. If you lose track, find yours in
`kb list` by its pane or page; if you can't, stop and ask. If kb won't start,
or its pane stays blank, `kb doctor` run in that pane says why.

### When the user closes your pane

If your kb is gone from `kb list`, or `kb ctl -i N` says it isn't running,
treat it as the user closing it (kb rarely crashes): they most likely wanted
it out of the way, which overrides an earlier ask to show it in a pane. If
the task can finish without them watching, carry on headless; if you can't
tell whether they want you to go on, ask.

`kb --headless URL` runs a kb in the background, with the same commands, and
prints its number. Like any kb, it has the signed-in profile only if no
other kb is running. It has a desktop viewport (1280x800, which `viewport
reset` returns to) and no pane. Stop it with `kb ctl -i N quit` when you're
done (with your pane gone, that's all there is to close); one left behind
quits after ten minutes without a command. `kb list` marks headless kbs.

## Selectors

CSS (the first visible match; it reaches into shadow DOM, and `a >>> b`
picks `b` inside `a`'s shadow root), `text=Some words` (the element showing
them), `@N` (a number from `links`), or `:focus`.

## Testing your own pages

After each change:

```sh
kb ctl -i N reload                          # or open URL; waits for the load, reports errors
kb ctl -i N check                           # exit 0 if healthy, 1 on console errors, failed requests or an error page
kb ctl -i N screenshot --out /tmp/page.png  # or your scratch directory; then read the image
```

Only `check` sets the exit status; the other commands report what they find.

Then try what you changed. Read the page's HTML for selectors
rather than guessing: `kb ctl -i N html form` for a form's fields,
`html body` for the whole page, including where a result will appear:

```sh
kb ctl -i N type '#email' 'a@b.co' --clear
kb ctl -i N select '#plan' Pro          # by value or label
kb ctl -i N click 'text=Save'           # fails if hidden, off screen or covered
kb ctl -i N wait '#result' --text Saved # the element the click should change, and words you expect
kb ctl -i N console --errors            # this page's; --all adds earlier pages'
kb ctl -i N network --failed
```

`click`, `type`, `press` and `select` answer once the page settles, with the
errors and dialogs they caused. What comes later (after a timer or a
request) isn't there yet, so please `wait` for it before reading. `wait --text`
matches visible text, case and all; if you don't know the words, `wait
SELECTOR --visible`, then `text SELECTOR`. A typed value shows only when
you read that field (`text '#name'`), not in the whole page's `text`. `wait` also takes `--count N` and
`--gone`.

When something looks wrong or does nothing:

| Command | Shows |
|---|---|
| `box SELECTOR` | where it is, whether it's rendered, what covers it |
| `style SELECTOR [PROPERTY] [--why]` | computed CSS; `--why` lists the rules that set it, winner first, with file and line |
| `listeners SELECTOR` | its event listeners; none may mean a parent handles the event |
| `storage [--clear]` | localStorage, sessionStorage and cookie names; `--clear` then `reload` tests a first visit |
| `html SELECTOR`, `eval 'JS'` | anything else; wrap declarations in `(() => { ... })()` |

Also:

- **Dark mode:** `scheme dark` (or `light`, or `auto` to follow the terminal).
- **Slow network:** `throttle slow` (or `fast`, `offline`), then `throttle off`.
- **Phone layout:** `viewport 375x800`, look (`text` or a screenshot), then
  `viewport reset` to return to the size the kb started at. A viewport stays set across
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
n=3                                              # your kb's number, from kb list
k() { kb ctl -i "$n" "$@"; }
k open "$base/login"
k type '#user' tomsmith; k type '#pass' 'SuperSecretPassword!'
k click 'button[type=submit]'
k wait 'text=You logged into a secure area'      # fails after 10 s (--timeout)
[ "$(k text '#count')" = "3" ] || { echo "FAIL: expected 3 items"; exit 1; }
k check --ignore optimizely.com,doubleclick.net  # third-party noise doesn't count
```

- `text SELECTOR` prefixes a hidden element's text with `(hidden: …)`;
  `wait --text` only matches visible text.
- `click --double` double-clicks; `click SELECTOR --at X,Y` clicks a point
  in an element (a canvas, a map). If the mouse click doesn't reach the
  page, `click` clicks by script instead and says so.
- `alert`, `confirm` and `prompt` are accepted and reported by the command
  that caused them; `dialog dismiss` or `dialog accept --text TEXT` changes
  that for later ones.
- Count with `wait SELECTOR --count N`, or `eval` for anything no command
  reads (an order, a count, one value).

## Reading and research

Read with text, not screenshots; it's far cheaper. The pane is often narrow,
so set a desktop viewport first:

```sh
kb ctl -i N viewport 1280x800
kb ctl -i N open 'https://html.duckduckgo.com/html/?q=sqlite+fts5+bm25'
kb ctl -i N links --match sqlite.org      # results, with the real address behind redirects
kb ctl -i N click @3
kb ctl -i N text                          # the page's main content
kb ctl -i N outline                       # title and headings (there's no title command)
kb ctl -i N text --section 'Ranking'      # one section, by its heading's words; scrolls to it
kb ctl -i N find 'bm25'                   # every place it appears, with its section
kb ctl -i N tables; kb ctl -i N tables 2  # list tables; print one as tab-separated rows
kb ctl -i N images                        # alt text, size, caption and source
```

- When you cite a page, leave the passage on screen (`find` and
  `text --section` scroll to it) so the user sees it.
- Console errors on other people's sites are their noise, not yours to fix.
- A bot check ("Just a moment", a CAPTCHA) is reported as one; kb can't pass
  it, so try another source. Searching many times in a row brings one on:
  search once, then read the results.
- Don't log in, post, or submit forms other than searches unless the user
  asks.

## Things to know

- One page per kb, no tabs; open a second pane to compare two pages.
- A window the page opens (a sign-in popup, a new tab) is shown in its
  place, and commands act on it: click and type in it as on any page. `click` says when one opens or closes, and
  `status` counts them. A finished popup closes itself; to dismiss one,
  `back` on its first page closes it.
- Screenshots are one image pixel per CSS pixel. `--size small` halves
  that, `--size sharp` gives device pixels, `--size 800` a set width.
  Prefer `--selector CSS` for one element; `--full` pages stop at 8000
  pixels tall.
- kb doesn't look inside iframes; open the frame's own address instead.

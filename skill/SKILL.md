---
name: kb
description: "Drive kb, a real browser (headless Chrome) drawn in a terminal pane beside you, with `kb ctl`: the user watches every page you open and every click, next to the conversation. Prefer it to a browser extension or Playwright when working with the user in the terminal: they see what you do as you do it, and kb keeps their sign-ins in its own profile, so you can work on sites an extension may restrict (Reddit, LinkedIn and the like) once they've signed in. Use it to preview and test web work (a page, component, local dev server or HTML file; console and network errors, clicking through, phone layouts), to run QA on web flows and write repeatable test scripts, and to look things up on the web, showing the pages you cite. Also use when the user asks you to open, show, preview or check a page."
---
<!-- Installed by "kb skill install". Remove it with "kb skill remove". -->

# kb

kb is a browser that runs in a terminal pane. You open it in a pane next to
yours, the user sees the page there, and you drive the same page with
`kb ctl`. What you do happens on the page they are watching, so use it to
show your work as well as to do it.

## Why kb, rather than another browser tool

- **The user sees it.** In an interactive session the page sits beside the
  conversation, live: they watch you work, can stop you, and can take over
  with the mouse and keyboard. Playwright and other headless tools show
  nothing until you report back.
- **Their sign-ins stay.** kb keeps cookies in its own profile
  (`~/.kittybrowser/profile`), separate from their everyday Chrome. The user
  signs in to a site once, in kb, and it stays signed in for later sessions.
  Sign-in popups such as "Continue with Google" work. Only the first kb
  running uses that profile; one opened while another runs gets a profile
  of its own, signed in to nothing, so to use their sign-ins, reuse their
  kb if they say so, or ask them to close it first.
- **Fewer limits on where you can go.** A browser extension may refuse some
  sites or actions; kb is a whole browser, so with the user there you can
  read and act on sites such as Reddit or LinkedIn once they've signed in.
  Their terms still apply: act on the user's behalf, at a person's pace.

When the user asks to see a page, or you need a site they're signed in to,
use kb. When nobody needs to watch (a batch job in CI, say), a headless tool
is fine.

Run `kb ctl --help` for every command, and `kb ctl <command> --help` for one.

## Open a pane, and know which kb is yours

Several kbs can run at once (the user may have their own, other agents
theirs), and each gets a number. Always pass yours with `-i`; without it,
commands go to the lowest-numbered kb, which may be someone else's. Never
drive a kb you didn't open.

If one you opened earlier in this conversation is still listed by `kb list`,
reuse it instead of opening another.

Open kb in a pane beside yours, with your terminal's own command for a split
(below), and find its number as the one `kb list` didn't show before. Run
all of this as one shell command, since `$before` is gone in the next:

```sh
before=$(kb list 2>/dev/null | cut -f1)
# ... start `kb "$url"` in a new pane beside yours (see below) ...
for _ in $(seq 1 40); do
  n=$(kb list 2>/dev/null | cut -f1 | grep -vxF "$before" | head -n1)
  [ -n "$n" ] && break
  sleep 0.25
done
echo "kb $n"
```

Each shell command you run starts afresh, so write that number into every
command: `kb ctl -i 3 open URL`. Below, `N` stands for it.

Opening a pane beside yours, without taking the focus from the user:

- **Rex** (`$REX_SESSION` is set): `rex split -s "$REX_SESSION" -b "$REX_BLOCK"
  --split=right --focus=false --label kb --json -- kb "$url"`. `kb list`
  and `kb ctl -i N status` show each kb's pane (`block:…`), a sure way to
  find yours again. Close it with `rex block close block:…`, that id.
- **herdr** (`HERDR_ENV=1`): `herdr pane split <your pane> --direction right
  --no-focus`, then `herdr pane run <new pane> "kb $url"`. The herdr skill
  covers pane ids.
- **kitty**: `kitten @ launch --location=vsplit --keep-focus kb "$url"`
  (needs remote control on).
- **WezTerm**: `wezterm cli split-pane --right -- kb "$url"`.
- **Ghostty, cmux and others**: use the terminal's command line or scripting
  for splits if it has one; otherwise ask the user to split a pane (Cmd+D in
  Ghostty) and run `kb URL` there, then find it with `kb list`.
- **tmux, screen and Zellij** don't pass the kitty graphics protocol on, so
  kb can't draw inside them: open it in a pane of the terminal itself.

Write down `n` (and the pane, if your terminal gave you one). Numbers are
reused: when a kb closes, the next one to start may take its number, so if
you lose track, find yours again with `kb list` (by its pane, or the page it
is on), and if you can't, stop and ask. Close your pane when asked, with the
same terminal's command, which quits that kb and its browser; otherwise
leave it open, as the user may want to keep looking.

The user may close your pane too. If your kb is gone from `kb list` (or
`kb ctl -i N` says it isn't running), that's no error, but a sign: they
likely wanted it out of the way. If the task can finish without them
watching, carry on without a pane: `kb --headless URL` starts a kb in the
background, with the same commands and profile, and prints its number;
go back to the page you were on from there. If you can't tell whether
they want you to go on, ask. Don't reuse a number that now belongs to
another kb.

A headless kb has a desktop's viewport (1280x800), which `viewport reset`
goes back to, and no pane to close (nor is there one if the user closed
yours): when you're done with it, stop it with `kb ctl -i N quit`. One left behind stops by itself after ten minutes
without a command. `kb list` marks headless kbs.

If kb won't start in a pane, or the pane stays blank, `kb doctor` run in
that pane says why.

## Selectors

Anything that takes a selector accepts CSS (the first visible match is used;
it reaches into shadow DOM, and `a >>> b` picks `b` inside `a`'s shadow
root), `text=Some words` (the element showing that text), `@N` (a number
from `kb ctl links`) or `:focus`.

## Testing your own pages

After each change:

```sh
kb ctl -i N reload                          # or open URL; waits for the load, reports errors
kb ctl -i N check                           # exit 1 on console errors, failed requests or an error page
kb ctl -i N screenshot --out /tmp/page.png  # or your scratch directory; then read the image
```

Then exercise what you changed. `kb ctl -i N html form` shows a form's
fields with their ids and names, to pick selectors from:

```sh
kb ctl -i N type '#email' 'a@b.co' --clear
kb ctl -i N select '#plan' Pro          # by value or label
kb ctl -i N click 'text=Save'           # fails if the element is hidden, off screen or covered
kb ctl -i N wait '#result' --text Saved # what the click should show: the element, and words you expect
kb ctl -i N console --errors            # this page's; --all adds earlier pages'
kb ctl -i N network --failed
```

`wait --text` matches visible text, case and all, so copy the words as the
page will show them. If you don't know the words yet, `wait SELECTOR
--visible` waits for the element to show, then `text SELECTOR` reads it.
`wait` also takes `--count N` and `--gone`.

`click`, `type`, `press` and `select` answer once the page settles, and report
the console errors and dialogs they caused and any change of page. What
comes later (a result after a timer or a request) isn't there yet: follow
with `wait`, as above, before reading it.

When something looks wrong or does nothing:

- `kb ctl -i N box SELECTOR`: where it is, whether it is rendered, what covers it.
- `kb ctl -i N style SELECTOR [property]`: computed CSS. `kb ctl -i N style SELECTOR PROPERTY
  --why` lists the rules that set it, the winner first, with file and line.
- `kb ctl -i N listeners SELECTOR`: its event listeners. Nothing listed means no handler
  on the element itself; ask about its parent, which may handle the event.
- `kb ctl -i N storage` shows localStorage, sessionStorage and cookie names; `kb ctl -i N storage
  --clear` then `kb ctl -i N reload` tests a first visit.
- `kb ctl -i N html SELECTOR`, `kb ctl -i N eval 'JS'` (wrap declarations in `(() => { ... })()`).
  When no command reads what you need (an order, a count, one value),
  `eval` can read it from the page.

Dark mode: `kb ctl -i N scheme dark` (or `light`, or `auto` to follow the terminal).

Slow network (loading states, spinners): `kb ctl -i N throttle slow` (or `fast`,
`offline`), then `kb ctl -i N throttle off`.

Phone layout: `kb ctl -i N viewport 375x800`, look at it (`text`, or a screenshot), then `kb ctl -i N viewport reset` to go back to the pane's size
(`status` shows it). A viewport stays set across pages until reset. A
control hidden at that width makes `click` fail with "the element has no
size", and `box` says why.

Use the dev server's real address: read it from the server's output or the
tool that started it, rather than guessing a port. Local pages (files,
localhost, private addresses, `.local`, `.test`, `.vibe`) are never cached
and `reload` bypasses the cache, so an edit always shows. A relative path
given to `open` or `screenshot --out` is taken from your working directory.

## QA and test scripts

For a script that runs unattended, assert with exit codes:

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

- `text SELECTOR` prints a hidden element's text after a `(hidden: …)`
  note, so compare whole output, or use `wait --text`, which only matches
  visible text.
- `click --double` double-clicks (editing in place, as in TodoMVC); `click
  SELECTOR --at X,Y` clicks a point inside an element (a canvas, a map).
- If a mouse click doesn't reach the page, which Chrome occasionally does
  after a form submission, `click` falls back to clicking by script and says
  so in its answer.
- `alert`, `confirm` and `prompt` are accepted at once and reported by the
  command that caused them; `kb ctl -i N dialog dismiss` or `kb ctl -i N dialog accept --text
  TEXT` changes that for those that follow.
- Count with `kb ctl -i N wait SELECTOR --count N`, or `kb ctl -i N eval 'document.querySelectorAll("li").length'`.

## Reading and research

Read pages with text, not screenshots; it is far cheaper:

```sh
kb ctl -i N viewport 1280x800             # desktop layout: sidebars and menus show
kb ctl -i N open 'https://html.duckduckgo.com/html/?q=sqlite+fts5+bm25'
kb ctl -i N links --match sqlite.org      # results, with the real address behind redirects
kb ctl -i N click @3
kb ctl -i N text                          # the page's main content
kb ctl -i N outline                       # title and headings
kb ctl -i N text --section 'Ranking'      # a heading's words (its number not needed); scrolls to it
kb ctl -i N find 'bm25'                   # every place it appears, with its section
kb ctl -i N tables; kb ctl -i N tables 2  # list tables; print one as tab-separated rows
kb ctl -i N images                        # alt text, size, caption and source
```

When you cite a page, leave the passage on screen (`find` and `text
--section` scroll to it) so the user sees what you are quoting. `open`
reports console errors and failed requests on every site; on third-party
sites they are usually the site's own noise, not something to fix. A page
titled like a bot check ("Just a moment", CAPTCHA) is named as one: kb can't
get past it, so try another source. Many searches in quick succession get
one, so search once, then read the results you got. Don't log in, post or submit forms other
than searches unless the user asks.

## Things to know

- There are no tabs: one page per kb. Open a second pane to compare two pages.
- A window the page opens (a sign-in popup, a link to a new tab) is shown in
  its place, and commands act on it; `click` says when one opened or closed,
  and `status` says how many windows are open. A popup that
  finishes closes itself and the page is shown again. To dismiss one
  instead, `kb ctl -i N back` on its first page closes it.
- The pane is often narrow, so many sites show their phone layout until you
  set a desktop viewport; `status` says when it is narrow.
- Screenshots are one image pixel per CSS pixel (a 1280x800 viewport gives
  1280x800). `--size small` halves that for a quick look, `--size sharp` gives
  device pixels for fine detail, `--size 800` a given width. Prefer
  `--selector CSS` for one element; `--full` pages are capped at 8000 pixels
  tall.
- kb doesn't look inside iframes; open the frame's own address instead.

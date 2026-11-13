---
name: kb
description: Drive kb, a real browser (headless Chrome) drawn in a terminal pane beside you that the user watches, with `kb ctl`. Use it to preview and test web work (a page, component, CSS, local dev server or HTML file: see it, check it for console or network errors, click through it, test a phone layout); to run QA on web flows and write repeatable test scripts; and to look things up and read on the web (search, docs, articles), showing the user the pages you cite. Also use when the user asks you to open, show, preview or check a page.
---
<!-- Installed by "kb skill install". Remove it with "kb skill remove". -->

# kb

kb is a browser that runs in a terminal pane. You open it in a pane next to
yours, the user sees the page there, and you drive the same page with
`kb ctl`. What you do happens on the page they are watching, so use it to
show your work as well as to do it.

Run `kb ctl --help` for every command, and `kb ctl <command> --help` for one.

## Open a pane, and know which kb is yours

Several kbs can run at once (the user may have their own, other agents
theirs), and each gets a number. Always pass yours with `-i`; without it,
commands go to the lowest-numbered kb, which may be someone else's. Never
drive a kb you didn't open.

If one you opened earlier in this conversation is still listed by `kb list`,
reuse it instead of opening another.

In Rex (`$REX_SESSION` is set), split a pane beside yours. `kb list` shows
each kb's number, page and pane, so find yours by the pane you created:

```sh
pane=$(rex split -s "$REX_SESSION" -b "$REX_BLOCK" --split=right --focus=false --label kb --json -- kb "$url" | jq -r '.block_ids[0]')
for _ in $(seq 1 40); do
  n=$(kb list 2>/dev/null | awk -F'\t' -v p="$pane" '$3 == p {print $1}')
  [ -n "$n" ] && break
  sleep 0.25
done
echo "kb $n in $pane"
k() { kb ctl -i "$n" "$@"; }   # then: k open URL, k check, ...
```

Write down both `n` and `pane`. Numbers are reused: when a kb closes, the
next one to start may take its number, so if you lose track, find yours again
by its pane with `kb list`, and if you can't, stop and ask. Close your pane
when asked with `rex block close -s "$REX_SESSION" "$pane"`; otherwise leave
it open, as the user may want to keep looking.

In any other terminal, ask the user to start `kb URL` in a pane of a terminal
with the kitty graphics protocol (Ghostty, kitty, WezTerm), then find its
number with `kb list`. If kb won't start there, or its pane stays blank,
`kb doctor` run in that pane says why.

## Selectors

Anything that takes a selector accepts CSS (the first visible match is used;
it reaches into shadow DOM, and `a >>> b` picks `b` inside `a`'s shadow
root), `text=Some words` (the element showing that text), `@N` (a number
from `kb ctl links`) or `:focus`.

## Testing your own pages

After each change:

```sh
k reload                         # or: k open URL. Waits for the load, reports errors
k check                          # exit 1 on console errors, failed requests or an error page
k screenshot --out /tmp/page.png # then read the image to look
```

Then exercise what you changed:

```sh
k click 'text=Save'              # fails if the element is hidden, off screen or covered
k type '#email' 'a@b.co' --clear
k select '#plan' Pro             # by value or label
k press Enter
k wait '.toast' --text Saved     # visible text only; --count N, --gone, --visible
k console --errors
k network --failed
```

`click`, `type`, `press` and `select` answer once the page settles, and report
the console errors and dialogs they caused and any change of page.

When something looks wrong or does nothing:

- `k box SELECTOR`: where it is, whether it is rendered, what covers it.
- `k style SELECTOR [property]`: computed CSS. `k style SELECTOR PROPERTY
  --why` lists the rules that set it, the winner first, with file and line.
- `k listeners SELECTOR`: its event listeners. Nothing listed means no handler
  on the element itself; ask about its parent, which may handle the event.
- `k storage` shows localStorage, sessionStorage and cookie names; `k storage
  --clear` then `k reload` tests a first visit.
- `k html SELECTOR`, `k eval 'JS'` (wrap declarations in `(() => { ... })()`).

Dark mode: `k scheme dark` (or `light`, or `auto` to follow the terminal).

Slow network (loading states, spinners): `k throttle slow` (or `fast`,
`offline`), then `k throttle off`.

Phone layout: `k viewport 375x800`, check it, then `k viewport reset`. A
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
k open "$base/login"
k type '#user' tomsmith; k type '#pass' 'SuperSecretPassword!'
k click 'button[type=submit]'
k wait 'text=You logged into a secure area'   # fails after 10 s (--timeout)
[ "$(k text '#count')" = "3" ] || { echo "FAIL: expected 3 items"; exit 1; }
k check --ignore optimizely.com,doubleclick.net   # third-party noise doesn't count
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
  command that caused them; `k dialog dismiss` or `k dialog accept --text
  TEXT` changes that for those that follow.
- Count with `k wait SELECTOR --count N`, or `k eval 'document.querySelectorAll("li").length'`.

## Reading and research

Read pages with text, not screenshots; it is far cheaper:

```sh
k viewport 1280x800              # desktop layout: sidebars and menus show
k open 'https://html.duckduckgo.com/html/?q=sqlite+fts5+bm25'
k links --match sqlite.org       # results, with the real address behind redirects
k click @3
k outline                        # title and headings
k text --section 'Ranking'       # just that section; the pane scrolls to it
k find 'bm25'                    # every place it appears, with its section
k tables; k tables 2             # list tables; print one as tab-separated rows
k images                         # alt text, size, caption and source
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
- The pane is often narrow, so many sites show their phone layout until you
  set a desktop viewport; `status` says when it is narrow.
- Screenshots are one image pixel per CSS pixel (a 1280x800 viewport gives
  1280x800). `--size small` halves that for a quick look, `--size sharp` gives
  device pixels for fine detail, `--size 800` a given width. Prefer
  `--selector CSS` for one element; `--full` pages are capped at 8000 pixels
  tall.
- kb doesn't look inside iframes; open the frame's own address instead.

"""Score a scroll: how closely the page followed the finger.

    python3 score.py TRACE [LOG SKEW [INJECT]]

TRACE is what kb wrote with KB_TRACE. With LOG from relay.py, the page is
also scored as it reached the terminal, and the time each frame took is
split into kb's queue, the link (pty, ssh, network) and the terminal. SKEW
is the clock of the machine kb ran on less this one's, in ms (0 if the
same). INJECT, from a replay, gives when each tick was typed in, which is
then taken as the finger's time.

Scored over the slow drags only (fewer than 40 ticks a second), where a
trackpad's jumps show most:

    err     pixels between page and finger, over each half second
    lag     the delay that best lines the page's path up with the finger's
    still   frames in which the page stood still while the finger moved
    jump    how far the page moved when it moved (median)
    uneven  the 90th percentile of that over the median
    fps     frames a second in which the page moved
    end     where the page came to rest past the finger's last tick
"""

import bisect
import sys


def quantile(values, p):
    values = sorted(values)
    return values[min(int(len(values) * p), len(values) - 1)] if values else float("nan")


def mean(values):
    return sum(values) / len(values) if values else float("nan")


events = [(float(f[0]), f[1], f[2:]) for f in (l.rstrip("\n").split("\t") for l in open(sys.argv[1])) if len(f) > 1]
# The pixels a tick scrolls, as kb last worked it out (it follows the pane).
rows = [(t, float(d[0])) for t, kind, d in events if kind == "row"]


def row_at(t):
    i = bisect.bisect_left([r[0] for r in rows], t)
    return rows[min(i, len(rows) - 1)][1] if rows else 14.0


skew = float(sys.argv[3]) if len(sys.argv) > 3 else 0.0
log = [l.split() for l in open(sys.argv[2])] if len(sys.argv) > 3 else []

ticks = [(t, int(d[0]), int(d[1])) for t, kind, d in events if kind == "tick"]
if len(sys.argv) > 4:
    typed = [l.rstrip("\n").split("\t") for l in open(sys.argv[4])]
    ticks = [(float(f[0]) + skew, int(f[1]), int(f[2])) for f in typed if len(f) == 3]
if not ticks:
    sys.exit("no ticks in the trace")

# Scrolls: ticks less than 400 ms apart.
scrolls = [[ticks[0]]]
for tick in ticks[1:]:
    if tick[0] - scrolls[-1][-1][0] > 400:
        scrolls.append([tick])
    else:
        scrolls[-1].append(tick)

# Where the page was as Chrome sent it, and as it reached the terminal: a
# frame is there once the terminal has taken every byte up to the end of it.
chrome = [(t, float(d[0].split()[1])) for t, kind, d in events if kind == "frame"]
took = [float(f[0]) + skew for f in log]
total = [int(f[1]) for f in log]
came = [float(f[2]) + skew for f in log if len(f) > 2]
flushes = [(t, int(d[1])) for t, kind, d in events if kind == "flushed" and len(d) > 1]
flushed_at, flushed = [f[0] for f in flushes], [f[1] for f in flushes]
screen, delays = [], {"queue": [], "link": [], "terminal": [], "all": []}
scrolled = None
for t, kind, d in events:
    if kind == "frame":
        scrolled = float(d[0].split()[1])
    elif kind == "written" and scrolled is not None and log and len(d) > 1:
        end = int(d[1])
        i, j = bisect.bisect_left(total, end), bisect.bisect_left(flushed, end)
        if i < len(total):
            screen.append((took[i], scrolled))
            delays["all"].append(took[i] - t)
            if j < len(flushed) and came:
                delays["queue"].append(flushed_at[j] - t)
                delays["link"].append(came[i] - flushed_at[j])
                delays["terminal"].append(took[i] - came[i])
screen.sort()


def score(shown):
    times = [s[0] for s in shown]

    def page(t):
        i = bisect.bisect_right(times, t) - 1
        return shown[i][1] if i >= 0 else 0

    errs, lags, ends, rates, jumps, still = [], [], [], [], [], [0, 0]
    for scroll in scrolls:
        if sum(1 for tick in scroll if tick[2]) < 3:
            continue
        a, b = scroll[0][0], scroll[-1][0]
        if len(scroll) / (b - a + 1e-9) * 1000 >= 40:
            continue
        at = [tick[0] for tick in scroll]
        went = []
        for tick in scroll:
            went.append((went[-1] if went else 0) + tick[2] * row_at(tick[0]))

        def finger(t):
            if t <= at[0]:
                return 0.0
            if t >= at[-1]:
                return went[-1]
            i = bisect.bisect_right(at, t) - 1
            return went[i] + (went[i + 1] - went[i]) * (t - at[i]) / (at[i + 1] - at[i])

        t = a
        while t <= b + 250:
            w = max(a, t - 500)
            errs.append(abs((page(t) - page(w)) - (finger(t) - finger(w))))
            t += 2
        p0 = page(a)
        lags.append(min((mean([abs(page(t) - p0 - finger(t - tau)) for t in range(int(a + tau), int(b), 4)] or [1e9]), tau) for tau in range(0, 400, 4))[1])
        ends.append(page(b + 600) - p0 - went[-1])
        moved = sum(1 for j in range(1, len(shown)) if a <= shown[j][0] <= b + 50 and shown[j][1] != shown[j - 1][1])
        rates.append(moved / ((b - a) / 1000 + 1e-9))
        t = a
        while t < b:
            page_moved = page(t + 16.67) - page(t)
            if abs(finger(t + 16.67) - finger(t)) > 0.5:
                still[0] += 1
                still[1] += page_moved == 0
            if page_moved:
                jumps.append(abs(page_moved))
            t += 16.67
    return (
        f"err {mean(errs):5.1f}px  lag {quantile(lags, .5):3.0f}ms  still {100 * still[1] / max(still[0], 1):3.0f}%  "
        f"jump {quantile(jumps, .5):4.1f}px  uneven {quantile(jumps, .9) / max(quantile(jumps, .5), .1):3.1f}x  "
        f"fps {quantile(rates, .5):4.1f}  end {quantile(ends, .5):+5.1f}px"
    )


print(f"chrome:   {score(chrome)}")
if screen:
    print(f"terminal: {score(screen)}")
    print("frame delay, median / 90th percentile / most, ms: " + "  ".join(
        f"{name} {quantile(v, .5):.0f}/{quantile(v, .9):.0f}/{max(v or [0]):.0f}" for name, v in delays.items() if v))
sends = [d for _, kind, d in events if kind == "send"]
print(f"scrolls sent {len(sends)}, across {sum(1 for d in sends if float(d[0]) != 0)}; frames {len(chrome)}, reached the terminal {len(screen)}")

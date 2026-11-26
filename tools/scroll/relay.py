"""Run kb (or ssh to a machine running kb) in a pty, pass it through to this
terminal, and note when its output got here.

    python3 relay.py LOG -- kb https://example.com
    python3 relay.py LOG -- ssh -t host 'KB_TRACE=trace.tsv kb https://example.com'

LOG gets a line per chunk of output: when this terminal took it (ms since
the epoch), how many bytes had come in all, and when it came out of the
pty. Matched with KB_TRACE's byte counts, it says when each frame reached
the terminal, even at the far end of an ssh link (see score.py).

To replay a recorded scroll, set REPLAY to a trace with `tick` lines and
INJECT to a file: once REPLAY.go exists, the ticks are typed in as the
terminal would send the wheel, at their recorded times, and each is noted
in INJECT with when it was sent.
"""

import fcntl
import os
import pty
import select
import signal
import sys
import termios
import threading
import time
import tty


def now():
    return time.time() * 1000


def replay(fd, ticks_path, inject_path):
    ticks = []
    for line in open(ticks_path):
        fields = line.rstrip("\n").split("\t")
        if len(fields) > 4 and fields[1] == "tick":
            x, y = fields[4].split(",")
            ticks.append((float(fields[0]), int(fields[2]), int(fields[3]), int(x), int(y)))
    while not os.path.exists(ticks_path + ".go"):
        time.sleep(0.05)
    inject = open(inject_path, "w", buffering=1)
    start, first = now(), ticks[0][0]
    for at, dx, dy, x, y in ticks:
        wait = start + (at - first) - now()
        if wait > 0:
            time.sleep(wait / 1000)
        # SGR wheel buttons: 64 up, 65 down, 66 left, 67 right.
        button = 65 if dy > 0 else 64 if dy < 0 else 67 if dx > 0 else 66
        os.write(fd, f"\x1b[<{button};{x};{y}M".encode())
        inject.write(f"{now():.1f}\t{dx}\t{dy}\n")
    inject.write(f"{now():.1f}\tdone\n")


def main():
    log = open(sys.argv[1], "w", buffering=1)
    command = sys.argv[sys.argv.index("--") + 1 :]
    pid, fd = pty.fork()
    if pid == 0:
        os.execvp(command[0], command)

    def resize(*_):
        try:
            fcntl.ioctl(fd, termios.TIOCSWINSZ, fcntl.ioctl(0, termios.TIOCGWINSZ, b"\0" * 8))
            os.kill(pid, signal.SIGWINCH)
        except OSError:
            pass

    resize()
    signal.signal(signal.SIGWINCH, resize)
    if os.environ.get("REPLAY"):
        threading.Thread(target=replay, args=(fd, os.environ["REPLAY"], os.environ["INJECT"]), daemon=True).start()

    saved = termios.tcgetattr(0)
    tty.setraw(0)
    total = 0
    try:
        while True:
            try:
                ready, _, _ = select.select([fd, 0], [], [])
            except InterruptedError:
                continue
            if fd in ready:
                try:
                    data = os.read(fd, 1 << 20)
                except OSError:
                    break
                if not data:
                    break
                came = now()
                view = memoryview(data)
                while view:
                    view = view[os.write(1, view) :]
                total += len(data)
                log.write(f"{now():.1f}\t{total}\t{came:.1f}\n")
            if 0 in ready:
                data = os.read(0, 65536)
                if not data:
                    break
                os.write(fd, data)
    finally:
        termios.tcsetattr(0, termios.TCSADRAIN, saved)


main()

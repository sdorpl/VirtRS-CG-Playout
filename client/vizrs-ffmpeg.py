#!/usr/bin/env python3
"""ffmpeg for ErsatzTV that overlays the VizRS graphics itself (VirtRS CG Playout).

Set as the FFmpeg path in ErsatzTV (Settings -> FFmpeg). Every call goes to the
real ffmpeg unchanged, except a transcode that reads the ErsatzTV graphics
canvas (`-i http://localhost:8409/internal/graphics/<channel>/<item>?...`):

1. the canvas is opened here and drained, so ErsatzTV still starts the VizRS
   script element with the item's template data (commands, ads, EPG);
2. the sidecar takes that script session over (`GET /ffmpeg`): ErsatzTV gets
   no more packets and its graphics engine waits at no cost;
3. ffmpeg reads raw yuva420p frames from the sidecar instead of the canvas
   (`GET /ffmpeg/frames`), through a socket passed as `pipe:N`.

ErsatzTV's own graphics elements of that item (text, image, watermarks in the
canvas) are not drawn then. Whenever the sidecar does not answer, the original
command runs: the item plays with ErsatzTV's graphics path.

With VIZRS_FFMPEG_FPS every transcode to HLS runs at that rate (ErsatzTV takes
the rate from the media): the video gets `fps=N` before the graphics overlay,
so the graphics move at N frames per second, and the output `-r N`.

    VIZRS_CG_URL       sidecar, e.g. http://cg:7700 (empty: no overlay)
    VIZRS_FFMPEG       real ffmpeg (default /usr/local/bin/ffmpeg)
    VIZRS_FFMPEG_FPS   channel frame rate, e.g. 50 (empty: the media's rate)

Standard library only (ErsatzTV images ship python3).
"""

import ctypes
import http.client
import json
import os
import re
import signal
import socket
import subprocess
import sys
import threading
import time
import urllib.parse

FFMPEG = os.environ.get("VIZRS_FFMPEG", "/usr/local/bin/ffmpeg")
SIDECAR = os.environ.get("VIZRS_CG_URL", "").rstrip("/")
FPS = os.environ.get("VIZRS_FFMPEG_FPS", "").strip()
CLAIM_WAIT_MS = 2000
TIMEOUT = 5
SCRIPT_END_S = 0.5
CANVAS_PATH = re.compile(r"^/internal/graphics/([^/]+)/\d+$")
# The first overlay of two labelled inputs: [video][graphics]overlay…
OVERLAY = re.compile(r"\[([^\]]+)\]\[([^\]]+)\](overlay\w*)")
COPY = {("-c", "copy"), ("-c:v", "copy"), ("-vcodec", "copy"), ("-codec:v", "copy")}
# Options of the canvas input that do not apply to the raw frames.
DROP_WITH_VALUE = {
    "-headers", "-protocol_whitelist", "-reconnect", "-reconnect_streamed", "-reconnect_delay_max",
    "-reconnect_on_network_error", "-rw_timeout", "-f", "-c:v", "-vcodec", "-codec:v", "-pix_fmt",
    "-video_size", "-s", "-framerate", "-r",
}


def log(message):
    print(f"vizrs-ffmpeg: {message}", file=sys.stderr, flush=True)


def plain(args):
    os.execv(FFMPEG, [FFMPEG, *with_rate(args)])


def with_rate(args):
    """A transcode to HLS at FPS: `fps` before the overlay, `-r` on the output."""
    pairs = list(zip(args, args[1:]))
    if not FPS or ("-f", "hls") not in pairs or COPY.intersection(pairs):
        return args

    out = list(args)
    for i, arg in enumerate(out[:-1]):
        if arg == "-filter_complex":
            out[i + 1] = OVERLAY.sub(
                lambda m: f"[{m[1]}]fps={FPS}[{m[1]}_fps];[{m[1]}_fps][{m[2]}]{m[3]}", out[i + 1], count=1)
    hls = pairs.index(("-f", "hls"))
    out[hls:hls] = ["-r", FPS]
    return out


def find_canvas(args):
    """(start of the canvas input options, index of its -i, ErsatzTV channel) or None."""
    start = 0
    for i, arg in enumerate(args[:-1]):
        if arg != "-i":
            continue
        parts = urllib.parse.urlsplit(args[i + 1])
        match = CANVAS_PATH.match(parts.path)
        if parts.scheme in ("http", "https") and match:
            # A region canvas only carries watermarks: no VizRS element on the item.
            if "region" in urllib.parse.parse_qs(parts.query):
                return None
            return start, i, urllib.parse.unquote(match.group(1))
        start = i + 2
    return None


def parse_headers(options):
    headers = {}
    for i, option in enumerate(options[:-1]):
        if option == "-headers":
            for line in re.split(r"\r?\n", options[i + 1]):
                name, sep, value = line.partition(":")
                if sep and name.strip():
                    headers[name.strip()] = value.strip()
    return headers


class Drain(threading.Thread):
    """Reads the ErsatzTV canvas and throws it away until closed."""

    def __init__(self, url, headers):
        super().__init__(daemon=True)
        self.url = urllib.parse.urlsplit(url)
        self.headers = headers
        self.connection = None

    def run(self):
        try:
            cls = http.client.HTTPSConnection if self.url.scheme == "https" else http.client.HTTPConnection
            self.connection = cls(self.url.hostname, self.url.port, timeout=TIMEOUT)
            path = self.url.path + ("?" + self.url.query if self.url.query else "")
            self.connection.request("GET", path, headers=self.headers)
            response = self.connection.getresponse()
            self.connection.sock.settimeout(None)
            while response.read1(1 << 16):
                pass
        except (OSError, http.client.HTTPException, AttributeError):
            pass

    def close(self):
        try:
            if self.connection and self.connection.sock:
                self.connection.sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass


def sidecar_get(path):
    url = urllib.parse.urlsplit(SIDECAR)
    connection = http.client.HTTPConnection(url.hostname, url.port or 80, timeout=TIMEOUT)
    connection.request("GET", path)
    response = connection.getresponse()
    return response.status, response.read()


def open_frames(path):
    """Socket positioned at the body of the sidecar's frame stream."""
    url = urllib.parse.urlsplit(SIDECAR)
    sock = socket.create_connection((url.hostname, url.port or 80), timeout=TIMEOUT)
    sock.sendall(f"GET {path} HTTP/1.0\r\nHost: {url.netloc}\r\n\r\n".encode("latin1"))
    head = b""
    # Byte by byte: nothing of the body may stay in a buffer here.
    while not head.endswith(b"\r\n\r\n"):
        byte = sock.recv(1)
        if not byte or len(head) > 8192:
            raise OSError("frame stream: no reply")
        head += byte
    status = head.split(b" ", 2)[1:2]
    if status != [b"200"]:
        raise OSError(f"frame stream: {head.splitlines()[0].decode('latin1', 'replace')}")
    sock.settimeout(None)
    return sock


def overlay_args(args, start, index, sock, info):
    options = args[start:index]
    kept = []
    i = 0
    while i < len(options):
        if options[i] in DROP_WITH_VALUE and i + 1 < len(options):
            i += 2
            continue
        kept.append(options[i])
        i += 1
    raw = ["-f", "rawvideo", "-pix_fmt", "yuva420p", "-video_size", f"{info['width']}x{info['height']}",
           "-framerate", info["rate"]]
    return with_rate([*args[:start], *kept, *raw, "-i", f"pipe:{sock.fileno()}", *args[index + 2:]])


_libc = None
try:
    _libc = ctypes.CDLL(None, use_errno=True)
except OSError:
    pass


def die_with_parent():
    # PR_SET_PDEATHSIG: ffmpeg stops when this wrapper is killed.
    if _libc is not None:
        _libc.prctl(1, signal.SIGTERM)


def main():
    args = sys.argv[1:]
    canvas = find_canvas(args) if SIDECAR else None
    # Not a canvas transcode: plain ffmpeg, at FPS when set.
    if not canvas:
        plain(args)

    start, index, channel = canvas
    headers = parse_headers(args[start:index])
    rate = headers.get("x-etv-frame-rate", "")
    drain = Drain(args[index + 1], headers)
    drain.start()

    try:
        query = urllib.parse.urlencode({"channel": channel, "wait": CLAIM_WAIT_MS})
        status, body = sidecar_get(f"/ffmpeg?{query}")
        if status != 200:
            raise OSError(f"no VizRS session on channel {channel} ({status})")
        info = json.loads(body)
        info["rate"] = FPS or rate or str(info["fps"])
        query = urllib.parse.urlencode({"session": info["session"], "fps": info["rate"]})
        sock = open_frames(f"/ffmpeg/frames?{query}")
    except (OSError, ValueError, KeyError, http.client.HTTPException) as error:
        log(f"{error}; ErsatzTV graphics instead")
        drain.close()
        plain(args)

    child = subprocess.Popen([FFMPEG, *overlay_args(args, start, index, sock, info)],
                             pass_fds=(sock.fileno(),), preexec_fn=die_with_parent)
    sock.close()

    def forward(signum, _frame):
        child.send_signal(signum)

    for signum in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP, signal.SIGQUIT):
        signal.signal(signum, forward)

    code = child.wait()
    # The sidecar ends the script when ffmpeg's frame stream closes; closing the
    # canvas before that makes ErsatzTV log a cancelled script element.
    time.sleep(SCRIPT_END_S)
    drain.close()
    return code if code >= 0 else 128 - code


if __name__ == "__main__":
    sys.exit(main())

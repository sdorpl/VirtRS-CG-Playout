#!/usr/bin/env python3
"""ErsatzTV script graphics element -> VirtRS CG sidecar.

ErsatzTV starts this for every playout item: the template data (JSON) comes
on stdin, ETV graphics packets go to stdout. It only forwards both ways, so
the browser keeps running in the sidecar between items.

    vizrs-etv-client.py http://cg:7700/etv?channel=1

If the sidecar is not reachable it exits without output: the item plays
without graphics. Standard library only (ErsatzTV images ship python3).
"""

import http.client
import sys
import urllib.parse

CONNECT_TIMEOUT = 5


def main():
    url = urllib.parse.urlsplit(sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:7700/etv")
    body = sys.stdin.buffer.read()
    path = url.path + ("?" + url.query if url.query else "")

    try:
        connection = http.client.HTTPConnection(url.hostname, url.port or 80, timeout=CONNECT_TIMEOUT)
        connection.request("POST", path, body=body, headers={"Content-Type": "application/json"})
        response = connection.getresponse()
        if response.status != 200:
            print(f"vizrs-etv: sidecar replied {response.status}", file=sys.stderr)
            return 0
        # The stream lasts as long as the item: no read timeout.
        connection.sock.settimeout(None)
    except OSError as error:
        print(f"vizrs-etv: {error}", file=sys.stderr)
        return 0

    out = sys.stdout.buffer
    try:
        while True:
            chunk = response.read1(1 << 20)
            if not chunk:
                break
            out.write(chunk)
            out.flush()
    except (BrokenPipeError, ConnectionError, KeyboardInterrupt):
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())

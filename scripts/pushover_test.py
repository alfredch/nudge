#!/usr/bin/env python3
"""Testsender für den Pushover-Desktop-Client. Liest PUSHOVER_TOKEN/PUSHOVER_USER aus der .env.

Aufruf: scripts/pushover_test.py <schritt> [--env PFAD]
  live      Prioritäten -1, 0, 1 (je eine Meldung, 3 s Abstand)
  silent    Priorität -2 (keine Notification erwartet)
  html      eine Meldung mit html=1, eine mit url/url_title
  backlog   3 Meldungen (vorher Daemon stoppen, danach starten)
  restart   1 Meldung als Referenz vor Ctrl-C/Neustart
"""

import argparse
import json
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime
from pathlib import Path

URL = "https://api.pushover.net/1/messages.json"


def load_env(path: Path) -> dict[str, str]:
    values = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if line and not line.startswith("#") and "=" in line:
            key, _, value = line.partition("=")
            values[key.strip()] = value.strip().strip('"').strip("'")
    return values


def send(cred: dict[str, str], title: str, message: str, **extra: str | int) -> None:
    fields = {"token": cred["PUSHOVER_TOKEN"], "user": cred["PUSHOVER_USER"],
              "title": f"[Test] {title}", "message": message, **{k: str(v) for k, v in extra.items()}}
    req = urllib.request.Request(URL, data=urllib.parse.urlencode(fields).encode(), method="POST")
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            body = json.loads(resp.read())
    except urllib.error.HTTPError as exc:
        sys.exit(f"HTTP {exc.code}: {exc.read()[:300].decode(errors='replace')}")
    print(f"{datetime.now():%H:%M:%S}  gesendet: {title!r}  request={body.get('request')}")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("step", choices=["live", "silent", "html", "backlog", "restart"])
    ap.add_argument("--env", type=Path, default=Path(__file__).resolve().parent.parent / ".env")
    args = ap.parse_args()
    cred = load_env(args.env)
    stamp = f"{datetime.now():%H:%M:%S}"

    if args.step == "live":
        for prio in (-1, 0, 1):
            send(cred, f"Priorität {prio}", f"Live-Test {stamp}, priority={prio}", priority=prio)
            time.sleep(3)
    elif args.step == "silent":
        send(cred, "Priorität -2", f"Darf keine Notification auslösen ({stamp})", priority=-2)
    elif args.step == "html":
        send(cred, "HTML", f'<b>fett</b>, <i>kursiv</i>, <u>unterstrichen</u>, <font color="#ff0000">rot</font>, '
             f'<a href="https://pushover.net/api">Link im Text</a> ({stamp})', html=1)
        time.sleep(3)
        send(cred, "URL", f"Meldung mit Zusatz-URL ({stamp})",
             url="https://pushover.net/api#urls", url_title="Pushover-API: Supplementary URLs")
    elif args.step == "backlog":
        for i in (1, 2, 3):
            send(cred, f"Backlog {i}/3", f"Gesendet bei gestopptem Daemon ({stamp})")
            time.sleep(1)
    elif args.step == "restart":
        send(cred, "Vor Neustart", f"Darf nach Ctrl-C und Neustart nicht nochmals erscheinen ({stamp})")


if __name__ == "__main__":
    main()

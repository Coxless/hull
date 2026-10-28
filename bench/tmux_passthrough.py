#!/usr/bin/env python3
"""tmux の中から、hull にどのイベントが届くかを確かめる (concept.md の「tmux の中での動き」)。

    python3 bench/tmux_passthrough.py --hull target/release/hull

tmux の設定を変えながら、ベル、タイトル、OSC 7 / 9 / 133 / 777 を出し、hull のプラグインに届いたものを表にする。
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

from hullbench import table

HERE = os.path.dirname(os.path.abspath(__file__))

# 確かめるシーケンス (名前, 出すもの, 届いたときのイベント)。
SEQUENCES = [
    ("ベル", r"\a", lambda e: e["event"] == "bell"),
    ("タイトル (OSC 0)", r"\033]0;hull-title\a", lambda e: e["event"] == "title" and "hull-title" in e["title"]),
    ("OSC 7", r"\033]7;file://localhost/tmp/hull-osc7\a", lambda e: e["event"] == "cwd"),
    ("OSC 9", r"\033]9;hull-osc9\a", lambda e: e["event"] == "notify" and "hull-osc9" in e["body"]),
    ("OSC 777", r"\033]777;notify;T;hull-osc777\a", lambda e: e["event"] == "notify" and "hull-osc777" in e["body"]),
    ("OSC 133", r"\033]133;A\a", lambda e: e["event"] == "prompt"),
]

CASES = [
    ("既定の設定", "", False),
    ("set-titles と focus-events", "set -g set-titles on\nset -g set-titles-string '#T'\nset -g focus-events on\n", False),
    ("allow-passthrough と DCS で包む", "set -g allow-passthrough on\n", True),
]


def wrap(seq):
    """tmux の passthrough: DCS tmux; ... ST で包み、中の ESC を二重にする。"""
    return r"\033Ptmux;" + seq.replace(r"\033", r"\033\033") + r"\033\\"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--hull", default=os.path.join(HERE, "..", "target", "release", "hull"))
    args = ap.parse_args()
    if not shutil.which("tmux"):
        sys.exit("tmux がありません (`sudo apt install tmux`)")
    tmp = tempfile.mkdtemp(prefix="hull-tmux-")
    config = os.path.join(tmp, "hull.toml")
    with open(config, "w") as f:
        f.write(f'[[plugin]]\nname = "logger"\ncommand = "{sys.executable}"\n'
                f'args = ["{os.path.join(HERE, "event_logger.py")}"]\n')
    version = subprocess.run(["tmux", "-V"], capture_output=True, text=True).stdout.strip()
    rows = []
    for title, tmux_conf, passthrough in CASES:
        conf = os.path.join(tmp, "tmux.conf")
        with open(conf, "w") as f:
            f.write(tmux_conf)
        log = os.path.join(tmp, "events.jsonl")
        open(log, "w").close()
        script = "sleep 1; " + " ".join(
            f"printf '{wrap(s) if passthrough else s}'; sleep 0.2;" for _, s, _ in SEQUENCES) + " sleep 1"
        cmd = [args.hull, "--config", config, "-e", "tmux", "-L", "hull-pt", "-f", conf,
               "new-session", script]
        subprocess.run(cmd, env=dict(os.environ, HULLBENCH_EVENTS=log), timeout=60)
        subprocess.run(["tmux", "-L", "hull-pt", "kill-server"], stderr=subprocess.DEVNULL)
        events = []
        with open(log) as f:
            for line in f:
                try:
                    events.append(json.loads(line))
                except ValueError:
                    pass
        rows.append([title] + ["届く" if any(check(e) for e in events) else "届かない"
                               for _, _, check in SEQUENCES])
        time.sleep(0.5)
    header = ["tmux の設定"] + [name for name, _, _ in SEQUENCES]
    print(f"{version}\n")
    print(table(header, rows), end="")


if __name__ == "__main__":
    main()

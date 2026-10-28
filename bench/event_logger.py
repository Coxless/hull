#!/usr/bin/env python3
"""hull のプラグインの例: 届いたイベントを 1 行ずつファイルに書く (環境変数 HULLBENCH_EVENTS)。

プラグインは普通の実行ファイルなので、Python でも書ける。fd 3 の Unix ソケットで、1 行に 1 つの JSON を読む。
"""
import os
import socket

sock = socket.socket(fileno=int(os.environ["HULL_PLUGIN_FD"]))
path = os.environ.get("HULLBENCH_EVENTS", "/dev/null")
with sock.makefile("r", encoding="utf-8") as events, open(path, "a", encoding="utf-8") as out:
    for line in events:
        out.write(line)
        out.flush()

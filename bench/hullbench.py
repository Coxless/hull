#!/usr/bin/env python3
"""hull と競合 (alacritty、ghostty、foot) を、同じマシン・同じ方法で測る。

    python3 bench/hullbench.py all --hull target/release/hull
    python3 bench/hullbench.py startup --terminals hull,foot --runs 20

測れるもの (concept.md の「速さを数字で示す」の表に対応する):

- startup     起動してから最初にバッファを出すまで (全端末)。hull は画面に出た時刻 (wp_presentation) も
- idle        何もせずに置いたときの、10 秒あたりの wakeup (文脈切り替え) と CPU 時間
- memory      スクロールバックを 1 万行埋めたときの RSS (GPU ドライバの分を含めた値と、含めない値)
- vim         vim の描画時間を 5 つの型で (1 文字の入力、1 行のスクロール、画面全体の変化、
              左右に分けた状態でのスクロール、tmux で左右に分けた状態でのスクロール)
- throughput  vtebench (別に入れておく)
- latency     hull だけ。キーを押してから画面に出るまで (人がキーを打つ)
- size        strip したリリースビルドの大きさ (hull だけ)

結果は Markdown の表で標準出力と `bench/results/` に書く。画面がついているときに測ること
(画面が消えていると、コンポジタがフレームを出さず、表示の時刻が測れない)。
"""

import argparse
import datetime
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
COLS, ROWS = 100, 30


def terminal_command(name, hull, cmd):
    """端末 `name` で `cmd` を動かすコマンド。窓の大きさは 100x30 にそろえる。"""
    if name == "hull":
        return [hull, "-e", *cmd]
    if name == "alacritty":
        return ["alacritty", "-o", f"window.dimensions.columns={COLS}",
                "-o", f"window.dimensions.lines={ROWS}", "-e", *cmd]
    if name == "foot":
        return ["foot", f"--window-size-chars={COLS}x{ROWS}", *cmd]
    if name == "ghostty":
        return ["ghostty", f"--window-width={COLS}", f"--window-height={ROWS}",
                "--gtk-single-instance=false", "-e", *cmd]
    raise ValueError(name)


def available(names, hull):
    out = []
    for n in names:
        exe = hull if n == "hull" else n
        if shutil.which(exe) or os.path.exists(exe):
            out.append(n)
        else:
            print(f"# {n} が見つからないので飛ばします", file=sys.stderr)
    return out


def descendants(pid):
    """pid とその子孫の pid (snap のように別のプロセスに置き換わる端末のため)。"""
    children = {}
    for p in os.listdir("/proc"):
        if not p.isdigit():
            continue
        try:
            with open(f"/proc/{p}/stat") as f:
                ppid = int(f.read().rsplit(")", 1)[1].split()[1])
            children.setdefault(ppid, []).append(int(p))
        except OSError:
            pass
    out, todo = [], [pid]
    while todo:
        p = todo.pop()
        out.append(p)
        todo.extend(children.get(p, []))
    return out


def comm(pid):
    try:
        with open(f"/proc/{pid}/comm") as f:
            return f.read().strip()
    except OSError:
        return ""


def find_terminal_pid(root, name):
    """端末のプロセス (中のシェルやコマンドではなく)。"""
    for p in descendants(root):
        c = comm(p)
        if c.startswith(name[:15]) or (name == "ghostty" and c.startswith("ghostty")):
            return p
    return root


def median(xs):
    return statistics.median(xs) if xs else float("nan")


# ---- startup ----

WAYLAND_TS = re.compile(r"^\[\s*(\d+)\.(\d+)\]")


def realtime_debug_ms():
    """libwayland の WAYLAND_DEBUG と同じ時計 (実時間のマイクロ秒を 32 ビットに丸め、ミリ秒にしたもの)。"""
    return (int(time.time() * 1e6) % 2**32) / 1000


def first_commit_ms(log, t0):
    """WAYLAND_DEBUG の記録から、最初にバッファを付けてコミットするまでの時間。"""
    attached = set()
    wrap = 2**32 / 1000
    for line in log.splitlines():
        m = WAYLAND_TS.match(line)
        if not m:
            continue
        ts = int(m.group(1)) + int(m.group(2)) / 1000
        a = re.search(r"-> (wl_surface#\d+)\.attach\(wl_buffer#\d+", line)
        if a:
            attached.add(a.group(1))
            continue
        c = re.search(r"-> (wl_surface#\d+)\.commit\(\)", line)
        if c and c.group(1) in attached:
            return (ts - t0) % wrap
    return None


def bench_startup(names, hull, runs):
    rows = []
    for name in names:
        commits, frames, gpu_frames = [], [], []
        for _ in range(runs):
            env = dict(os.environ, WAYLAND_DEBUG="1")
            t0 = realtime_debug_ms()
            p = subprocess.run(terminal_command(name, hull, ["sh", "-c", "sleep 0.7"]),
                               capture_output=True, text=True, env=env, timeout=30)
            ms = first_commit_ms(p.stderr, t0)
            if ms is not None:
                commits.append(ms)
            time.sleep(0.5)
        if name == "hull":
            for _ in range(runs):
                t0 = time.clock_gettime(time.CLOCK_MONOTONIC)
                p = subprocess.run([hull, "--bench-startup", "-e", "sleep", "5"],
                                   capture_output=True, text=True, timeout=30)
                for kind, sec, nsec in re.findall(r"hull-(\S+) clk_id=1 sec=(\d+) nsec=(\d+)", p.stdout):
                    ms = (int(sec) + int(nsec) / 1e9 - t0) * 1000
                    (frames if kind == "first-frame" else gpu_frames).append(ms)
                time.sleep(0.5)
        rows.append([name, f"{median(commits):.1f}",
                     f"{median(frames):.1f}" if frames else "-",
                     f"{median(gpu_frames):.1f}" if gpu_frames else "-",
                     str(len(commits))])
    return table(["端末", "最初のコミット (ms)", "画面に出た (ms)", "GPU で描いて画面に出た (ms)", "回数"], rows,
                 "中央値。最初のコミットは WAYLAND_DEBUG の時刻で、どの端末も同じ方法で測る。"
                 "画面に出た時刻は wp_presentation で、hull だけ。")


# ---- idle ----

def ctxt_and_ticks(pid):
    switches = 0
    for t in os.listdir(f"/proc/{pid}/task"):
        try:
            with open(f"/proc/{pid}/task/{t}/status") as f:
                for line in f:
                    if "ctxt_switches" in line:
                        switches += int(line.split()[1])
        except OSError:
            pass
    with open(f"/proc/{pid}/stat") as f:
        fields = f.read().rsplit(")", 1)[1].split()
    return switches, int(fields[11]) + int(fields[12])


def bench_idle(names, hull, seconds=10):
    rows = []
    hz = os.sysconf("SC_CLK_TCK")
    for name in names:
        p = subprocess.Popen(terminal_command(name, hull, ["sleep", str(seconds + 15)]),
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(5)
        pid = find_terminal_pid(p.pid, name)
        s0, t0 = ctxt_and_ticks(pid)
        time.sleep(seconds)
        s1, t1 = ctxt_and_ticks(pid)
        p.terminate()
        p.wait()
        rows.append([name, str(s1 - s0), f"{(t1 - t0) / hz * 1000:.0f}"])
    return table(["端末", f"{seconds} 秒の wakeup (文脈切り替え)", "CPU 時間 (ms)"], rows,
                 "窓を開いて 5 秒置いたあと、何も入力せずに測る。")


# ---- memory ----

# GPU ドライバとその道具のマッピング。ヒープの中のドライバの分は分けられないので、含めない値は上限の目安。
DRIVER = re.compile(r"nvidia|libLLVM|libgallium|/dri/|/dev/dri|libvulkan|libEGL_mesa|libGLX|libglapi|libdrm|mesa")


def rss(pid):
    total = driver = 0
    name = ""
    with open(f"/proc/{pid}/smaps") as f:
        for line in f:
            if re.match(r"^[0-9a-f]+-[0-9a-f]+ ", line):
                parts = line.split()
                name = parts[5] if len(parts) > 5 else ""
            elif line.startswith("Rss:"):
                kb = int(line.split()[1])
                total += kb
                if DRIVER.search(name):
                    driver += kb
    return total, total - driver


def bench_memory(names, hull):
    rows = []
    fill = ("i=0; while [ $i -lt 10000 ]; do "
            "echo \"$i the quick brown fox jumps over the lazy dog 0123456789 abcdefghijklmnopqrstuvwxyz\"; "
            "i=$((i+1)); done; sleep 30")
    for name in names:
        p = subprocess.Popen(terminal_command(name, hull, ["sh", "-c", fill]),
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(8)
        pid = find_terminal_pid(p.pid, name)
        total, without = rss(pid)
        p.terminate()
        p.wait()
        rows.append([name, f"{total / 1024:.1f}", f"{without / 1024:.1f}"])
    return table(["端末", "RSS (MB)", "RSS、GPU ドライバのライブラリを除く (MB)"], rows,
                 "スクロールバックを 1 万行 (1 行 90 文字ほど) 埋めて測る。"
                 "ドライバがヒープに取った分は分けられないので、右の列にも含まれる。")


# ---- vim ----

VIM_SCRIPT = r"""
set nocompatible noswapfile nomore lazyredraw& number
let s:n = str2nr($HULLBENCH_N)
function! s:Run(kind) abort
  let t = reltime()
  for i in range(s:n)
    if a:kind ==# 'type'
      execute "normal! A" . (i % 10)
    elseif a:kind ==# 'scroll' || a:kind ==# 'vsplit' || a:kind ==# 'tmux'
      execute "normal! \<C-e>"
    elseif a:kind ==# 'full'
      execute (i % 2 ? 'buffer 1' : 'buffer 2')
    endif
    redraw
  endfor
  return reltimefloat(reltime(t)) * 1000.0 / s:n
endfunction
let s:kind = $HULLBENCH_KIND
if s:kind ==# 'full'
  badd $HULLBENCH_FILE2
endif
if s:kind ==# 'vsplit'
  vsplit
  wincmd h
endif
let s:ms = s:Run(s:kind)
call writefile([printf('%.4f', s:ms)], $HULLBENCH_OUT)
qall!
"""


def bench_vim(names, hull, n=300):
    if not shutil.which("vim"):
        return "vim が見つからないので、vim の描画時間は測れませんでした (`sudo apt install vim`)。\n"
    has_tmux = shutil.which("tmux") is not None
    kinds = ["type", "scroll", "full", "vsplit"] + (["tmux"] if has_tmux else [])
    labels = {"type": "1 文字の入力", "scroll": "1 行のスクロール", "full": "画面全体の変化",
              "vsplit": "左右に分けてスクロール", "tmux": "tmux で左右に分けてスクロール"}
    tmp = tempfile.mkdtemp(prefix="hullbench-")
    script = os.path.join(tmp, "bench.vim")
    with open(script, "w") as f:
        f.write(VIM_SCRIPT)
    file1 = os.path.join(tmp, "a.txt")
    file2 = os.path.join(tmp, "b.txt")
    with open(file1, "w") as f:
        f.writelines(f"{i:6} let mut value = compute(alpha, beta, gamma); // コメント {i}\n" for i in range(20000))
    with open(file2, "w") as f:
        f.writelines(f"{i:6} fn other_{i}(x: u32) -> u32 {{ x.wrapping_mul({i}) }}\n" for i in range(20000))
    rows = []
    for name in names:
        row = [name]
        for kind in kinds:
            out = os.path.join(tmp, f"{name}-{kind}.txt")
            env = dict(os.environ, HULLBENCH_N=str(n), HULLBENCH_KIND=kind,
                       HULLBENCH_OUT=out, HULLBENCH_FILE2=file2)
            vim = ["vim", "-u", "NONE", "-N", "-S", script, file1]
            if kind == "tmux":
                inner = " ".join(vim)
                cmd = ["tmux", "-L", "hullbench", "-f", "/dev/null", "new-session",
                       f"env HULLBENCH_N={n} HULLBENCH_KIND=tmux HULLBENCH_OUT={out} {inner}; tmux kill-server",
                       ";", "split-window", "-h", "-d", "sleep 600"]
            else:
                cmd = vim
            subprocess.run(terminal_command(name, hull, cmd), env=env, timeout=600,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                with open(out) as f:
                    row.append(f"{float(f.read()):.3f}")
            except (OSError, ValueError):
                row.append("-")
        rows.append(row)
    return table(["端末"] + [labels[k] + " (ms)" for k in kinds], rows,
                 f"vim で {n} 回操作して、1 回あたりの時間 (redraw まで)。小さいほど速い。")


# ---- throughput ----

def bench_throughput(names, hull, vtebench, benchmarks):
    if not vtebench or not os.path.exists(vtebench):
        return ("vtebench がないので、スループットは測れませんでした "
                "(https://github.com/alacritty/vtebench をビルドし、`--vtebench` と `--vtebench-benchmarks` で渡す)。\n")
    tmp = tempfile.mkdtemp(prefix="hullbench-vte-")
    lines = []
    for name in names:
        dat = os.path.join(tmp, f"{name}.dat")
        subprocess.run(terminal_command(name, hull, [vtebench, "-b", benchmarks, "--dat", dat]),
                       timeout=3600, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        lines.append(f"- {name}: {dat}")
    return ("vtebench の結果 (.dat)。`vtebench` の `gnuplot` の手順で図にできる。\n\n" + "\n".join(lines) + "\n")


def bench_size(hull):
    size = os.path.getsize(hull)
    return table(["バイナリ", "大きさ (MB)"], [[os.path.basename(hull), f"{size / 1e6:.2f}"]],
                 "strip したリリースビルド (`cargo build --release`)。")


def table(header, rows, note=""):
    out = "| " + " | ".join(header) + " |\n| " + " | ".join("---" for _ in header) + " |\n"
    for r in rows:
        out += "| " + " | ".join(r) + " |\n"
    if note:
        out += f"\n{note}\n"
    return out


def machine():
    gpu = ""
    try:
        gpu = subprocess.run(["sh", "-c", "lspci | grep -i -E 'vga|3d' | cut -d: -f3"],
                             capture_output=True, text=True).stdout.strip().replace("\n", " / ")
    except OSError:
        pass
    desktop = os.environ.get("XDG_CURRENT_DESKTOP", "")
    return f"{platform.platform()} / {desktop} / GPU: {gpu}"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("what", choices=["all", "startup", "idle", "memory", "vim", "throughput", "size"])
    ap.add_argument("--hull", default=os.path.join(HERE, "..", "target", "release", "hull"))
    ap.add_argument("--terminals", default="hull,alacritty,foot,ghostty")
    ap.add_argument("--runs", type=int, default=10)
    ap.add_argument("--vtebench")
    ap.add_argument("--vtebench-benchmarks")
    args = ap.parse_args()
    hull = os.path.abspath(args.hull)
    names = available(args.terminals.split(","), hull)

    sections = []
    todo = ["size", "startup", "idle", "memory", "vim", "throughput"] if args.what == "all" else [args.what]
    for what in todo:
        title = {"size": "バイナリ", "startup": "起動", "idle": "アイドル", "memory": "メモリ",
                 "vim": "vim の描画時間", "throughput": "スループット"}[what]
        print(f"# {title} を測っています", file=sys.stderr)
        if what == "size":
            body = bench_size(hull)
        elif what == "startup":
            body = bench_startup(names, hull, args.runs)
        elif what == "idle":
            body = bench_idle(names, hull)
        elif what == "memory":
            body = bench_memory(names, hull)
        elif what == "vim":
            body = bench_vim(names, hull)
        else:
            body = bench_throughput(names, hull, args.vtebench, args.vtebench_benchmarks)
        sections.append(f"## {title}\n\n{body}")

    now = datetime.datetime.now().strftime("%Y-%m-%d %H:%M")
    report = f"# hull のベンチマーク ({now})\n\n{machine()}\n\n" + "\n".join(sections)
    print(report)
    os.makedirs(os.path.join(HERE, "results"), exist_ok=True)
    path = os.path.join(HERE, "results", datetime.datetime.now().strftime("%Y%m%d-%H%M") + f"-{args.what}.md")
    with open(path, "w") as f:
        f.write(report)
    print(f"# {path} に書きました", file=sys.stderr)


if __name__ == "__main__":
    main()

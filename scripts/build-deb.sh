#!/bin/sh
# hull の .deb を作る (Ubuntu 24.04 以降の amd64)。
#
#   scripts/build-deb.sh [出力先のディレクトリ]
#
# 本体と同梱のプラグイン 3 つをリリースビルドし、dpkg-deb でまとめる。
# パッケージの名前は v0.1 / v0.2 から続けて hull-terminal にし、入れれば古い版を置き換える。
set -eu

cd "$(dirname "$0")/.."
out=${1:-target/deb}
version=$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
arch=$(dpkg --print-architecture)
pkg=target/deb-root

cargo build --release --workspace

rm -rf "$pkg"
install -Dm755 target/release/hull "$pkg/usr/bin/hull"
for name in search url-hints claude-status; do
    install -Dm755 "target/release/hull-$name" "$pkg/usr/lib/hull/plugins/hull-$name"
done
install -Dm644 packaging/hull.desktop "$pkg/usr/share/applications/hull.desktop"
install -Dm644 packaging/hull.svg "$pkg/usr/share/icons/hicolor/scalable/apps/hull.svg"
install -Dm644 crates/hull/hull.toml "$pkg/usr/share/doc/hull-terminal/hull.toml.example"
install -Dm644 README.md "$pkg/usr/share/doc/hull-terminal/README.md"
install -Dm644 docs/plugins.md "$pkg/usr/share/doc/hull-terminal/plugins.md"
install -Dm644 LICENSE "$pkg/usr/share/doc/hull-terminal/copyright"

size=$(du -sk "$pkg" | cut -f1)
mkdir -p "$pkg/DEBIAN"
cat > "$pkg/DEBIAN/control" <<CONTROL
Package: hull-terminal
Version: $version
Architecture: $arch
Maintainer: Coxless <201592128+Coxless@users.noreply.github.com>
Installed-Size: $size
Depends: libc6 (>= 2.39), libegl1, libwayland-client0, libwayland-egl1, libxkbcommon0, libfontconfig1
Recommends: fonts-noto-cjk, fonts-noto-color-emoji, libnotify-bin, xdg-utils
Suggests: tmux
Section: x11
Priority: optional
Homepage: https://github.com/Coxless/hull
Description: Ubuntu 専用に削った、速いターミナル
 GPU (GLES3) で変わった行だけを描き、Wayland と直接話す、Ubuntu + GNOME 専用の端末。
 タブ・分割・セッションは持たず tmux に任せ、足りない機能はプロセスの外で動く
 プラグインで足す。検索、URL ヒント、Claude Code の状態のプラグインを同梱する。
CONTROL

mkdir -p "$out"
deb="$out/hull-terminal_${version}_${arch}.deb"
dpkg-deb --root-owner-group --build "$pkg" "$deb"
echo "$deb"

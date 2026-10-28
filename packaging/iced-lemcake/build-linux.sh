#!/usr/bin/env bash
# Builds the "iced lemon cake" package for Linux x86-64: Lemonade 2026.43.3 with the Cake and lemonade-burn
# backends (lemond + lemonade CLI), the Rust bridge, and the lemonade-burn server.
#
#   build-linux.sh WORKDIR
#
# Needs: git, cmake, ninja, g++, python3, cargo, curl. Build on the OLDEST glibc you want to support
# (the shipped package needs glibc >= the build host's; t440 has 2.39, so Ubuntu 24.04 and newer).
# Env: LEMONADE_FORK (default /codebase/repos/lemonade-fork, branch iced-lemcake/2026.43.3)
#      BRIDGE_REPO   (default /data/repos/lemonade_cake_integration, branch bridge/runtime-name-2026-10-22)
set -euo pipefail
W="$(realpath -m "${1:?usage: build-linux.sh WORKDIR}")"; HERE="$(cd "$(dirname "$0")" && pwd)"
FORK="${LEMONADE_FORK:-/codebase/repos/lemonade-fork}"; BRIDGE="${BRIDGE_REPO:-/data/repos/lemonade_cake_integration}"
VER=2026.43.3; NAME="iced-lemcake-$VER-x86_64-linux"
BURN_URL=https://github.com/festeraeb/lemonade-burn/releases/download/v0.1.1/lemonade-burn-server-0.1.1-x86_64-linux.tar.gz
BURN_SHA=3b97e6b573716bb312a2cb601fa6cb35543384f2c11c234c826dc9fda05b7145
mkdir -p "$W/src" "$W/bridge" "$W/burn" "$W/$NAME"
git -C "$FORK" archive iced-lemcake/$VER | tar x -C "$W/src"
(cd "$W/src" && patch -p1 < "$HERE"/0001-*.patch)
(cd "$W/src" && cmake -B build -G Ninja -DCMAKE_BUILD_TYPE=Release -DBUILD_WEB_APP=OFF >"$W/cmake.log" 2>&1 \
  && cmake --build build -j"$(nproc)" --target lemond lemonade >"$W/build.log" 2>&1)
git -C "$BRIDGE" archive bridge/runtime-name-2026-10-22 | tar x -C "$W/bridge"
(cd "$W/bridge/rust_bridge" && touch Cargo.toml src/main.rs && cargo build --release --target-dir "$W/bridge/target")
curl -fsSL "$BURN_URL" -o "$W/burn/burn.tar.gz"; echo "$BURN_SHA  $W/burn/burn.tar.gz" | sha256sum -c -
tar xzf "$W/burn/burn.tar.gz" -C "$W/burn"
cp "$W/src/build/lemond" "$W/src/build/lemonade" "$W/$NAME/"; cp -r "$W/src/build/resources" "$W/$NAME/resources"
cp "$W/bridge/target/release/lemonade-cake-bridge" "$W/burn/lemonade-burn-server" "$W/$NAME/"
cp "$W/src/LICENSE" "$W/$NAME/LICENSE-lemonade"; cp "$W/burn/LICENSE-"* "$BRIDGE/LICENSE" "$W/$NAME/" 2>/dev/null || true
cp "$HERE"/0001-*.patch "$W/$NAME/"
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime=@0 -czf "$W/$NAME.tar.gz" -C "$W/$NAME" .
sha256sum "$W/$NAME.tar.gz"

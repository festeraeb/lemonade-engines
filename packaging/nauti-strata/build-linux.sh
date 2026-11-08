#!/usr/bin/env bash
# Builds the Nauti-Strata package for the Lemonade Stand (Linux x86-64, NVIDIA CUDA 12).
#
#   build-linux.sh WORKDIR COMMIT
#
# COMMIT is a Nauti-Strata commit (github.com/festeraeb/Nauti-Strata). Two engines are built from it, one with an AVX2
# floor and one with an AVX floor (stand/start.sh picks by /proc/cpuinfo), for sm_60, sm_61, sm_75 and sm_86, no RDMA.
# The CUDA 12.8 runtime, cuBLAS and cuBLASLt are bundled (NVIDIA's redistributable list); the driver is the user's.
# Build on the OLDEST glibc you want to support (t440: 2.39, so Ubuntu 24.04 and newer).
# Env: NAUTI_STRATA (default /codebase/repos/Nauti-Strata), CUDA (default /usr/local/cuda-12.8), JOBS (default 16)
set -euo pipefail
W="$(realpath -m "${1:?usage: build-linux.sh WORKDIR COMMIT}")"; C="${2:?usage: build-linux.sh WORKDIR COMMIT}"
HERE="$(cd "$(dirname "$0")" && pwd)"; REPO="${NAUTI_STRATA:-/codebase/repos/Nauti-Strata}"
CUDA="${CUDA:-/usr/local/cuda-12.8}"; JOBS="${JOBS:-16}"
VER="$(git -C "$REPO" show "$C:CMakeLists.txt" | sed -n 's/^project(strata VERSION \([0-9.]*\).*/\1/p')"
SHORT="$(git -C "$REPO" rev-parse --short=8 "$C")"
NAME="nauti-strata-$VER-$SHORT-x86_64-linux-cuda12"; OUT="$W/$NAME"
mkdir -p "$W"; rm -rf "$W/src" "$OUT"; git -C "$REPO" archive --prefix=src/ "$C" | tar x -C "$W"
for v in avx2 avx; do
  [ -x "$W/b-$v/strata-poly" ] && continue
  if [ $v = avx2 ]; then floor=-DSTRATA_PORTABLE=ON; else floor=-DSTRATA_ISA_FLOOR=avx; fi
  nice -n 19 cmake -S "$W/src" -B "$W/b-$v" -G Ninja -DCMAKE_BUILD_TYPE=Release -DSTRATA_ENABLE_CUDA=ON \
    -DSTRATA_EXPERIMENTAL_SM60=ON -DCMAKE_CUDA_ARCHITECTURES="60;61;75;86" -DCMAKE_CUDA_COMPILER="$CUDA/bin/nvcc" \
    -DSTRATA_RDMA=OFF -DSTRATA_BUILD_TESTS=OFF $floor > "$W/cfg-$v.log" 2>&1
  nice -n 19 cmake --build "$W/b-$v" --target strata-poly -j "$JOBS" > "$W/build-$v.log" 2>&1
done
mkdir -p "$OUT/bin" "$OUT/lib" "$OUT/stand"
for v in avx2 avx; do install -m755 "$W/b-$v/strata-poly" "$OUT/bin/strata-poly-$v"; strip "$OUT/bin/strata-poly-$v"; done
for l in libcudart.so.12 libcublas.so.12 libcublasLt.so.12; do cp -L "$CUDA/lib64/$l" "$OUT/lib/$l"; done
cp -r "$W/src/serve" "$W/src/tools" "$W/src/data" "$OUT/"
find "$OUT/serve" "$OUT/tools" -name 'test_*' -delete; find "$OUT" -name __pycache__ -prune -exec rm -rf {} +
cp "$W/src/LICENSE" "$OUT/LICENSE"; cp "$CUDA/EULA.txt" "$OUT/lib/NVIDIA-EULA.txt"
cp "$HERE"/stand/* "$OUT/stand/"; chmod 755 "$OUT/stand/"*.sh
printf '{"name":"Nauti-Strata","version":"%s","commit":"%s","archs":[60,61,75,86],"cuda":"12.8","engines":["avx2","avx"]}\n' \
  "$VER" "$(git -C "$REPO" rev-parse "$C")" > "$OUT/BUILD.json"
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime=@0 -czf "$W/$NAME.tar.gz" -C "$OUT" .
(cd "$W" && sha256sum "$NAME.tar.gz" > "$NAME.tar.gz.sha256" && cat "$NAME.tar.gz.sha256")

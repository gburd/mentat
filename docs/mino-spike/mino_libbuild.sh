#!/bin/bash
# Build libmino.a from the Makefile's exact SRCS globs (flat, per-dir), minus main.c.
set -e
cd ~/mino
CFLAGS="-std=c99 -O2 -DMINO_CPJIT=1 -Wno-array-bounds"
INC="-Isrc -Isrc/public -Isrc/runtime -Isrc/gc -Isrc/eval -Isrc/values -Isrc/collections -Isrc/prim -Isrc/async -Isrc/interop -Isrc/diag -Isrc/vendor/imath -Isrc/vendor/bearssl -Isrc/vendor/bearssl/inc -Isrc/vendor/miniz -Isrc/vendor/miniz/upstream"
# EXACT Makefile globs (flat wildcards), excluding main.c.
SRCS=$(ls src/eval/*.c src/eval/bc/*.c src/eval/bc/jit/*.c src/diag/*.c \
  src/runtime/*.c src/gc/*.c src/public/*.c src/values/*.c src/collections/*.c \
  src/prim/*.c src/interop/*.c src/regex/*.c src/async/*.c \
  src/vendor/imath/*.c src/vendor/bearssl/*.c src/vendor/miniz/*.c 2>/dev/null)
n=$(echo "$SRCS" | wc -l)
echo "compiling $n source files (parallel) ..."
mkdir -p /tmp/minobuild
find /tmp/minobuild -name '*.o' -delete 2>/dev/null || true
export CFLAGS INC
compile_one(){ f="$1"; o=/tmp/minobuild/$(echo "$f" | tr '/' '_').o; cc $CFLAGS $INC -c "$f" -o "$o"; }
export -f compile_one
echo "$SRCS" | xargs -P4 -I{} bash -c 'compile_one "$@"' _ {}
echo "compiled $(ls /tmp/minobuild/*.o | wc -l) objects (expected $n)"
ar rcs /tmp/minobuild/libmino.a /tmp/minobuild/*.o
echo "libmino.a: $(ls -la /tmp/minobuild/libmino.a | awk '{print $5}') bytes"
cc $CFLAGS -Isrc /tmp/embed_smoke.c /tmp/minobuild/libmino.a -lm -lpthread -o /tmp/embed_smoke
echo "--- running smoke test ---"
/tmp/embed_smoke

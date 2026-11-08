#!/bin/sh
set -eu
if [ "$#" -ne 2 ]; then
    echo "usage: sh build.sh aarch64|x86_64 OUTPUT_DIRECTORY" >&2
    exit 2
fi
case "$1" in aarch64|x86_64) ;; *) echo "unsupported architecture: $1" >&2; exit 2;; esac
source_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mkdir -p -- "$2"
zig cc -target "$1-linux-gnu.2.17" -O2 -s -shared -fPIC -Wall -Wextra -Werror "$source_dir/reclaim.c" -pthread -ldl -o "$2/libpvisor-reclaim.so"
zig cc -target "$1-linux-gnu.2.17" -O0 -s -Wall -Wextra -Werror "$source_dir/subject.c" -pthread -o "$2/subject"
zig cc -target "$1-linux-gnu.2.17" -O2 -s -shared -fPIC -Wall -Wextra -Werror "$source_dir/interposer.c" -o "$2/interposer.so"
zig cc -target "$1-linux-musl" -static -O2 -s -Wall -Wextra -Werror "$source_dir/uninstrumented.c" -o "$2/static-subject"

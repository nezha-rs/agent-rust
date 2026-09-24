#!/usr/bin/env bash
set -euo pipefail

args=()
for arg in "$@"; do
    case "$arg" in
        --target=powerpc-unknown-linux-musl)
            args+=(--target=powerpc-linux-musl)
            ;;
        crt1.o|crti.o|crtbegin.o|crtend.o|crtn.o|-nostartfiles)
            # Zig supplies the musl startup objects when linking.
            ;;
        *)
            args+=("$arg")
            ;;
    esac
done

exec "${ZIG:-zig}" cc -target powerpc-linux-musl -static "${args[@]}"

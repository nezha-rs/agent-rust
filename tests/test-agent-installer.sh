#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
sed '/^case "${1:-}" in$/,$d' "$root/agent.sh" > "$fixture/functions.sh"
. "$fixture/functions.sh"
as_root() { "$@"; }
download() { cp "$root/target/release-upload/SHA256SUMS.txt" "$2"; }

NZ_ARCH=ppc select_asset
[ "$ASSET" = 'UPX-nezha-agent-rust-v2.1.0-linux-ppc32-be-static-elf' ]
NZ_ARCH=mips64le_softfloat select_asset
[ "$ASSET" = 'nezha-agent-rust-v2.1.0-linux-mips64-le-softfloat-static-elf' ]
NZ_ARCH=armv7_softfloat select_asset
[ "$ASSET" = 'UPX-nezha-agent-rust-v2.1.0-linux-armv7-cortex-a9-softfloat-no-vfp-static-elf' ]

for arch in 386 amd64 armv5 armv6 armv7_softfloat armv7_hardfloat arm64 \
    mips_softfloat mips_hardfloat mipsle_softfloat mipsle_hardfloat \
    mips64_softfloat mips64_hardfloat mips64le_softfloat mips64le_hardfloat \
    ppc ppc64 ppc64le s390x; do
    NZ_ARCH=$arch select_asset >/dev/null
    [ -n "$ASSET_SHA" ]
    [ -f "$root/target/release-upload/$ASSET" ]
done

uname() { if [ "$1" = -s ]; then printf 'OpenBSD\n'; else command uname "$@"; fi; }
for arch in 386 amd64 arm64 armv5 armv6 armv7_softfloat; do
    NZ_ARCH=$arch select_asset >/dev/null
    [ "$ASSET" = "$ORIGINAL" ]
    [ -f "$root/target/release-upload/$ASSET" ]
done
unset -f uname

ASSET=UPX-test ORIGINAL=test ASSET_SHA=upx-check ORIGINAL_SHA=raw-check
fetch_selected() { [ "$ASSET" = test ]; }
fetch_binary
[ "$ASSET" = test ]

printf 'CPU architecture\t: 7\nFeatures\t: half thumb fastmult\n' > "$fixture/cpuinfo"
MACHINE=armv7l ABI=arm_cortex-a9 NZ_CPUINFO_PATH="$fixture/cpuinfo"
export NZ_CPUINFO_PATH
[ "$(arm_arch)" = armv7_softfloat ]
printf 'CPU architecture\t: 7\nFeatures\t: half thumb vfp\n' > "$fixture/cpuinfo"
ABI=armhf
[ "$(arm_arch)" = armv7_hardfloat ]

CONFIG_DIR="$fixture/config"
NZ_BUSYBOX_RCS_PATH="$fixture/rcS"
NZ_NO_START=1
export NZ_BUSYBOX_RCS_PATH NZ_NO_START
mkdir -p "$CONFIG_DIR"
printf '#!/bin/sh\nsleep 2\n' > "$NZ_BUSYBOX_RCS_PATH"
cp "$NZ_BUSYBOX_RCS_PATH" "$fixture/original-rcS"
printf '#!/bin/sh\ntouch %s\n' "$fixture/started" > "$CONFIG_DIR/supervise.sh"
chmod 755 "$CONFIG_DIR/supervise.sh"
install_busybox_rcs
install_busybox_rcs
[ "$(grep -c '# BEGIN nezha-agent-rust boot hook' "$NZ_BUSYBOX_RCS_PATH")" = 1 ]
sh "$NZ_BUSYBOX_RCS_PATH" &
rcs_pid=$!
sleep 1
[ -f "$fixture/started" ]
wait "$rcs_pid"
remove_busybox_rcs_hook
cmp "$NZ_BUSYBOX_RCS_PATH" "$fixture/original-rcS"

AGENT_DIR="$fixture/agent"
CONFIG_DIR="$fixture/config"
RUNTIME_DIR="$fixture/volatile"
RUNTIME_BINARY="$AGENT_DIR/$SERVICE_NAME"
TEMP_BINARY="$fixture/download"
NZ_SERVER=dashboard.example:8009
NZ_CLIENT_SECRET=test-secret
NZ_TLS=false
export NZ_SERVER NZ_CLIENT_SECRET NZ_TLS
select_asset() { ASSET=UPX-test-binary; ASSET_SHA=test-checksum; }
fetch_binary() {
    cp "$root/target/release-upload/UPX-nezha-agent-rust-v2.1.0-linux-x86-64-static-elf" "$TEMP_BINARY"
    ASSET_SIZE=$(wc -c < "$TEMP_BINARY" | tr -d '[:space:]')
}
detect_init() { INIT_SYSTEM=busybox-rcs; }
install_agent
[ -x "$RUNTIME_BINARY" ]
[ -x "$CONFIG_DIR/run.sh" ]
[ -x "$CONFIG_DIR/supervise.sh" ]
grep -q "server: 'dashboard.example:8009'" "$CONFIG_DIR/config.yml"
grep -q "client_secret: 'test-secret'" "$CONFIG_DIR/config.yml"
sh -n "$CONFIG_DIR/run.sh" "$CONFIG_DIR/supervise.sh"
remove_busybox_rcs_hook
cmp "$NZ_BUSYBOX_RCS_PATH" "$fixture/original-rcS"
printf '%s\n' 'agent installer tests passed'

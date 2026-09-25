#!/bin/sh

# Rust Agent installer based on the service and volatile-storage flow in
# https://github.com/nezha-rs/agent/blob/main/agent.sh

umask 077

RELEASE_TAG=v2.1.0
RELEASE_BASE="https://github.com/nezha-rs/agent-rust/releases/download/$RELEASE_TAG"
SUMS_SHA256=0e8c9641b9b740d5bb768fb831ae4c07335fa5311aba5ed7d0e45de3694ed48a
AGENT_DIR="${NZ_AGENT_PATH:-/opt/nezha-rust/agent}"
CONFIG_DIR="${NZ_CONFIG_DIR:-/etc/nezha-agent-rust}"
RUNTIME_DIR="${NZ_VOLATILE_RUNTIME_DIR:-/tmp/nezha-agent-rust}"
TEMP_DIR="${TMPDIR:-/tmp}"
TEMP_BINARY="$TEMP_DIR/nezha-agent-rust.$$.download"
TEMP_SUMS="$TEMP_DIR/nezha-agent-rust.$$.sha256sums"
SERVICE_NAME=nezha-agent-rust
RUNTIME_BINARY="$AGENT_DIR/$SERVICE_NAME"

cleanup() { rm -f "$TEMP_BINARY" "$TEMP_SUMS"; }
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
info() { printf '%s\n' "$*"; }
die() { printf 'Error: %s\n' "$*" >&2; exit 1; }
has() { command -v "$1" >/dev/null 2>&1; }

as_root() {
    if [ "$(id -u)" = 0 ]; then "$@"
    elif has sudo; then sudo "$@"
    else die 'Root privileges or sudo are required.'
    fi
}

download() {
    url=$1 destination=$2
    if has curl && curl -fsSL --connect-timeout 20 --max-time 180 --retry 2 -o "$destination" "$url"; then return 0; fi
    if has wget && wget -q -O "$destination" "$url"; then return 0; fi
    if has uclient-fetch && uclient-fetch -q -O "$destination" "$url"; then return 0; fi
    if has busybox && busybox wget -q -O "$destination" "$url"; then return 0; fi
    return 1
}

sha256_file() {
    if has sha256sum; then sha256sum "$1" | awk '{print $1}'
    elif has shasum; then shasum -a 256 "$1" | awk '{print $1}'
    elif has openssl; then openssl dgst -sha256 "$1" | sed 's/^.*= //'
    elif has busybox; then busybox sha256sum "$1" | awk '{print $1}'
    else return 1
    fi
}

package_abi() {
    if has opkg; then opkg print-architecture 2>/dev/null | awk '$1=="arch" && $2!="all" {a=$2} END {print a}'
    elif has apk; then apk --print-arch 2>/dev/null
    elif has dpkg; then dpkg --print-architecture 2>/dev/null
    elif has rpm; then rpm --eval '%{_arch}' 2>/dev/null
    fi
}

elf_byte() {
    offset=$1
    for path in /bin/busybox /bin/sh /usr/bin/env; do
        if [ -r "$path" ]; then
            dd if="$path" bs=1 skip="$offset" count=1 2>/dev/null | od -An -tu1 2>/dev/null | tr -d '[:space:]'
            return
        fi
    done
}

arm_arch() {
    case "$MACHINE:$ABI" in
        armv5*:*|*:armv5*|*:armel*) printf 'armv5\n'; return ;;
        armv6*:*|*:armv6*) printf 'armv6\n'; return ;;
    esac
    cpu_arch=$(sed -n 's/^CPU architecture[[:space:]]*:[[:space:]]*//p' "${NZ_CPUINFO_PATH:-/proc/cpuinfo}" 2>/dev/null | head -n 1)
    case "$MACHINE:$ABI:$cpu_arch" in
        armv7*:*|armv8l:*|*:armv7*:*|*:arm_cortex-a*:*|*:*:7|*:*:8)
            features=$(sed -n 's/^[Ff]eatures[[:space:]]*:[[:space:]]*//p' "${NZ_CPUINFO_PATH:-/proc/cpuinfo}" 2>/dev/null | head -n 1)
            case " $features :$ABI" in
                *' vfp '*:*hf*|*' vfpv3 '*:*hf*|*' vfpv4 '*:*hf*) printf 'armv7_hardfloat\n' ;;
                *) printf 'armv7_softfloat\n' ;;
            esac ;;
        *:*:6) printf 'armv6\n' ;;
        *) printf 'armv5\n' ;;
    esac
}

detect_arch() {
    MACHINE=$(uname -m)
    ABI=$(package_abi)
    BITS=$(getconf LONG_BIT 2>/dev/null || true)
    case "${NZ_ARCH:-}" in
        '') ;;
        386|amd64|armv5|armv6|armv7_softfloat|armv7_hardfloat|arm64|mips_softfloat|mips_hardfloat|mipsle_softfloat|mipsle_hardfloat|mips64_softfloat|mips64_hardfloat|mips64le_softfloat|mips64le_hardfloat|ppc|ppc64|ppc64le|s390x)
            ARCH=$NZ_ARCH; return ;;
        *) die "Unsupported NZ_ARCH override: $NZ_ARCH" ;;
    esac
    case "$MACHINE:$ABI" in
        *:i?86|*:x86|*:x86_64) if [ "$BITS" = 32 ]; then ARCH=386; else ARCH=amd64; fi ;;
        x86_64:*|amd64:*) ARCH=amd64 ;;
        i?86:*|x86:*) ARCH=386 ;;
        aarch64:*|arm64:*) ARCH=arm64 ;;
        arm*:*) ARCH=$(arm_arch) ;;
        mips64el:*|mips64le:*) if [ "$BITS" = 32 ]; then ARCH=mipsle; else ARCH=mips64le; fi ;;
        mips64:*|mips64eb:*) if [ "$BITS" = 32 ]; then ARCH=mips; else ARCH=mips64; fi ;;
        mipsel:*|mipsle:*) ARCH=mipsle ;;
        mipseb:*) ARCH=mips ;;
        mips:*)
            case "$(elf_byte 5)" in 1) ARCH=mipsle ;; 2) ARCH=mips ;; *) die 'MIPS endian unknown; set NZ_ARCH.' ;; esac ;;
        ppc64le:*|powerpc64le:*) ARCH=ppc64le ;;
        ppc64:*|powerpc64:*) if [ "$BITS" = 32 ]; then ARCH=ppc; else ARCH=ppc64; fi ;;
        ppc:*|powerpc:*) ARCH=ppc ;;
        s390x:*) ARCH=s390x ;;
        *) die "Unsupported CPU: uname -m=$MACHINE, package ABI=${ABI:-none}" ;;
    esac
    case "$ARCH" in
        mips|mipsle|mips64|mips64le)
            case "$ABI" in *hf*|*hard*) ARCH="${ARCH}_hardfloat" ;; *) ARCH="${ARCH}_softfloat" ;; esac ;;
    esac
}

select_original() {
    OS=$(uname -s)
    detect_arch
    case "$OS:$ARCH" in
        Linux:386) suffix=linux-x86-32-static-elf ;;
        Linux:amd64) suffix=linux-x86-64-static-elf ;;
        Linux:arm64) suffix=linux-arm64-static-elf ;;
        Linux:armv5) suffix=linux-armv5te-softfloat-static-elf ;;
        Linux:armv6) suffix=linux-armv6-softfloat-static-elf ;;
        Linux:armv7_softfloat) suffix=linux-armv7-cortex-a9-softfloat-no-vfp-static-elf ;;
        Linux:armv7_hardfloat) suffix=linux-armv7-hardfloat-static-elf ;;
        Linux:mips_softfloat) suffix=linux-mips32-be-softfloat-static-elf ;;
        Linux:mips_hardfloat) suffix=linux-mips32-be-hardfloat-static-elf ;;
        Linux:mipsle_softfloat) suffix=linux-mips32-le-softfloat-static-elf ;;
        Linux:mipsle_hardfloat) suffix=linux-mips32-le-hardfloat-static-elf ;;
        Linux:mips64_softfloat) suffix=linux-mips64-be-softfloat-static-elf ;;
        Linux:mips64_hardfloat) suffix=linux-mips64-be-hardfloat-static-elf ;;
        Linux:mips64le_softfloat) suffix=linux-mips64-le-softfloat-static-elf ;;
        Linux:mips64le_hardfloat) suffix=linux-mips64-le-hardfloat-static-elf ;;
        Linux:ppc) suffix=linux-ppc32-be-static-elf ;;
        Linux:ppc64) suffix=linux-ppc64-be-static-elf ;;
        Linux:ppc64le) suffix=linux-ppc64-le-static-elf ;;
        Linux:s390x) suffix=linux-s390x-static-elf ;;
        OpenBSD:386) suffix=openbsd-x86-32-static-elf ;;
        OpenBSD:amd64) suffix=openbsd-x86-64-static-elf ;;
        OpenBSD:arm64) suffix=openbsd-arm64-static-elf ;;
        OpenBSD:armv5) suffix=openbsd-armv5te-eabi5-experimental-static-elf ;;
        OpenBSD:armv6) suffix=openbsd-armv6-eabi5-experimental-static-elf ;;
        OpenBSD:armv7_softfloat|OpenBSD:armv7_hardfloat) suffix=openbsd-armv7-eabi5-static-elf ;;
        *) die "No Rust Release binary for $OS/$ARCH" ;;
    esac
    ORIGINAL="nezha-agent-rust-$RELEASE_TAG-$suffix"
}

sum_for() { awk -v name="$1" '{sub(/\r$/, "", $2); if ($2==name) {print $1; exit}}' "$TEMP_SUMS"; }

select_asset() {
    select_original
    download "$RELEASE_BASE/SHA256SUMS.txt" "$TEMP_SUMS" || die 'Could not download Release checksums.'
    [ "$(sha256_file "$TEMP_SUMS")" = "$SUMS_SHA256" ] || die 'Release checksum list differs from the pinned version.'
    ORIGINAL_SHA=$(sum_for "$ORIGINAL")
    [ -n "$ORIGINAL_SHA" ] || die "Release does not contain $ORIGINAL"
    ASSET="UPX-$ORIGINAL"
    ASSET_SHA=$(sum_for "$ASSET")
    if [ -z "$ASSET_SHA" ]; then ASSET=$ORIGINAL; ASSET_SHA=$ORIGINAL_SHA; fi
    info "Detected: $OS $MACHINE ($ARCH, ABI ${ABI:-unknown})"
    case "$ORIGINAL" in *experimental*) info 'This OpenBSD ARM build is experimental and was not verified on target hardware.' ;; esac
    info "Selected: $ASSET"
}

verify_header() {
    has od || return 0
    header=$(od -An -tx1 -N20 "$TEMP_BINARY" 2>/dev/null | tr -d '[:space:]')
    case "$header" in 7f454c46*) ;; *) return 1 ;; esac
    class=$(printf '%s' "$header" | cut -c9-10)
    data=$(printf '%s' "$header" | cut -c11-12)
    machine=$(printf '%s' "$header" | cut -c37-40)
    case "$ARCH" in
        386) [ "$class:$data:$machine" = 01:01:0300 ] ;;
        amd64) [ "$class:$data:$machine" = 02:01:3e00 ] ;;
        arm64) [ "$class:$data:$machine" = 02:01:b700 ] ;;
        arm*) [ "$class:$data:$machine" = 01:01:2800 ] ;;
        mips64le*) [ "$class:$data:$machine" = 02:01:0800 ] ;;
        mips64*) [ "$class:$data:$machine" = 02:02:0008 ] ;;
        mipsle*) [ "$class:$data:$machine" = 01:01:0800 ] ;;
        mips*) [ "$class:$data:$machine" = 01:02:0008 ] ;;
        ppc64le) [ "$class:$data:$machine" = 02:01:1500 ] ;;
        ppc64) [ "$class:$data:$machine" = 02:02:0015 ] ;;
        ppc) [ "$class:$data:$machine" = 01:02:0014 ] ;;
        s390x) [ "$class:$data:$machine" = 02:02:0016 ] ;;
        *) return 1 ;;
    esac
}

fetch_selected() {
    info "Downloading $RELEASE_BASE/$ASSET"
    rm -f "$TEMP_BINARY"
    download "$RELEASE_BASE/$ASSET" "$TEMP_BINARY" || return 2
    [ -s "$TEMP_BINARY" ] || return 2
    [ "$(sha256_file "$TEMP_BINARY")" = "$ASSET_SHA" ] || die "SHA-256 mismatch for $ASSET"
    verify_header || die "ELF header does not match $OS/$ARCH"
    chmod 755 "$TEMP_BINARY" || die 'Could not make binary executable.'
    version=$("$TEMP_BINARY" --version 2>&1) || return 2
    case "$version" in *'nezha-agent-rust 2.1.0'*) ;; *) return 2 ;; esac
    ASSET_SIZE=$(wc -c < "$TEMP_BINARY" | tr -d '[:space:]')
    info "Verified $ASSET ($ASSET_SIZE bytes, SHA-256 $ASSET_SHA)"
    return 0
}

fetch_binary() {
    if fetch_selected; then return 0; fi
    [ "$ASSET" != "$ORIGINAL" ] || die "Download or runtime check failed for $ORIGINAL"
    info 'UPX copy could not run or download; trying the original binary.'
    ASSET=$ORIGINAL ASSET_SHA=$ORIGINAL_SHA
    fetch_selected || die "Download or runtime check failed for $ORIGINAL"
}

shell_quote() { printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"; }
yaml_quote() { printf "'%s'" "$(printf '%s' "$1" | sed "s/'/''/g")"; }
boolean() {
    case "$1" in true|TRUE|1|yes|on) printf true ;; false|FALSE|0|no|off|'') printf false ;; *) die "Invalid boolean: $1" ;; esac
}

write_config() {
    [ -n "${NZ_SERVER:-}" ] || die 'NZ_SERVER is required.'
    [ -n "${NZ_CLIENT_SECRET:-}" ] || die 'NZ_CLIENT_SECRET is required.'
    as_root mkdir -p "$CONFIG_DIR" || die 'Cannot create config directory.'
    uuid=${NZ_UUID:-}
    if [ -z "$uuid" ] && [ -f "$CONFIG_DIR/config.yml" ]; then
        uuid=$(sed -n "s/^uuid: '\([^']*\)'$/\1/p" "$CONFIG_DIR/config.yml" | head -n 1)
    fi
    if [ -z "$uuid" ]; then
        if [ -r /proc/sys/kernel/random/uuid ]; then uuid=$(head -n 1 /proc/sys/kernel/random/uuid)
        elif has uuidgen; then uuid=$(uuidgen)
        else die 'NZ_UUID or uuidgen is required.'
        fi
    fi
    config_temp="$TEMP_DIR/nezha-agent-rust.$$.config"
    {
        printf 'server: %s\n' "$(yaml_quote "$NZ_SERVER")"
        printf 'client_secret: %s\n' "$(yaml_quote "$NZ_CLIENT_SECRET")"
        printf 'uuid: %s\n' "$(yaml_quote "$uuid")"
        printf 'tls: %s\n' "$(boolean "${NZ_TLS:-false}")"
        printf 'disable_auto_update: %s\n' "$(boolean "${NZ_DISABLE_AUTO_UPDATE:-true}")"
        printf 'disable_force_update: %s\n' "$(boolean "${NZ_DISABLE_FORCE_UPDATE:-false}")"
        printf 'disable_command_execute: %s\n' "$(boolean "${NZ_DISABLE_COMMAND_EXECUTE:-false}")"
        printf 'skip_connection_count: %s\n' "$(boolean "${NZ_SKIP_CONNECTION_COUNT:-false}")"
    } > "$config_temp"
    as_root cp "$config_temp" "$CONFIG_DIR/config.yml" || die 'Cannot install config.'
    as_root chmod 600 "$CONFIG_DIR/config.yml"
    rm -f "$config_temp"
}

write_runtime_files() {
    env_temp="$TEMP_DIR/nezha-agent-rust.$$.env"
    {
        printf 'RUNTIME_URL=%s\n' "$(shell_quote "$RELEASE_BASE/$ASSET")"
        printf 'RUNTIME_SHA=%s\n' "$(shell_quote "$ASSET_SHA")"
        printf 'FALLBACK_URL=%s\n' "$(shell_quote "$RELEASE_BASE/$ORIGINAL")"
        printf 'FALLBACK_SHA=%s\n' "$(shell_quote "$ORIGINAL_SHA")"
        printf 'RUNTIME_BINARY=%s\n' "$(shell_quote "$RUNTIME_BINARY")"
        printf 'RUNTIME_CONFIG=%s\n' "$(shell_quote "$CONFIG_DIR/config.yml")"
    } > "$env_temp"
    as_root cp "$env_temp" "$CONFIG_DIR/runtime.env" || die 'Cannot install runtime metadata.'
    as_root chmod 600 "$CONFIG_DIR/runtime.env"
    rm -f "$env_temp"
    runner_temp="$TEMP_DIR/nezha-agent-rust.$$.runner"
    cat > "$runner_temp" <<'RUNNER'
#!/bin/sh
set -u
. "${0%/*}/runtime.env"
sha() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'
    elif command -v busybox >/dev/null 2>&1; then busybox sha256sum "$1" | awk '{print $1}'
    elif command -v openssl >/dev/null 2>&1; then openssl dgst -sha256 "$1" | sed 's/^.*= //'
    else return 1; fi
}
valid() {
    expected="$2"
    [ -s "$1" ] && [ "$(sha "$1")" = "$expected" ] || return 1
    chmod 755 "$1" || return 1
    "$1" --version 2>/dev/null | grep -q 'nezha-agent-rust 2.1.0'
}
fetch() {
    url="$1" destination="$2"
    if command -v curl >/dev/null 2>&1; then curl -fsSL --retry 3 -o "$destination" "$url"
    elif command -v wget >/dev/null 2>&1; then wget -q -O "$destination" "$url"
    elif command -v uclient-fetch >/dev/null 2>&1; then uclient-fetch -q -O "$destination" "$url"
    elif command -v busybox >/dev/null 2>&1; then busybox wget -q -O "$destination" "$url"
    else return 1; fi
}
install_fetched() {
    url="$1" expected="$2"
    temp="$RUNTIME_BINARY.download.$$"
    trap 'rm -f "$temp"' EXIT HUP INT TERM
    fetch "$url" "$temp" || return 1
    valid "$temp" "$expected" || return 1
    mv -f "$temp" "$RUNTIME_BINARY" || return 1
    trap - EXIT HUP INT TERM
}
if ! valid "$RUNTIME_BINARY" "$RUNTIME_SHA"; then
    mkdir -p "${RUNTIME_BINARY%/*}" || exit 1
    install_fetched "$RUNTIME_URL" "$RUNTIME_SHA" || \
        install_fetched "$FALLBACK_URL" "$FALLBACK_SHA" || exit 1
fi
exec "$RUNTIME_BINARY" -c "$RUNTIME_CONFIG"
RUNNER
    as_root cp "$runner_temp" "$CONFIG_DIR/run.sh" || die 'Cannot install runner.'
    as_root chmod 700 "$CONFIG_DIR/run.sh"
    rm -f "$runner_temp"
    supervisor_temp="$TEMP_DIR/nezha-agent-rust.$$.supervisor"
    cat > "$supervisor_temp" <<'SUPERVISOR'
#!/bin/sh
runner="${0%/*}/run.sh"
pidfile="${0%/*}/supervise.pid"
if [ -f "$pidfile" ] && kill -0 "$(cat "$pidfile")" 2>/dev/null; then exit 0; fi
echo $$ > "$pidfile"
trap 'rm -f "$pidfile"' EXIT
child=
trap '[ -z "$child" ] || { kill "$child" 2>/dev/null || true; wait "$child" 2>/dev/null || true; }; exit 0' HUP INT TERM
while :; do
    "$runner" & child=$!
    wait "$child" || true
    child=
    sleep 5
done
SUPERVISOR
    as_root cp "$supervisor_temp" "$CONFIG_DIR/supervise.sh" || die 'Cannot install supervisor.'
    as_root chmod 700 "$CONFIG_DIR/supervise.sh"
    rm -f "$supervisor_temp"
}

detect_init() {
    if [ -n "${NZ_INIT_SYSTEM:-}" ]; then INIT_SYSTEM=$NZ_INIT_SYSTEM
    elif [ -f /etc/openwrt_release ] || [ -x /sbin/procd ]; then INIT_SYSTEM=openwrt
    elif [ -d /run/systemd/system ] && has systemctl; then INIT_SYSTEM=systemd
    elif has rc-service && has rc-update; then INIT_SYSTEM=openrc
    elif [ -r /etc/inittab ] && grep -q '::sysinit:.*rcS' /etc/inittab; then INIT_SYSTEM=busybox-rcs
    elif [ -d /etc/init.d ]; then INIT_SYSTEM=sysv
    elif has crontab; then INIT_SYSTEM=cron
    else die 'No supported startup manager found.'
    fi
    case "$INIT_SYSTEM" in systemd|openwrt|openrc|sysv|busybox-rcs|cron) ;; *) die "Unsupported NZ_INIT_SYSTEM=$INIT_SYSTEM" ;; esac
}

busybox_rcs_path() {
    rcs=${NZ_BUSYBOX_RCS_PATH:-}
    if [ -z "$rcs" ] && [ -r /etc/inittab ]; then
        rcs=$(sed -n 's/^.*::sysinit:\([^[:space:]]*rcS\).*$/\1/p' /etc/inittab | head -n 1)
    fi
    [ -n "$rcs" ] || rcs=/etc/init.d/rcS
    printf '%s\n' "$rcs"
}

stop_supervisor() {
    if as_root test -f "$CONFIG_DIR/supervise.pid"; then
        supervisor_pid=$(as_root cat "$CONFIG_DIR/supervise.pid")
        as_root kill "$supervisor_pid" 2>/dev/null || true
        sleep 1
    fi
}

install_busybox_rcs() {
    rcs=$(busybox_rcs_path)
    [ -f "$rcs" ] && [ ! -L "$rcs" ] || die "BusyBox rcS must be a regular file: $rcs"
    if ! grep -Fq '# BEGIN nezha-agent-rust boot hook' "$rcs"; then
        as_root cp -p "$rcs" "$rcs.nezha-agent-rust.bak" || die 'Cannot back up rcS.'
        hook_temp="$TEMP_DIR/nezha-agent-rust.$$.rcs"
        {
            IFS= read -r first || true
            printf '%s\n' "$first"
            printf '%s\n' '# BEGIN nezha-agent-rust boot hook'
            printf '( while [ ! -x %s ]; do sleep 2; done; exec %s ) >/dev/null 2>&1 &\n' \
                "$(shell_quote "$CONFIG_DIR/supervise.sh")" "$(shell_quote "$CONFIG_DIR/supervise.sh")"
            printf '%s\n' '# END nezha-agent-rust boot hook'
            cat
        } < "$rcs" > "$hook_temp"
        as_root cp "$hook_temp" "$rcs.nezha-agent-rust.new.$$" || die 'Cannot stage rcS.'
        as_root chmod 755 "$rcs.nezha-agent-rust.new.$$"
        as_root mv "$rcs.nezha-agent-rust.new.$$" "$rcs" || die 'Cannot update rcS.'
        rm -f "$hook_temp"
    fi
    stop_supervisor
    if [ "${NZ_NO_START:-0}" != 1 ]; then as_root "$CONFIG_DIR/supervise.sh" >/dev/null 2>&1 & fi
}

remove_busybox_rcs_hook() {
    rcs=$(busybox_rcs_path)
    [ -f "$rcs" ] && [ ! -L "$rcs" ] || return 0
    grep -Fq '# BEGIN nezha-agent-rust boot hook' "$rcs" || return 0
    hook_temp="$TEMP_DIR/nezha-agent-rust.$$.rcs"
    awk '
        $0 == "# BEGIN nezha-agent-rust boot hook" {skip=1; next}
        $0 == "# END nezha-agent-rust boot hook" {skip=0; next}
        !skip {print}
    ' "$rcs" > "$hook_temp"
    as_root cp "$hook_temp" "$rcs.nezha-agent-rust.new.$$" || die 'Cannot stage rcS removal.'
    as_root chmod 755 "$rcs.nezha-agent-rust.new.$$"
    as_root mv "$rcs.nezha-agent-rust.new.$$" "$rcs" || die 'Cannot remove rcS hook.'
    rm -f "$hook_temp"
}

install_service() {
    case "$INIT_SYSTEM" in
        busybox-rcs) install_busybox_rcs ;;
        systemd)
            unit_temp="$TEMP_DIR/nezha-agent-rust.$$.service"
            {
                printf '%s\n' '[Unit]' 'Description=Nezha Rust Agent' 'After=network-online.target' 'Wants=network-online.target' '' '[Service]' 'Type=simple'
                printf 'ExecStart=%s\n' "$CONFIG_DIR/run.sh"
                printf '%s\n' 'Restart=always' 'RestartSec=5' '' '[Install]' 'WantedBy=multi-user.target'
            } > "$unit_temp"
            as_root cp "$unit_temp" "/etc/systemd/system/$SERVICE_NAME.service" || die 'Cannot install systemd unit.'
            rm -f "$unit_temp"
            as_root systemctl daemon-reload && as_root systemctl enable "$SERVICE_NAME" || die 'Cannot enable systemd unit.'
            [ "${NZ_NO_START:-0}" = 1 ] || as_root systemctl restart "$SERVICE_NAME" || die 'Cannot start systemd unit.' ;;
        openwrt)
            init_temp="$TEMP_DIR/nezha-agent-rust.$$.init"
            {
                printf '%s\n' '#!/bin/sh /etc/rc.common' 'START=99' 'STOP=10' 'USE_PROCD=1' '' 'start_service() {' '    procd_open_instance'
                printf '    procd_set_param command %s\n' "$(shell_quote "$CONFIG_DIR/run.sh")"
                printf '%s\n' '    procd_set_param respawn 5 5 0' '    procd_close_instance' '}'
            } > "$init_temp"
            as_root cp "$init_temp" "/etc/init.d/$SERVICE_NAME" && as_root chmod 755 "/etc/init.d/$SERVICE_NAME" || die 'Cannot install procd service.'
            rm -f "$init_temp"
            as_root "/etc/init.d/$SERVICE_NAME" enable || die 'Cannot enable procd service.'
            [ "${NZ_NO_START:-0}" = 1 ] || as_root "/etc/init.d/$SERVICE_NAME" restart || die 'Cannot start procd service.' ;;
        openrc)
            init_temp="$TEMP_DIR/nezha-agent-rust.$$.init"
            {
                printf '%s\n' '#!/sbin/openrc-run' 'description="Nezha Rust Agent"'
                printf 'command=%s\n' "$(shell_quote "$CONFIG_DIR/run.sh")"
                printf '%s\n' 'command_background="no"' 'supervisor="supervise-daemon"' 'respawn_delay="5"' 'depend() { need net; }'
            } > "$init_temp"
            as_root cp "$init_temp" "/etc/init.d/$SERVICE_NAME" && as_root chmod 755 "/etc/init.d/$SERVICE_NAME" || die 'Cannot install OpenRC service.'
            rm -f "$init_temp"
            as_root rc-update add "$SERVICE_NAME" default || die 'Cannot enable OpenRC service.'
            [ "${NZ_NO_START:-0}" = 1 ] || as_root rc-service "$SERVICE_NAME" restart || die 'Cannot start OpenRC service.' ;;
        sysv)
            init_temp="$TEMP_DIR/nezha-agent-rust.$$.init"
            {
                printf '%s\n' '#!/bin/sh' '### BEGIN INIT INFO' '# Provides: nezha-agent-rust' '# Required-Start: $network' '# Required-Stop: $network' '# Default-Start: 2 3 4 5' '# Default-Stop: 0 1 6' '### END INIT INFO'
                printf 'SUPERVISOR=%s\n' "$(shell_quote "$CONFIG_DIR/supervise.sh")"
                printf '%s\n' 'case "$1" in' '  start) "$SUPERVISOR" >/dev/null 2>&1 & ;;' '  stop) [ -f "${SUPERVISOR%/*}/supervise.pid" ] && kill "$(cat "${SUPERVISOR%/*}/supervise.pid")" 2>/dev/null || true ;;' '  restart) "$0" stop; "$0" start ;;' 'esac'
            } > "$init_temp"
            as_root cp "$init_temp" "/etc/init.d/$SERVICE_NAME" && as_root chmod 755 "/etc/init.d/$SERVICE_NAME" || die 'Cannot install SysV service.'
            rm -f "$init_temp"
            if has update-rc.d; then as_root update-rc.d "$SERVICE_NAME" defaults; elif has chkconfig; then as_root chkconfig "$SERVICE_NAME" on; else die 'No SysV enable command.'; fi
            [ "${NZ_NO_START:-0}" = 1 ] || as_root "/etc/init.d/$SERVICE_NAME" restart || die 'Cannot start SysV service.' ;;
        cron)
            cron_temp="$TEMP_DIR/nezha-agent-rust.$$.cron"
            (crontab -l 2>/dev/null | grep -v "$CONFIG_DIR/supervise.sh" || true; printf '@reboot %s >/dev/null 2>&1\n' "$CONFIG_DIR/supervise.sh") > "$cron_temp"
            as_root crontab "$cron_temp" || die 'Cannot install cron entry.'
            rm -f "$cron_temp"
            stop_supervisor
            if [ "${NZ_NO_START:-0}" != 1 ]; then as_root "$CONFIG_DIR/supervise.sh" >/dev/null 2>&1 & fi ;;
    esac
}

install_agent() {
    select_asset
    fetch_binary
    detect_init
    persistent_ready=1
    as_root mkdir -p "$AGENT_DIR" 2>/dev/null || persistent_ready=0
    available=$(df -Pk "$AGENT_DIR" 2>/dev/null | awk 'NR==2 {print $4}')
    required=$(( (ASSET_SIZE + 1048575) / 1024 + 1024 ))
    if [ "${NZ_FORCE_VOLATILE:-0}" = 1 ] || [ "$persistent_ready" = 0 ] || \
        { [ -n "$available" ] && [ "$available" -lt "$required" ]; }; then
        RUNTIME_BINARY="$RUNTIME_DIR/$SERVICE_NAME"
        info "Using volatile binary storage: $RUNTIME_BINARY"
    fi
    as_root mkdir -p "${RUNTIME_BINARY%/*}" || die 'Cannot create binary directory.'
    stop_supervisor
    binary_stage="$RUNTIME_BINARY.new.$$"
    as_root cp "$TEMP_BINARY" "$binary_stage" && as_root chmod 755 "$binary_stage" && \
        as_root mv -f "$binary_stage" "$RUNTIME_BINARY" || die 'Cannot install binary.'
    write_config
    write_runtime_files
    install_service
    info "Installed $SERVICE_NAME ($INIT_SYSTEM), binary: $RUNTIME_BINARY"
}

uninstall_agent() {
    detect_init
    case "$INIT_SYSTEM" in
        systemd)
            as_root systemctl stop "$SERVICE_NAME" 2>/dev/null || true
            as_root systemctl disable "$SERVICE_NAME" 2>/dev/null || true
            as_root rm -f "/etc/systemd/system/$SERVICE_NAME.service"
            as_root systemctl daemon-reload 2>/dev/null || true ;;
        openwrt)
            as_root "/etc/init.d/$SERVICE_NAME" stop 2>/dev/null || true
            as_root "/etc/init.d/$SERVICE_NAME" disable 2>/dev/null || true
            as_root rm -f "/etc/init.d/$SERVICE_NAME" ;;
        openrc)
            as_root rc-service "$SERVICE_NAME" stop 2>/dev/null || true
            as_root rc-update del "$SERVICE_NAME" default 2>/dev/null || true
            as_root rm -f "/etc/init.d/$SERVICE_NAME" ;;
        sysv)
            as_root "/etc/init.d/$SERVICE_NAME" stop 2>/dev/null || true
            if has update-rc.d; then as_root update-rc.d -f "$SERVICE_NAME" remove 2>/dev/null || true; fi
            if has chkconfig; then as_root chkconfig "$SERVICE_NAME" off 2>/dev/null || true; fi
            as_root rm -f "/etc/init.d/$SERVICE_NAME" ;;
        busybox-rcs) remove_busybox_rcs_hook ;;
        cron)
            cron_temp="$TEMP_DIR/nezha-agent-rust.$$.cron"
            crontab -l 2>/dev/null | grep -v "$CONFIG_DIR/supervise.sh" > "$cron_temp" || true
            as_root crontab "$cron_temp" 2>/dev/null || true
            rm -f "$cron_temp" ;;
    esac
    stop_supervisor
    as_root rm -f "$CONFIG_DIR/config.yml" "$CONFIG_DIR/runtime.env" "$CONFIG_DIR/run.sh" \
        "$CONFIG_DIR/supervise.sh" "$CONFIG_DIR/supervise.pid" \
        "$AGENT_DIR/$SERVICE_NAME" "$RUNTIME_DIR/$SERVICE_NAME"
    info "Uninstalled $SERVICE_NAME"
}

case "${1:-}" in
    --detect|detect) select_asset; info "URL: $RELEASE_BASE/$ASSET" ;;
    --check-download) select_asset; fetch_binary; info 'Download verification passed; no service files changed.' ;;
    uninstall) uninstall_agent ;;
    --help|-h)
        printf '%s\n' 'Usage: NZ_SERVER=host:port NZ_CLIENT_SECRET=secret sh agent.sh' \
            '       sh agent.sh --detect | --check-download' \
            '       sh agent.sh uninstall' \
            'Optional: NZ_TLS, NZ_ARCH, NZ_INIT_SYSTEM, NZ_FORCE_VOLATILE=1, NZ_NO_START=1' \
            'BusyBox rcS: NZ_INIT_SYSTEM=busybox-rcs [NZ_BUSYBOX_RCS_PATH=/path/to/rcS]' ;;
    '') install_agent ;;
    *) die "Unknown option: $1" ;;
esac

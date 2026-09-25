#!/bin/sh

# Nezha Agent v2.3.5 repacked installer.
# Supports Linux, FreeBSD and macOS binaries published by nezha-rs/agent.

NZ_BASE_PATH="${NZ_BASE_PATH:-/opt/nezha-rust}"
NZ_AGENT_PATH="${NZ_AGENT_PATH:-${NZ_BASE_PATH}/agent}"
NZ_RELEASE_TAG='v2.1.0'
NZ_RELEASE_REPOSITORY='nezha-rs/agent-rust'
NZ_RELEASE_BASE="https://github.com/${NZ_RELEASE_REPOSITORY}/releases/download/${NZ_RELEASE_TAG}"
NZ_DOWNLOAD_TIMEOUT="${NZ_DOWNLOAD_TIMEOUT:-180}"
# 0 keeps certificate verification strict, 1 always skips it, and auto retries
# without verification only after a certificate-chain error.
NZ_INSECURE_TLS="${NZ_INSECURE_TLS:-auto}"
CPUINFO_PATH="${NZ_CPUINFO_PATH:-/proc/cpuinfo}"
NZ_VOLATILE_CONFIG_DIR="${NZ_VOLATILE_CONFIG_DIR:-/etc/nezha-agent}"
NZ_VOLATILE_RUNTIME_DIR="${NZ_VOLATILE_RUNTIME_DIR:-/tmp/nezha-agent}"
NZ_OPENWRT_INIT_DIR="${NZ_OPENWRT_INIT_DIR:-/etc/init.d}"
NZ_OPENWRT_RC_COMMON="${NZ_OPENWRT_RC_COMMON:-/etc/rc.common}"
NZ_SYSTEMD_DIR="${NZ_SYSTEMD_DIR:-/etc/systemd/system}"
NZ_FORCE_VOLATILE="${NZ_FORCE_VOLATILE:-0}"
NZ_NO_START="${NZ_NO_START:-0}"

red='\033[0;31m'
green='\033[0;32m'
yellow='\033[0;33m'
plain='\033[0m'

LOG_FILE="${TMPDIR:-/tmp}/nezha-agent-install.$$.log"
TEMP_BINARY="${TMPDIR:-/tmp}/nezha-agent.download.$$"
DEBUG_OUTPUT="${TMPDIR:-/tmp}/nezha-agent-debug.$$.log"
DEBUG_COMMAND_FILE="${TMPDIR:-/tmp}/nezha-agent-debug-command.$$"
NOTIFICATION_SENT=0
DOWNLOAD_TOOL=""
INSTALL_MODE="not-selected"
INSTALL_BINARY_PATH=""
INSTALL_CONFIG_PATH=""
KEEPALIVE_METHOD=""

cleanup() {
    rm -f "$TEMP_BINARY"
    rm -f "$DEBUG_OUTPUT" "$DEBUG_COMMAND_FILE"
    [ "$LOG_FILE" = /dev/null ] || rm -f "$LOG_FILE"
}

append_log() {
    printf '%s\n' "$*" >> "$LOG_FILE" 2>/dev/null || true
}

info() {
    printf "${yellow}%s${plain}\n" "$*"
    append_log "INFO: $*"
}

success() {
    printf "${green}%s${plain}\n" "$*"
    append_log "SUCCESS: $*"
}

err() {
    printf "${red}%s${plain}\n" "$*" >&2
    append_log "ERROR: $*"
}

has_cmd() {
    command -v "$1" >/dev/null 2>&1
}

run_as_root() {
    if [ "$(id -u 2>/dev/null || printf 1)" = "0" ]; then
        "$@"
    elif has_cmd sudo; then
        sudo "$@"
    else
        err "Root privileges are required and sudo is not installed."
        return 1
    fi
}

tls_certificate_error() {
    log_path="$1"
    tail -n 40 "$log_path" 2>/dev/null | grep -Eiq \
        'certificate|issuer|unknown ca|verify failed|unable to verify|unable to get local issuer|self-signed|x509'
}

auto_insecure_tls() {
    [ "$NZ_INSECURE_TLS" = auto ] || return 1
    tls_certificate_error "$LOG_FILE"
}

download_to() {
    url="$1"
    destination="$2"
    client_found=0

    if has_cmd curl; then
        client_found=1
        DOWNLOAD_TOOL="curl"
        rm -f "$destination"
        curl_args="-fL --connect-timeout 20 --max-time $NZ_DOWNLOAD_TIMEOUT"
        [ "$NZ_INSECURE_TLS" = 1 ] && curl_args="$curl_args --insecure"
        curl $curl_args \
            --retry 3 --retry-delay 2 -o "$destination" "$url" >> "$LOG_FILE" 2>&1 && return 0
        if auto_insecure_tls; then
            append_log "WARN: certificate verification failed; retrying curl without certificate verification"
            rm -f "$destination"
            curl_args="-fL --connect-timeout 20 --max-time $NZ_DOWNLOAD_TIMEOUT --insecure"
            curl $curl_args \
                --retry 3 --retry-delay 2 -o "$destination" "$url" >> "$LOG_FILE" 2>&1 && return 0
        fi
        append_log "WARN: curl download failed; trying another available client"
    fi
    if has_cmd wget; then
        client_found=1
        DOWNLOAD_TOOL="wget"
        rm -f "$destination"
        if [ "$NZ_INSECURE_TLS" = 1 ]; then
            wget --no-check-certificate -O "$destination" "$url" >> "$LOG_FILE" 2>&1 && return 0
        else
            wget -O "$destination" "$url" >> "$LOG_FILE" 2>&1 && return 0
        fi
        if auto_insecure_tls; then
            append_log "WARN: certificate verification failed; retrying wget without certificate verification"
            rm -f "$destination"
            wget --no-check-certificate -O "$destination" "$url" >> "$LOG_FILE" 2>&1 && return 0
        fi
        append_log "WARN: wget download failed; trying another available client"
    fi
    if has_cmd uclient-fetch; then
        client_found=1
        DOWNLOAD_TOOL="uclient-fetch"
        rm -f "$destination"
        uclient-fetch -O "$destination" "$url" >> "$LOG_FILE" 2>&1 && return 0
        if auto_insecure_tls; then
            append_log "WARN: certificate verification failed; retrying uclient-fetch without certificate verification"
            rm -f "$destination"
            uclient-fetch --no-check-certificate -O "$destination" "$url" >> "$LOG_FILE" 2>&1 && return 0
        fi
        append_log "WARN: uclient-fetch download failed; trying BusyBox wget"
    fi
    if has_cmd busybox && busybox wget --help >/dev/null 2>&1; then
        client_found=1
        DOWNLOAD_TOOL="busybox wget"
        rm -f "$destination"
        busybox wget -O "$destination" "$url" >> "$LOG_FILE" 2>&1 && return 0
        if auto_insecure_tls && busybox wget --help 2>&1 | grep -q -- '--no-check-certificate'; then
            append_log "WARN: certificate verification failed; retrying BusyBox wget without certificate verification"
            rm -f "$destination"
            busybox wget --no-check-certificate -O "$destination" "$url" >> "$LOG_FILE" 2>&1 && return 0
        fi
    fi
    if [ "$client_found" = 0 ]; then
        err "A download tool is required: curl, wget, uclient-fetch, or BusyBox wget."
    fi
    return 1
}

url_encode() {
    # Encode the form separators and whitespace used by our Telegram payload.
    # BusyBox awk is available on minimal OpenWrt images where the od applet is not.
    printf '%s' "$1" | awk '
        BEGIN { ORS = "" }
        {
            if (NR > 1) printf "%%0A"
            gsub(/%/, "%25")
            gsub(/\+/, "%2B")
            gsub(/&/, "%26")
            gsub(/=/, "%3D")
            gsub(/\r/, "%0D")
            gsub(/\t/, "%09")
            gsub(/ /, "+")
            printf "%s", $0
        }
    '
}

send_telegram_message() {
    message="$1"

    [ -n "${TG_BOT_TOKEN:-}" ] || return 0
    [ -n "${TG_CHAT_ID:-}" ] || return 0
    api_url="https://api.telegram.org/bot${TG_BOT_TOKEN}/sendMessage"

    if has_cmd curl; then
        if curl -fsS --connect-timeout 20 --max-time 45 --retry 2 \
            --data-urlencode "chat_id=${TG_CHAT_ID}" \
            --data-urlencode "text=${message}" \
            --data-urlencode "disable_web_page_preview=true" \
            "$api_url" >/dev/null 2>&1; then
            return 0
        fi
    fi
    if has_cmd wget; then
        post_data="chat_id=$(url_encode "$TG_CHAT_ID")&text=$(url_encode "$message")&disable_web_page_preview=true"
        if wget -qO- --post-data="$post_data" "$api_url" >/dev/null 2>&1; then
            return 0
        fi
    fi
    if has_cmd uclient-fetch; then
        post_data="chat_id=$(url_encode "$TG_CHAT_ID")&text=$(url_encode "$message")&disable_web_page_preview=true"
        if uclient-fetch -qO- --post-data="$post_data" "$api_url" >/dev/null 2>&1; then
            return 0
        fi
    fi
    if has_cmd busybox && busybox wget --help 2>&1 | grep -q -- '--post-data'; then
        post_data="chat_id=$(url_encode "$TG_CHAT_ID")&text=$(url_encode "$message")&disable_web_page_preview=true"
        if busybox wget -qO- --post-data="$post_data" "$api_url" >/dev/null 2>&1; then
            return 0
        fi
    fi
    append_log "WARN: Telegram notification failed or no compatible HTTP POST client was found"
    return 1
}

sanitize_file() {
    input_file="$1"
    max_lines="${2:-20}"
    max_columns="${3:-140}"

    if [ ! -r "$input_file" ]; then
        return 0
    fi
    awk -v secret="${NZ_CLIENT_SECRET:-}" -v uuid="${NZ_UUID:-}" -v bot_token="${TG_BOT_TOKEN:-}" '
        {
            line = $0
            if (length(secret) > 0) {
                while ((position = index(line, secret)) > 0) {
                    line = substr(line, 1, position - 1) "***" substr(line, position + length(secret))
                }
            }
            if (length(uuid) > 0) {
                while ((position = index(line, uuid)) > 0) {
                    line = substr(line, 1, position - 1) "***" substr(line, position + length(uuid))
                }
            }
            if (length(bot_token) > 0) {
                while ((position = index(line, bot_token)) > 0) {
                    line = substr(line, 1, position - 1) "***" substr(line, position + length(bot_token))
                }
            }
            print line
        }
    ' "$input_file" | sed \
        -e 's/\(NZ_CLIENT_SECRET[=:][[:space:]]*\)[^[:space:]]*/\1***/g' \
        -e 's/\([Cc]lient[_ -]*[Ss]ecret[=:][[:space:]]*\)[^[:space:]]*/\1***/g' \
        -e 's/\(NZ_UUID[=:][[:space:]]*\)[^[:space:]]*/\1***/g' \
        -e 's/\(TG_BOT_TOKEN[=:][[:space:]]*\)[^[:space:]]*/\1***/g' | \
        tail -n "$max_lines" | cut -c "1-$max_columns"
}

sanitize_log() {
    sanitize_file "$LOG_FILE" 20 140
}

notify_result() {
    status="$1"
    [ "$NOTIFICATION_SENT" = "0" ] || return 0
    NOTIFICATION_SENT=1
    [ -n "${TG_BOT_TOKEN:-}" ] && [ -n "${TG_CHAT_ID:-}" ] || return 0

    host_name="$(hostname 2>/dev/null || uname -n 2>/dev/null || printf unknown)"
    kernel="$(uname -sr 2>/dev/null || printf unknown)"
    details="$(sanitize_log)"
    message="Nezha Agent installation: ${status}
Host: ${host_name}
Kernel: ${kernel}
System: ${OS_DETAILS:-unknown}
uname -m: ${MACHINE:-unknown}
Package ABI: ${PACKAGE_ABI:-none}
Platform: ${DETECTED_OS:-unknown}
Architecture: ${DETECTED_ARCH:-unknown}
CPU details: ${CPU_DETAILS:-unknown}
Asset: ${ASSET_NAME:-not-selected}
Downloader: ${DOWNLOAD_TOOL:-not-used}
Install mode: ${INSTALL_MODE:-not-selected}
Binary: ${INSTALL_BINARY_PATH:-not-selected}
Config: ${INSTALL_CONFIG_PATH:-not-selected}
Keepalive: ${KEEPALIVE_METHOD:-not-selected}
Server: ${NZ_SERVER:-not-set}
TLS: ${NZ_TLS:-false}

Install log:
${details}"
    if ! send_telegram_message "$message"; then
        err "Telegram notification failed. Check network access, Bot Token, Chat ID, and HTTP POST support."
        return 1
    fi
    return 0
}

die() {
    err "$*"
    notify_result failed
    rm -f "$TEMP_BINARY"
    exit 1
}

detect_package_abi() {
    PACKAGE_ABI=""
    if has_cmd opkg; then
        PACKAGE_ABI="$(opkg print-architecture 2>/dev/null | awk '$1 == "arch" && $2 != "all" && $2 != "noarch" { value=$2 } END { print value }')"
    elif has_cmd apk; then
        PACKAGE_ABI="$(apk --print-arch 2>/dev/null || true)"
    elif has_cmd dpkg; then
        PACKAGE_ABI="$(dpkg --print-architecture 2>/dev/null || true)"
    elif has_cmd rpm; then
        PACKAGE_ABI="$(rpm --eval '%{_arch}' 2>/dev/null || true)"
    fi
}

elf_data_byte() {
    for elf_file in /bin/busybox /bin/sh /usr/bin/env; do
        if [ -r "$elf_file" ] && has_cmd dd && has_cmd od; then
            byte="$(dd if="$elf_file" bs=1 skip=5 count=1 2>/dev/null | od -An -tu1 2>/dev/null | tr -d '[:space:]')"
            case "$byte" in
                1|2) printf '%s\n' "$byte"; return 0 ;;
            esac
        fi
    done
    return 1
}

detect_mips_endian() {
    case "$PACKAGE_ABI" in
        mipsel*|mipsle*) printf '%s\n' little; return 0 ;;
        mips64el*|mips64le*) printf '%s\n' little; return 0 ;;
        mips_*|mips32*|mips64*) printf '%s\n' big; return 0 ;;
    esac

    data_byte="$(elf_data_byte 2>/dev/null || true)"
    case "$data_byte" in
        1) printf '%s\n' little; return 0 ;;
        2) printf '%s\n' big; return 0 ;;
    esac

    if has_cmd getconf; then
        byte_order="$(getconf BYTE_ORDER 2>/dev/null || true)"
        case "$byte_order" in
            1234|*LITTLE*|*little*) printf '%s\n' little; return 0 ;;
            4321|*BIG*|*big*) printf '%s\n' big; return 0 ;;
        esac
    fi
    return 1
}

arm_has_vfp() {
    cpu_features="$(sed -n 's/^[Ff]eatures[[:space:]]*:[[:space:]]*//p' "$CPUINFO_PATH" 2>/dev/null | head -n 1)"
    case " $cpu_features " in
        *" vfp "*|*" vfpv3 "*|*" vfpv4 "*) return 0 ;;
    esac
    case "$PACKAGE_ABI" in
        *hf*|arm_cortex-a*_vfp*) return 0 ;;
    esac
    return 1
}

select_asset() {
    os="$1"
    arch="$2"

    if [ "$NZ_RELEASE_TAG" != 'v2.3.5-repacked' ] || [ "$NZ_RELEASE_REPOSITORY" != 'nezha-rs/agent' ]; then
        return 1
    fi

    case "${os}:${arch}" in
        linux:amd64)
            ASSET_NAME='nezha-agent-v2.3.5-linux-x86-64-goamd64-v1-sse2-static-upx-best-lzma'
            ASSET_SIZE=5373956
            ASSET_SHA256='d943b727543ed11b0907547f66da371a532d32382728f5dedab26d34cfafc24a'
            ;;
        linux:386)
            ASSET_NAME='nezha-agent-v2.3.5-linux-x86-32-go386-sse2-static-upx-best-lzma'
            ASSET_SIZE=4667608
            ASSET_SHA256='3a906081c626ba3c07c47f6fd73856cec3e865880ecf3f0a39da15dc3cd2a8a2'
            ;;
        linux:arm5)
            ASSET_NAME='nezha-agent-v2.3.5-linux-arm-32-goarm5-software-float-no-vfp-static-custom-build-upx-best-lzma'
            ASSET_SIZE=4353796
            ASSET_SHA256='a6533ee04e6fe1cb24fc72a7c967ae043e991adddfe91e1ef61c882ddf8cd0d7'
            ;;
        linux:arm6)
            ASSET_NAME='nezha-agent-v2.3.5-linux-arm-32-goarm6-vfpv1-required-static-upx-best-lzma'
            ASSET_SIZE=4357692
            ASSET_SHA256='4ed8d95cd8f2e9f91782944dbbc1d837a875a00693a4fa195f83109d70a848ac'
            ;;
        linux:arm64)
            ASSET_NAME='nezha-agent-v2.3.5-linux-arm64-aarch64-goarm64-v8.0-static-upx-best-lzma'
            ASSET_SIZE=4397516
            ASSET_SHA256='ead85f5c0e30b73c399a58f3eb84208c9a8e1d60856a678a52cfbaa3fe3ff7d2'
            ;;
        linux:mips)
            ASSET_NAME='nezha-agent-v2.3.5-linux-mips-32-be-mips32r1-gomips-softfloat-static-upx-best-lzma'
            ASSET_SIZE=4254172
            ASSET_SHA256='18b1f214351ea5f1a8f262ed62a1e78d4866a51a43adbd4094c837b95ebffb40'
            ;;
        linux:mipsle)
            ASSET_NAME='nezha-agent-v2.3.5-linux-mips-32-le-mips32r1-gomips-softfloat-static-upx-best-lzma'
            ASSET_SIZE=4341232
            ASSET_SHA256='688c1f5e592bfdeffe7a2dbb795bcf0d4ec7b9654b5c6ce4c1ea323a65554a54'
            ;;
        linux:riscv64)
            ASSET_NAME='nezha-agent-v2.3.5-linux-riscv64-rva20u64-rv64imafd-static-upx-best-lzma'
            ASSET_SIZE=4736324
            ASSET_SHA256='4300dde63e8125068869b00ea8bbbbb324ba7622f7d29b299a782305e1387afa'
            ;;
        linux:s390x)
            ASSET_NAME='nezha-agent-v2.3.5-linux-s390x-z13-min-static-original-upx-unsupported'
            ASSET_SIZE=19071138
            ASSET_SHA256='7dc659216cc98b7fc3442e2140ff2bb9b9e525780d434f2d8853135d81386b9c'
            ;;
        linux:loong64)
            ASSET_NAME='nezha-agent-v2.3.5-linux-loongarch64-la364-min-static-original-upx-unsupported'
            ASSET_SIZE=18219170
            ASSET_SHA256='5b5e2a806963c91be4d88d8df2e57c6af0c5c635f2eeaf0d34fc55b088451eb8'
            ;;
        freebsd:amd64)
            ASSET_NAME='nezha-agent-v2.3.5-freebsd-x86-64-goamd64-v1-sse2-static-original-upx-unsupported'
            ASSET_SIZE=18026668
            ASSET_SHA256='86d10be9c0350c692a66179c20f3ecd03c912f6861fa9fc963aa341087044e96'
            ;;
        freebsd:386)
            ASSET_NAME='nezha-agent-v2.3.5-freebsd-x86-32-go386-sse2-static-original-upx-unsupported'
            ASSET_SIZE=16900268
            ASSET_SHA256='3b9b2b7daf66026b68e76038f13b383dabd12ed3be6846993b25c28392d27c8a'
            ;;
        freebsd:arm6)
            ASSET_NAME='nezha-agent-v2.3.5-freebsd-arm-32-goarm6-vfpv1-required-static-original-upx-unsupported'
            ASSET_SIZE=17039532
            ASSET_SHA256='f39d186c8ad45963d4acae9464686444f6604aa67751c7ad88a222d41cb35eeb'
            ;;
        freebsd:arm64)
            ASSET_NAME='nezha-agent-v2.3.5-freebsd-arm64-aarch64-goarm64-v8.0-static-original-upx-unsupported'
            ASSET_SIZE=16777388
            ASSET_SHA256='f79a2f8dbe78e0de74efd2ac65adc322302f19f69826021df0f3c7cf4a63aa93'
            ;;
        darwin:amd64)
            ASSET_NAME='nezha-agent-v2.3.5-darwin-x86-64-goamd64-v1-sse2-cgo0-macho'
            ASSET_SIZE=18726016
            ASSET_SHA256='ab7e439a8e07d7e1fbff72d658c9e46cf0e3483620e47e17e7fab87cfc992fdc'
            ;;
        darwin:arm64)
            ASSET_NAME='nezha-agent-v2.3.5-darwin-arm64-aarch64-goarm64-v8.0-cgo0-macho'
            ASSET_SIZE=17650370
            ASSET_SHA256='d7a13f0175c5bdc08077b54ab7a0bcc7ec20b4e9e2c3c0956e31827d294ca4f7'
            ;;
        *)
            return 1
            ;;
    esac
}

arm_rust_arch() {
    cpu_arch="$(sed -n 's/^CPU architecture[[:space:]]*:[[:space:]]*//p' "$CPUINFO_PATH" 2>/dev/null | head -n 1)"
    case "$cpu_arch" in
        7|8|9) if arm_has_vfp; then printf 'armv7_hardfloat\n'; else printf 'armv7_softfloat\n'; fi ;;
        *) if arm_has_vfp; then printf 'arm6\n'; else printf 'arm5\n'; fi ;;
    esac
}

# Rust Release asset selection is kept separate from the upstream Go selector.
select_rust_asset() {
    os="$1"
    arch="$2"
    case "$os:$arch" in
        linux:amd64) suffix=linux-x86-64-static-elf ;;
        linux:386) suffix=linux-x86-32-static-elf ;;
        linux:arm5) suffix=linux-armv5te-softfloat-static-elf ;;
        linux:arm6) suffix=linux-armv6-softfloat-static-elf ;;
        linux:armv7_softfloat) suffix=linux-armv7-cortex-a9-softfloat-no-vfp-static-elf ;;
        linux:armv7_hardfloat) suffix=linux-armv7-hardfloat-static-elf ;;
        linux:arm64) suffix=linux-arm64-static-elf ;;
        linux:mips) suffix=linux-mips32-be-softfloat-static-elf ;;
        linux:mipsle) suffix=linux-mips32-le-softfloat-static-elf ;;
        linux:mips64) suffix=linux-mips64-be-softfloat-static-elf ;;
        linux:mips64le) suffix=linux-mips64-le-softfloat-static-elf ;;
        linux:ppc) suffix=linux-ppc32-be-static-elf ;;
        linux:ppc64) suffix=linux-ppc64-be-static-elf ;;
        linux:ppc64le) suffix=linux-ppc64-le-static-elf ;;
        linux:s390x) suffix=linux-s390x-static-elf ;;
        openbsd:386) suffix=openbsd-x86-32-static-elf ;;
        openbsd:amd64) suffix=openbsd-x86-64-static-elf ;;
        openbsd:arm64) suffix=openbsd-arm64-static-elf ;;
        openbsd:arm5) suffix=openbsd-armv5te-eabi5-experimental-static-elf ;;
        openbsd:arm6) suffix=openbsd-armv6-eabi5-experimental-static-elf ;;
        openbsd:armv7_softfloat|openbsd:armv7_hardfloat) suffix=openbsd-armv7-eabi5-static-elf ;;
        *) return 1 ;;
    esac
    RUST_ORIGINAL_ASSET="nezha-agent-rust-${NZ_RELEASE_TAG}-${suffix}"
    ASSET_NAME="UPX-${RUST_ORIGINAL_ASSET}"
    # Actual release sizes are used only for free-space estimates.
    case "$ASSET_NAME" in
        *x86-64*) ASSET_SIZE=1563816 ;;
        *x86-32*) ASSET_SIZE=1418700 ;;
        *arm64*) ASSET_SIZE=1420380 ;;
        *armv5*) ASSET_SIZE=1207400 ;;
        *armv6*) ASSET_SIZE=1209624 ;;
        *armv7-cortex*) ASSET_SIZE=1182552 ;;
        *armv7-hardfloat*) ASSET_SIZE=1193240 ;;
        *mips32-be-hardfloat*) ASSET_SIZE=1481988 ;;
        *mips32-be-softfloat*) ASSET_SIZE=1485208 ;;
        *mips32-le-hardfloat*) ASSET_SIZE=1513292 ;;
        *mips32-le-softfloat*) ASSET_SIZE=1517364 ;;
        *ppc32*) ASSET_SIZE=1281888 ;;
        *ppc64-be*) ASSET_SIZE=1284104 ;;
        *ppc64-le*) ASSET_SIZE=1400876 ;;
        *) ASSET_SIZE=4194304 ;;
    esac
    return 0
}

detect_platform() {
    SYSTEM="$(uname -s 2>/dev/null || true)"
    MACHINE="$(uname -m 2>/dev/null || true)"
    BITS="$(getconf LONG_BIT 2>/dev/null || true)"
    detect_package_abi

    OS_DETAILS=""
    if [ -r /etc/openwrt_release ]; then
        OS_DETAILS="$(sed -n 's/^DISTRIB_DESCRIPTION=//p' /etc/openwrt_release 2>/dev/null | head -n 1 | sed "s/^['\"]//;s/['\"]$//")"
    elif [ -r /etc/os-release ]; then
        OS_DETAILS="$(sed -n 's/^PRETTY_NAME=//p' /etc/os-release 2>/dev/null | head -n 1 | sed 's/^"//;s/"$//')"
    fi
    [ -n "$OS_DETAILS" ] || OS_DETAILS="$SYSTEM"

    case "$SYSTEM" in
        Linux) DETECTED_OS=linux ;;
        FreeBSD) DETECTED_OS=freebsd ;;
        Darwin) DETECTED_OS=darwin ;;
        *) die "Unsupported operating system: ${SYSTEM:-unknown}" ;;
    esac

    if [ -n "${NZ_ARCH:-}" ]; then
        case "$NZ_ARCH" in
            amd64|386|arm5|arm6|armv7_softfloat|armv7_hardfloat|arm64|mips|mipsle|mips64|mips64le|riscv64|s390x|loong64|ppc|ppc64|ppc64le)
                DETECTED_ARCH="$NZ_ARCH"
                CPU_DETAILS="manual override NZ_ARCH=$NZ_ARCH"
                ;;
            arm)
                DETECTED_ARCH="$(arm_rust_arch)"
                CPU_DETAILS="manual ARM override; VFP auto-detected"
                ;;
            *) die "Unsupported NZ_ARCH override: $NZ_ARCH" ;;
        esac
    else
        case "$PACKAGE_ABI" in
            x86_64*|amd64) DETECTED_ARCH=amd64 ;;
            i386*|i486*|i586*|i686*|x86) DETECTED_ARCH=386 ;;
            aarch64*|arm64*) DETECTED_ARCH=arm64 ;;
            armv5*|arm_*soft*|armel*) DETECTED_ARCH=arm5 ;;
            armv6*|armv7*|arm_cortex-a*|armhf*)
                DETECTED_ARCH="$(arm_rust_arch)"
                ;;
            mipsel*|mipsle*) DETECTED_ARCH=mipsle ;;
            mips_*) DETECTED_ARCH=mips ;;
            mips64el*|mips64le*) DETECTED_ARCH=mips64le ;;
            mips64*) DETECTED_ARCH=mips64 ;;
            ppc64le*|powerpc64le*) DETECTED_ARCH=ppc64le ;;
            ppc64*|powerpc64*)
                if [ "${BITS:-64}" = 32 ]; then DETECTED_ARCH=ppc; else DETECTED_ARCH=ppc64; fi
                ;;
            ppc*|powerpc*) DETECTED_ARCH=ppc ;;
            riscv64*) DETECTED_ARCH=riscv64 ;;
            s390x*) DETECTED_ARCH=s390x ;;
            loongarch64*|loong64*) DETECTED_ARCH=loong64 ;;
            *) DETECTED_ARCH='' ;;
        esac

        if [ -z "$DETECTED_ARCH" ]; then
            case "$MACHINE" in
                x86_64|amd64) DETECTED_ARCH=amd64 ;;
                i386|i486|i586|i686|x86) DETECTED_ARCH=386 ;;
                aarch64|arm64|arm64v8*) DETECTED_ARCH=arm64 ;;
                armv5*) DETECTED_ARCH=arm5 ;;
                arm|armv6*|armv7*|armv8l)
                    DETECTED_ARCH="$(arm_rust_arch)"
                    ;;
                mipsel|mipsle) DETECTED_ARCH=mipsle ;;
                mipseb) DETECTED_ARCH=mips ;;
                mips)
                    mips_endian="$(detect_mips_endian 2>/dev/null || true)"
                    case "$mips_endian" in
                        little) DETECTED_ARCH=mipsle ;;
                        big) DETECTED_ARCH=mips ;;
                        *) die "Could not determine MIPS byte order; set NZ_ARCH=mipsle or NZ_ARCH=mips." ;;
                    esac
                    ;;
            mips64le) DETECTED_ARCH=mips64le ;;
            mips64) DETECTED_ARCH=mips64 ;;
                ppc64le|powerpc64le) DETECTED_ARCH=ppc64le ;;
                ppc64|powerpc64)
                    if [ "${BITS:-64}" = 32 ]; then DETECTED_ARCH=ppc; else DETECTED_ARCH=ppc64; fi
                    ;;
                ppc|powerpc) DETECTED_ARCH=ppc ;;
                riscv64) DETECTED_ARCH=riscv64 ;;
                s390x) DETECTED_ARCH=s390x ;;
                loongarch64|loong64) DETECTED_ARCH=loong64 ;;
                *) die "Unsupported architecture: uname -m=${MACHINE:-unknown}, package ABI=${PACKAGE_ABI:-none}." ;;
            esac
        fi

        case "$DETECTED_ARCH" in
            arm5|arm6)
                cpu_arch="$(sed -n 's/^CPU architecture[[:space:]]*:[[:space:]]*//p' "$CPUINFO_PATH" 2>/dev/null | head -n 1)"
                cpu_features="$(sed -n 's/^[Ff]eatures[[:space:]]*:[[:space:]]*//p' "$CPUINFO_PATH" 2>/dev/null | head -n 1)"
                CPU_DETAILS="ARMv${cpu_arch:-unknown}; features=${cpu_features:-unknown}; selected GOARM${DETECTED_ARCH#arm}"
                ;;
            mips|mipsle)
                CPU_DETAILS="MIPS32 softfloat; endian=${DETECTED_ARCH#mips}"
                [ "$DETECTED_ARCH" = mips ] && CPU_DETAILS="MIPS32 softfloat; endian=big"
                [ "$DETECTED_ARCH" = mipsle ] && CPU_DETAILS="MIPS32 softfloat; endian=little"
                ;;
            *) CPU_DETAILS="release baseline for $DETECTED_ARCH" ;;
        esac
    fi

    select_rust_asset "$DETECTED_OS" "$DETECTED_ARCH" || \
        die "No ${NZ_RELEASE_TAG} binary for ${DETECTED_OS}/${DETECTED_ARCH}."

    info "Detected OS: $SYSTEM -> $DETECTED_OS"
    info "System details: $OS_DETAILS"
    info "Detected machine: $MACHINE"
    info "Detected package ABI: ${PACKAGE_ABI:-none}"
    info "Selected architecture: $DETECTED_ARCH"
    info "CPU details: $CPU_DETAILS"
    info "Selected asset: $ASSET_NAME"
}

file_size() {
    wc -c < "$1" | tr -d '[:space:]'
}

required_space_kb() {
    required_bytes="$1"
    printf '%s\n' "$(( (required_bytes + 1048575) / 1024 + 1024 ))"
}

available_space_kb() {
    df -Pk "$1" 2>/dev/null | awk 'NR == 2 { print $4 }'
}

has_free_space() {
    path="$1"
    required_bytes="$2"
    available_kb="$(available_space_kb "$path")"
    [ -n "$available_kb" ] || return 1
    required_kb="$(required_space_kb "$required_bytes")"
    [ "$available_kb" -ge "$required_kb" ]
}

ensure_free_space() {
    path="$1"
    required_bytes="$2"
    available_kb="$(available_space_kb "$path")"
    [ -n "$available_kb" ] || return 0
    required_kb="$(required_space_kb "$required_bytes")"
    [ "$available_kb" -ge "$required_kb" ] || \
        die "Not enough free space at $path: need at least ${required_kb} KiB, have ${available_kb} KiB."
}

sha256_file() {
    file="$1"
    if has_cmd sha256sum; then
        sha256sum "$file" | awk '{print $1}'
    elif has_cmd shasum; then
        shasum -a 256 "$file" | awk '{print $1}'
    elif has_cmd openssl; then
        openssl dgst -sha256 "$file" | sed 's/^.*= //'
    elif has_cmd busybox && busybox sha256sum --help >/dev/null 2>&1; then
        busybox sha256sum "$file" | awk '{print $1}'
    else
        return 1
    fi
}

verify_binary_header() {
    file="$1"
    has_cmd od || return 0
    header="$(od -An -tx1 -N20 "$file" 2>/dev/null | tr -d ' \n')"

    case "$DETECTED_OS" in
        linux|freebsd)
            case "$header" in 7f454c46*) ;; *) return 1 ;; esac
            elf_class="$(printf '%s' "$header" | cut -c9-10)"
            elf_data="$(printf '%s' "$header" | cut -c11-12)"
            elf_machine="$(printf '%s' "$header" | cut -c37-40)"
            case "$DETECTED_ARCH" in
                amd64) [ "$elf_class:$elf_data:$elf_machine" = '02:01:3e00' ] ;;
                386) [ "$elf_class:$elf_data:$elf_machine" = '01:01:0300' ] ;;
                arm5|arm6) [ "$elf_class:$elf_data:$elf_machine" = '01:01:2800' ] ;;
                arm64) [ "$elf_class:$elf_data:$elf_machine" = '02:01:b700' ] ;;
                mipsle) [ "$elf_class:$elf_data:$elf_machine" = '01:01:0800' ] ;;
                mips) [ "$elf_class:$elf_data:$elf_machine" = '01:02:0008' ] ;;
                riscv64) [ "$elf_class:$elf_data:$elf_machine" = '02:01:f300' ] ;;
                s390x) [ "$elf_class:$elf_data:$elf_machine" = '02:02:0016' ] ;;
                loong64) [ "$elf_class:$elf_data:$elf_machine" = '02:01:0201' ] ;;
                *) return 1 ;;
            esac
            ;;
        darwin)
            case "$DETECTED_ARCH:$header" in
                amd64:cffaedfe*) return 0 ;;
                arm64:cffaedfe*) return 0 ;;
                *) return 1 ;;
            esac
            ;;
    esac
}

verify_download() {
    verify_binary_header "$TEMP_BINARY" || \
        die "Binary header does not match detected platform ${DETECTED_OS}/${DETECTED_ARCH}."

    chmod +x "$TEMP_BINARY" || die "Could not make the downloaded binary executable."
    version_output="$("$TEMP_BINARY" --version 2>&1)"
    version_exit=$?
    if [ "$version_exit" -ne 0 ]; then
        die "Downloaded binary failed its runtime test (exit $version_exit): $version_output"
    fi
    case "$version_output" in
        *' version 2.3.5'*) ;;
        *) die "Unexpected binary version output: $version_output" ;;
    esac
    info "Runtime test: $version_output"
}

verify_rust_download() {
    [ -s "$TEMP_BINARY" ] || \
        die "Downloaded Rust asset is empty."
    header="$(od -An -tx1 -N20 "$TEMP_BINARY" 2>/dev/null | tr -d '[:space:]')"
    case "$header" in 7f454c46*) ;; *) die "Rust asset is not an ELF executable." ;; esac
    class="$(printf '%s' "$header" | cut -c9-10)"
    data="$(printf '%s' "$header" | cut -c11-12)"
    machine="$(printf '%s' "$header" | cut -c37-40)"
    case "$DETECTED_ARCH" in
        386) [ "$class:$data:$machine" = 01:01:0300 ] ;;
        amd64) [ "$class:$data:$machine" = 02:01:3e00 ] ;;
        arm64) [ "$class:$data:$machine" = 02:01:b700 ] ;;
        arm*) [ "$class:$data:$machine" = 01:01:2800 ] ;;
        mips64le) [ "$class:$data:$machine" = 02:01:0800 ] ;;
        mips64) [ "$class:$data:$machine" = 02:02:0008 ] ;;
        mipsle) [ "$class:$data:$machine" = 01:01:0800 ] ;;
        mips) [ "$class:$data:$machine" = 01:02:0008 ] ;;
        ppc64le) [ "$class:$data:$machine" = 02:01:1500 ] ;;
        ppc64) [ "$class:$data:$machine" = 02:02:0015 ] ;;
        ppc) [ "$class:$data:$machine" = 01:02:0014 ] ;;
        s390x) [ "$class:$data:$machine" = 02:02:0016 ] ;;
        *) return 1 ;;
    esac || die "Rust ELF header does not match detected $DETECTED_ARCH."
    chmod +x "$TEMP_BINARY" || die "Could not make the Rust asset executable."
    version_output="$("$TEMP_BINARY" --version 2>&1)" || return 1
    case "$version_output" in
        *'nezha-agent-rust 2.1.0'*) ;;
        *) return 1 ;;
    esac
    # Keep the actual size only as a space-estimation hint for volatile mode;
    # it is not used to accept or reject a binary.
    ASSET_SIZE="$(file_size "$TEMP_BINARY")"
}

download_rust_asset() {
    download_url="${NZ_RELEASE_BASE}/${ASSET_NAME}"
    info "Downloading from: $download_url"
    rm -f "$TEMP_BINARY"
    ensure_free_space "${TMPDIR:-/tmp}" "$ASSET_SIZE"
    if download_to "$download_url" "$TEMP_BINARY" 2>/dev/null && verify_rust_download; then
        return 0
    fi
    case "$ASSET_NAME" in
        UPX-*)
            ASSET_NAME="$RUST_ORIGINAL_ASSET"
            download_url="${NZ_RELEASE_BASE}/${ASSET_NAME}"
            info "UPX asset unavailable or incompatible; falling back to $download_url"
            rm -f "$TEMP_BINARY"
            ensure_free_space "${TMPDIR:-/tmp}" "$ASSET_SIZE"
            download_to "$download_url" "$TEMP_BINARY" 2>/dev/null && verify_rust_download && return 0
            ;;
    esac
    die "Rust Agent download or runtime check failed: $download_url"
}

shell_quote() {
    printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"
}

yaml_quote() {
    printf "'%s'" "$(printf '%s' "$1" | sed "s/'/''/g")"
}

normalize_boolean() {
    value="$1"
    name="$2"
    case "$value" in
        true|TRUE|True|1|yes|YES|Yes|on|ON|On) printf '%s\n' true ;;
        false|FALSE|False|0|no|NO|No|off|OFF|Off|'') printf '%s\n' false ;;
        *) die "$name must be true or false, got: $value" ;;
    esac
}

generate_uuid() {
    if [ -r /proc/sys/kernel/random/uuid ]; then
        sed -n '1p' /proc/sys/kernel/random/uuid
        return 0
    fi
    if has_cmd uuidgen; then
        uuidgen
        return 0
    fi

    uuid_seed="${TMPDIR:-/tmp}/nezha-agent-uuid.$$"
    printf '%s:%s:%s\n' "$(date +%s 2>/dev/null || printf 0)" "$$" \
        "$(hostname 2>/dev/null || printf unknown)" > "$uuid_seed"
    uuid_hash="$(sha256_file "$uuid_seed" 2>/dev/null || true)"
    rm -f "$uuid_seed"
    [ -n "$uuid_hash" ] || return 1
    printf '%s-%s-%s-%s-%s\n' \
        "$(printf '%s' "$uuid_hash" | cut -c 1-8)" \
        "$(printf '%s' "$uuid_hash" | cut -c 9-12)" \
        "$(printf '%s' "$uuid_hash" | cut -c 13-16)" \
        "$(printf '%s' "$uuid_hash" | cut -c 17-20)" \
        "$(printf '%s' "$uuid_hash" | cut -c 21-32)"
}

copy_root_file() {
    source_file="$1"
    destination_file="$2"
    file_mode="$3"
    run_as_root cp -f "$source_file" "$destination_file" || return 1
    run_as_root chmod "$file_mode" "$destination_file" || return 1
}

write_volatile_config() {
    config_path="$1"
    config_dir="${config_path%/*}"
    [ "$config_dir" != "$config_path" ] || config_dir='.'
    config_temp="${TMPDIR:-/tmp}/nezha-agent-config.$$"
    uuid_value="${NZ_UUID:-}"
    [ -n "$uuid_value" ] || uuid_value="$(generate_uuid)"
    [ -n "$uuid_value" ] || die "Could not generate an agent UUID. Set NZ_UUID explicitly."

    tls_value="$(normalize_boolean "${NZ_TLS:-false}" NZ_TLS)"
    disable_auto_update_value="$(normalize_boolean "${NZ_DISABLE_AUTO_UPDATE:-true}" NZ_DISABLE_AUTO_UPDATE)"
    disable_force_update_value="$(normalize_boolean "${NZ_DISABLE_FORCE_UPDATE:-${DISABLE_FORCE_UPDATE:-false}}" NZ_DISABLE_FORCE_UPDATE)"
    disable_command_execute_value="$(normalize_boolean "${NZ_DISABLE_COMMAND_EXECUTE:-false}" NZ_DISABLE_COMMAND_EXECUTE)"
    skip_connection_count_value="$(normalize_boolean "${NZ_SKIP_CONNECTION_COUNT:-false}" NZ_SKIP_CONNECTION_COUNT)"

    {
        printf 'server: %s\n' "$(yaml_quote "$NZ_SERVER")"
        printf 'client_secret: %s\n' "$(yaml_quote "$NZ_CLIENT_SECRET")"
        printf 'uuid: %s\n' "$(yaml_quote "$uuid_value")"
        printf 'tls: %s\n' "$tls_value"
        printf 'disable_auto_update: %s\n' "$disable_auto_update_value"
        printf 'disable_force_update: %s\n' "$disable_force_update_value"
        printf 'disable_command_execute: %s\n' "$disable_command_execute_value"
        printf 'skip_connection_count: %s\n' "$skip_connection_count_value"
    } > "$config_temp" || die "Could not prepare volatile-mode configuration."

    run_as_root mkdir -p "$config_dir" || die "Could not create $config_dir."
    copy_root_file "$config_temp" "$config_path" 600 || die "Could not install $config_path."
    rm -f "$config_temp"
}

write_volatile_runtime_env() {
    env_path="$1"
    env_temp="${TMPDIR:-/tmp}/nezha-agent-runtime-env.$$"
    {
        printf 'NZ_RUNTIME_URL=%s\n' "$(shell_quote "$download_url")"
        printf 'NZ_RUNTIME_SIZE=%s\n' "$(shell_quote "$ASSET_SIZE")"
        printf 'NZ_RUNTIME_SHA256=%s\n' "$(shell_quote "")"
        printf 'NZ_RUNTIME_FALLBACK_URL=%s\n' "$(shell_quote "${NZ_RELEASE_BASE}/${RUST_ORIGINAL_ASSET}")"
        printf 'NZ_RUNTIME_DIR=%s\n' "$(shell_quote "$NZ_VOLATILE_RUNTIME_DIR")"
        printf 'NZ_RUNTIME_BINARY=%s\n' "$(shell_quote "$NZ_VOLATILE_RUNTIME_DIR/nezha-agent")"
        printf 'NZ_RUNTIME_CONFIG=%s\n' "$(shell_quote "$INSTALL_CONFIG_PATH")"
        printf 'NZ_RUNTIME_VERSION=%s\n' "$(shell_quote 'nezha-agent-rust 2.1.0')"
        printf 'NZ_RUNTIME_DOWNLOAD_TIMEOUT=%s\n' "$(shell_quote "$NZ_DOWNLOAD_TIMEOUT")"
        printf 'NZ_RUNTIME_INSECURE_TLS=%s\n' "$(shell_quote "$NZ_INSECURE_TLS")"
        printf 'NZ_KEEPALIVE_METHOD=%s\n' "$(shell_quote "$KEEPALIVE_METHOD")"
    } > "$env_temp" || die "Could not prepare volatile runtime metadata."
    copy_root_file "$env_temp" "$env_path" 600 || die "Could not install $env_path."
    rm -f "$env_temp"
}

write_volatile_runner() {
    runner_path="$1"
    runner_temp="${TMPDIR:-/tmp}/nezha-agent-runner.$$"
    cat > "$runner_temp" <<'RUNNEREOF'
#!/bin/sh

set -eu

RUNNER_DIR="${0%/*}"
[ "$RUNNER_DIR" != "$0" ] || RUNNER_DIR='.'
ENV_FILE="$RUNNER_DIR/runtime.env"
[ -r "$ENV_FILE" ] || {
    echo "missing runtime metadata: $ENV_FILE" >&2
    exit 1
}
. "$ENV_FILE"

has_cmd() {
    command -v "$1" >/dev/null 2>&1
}

tls_certificate_error() {
    log_path="$1"
    tail -n 40 "$log_path" 2>/dev/null | grep -Eiq \
        'certificate|issuer|unknown ca|verify failed|unable to verify|unable to get local issuer|self-signed|x509'
}

auto_insecure_tls() {
    [ "$NZ_RUNTIME_INSECURE_TLS" = auto ] || return 1
    tls_certificate_error "$1"
}

download_to() {
    url="$1"
    destination="$2"
    found=0
    download_log="${destination}.log"
    rm -f "$download_log"

    if has_cmd curl; then
        found=1
        rm -f "$destination"
        curl_args="-fL --connect-timeout 20 --max-time $NZ_RUNTIME_DOWNLOAD_TIMEOUT"
        [ "$NZ_RUNTIME_INSECURE_TLS" = 1 ] && curl_args="$curl_args --insecure"
        curl $curl_args \
            --retry 3 --retry-delay 2 -o "$destination" "$url" >> "$download_log" 2>&1 && { rm -f "$download_log"; return 0; }
        cat "$download_log" >&2
        if auto_insecure_tls "$download_log"; then
            echo 'certificate verification failed; retrying curl without certificate verification' >&2
            rm -f "$destination"
            curl_args="-fL --connect-timeout 20 --max-time $NZ_RUNTIME_DOWNLOAD_TIMEOUT --insecure"
            curl $curl_args \
                --retry 3 --retry-delay 2 -o "$destination" "$url" >> "$download_log" 2>&1 && { rm -f "$download_log"; return 0; }
            cat "$download_log" >&2
        fi
    fi
    if has_cmd wget; then
        found=1
        rm -f "$destination"
        if [ "$NZ_RUNTIME_INSECURE_TLS" = 1 ]; then
            wget --no-check-certificate -O "$destination" "$url" >> "$download_log" 2>&1 && { rm -f "$download_log"; return 0; }
        else
            wget -O "$destination" "$url" >> "$download_log" 2>&1 && { rm -f "$download_log"; return 0; }
        fi
        cat "$download_log" >&2
        if auto_insecure_tls "$download_log"; then
            echo 'certificate verification failed; retrying wget without certificate verification' >&2
            rm -f "$destination"
            wget --no-check-certificate -O "$destination" "$url" >> "$download_log" 2>&1 && { rm -f "$download_log"; return 0; }
            cat "$download_log" >&2
        fi
    fi
    if has_cmd uclient-fetch; then
        found=1
        rm -f "$destination"
        uclient-fetch -O "$destination" "$url" >> "$download_log" 2>&1 && { rm -f "$download_log"; return 0; }
        cat "$download_log" >&2
        if auto_insecure_tls "$download_log"; then
            echo 'certificate verification failed; retrying uclient-fetch without certificate verification' >&2
            rm -f "$destination"
            uclient-fetch --no-check-certificate -O "$destination" "$url" >> "$download_log" 2>&1 && { rm -f "$download_log"; return 0; }
            cat "$download_log" >&2
        fi
    fi
    if has_cmd busybox && busybox wget --help >/dev/null 2>&1; then
        found=1
        rm -f "$destination"
        busybox wget -O "$destination" "$url" >> "$download_log" 2>&1 && { rm -f "$download_log"; return 0; }
        cat "$download_log" >&2
        if auto_insecure_tls "$download_log" && busybox wget --help 2>&1 | grep -q -- '--no-check-certificate'; then
            echo 'certificate verification failed; retrying BusyBox wget without certificate verification' >&2
            rm -f "$destination"
            busybox wget --no-check-certificate -O "$destination" "$url" >> "$download_log" 2>&1 && { rm -f "$download_log"; return 0; }
            cat "$download_log" >&2
        fi
    fi
    rm -f "$download_log"
    [ "$found" -ne 0 ] || echo 'curl, wget, uclient-fetch, or BusyBox wget is required' >&2
    return 1
}

sha256_file() {
    file="$1"
    if has_cmd sha256sum; then
        sha256sum "$file" | awk '{print $1}'
    elif has_cmd shasum; then
        shasum -a 256 "$file" | awk '{print $1}'
    elif has_cmd openssl; then
        openssl dgst -sha256 "$file" | sed 's/^.*= //'
    elif has_cmd busybox && busybox sha256sum --help >/dev/null 2>&1; then
        busybox sha256sum "$file" | awk '{print $1}'
    else
        return 1
    fi
}

binary_ok() {
    binary="$1"
    [ -s "$binary" ] || return 1
    if [ -n "$NZ_RUNTIME_SHA256" ]; then
        actual_sha256="$(sha256_file "$binary" 2>/dev/null || true)"
        [ "$actual_sha256" = "$NZ_RUNTIME_SHA256" ] || return 1
    fi
    chmod 755 "$binary" || return 1
    version_output="$("$binary" --version 2>&1)" || return 1
    case "$version_output" in
        *"$NZ_RUNTIME_VERSION"*) return 0 ;;
        *) return 1 ;;
    esac
}

ensure_space() {
    available_kb="$(df -Pk "$NZ_RUNTIME_DIR" 2>/dev/null | awk 'NR == 2 { print $4 }')"
    required_kb=$(( (NZ_RUNTIME_SIZE + 1048575) / 1024 + 1024 ))
    [ -n "$available_kb" ] && [ "$available_kb" -ge "$required_kb" ]
}

ensure_binary() {
    if binary_ok "$NZ_RUNTIME_BINARY"; then
        return 0
    fi

    rm -f "$NZ_RUNTIME_BINARY"
    mkdir -p "$NZ_RUNTIME_DIR"
    ensure_space || {
        echo "not enough free space in $NZ_RUNTIME_DIR" >&2
        return 1
    }

    temporary_binary="${NZ_RUNTIME_BINARY}.download.$$"
    trap 'rm -f "$temporary_binary"' EXIT HUP INT TERM
    download_to "$NZ_RUNTIME_URL" "$temporary_binary" || {
        rm -f "$temporary_binary"
        download_to "$NZ_RUNTIME_FALLBACK_URL" "$temporary_binary" || return 1
    }
    binary_ok "$temporary_binary" || {
        echo 'downloaded Nezha Agent failed size or runtime verification' >&2
        return 1
    }
    mv -f "$temporary_binary" "$NZ_RUNTIME_BINARY"
    trap - EXIT HUP INT TERM
}

[ -r "$NZ_RUNTIME_CONFIG" ] || {
    echo "missing agent config: $NZ_RUNTIME_CONFIG" >&2
    exit 1
}
ensure_binary
exec "$NZ_RUNTIME_BINARY" -c "$NZ_RUNTIME_CONFIG"
RUNNEREOF
    copy_root_file "$runner_temp" "$runner_path" 700 || die "Could not install $runner_path."
    rm -f "$runner_temp"
}

write_volatile_supervisor() {
    supervisor_path="$1"
    supervisor_temp="${TMPDIR:-/tmp}/nezha-agent-supervisor.$$"
    runner_path="$NZ_VOLATILE_CONFIG_DIR/run.sh"
    {
        printf '%s\n' '#!/bin/sh' '' 'set -eu' ''
        printf 'while :; do\n    %s || true\n    sleep 5\ndone\n' "$(shell_quote "$runner_path")"
    } > "$supervisor_temp"
    copy_root_file "$supervisor_temp" "$supervisor_path" 700 || die "Could not install $supervisor_path."
    rm -f "$supervisor_temp"
}

detect_keepalive_method() {
    if [ -n "${NZ_INIT_SYSTEM:-}" ]; then
        case "$NZ_INIT_SYSTEM" in
            openwrt|systemd|openrc|sysv|cron) printf '%s\n' "$NZ_INIT_SYSTEM" ;;
            *) die "Unsupported NZ_INIT_SYSTEM: $NZ_INIT_SYSTEM" ;;
        esac
    elif [ -f /etc/openwrt_release ] || [ -x /sbin/procd ]; then
        printf '%s\n' openwrt
    elif has_cmd systemctl && [ -d /etc/systemd/system ]; then
        printf '%s\n' systemd
    elif has_cmd rc-service && has_cmd rc-update && [ -d /etc/init.d ]; then
        printf '%s\n' openrc
    elif [ -d /etc/init.d ]; then
        printf '%s\n' sysv
    elif has_cmd crontab; then
        printf '%s\n' cron
    else
        return 1
    fi
}

install_openwrt_keepalive() {
    service_path="$NZ_OPENWRT_INIT_DIR/nezha-agent"
    service_temp="${TMPDIR:-/tmp}/nezha-agent-openwrt.$$"
    runner_path="$NZ_VOLATILE_CONFIG_DIR/run.sh"
    run_as_root mkdir -p "$NZ_OPENWRT_INIT_DIR" || die "Could not create $NZ_OPENWRT_INIT_DIR."
    {
        printf '#!/bin/sh %s\n\n' "$NZ_OPENWRT_RC_COMMON"
        printf '%s\n' 'START=99' 'STOP=10' 'USE_PROCD=1' '' 'start_service() {'
        printf '%s\n' '    procd_open_instance'
        printf '    procd_set_param command %s\n' "$(shell_quote "$runner_path")"
        printf '%s\n' '    procd_set_param respawn 5 5 0' '    procd_close_instance' '}'
    } > "$service_temp"
    copy_root_file "$service_temp" "$service_path" 755 || die "Could not install $service_path."
    rm -f "$service_temp"
    run_as_root "$service_path" enable || die "Could not enable the OpenWrt nezha-agent service."
    if [ "$NZ_NO_START" != 1 ]; then
        run_as_root "$service_path" restart >/dev/null 2>&1 || \
            run_as_root "$service_path" start || die "Could not start the OpenWrt nezha-agent service."
    fi
}

install_systemd_keepalive() {
    service_path="$NZ_SYSTEMD_DIR/nezha-agent.service"
    service_temp="${TMPDIR:-/tmp}/nezha-agent-systemd.$$"
    runner_path="$NZ_VOLATILE_CONFIG_DIR/run.sh"
    run_as_root mkdir -p "$NZ_SYSTEMD_DIR" || die "Could not create $NZ_SYSTEMD_DIR."
    {
        printf '%s\n' '[Unit]' 'Description=Nezha monitoring agent' 'Wants=network-online.target' 'After=network-online.target' ''
        printf '%s\n' '[Service]' 'Type=simple' 'User=root'
        printf 'ExecStart=%s\n' "$runner_path"
        printf '%s\n' 'Restart=always' 'RestartSec=5' 'StartLimitIntervalSec=0' '' '[Install]' 'WantedBy=multi-user.target'
    } > "$service_temp"
    copy_root_file "$service_temp" "$service_path" 644 || die "Could not install $service_path."
    rm -f "$service_temp"
    run_as_root systemctl daemon-reload || die "systemctl daemon-reload failed."
    run_as_root systemctl enable nezha-agent || die "Could not enable the systemd nezha-agent service."
    if [ "$NZ_NO_START" != 1 ]; then
        run_as_root systemctl restart nezha-agent || die "Could not start the systemd nezha-agent service."
    fi
}

install_openrc_keepalive() {
    service_path="/etc/init.d/nezha-agent"
    service_temp="${TMPDIR:-/tmp}/nezha-agent-openrc.$$"
    runner_path="$NZ_VOLATILE_CONFIG_DIR/run.sh"
    {
        printf '%s\n' '#!/sbin/openrc-run' '' 'description="Nezha monitoring agent"'
        printf 'command=%s\n' "$(shell_quote "$runner_path")"
        printf '%s\n' 'command_background="no"' 'supervisor="supervise-daemon"' 'respawn_delay="5"' 'respawn_max="0"' '' 'depend() {' '    need net' '}'
    } > "$service_temp"
    copy_root_file "$service_temp" "$service_path" 755 || die "Could not install $service_path."
    rm -f "$service_temp"
    run_as_root rc-update add nezha-agent default || die "Could not enable the OpenRC nezha-agent service."
    if [ "$NZ_NO_START" != 1 ]; then
        run_as_root rc-service nezha-agent restart || run_as_root rc-service nezha-agent start || \
            die "Could not start the OpenRC nezha-agent service."
    fi
}

install_sysv_keepalive() {
    supervisor_path="$NZ_VOLATILE_CONFIG_DIR/supervise.sh"
    write_volatile_supervisor "$supervisor_path"
    service_path="/etc/init.d/nezha-agent"
    service_temp="${TMPDIR:-/tmp}/nezha-agent-sysv.$$"
    {
        printf '%s\n' '#!/bin/sh' '### BEGIN INIT INFO' '# Provides: nezha-agent' '# Required-Start: $network' '# Required-Stop: $network' '# Default-Start: 2 3 4 5' '# Default-Stop: 0 1 6' '# Short-Description: Nezha monitoring agent' '### END INIT INFO' ''
        printf 'PIDFILE=%s\n' "$(shell_quote '/var/run/nezha-agent.pid')"
        printf 'SUPERVISOR=%s\n\n' "$(shell_quote "$supervisor_path")"
        printf '%s\n' 'start() {' '    if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then return 0; fi' '    nohup "$SUPERVISOR" >/dev/null 2>&1 &' '    echo "$!" > "$PIDFILE"' '}' '' 'stop() {' '    if [ -f "$PIDFILE" ]; then kill "$(cat "$PIDFILE")" 2>/dev/null || true; rm -f "$PIDFILE"; fi' '    pkill -f /etc/nezha-agent/run.sh 2>/dev/null || true' '}' '' 'case "${1:-}" in' '    start) start ;;' '    stop) stop ;;' '    restart) stop; sleep 1; start ;;' '    *) echo "Usage: $0 {start|stop|restart}"; exit 1 ;;' 'esac'
    } > "$service_temp"
    copy_root_file "$service_temp" "$service_path" 755 || die "Could not install $service_path."
    rm -f "$service_temp"
    if has_cmd update-rc.d; then run_as_root update-rc.d nezha-agent defaults || die "Could not enable nezha-agent."; fi
    if has_cmd chkconfig; then run_as_root chkconfig nezha-agent on || die "Could not enable nezha-agent."; fi
    if [ "$NZ_NO_START" != 1 ]; then run_as_root "$service_path" restart || die "Could not start nezha-agent."; fi
}

install_cron_keepalive() {
    supervisor_path="$NZ_VOLATILE_CONFIG_DIR/supervise.sh"
    write_volatile_supervisor "$supervisor_path"
    cron_temp="${TMPDIR:-/tmp}/nezha-agent-cron.$$"
    (crontab -l 2>/dev/null | grep -v "$supervisor_path" || true; printf '@reboot %s >/dev/null 2>&1\n' "$supervisor_path") > "$cron_temp"
    crontab "$cron_temp" || die "Could not install the @reboot cron entry."
    rm -f "$cron_temp"
    if [ "$NZ_NO_START" != 1 ]; then nohup "$supervisor_path" >/dev/null 2>&1 & fi
}

install_volatile_keepalive() {
    case "$KEEPALIVE_METHOD" in
        openwrt) install_openwrt_keepalive ;;
        systemd) install_systemd_keepalive ;;
        openrc) install_openrc_keepalive ;;
        sysv) install_sysv_keepalive ;;
        cron) install_cron_keepalive ;;
        *) die "No supported boot-time service manager was detected." ;;
    esac
}

install_volatile_agent() {
    INSTALL_MODE='volatile (/tmp, re-downloaded after reboot)'
    INSTALL_CONFIG_PATH="$NZ_VOLATILE_CONFIG_DIR/config.yml"
    INSTALL_BINARY_PATH="$NZ_VOLATILE_RUNTIME_DIR/nezha-agent"
    KEEPALIVE_METHOD="$(detect_keepalive_method || true)"
    [ -n "$KEEPALIVE_METHOD" ] || die "No supported boot-time service manager was detected for volatile mode."

    info "Persistent storage cannot hold the binary; using volatile runtime mode."
    info "Persistent config directory: $NZ_VOLATILE_CONFIG_DIR"
    info "Volatile binary path: $INSTALL_BINARY_PATH"
    info "Boot keepalive method: $KEEPALIVE_METHOD"

    write_volatile_config "$INSTALL_CONFIG_PATH"
    write_volatile_runtime_env "$NZ_VOLATILE_CONFIG_DIR/runtime.env"
    write_volatile_runner "$NZ_VOLATILE_CONFIG_DIR/run.sh"

    run_as_root mkdir -p "$NZ_VOLATILE_RUNTIME_DIR" || die "Could not create $NZ_VOLATILE_RUNTIME_DIR."
    run_as_root rm -f "$INSTALL_BINARY_PATH"
    if ! run_as_root mv -f "$TEMP_BINARY" "$INSTALL_BINARY_PATH" 2>/dev/null; then
        run_as_root cp -f "$TEMP_BINARY" "$INSTALL_BINARY_PATH" || die "Could not install the volatile binary."
        rm -f "$TEMP_BINARY"
    fi
    run_as_root chmod 755 "$INSTALL_BINARY_PATH" || die "Could not make the volatile binary executable."
    install_volatile_keepalive

    success "Nezha Agent installed in volatile runtime mode."
    success "After each reboot, $NZ_VOLATILE_CONFIG_DIR/run.sh verifies or downloads the binary before starting it."
    notify_result success
    rm -f "$LOG_FILE"
}

choose_config_path() {
    path="$NZ_AGENT_PATH/config.yml"
    if [ -f "$path" ]; then
        if [ -r /dev/urandom ] && has_cmd od; then
            suffix="$(od -An -N3 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n')"
        else
            suffix="$$"
        fi
        path="$NZ_AGENT_PATH/config-${suffix}.yml"
    fi
    printf '%s\n' "$path"
}

install_agent() {
    [ -n "${NZ_SERVER:-}" ] || die "NZ_SERVER must not be empty."
    [ -n "${NZ_CLIENT_SECRET:-}" ] || die "NZ_CLIENT_SECRET must not be empty."

    detect_platform
    download_rust_asset

    run_as_root mkdir -p "$NZ_AGENT_PATH" || die "Could not create $NZ_AGENT_PATH."
    # The Rust binary's `service install` command manages systemd only. Use the
    # installer-managed runner for legacy init systems instead of failing after
    # the binary has already been downloaded and copied into place.
    KEEPALIVE_METHOD="$(detect_keepalive_method || true)"
    if [ "$NZ_FORCE_VOLATILE" = 1 ] || [ "$KEEPALIVE_METHOD" != systemd ] || \
        ! has_free_space "$NZ_AGENT_PATH" "$ASSET_SIZE"; then
        install_volatile_agent
        return 0
    fi

    INSTALL_MODE='persistent binary'
    target_binary="$NZ_AGENT_PATH/nezha-agent"
    INSTALL_BINARY_PATH="$target_binary"
    backup_binary="$NZ_AGENT_PATH/nezha-agent.backup"
    if run_as_root test -f "$target_binary"; then
        run_as_root cp -f "$target_binary" "$backup_binary" || die "Could not back up existing agent."
        info "Existing agent backed up to: $backup_binary"
    fi
    run_as_root cp -f "$TEMP_BINARY" "$target_binary" || die "Could not install the verified binary."
    run_as_root chmod 755 "$target_binary" || die "Could not set executable permissions."
    rm -f "$TEMP_BINARY"

    path="$(choose_config_path)"
    INSTALL_CONFIG_PATH="$path"
    run_as_root "$target_binary" service -c "$path" uninstall >/dev/null 2>&1 || true

    info "Installing service with config: $path"
    install_service() {
        if [ -n "${NZ_UUID:-}" ]; then
            run_as_root env \
                "NZ_UUID=$NZ_UUID" \
                "NZ_SERVER=$NZ_SERVER" \
                "NZ_CLIENT_SECRET=$NZ_CLIENT_SECRET" \
                "NZ_TLS=${NZ_TLS:-false}" \
                "NZ_DISABLE_AUTO_UPDATE=${NZ_DISABLE_AUTO_UPDATE:-true}" \
                "NZ_DISABLE_FORCE_UPDATE=${NZ_DISABLE_FORCE_UPDATE:-${DISABLE_FORCE_UPDATE:-false}}" \
                "NZ_DISABLE_COMMAND_EXECUTE=${NZ_DISABLE_COMMAND_EXECUTE:-false}" \
                "NZ_SKIP_CONNECTION_COUNT=${NZ_SKIP_CONNECTION_COUNT:-false}" \
                "$target_binary" service -c "$path" install
        else
            run_as_root env \
                "NZ_SERVER=$NZ_SERVER" \
                "NZ_CLIENT_SECRET=$NZ_CLIENT_SECRET" \
                "NZ_TLS=${NZ_TLS:-false}" \
                "NZ_DISABLE_AUTO_UPDATE=${NZ_DISABLE_AUTO_UPDATE:-true}" \
                "NZ_DISABLE_FORCE_UPDATE=${NZ_DISABLE_FORCE_UPDATE:-${DISABLE_FORCE_UPDATE:-false}}" \
                "NZ_DISABLE_COMMAND_EXECUTE=${NZ_DISABLE_COMMAND_EXECUTE:-false}" \
                "NZ_SKIP_CONNECTION_COUNT=${NZ_SKIP_CONNECTION_COUNT:-false}" \
                "$target_binary" service -c "$path" install
        fi
    }
    if ! install_service >> "$LOG_FILE" 2>&1; then
        run_as_root "$target_binary" service -c "$path" uninstall >/dev/null 2>&1 || true
        if run_as_root test -f "$backup_binary"; then
            run_as_root cp -f "$backup_binary" "$target_binary" || true
        fi
        die "Nezha Agent service installation failed. See the log included in the Telegram notification."
    fi

    success "Nezha Agent installed successfully."
    notify_result success
    rm -f "$LOG_FILE"
}

check_download() {
    detect_platform
    download_rust_asset
    success "Download verification completed; no system files were changed."
    cleanup
    rm -f "$LOG_FILE"
}

collect_debug_context() {
    SYSTEM="$(uname -s 2>/dev/null || printf unknown)"
    MACHINE="$(uname -m 2>/dev/null || printf unknown)"
    detect_package_abi

    OS_DETAILS=""
    if [ -r /etc/openwrt_release ]; then
        OS_DETAILS="$(sed -n 's/^DISTRIB_DESCRIPTION=//p' /etc/openwrt_release 2>/dev/null | head -n 1 | sed "s/^['\"]//;s/['\"]$//")"
    elif [ -r /etc/os-release ]; then
        OS_DETAILS="$(sed -n 's/^PRETTY_NAME=//p' /etc/os-release 2>/dev/null | head -n 1 | sed 's/^"//;s/"$//')"
    fi
    [ -n "$OS_DETAILS" ] || OS_DETAILS="$SYSTEM"

    DEBUG_CPU_DETAILS="$(sed -n \
        -e 's/^system type[[:space:]]*:[[:space:]]*/system=/p' \
        -e 's/^model name[[:space:]]*:[[:space:]]*/model=/p' \
        -e 's/^CPU architecture[[:space:]]*:[[:space:]]*/architecture=/p' \
        -e 's/^[Ff]eatures[[:space:]]*:[[:space:]]*/features=/p' \
        "$CPUINFO_PATH" 2>/dev/null | head -n 4 | tr '\n' ';' | cut -c 1-300)"
    [ -n "$DEBUG_CPU_DETAILS" ] || DEBUG_CPU_DETAILS="not available"
}

run_debug_command() {
    debug_command="$1"

    [ -n "$debug_command" ] || {
        err "Debug command must not be empty."
        return 2
    }
    [ -n "${TG_BOT_TOKEN:-}" ] || {
        err "TG_BOT_TOKEN is required in command debug mode."
        return 2
    }
    [ -n "${TG_CHAT_ID:-}" ] || {
        err "TG_CHAT_ID is required in command debug mode."
        return 2
    }

    collect_debug_context
    printf '%s\n' "$debug_command" > "$DEBUG_COMMAND_FILE"
    info "Command debug mode: no installation or binary download will be performed."
    info "Executing: $debug_command"

    sh -c "$debug_command" > "$DEBUG_OUTPUT" 2>&1
    command_status=$?

    if [ -s "$DEBUG_OUTPUT" ]; then
        cat "$DEBUG_OUTPUT"
    else
        printf '%s\n' '(command produced no output)'
    fi

    command_for_message="$(sanitize_file "$DEBUG_COMMAND_FILE" 3 300)"
    output_for_message="$(sanitize_file "$DEBUG_OUTPUT" 20 120)"
    [ -n "$output_for_message" ] || output_for_message="(no output)"
    host_name="$(hostname 2>/dev/null || uname -n 2>/dev/null || printf unknown)"
    kernel="$(uname -sr 2>/dev/null || printf unknown)"
    message="Nezha command debug
Host: ${host_name}
Kernel: ${kernel}
System: ${OS_DETAILS}
uname -m: ${MACHINE}
Package ABI: ${PACKAGE_ABI:-none}
CPU: ${DEBUG_CPU_DETAILS}
Command: ${command_for_message}
Exit code: ${command_status}

Output (last 20 lines):
${output_for_message}"

    if send_telegram_message "$message"; then
        success "Telegram command result notification sent (exit code: $command_status)."
    else
        err "Telegram command result notification failed."
        [ "$command_status" -ne 0 ] && return "$command_status"
        return 1
    fi

    return "$command_status"
}

uninstall_agent() {
    found=0
    for file in "$NZ_AGENT_PATH"/config*.yml; do
        [ -f "$file" ] || continue
        found=1
        run_as_root "$NZ_AGENT_PATH/nezha-agent" service -c "$file" uninstall || true
        run_as_root rm -f "$file" || true
    done
    [ "$found" = 1 ] || info "No installed configuration files were found."
    success "Uninstallation completed."
}

: > "$LOG_FILE" 2>/dev/null || LOG_FILE="/dev/null"
trap cleanup EXIT
trap 'cleanup; exit 1' HUP INT TERM

case "${1:-}" in
    uninstall)
        uninstall_agent
        ;;
    --detect|detect)
        detect_platform
        info "Asset size: $ASSET_SIZE bytes"
        ;;
    --check-download)
        check_download
        ;;
    --debug-command)
        shift
        [ "$#" -gt 0 ] || {
            err "Usage: sh agent.sh --debug-command 'command'"
            exit 2
        }
        run_debug_command "$*"
        exit $?
        ;;
    --debug-command=*)
        debug_command_value="${1#--debug-command=}"
        run_debug_command "$debug_command_value"
        exit $?
        ;;
    --help|-h)
        cat <<'EOF'
Usage:
  env NZ_SERVER=host:port NZ_CLIENT_SECRET=secret [NZ_TLS=false] sh agent.sh
  sh agent.sh --detect
  sh agent.sh --check-download
  env TG_BOT_TOKEN=token TG_CHAT_ID=chat sh agent.sh --debug-command 'command'
  sh agent.sh uninstall

Optional environment variables:
  NZ_UUID, NZ_ARCH, NZ_BASE_PATH, NZ_AGENT_PATH
  NZ_DISABLE_AUTO_UPDATE, NZ_DISABLE_FORCE_UPDATE
  NZ_DISABLE_COMMAND_EXECUTE, NZ_SKIP_CONNECTION_COUNT
  NZ_FORCE_VOLATILE=1, NZ_NO_START=1, NZ_INIT_SYSTEM
  NZ_VOLATILE_CONFIG_DIR, NZ_VOLATILE_RUNTIME_DIR
  TG_BOT_TOKEN, TG_CHAT_ID

Command debug mode executes the trusted command through /bin/sh -c, sends its
combined output and exit code to Telegram, and does not install Nezha Agent.

If persistent storage cannot hold the verified binary, the installer keeps
small configuration and runner files persistently, stores the binary under
/tmp, and downloads plus verifies it automatically after every reboot.

NZ_ARCH values: amd64, 386, arm5, arm6, arm64, mips, mipsle,
                riscv64, s390x, loong64
EOF
        ;;
    '')
        install_agent
        ;;
    *)
        die "Unknown option: $1"
        ;;
esac

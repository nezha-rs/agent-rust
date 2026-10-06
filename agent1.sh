#!/usr/bin/env sh

# Nezha Agent Rust v2.1.0 installer.
# Installs Linux and OpenBSD binaries published by nezha-rs/agent-rust.

NZ_PATH_EXPLICIT="${NZ_AGENT_PATH:+1}${NZ_BASE_PATH:+1}"
NZ_BASE_PATH="${NZ_BASE_PATH:-/opt/nezha-rust}"
NZ_AGENT_PATH="${NZ_AGENT_PATH:-${NZ_BASE_PATH}/agent}"
NZ_RELEASE_TAG='v2.1.0'
NZ_RELEASE_REPOSITORY='nezha-rs/agent-rust'
NZ_RELEASE_BASE="${NZ_RELEASE_BASE:-https://github.com/${NZ_RELEASE_REPOSITORY}/releases/download/${NZ_RELEASE_TAG}}"
NZ_DOWNLOAD_TIMEOUT="${NZ_DOWNLOAD_TIMEOUT:-180}"
# 0 keeps certificate verification strict, 1 always skips it, and auto retries
# without verification only after a certificate-chain error.
NZ_INSECURE_TLS="${NZ_INSECURE_TLS:-auto}"
CPUINFO_PATH="${NZ_CPUINFO_PATH:-/proc/cpuinfo}"
NZ_VOLATILE_CONFIG_DIR="${NZ_VOLATILE_CONFIG_DIR:-/etc/nezha-agent}"
NZ_VOLATILE_RUNTIME_DIR="${NZ_VOLATILE_RUNTIME_DIR:-/tmp/nezha-agent}"
NZ_OPENWRT_INIT_DIR="${NZ_OPENWRT_INIT_DIR:-/etc/init.d}"
NZ_OPENWRT_RC_COMMON="${NZ_OPENWRT_RC_COMMON:-/etc/rc.common}"
NZ_INIT_DIR="${NZ_INIT_DIR:-/etc/init.d}"
NZ_SYSTEMD_DIR="${NZ_SYSTEMD_DIR:-/etc/systemd/system}"
NZ_BUSYBOX_RCS_PATH="${NZ_BUSYBOX_RCS_PATH:-}"
NZ_FORCE_VOLATILE="${NZ_FORCE_VOLATILE:-0}"
NZ_NO_START="${NZ_NO_START:-0}"
NZ_INSTALL_REGISTRY="${NZ_INSTALL_REGISTRY:-/etc/nezha-agent/installations}"

red='\033[0;31m'
green='\033[0;32m'
yellow='\033[0;33m'
plain='\033[0m'

LOG_FILE="/dev/null"
TEMP_BINARY=""
DEBUG_OUTPUT="${TMPDIR:-/tmp}/nezha-agent-debug.$$.log"
DEBUG_COMMAND_FILE="${TMPDIR:-/tmp}/nezha-agent-debug-command.$$"
NOTIFICATION_SENT=0
DOWNLOAD_TOOL=""
INSTALL_MODE="not-selected"
INSTALL_BINARY_PATH=""
INSTALL_CONFIG_PATH=""
KEEPALIVE_METHOD=""
KEEPALIVE_DIR=""
STORAGE_MODE=""
INSTALL_BACKUP_PATH=""
RUNTIME_ALLOW_DOWNLOAD=""
NATIVE_SYSTEMD_MIGRATION=0
WORK_DIR=""
DOWNLOAD_DIR=""
EXISTING_DIR=""
BUSYBOX_PATH="${NZ_BUSYBOX_PATH:-}"
BUSYBOX_APPLETS=""
BUSYBOX_FALLBACK_MISSING="${BUSYBOX_FALLBACK_MISSING:-}"

cleanup() {
    [ -z "$DOWNLOAD_DIR" ] || run_as_root rm -rf "$DOWNLOAD_DIR"
    [ -z "$WORK_DIR" ] || rm -rf "$WORK_DIR"
    [ -z "$TEMP_BINARY" ] || rm -f "$TEMP_BINARY"
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

TOOL_COMPATIBILITY_CODE='has_cmd() {
    command -v "$1" >/dev/null 2>&1
}

compatibility_quote() (
    quote_value="$1"
    printf "'\''"
    while :; do
        case "$quote_value" in
            *"'\''"*)
                printf "%s'\''\\\\'\'''\''" "${quote_value%%\'\''*}"
                quote_value="${quote_value#*\'\''}" ;;
            *) printf "%s'\''" "$quote_value"; break ;;
        esac
    done
)

find_busybox() {
    BUSYBOX_PATH="$(
        for compat_candidate in "${NZ_BUSYBOX_PATH:-${BUSYBOX_PATH:-}}" \
            "$(command -v busybox 2>/dev/null || true)" \
            /bin/busybox /sbin/busybox /usr/bin/busybox /usr/sbin/busybox; do
            [ -n "$compat_candidate" ] && [ -x "$compat_candidate" ] || continue
            case "$compat_candidate" in
                /*) ;;
                */*) compat_candidate="$(cd "${compat_candidate%/*}" && pwd -P)/${compat_candidate##*/}" || continue ;;
                *) continue ;;
            esac
            printf '\''%s\n'\'' "$compat_candidate"
            exit 0
        done
        exit 1
    )" || return 1
}

busybox_has_applet() {
    case " $BUSYBOX_APPLETS " in
        *" $1 "*) return 0 ;;
        *) return 1 ;;
    esac
}

busybox_fallback_needed() {
    case " ${BUSYBOX_FALLBACK_MISSING:-} " in
        *" $1 "*) return 0 ;;
        *) return 1 ;;
    esac
}

busybox_run_applet() {
    "$BUSYBOX_PATH" "$@"
}

setup_busybox_fallbacks() {
    for compat_applet in ${BUSYBOX_FALLBACK_MISSING:-}; do
        unset -f "$compat_applet"
    done
    BUSYBOX_FALLBACK_MISSING='\'''\''
    BUSYBOX_APPLETS='\'''\''
    find_busybox || return 0
    BUSYBOX_APPLETS="$(
        compat_listing="$("$BUSYBOX_PATH" --list 2>/dev/null)" || exit 1
        IFS='\'' 	
'\''
        set -f
        for compat_item in $compat_listing; do printf '\''%s '\'' "$compat_item"; done
    )" || BUSYBOX_APPLETS='\'''\''
    for compat_applet in awk sed grep cut tr head tail wc df mktemp mkdir rmdir cp mv rm chmod chown cat env od hexdump dd getconf uname id find hostname date sleep kill ps nohup stat sha256sum md5sum wget crontab sh readlink dirname basename ls sort uniq xargs expr touch ln tee du timeout; do
        busybox_has_applet "$compat_applet" || continue
        command -v "$compat_applet" >/dev/null 2>&1 && continue
        BUSYBOX_FALLBACK_MISSING="$BUSYBOX_FALLBACK_MISSING $compat_applet"
        eval "$compat_applet() { busybox_run_applet $compat_applet \"\$@\"; }"
    done
}

compatibility_bootstrap() {
    printf '\''TOOL_COMPATIBILITY_CODE=%s\n'\'' "$(compatibility_quote "$TOOL_COMPATIBILITY_CODE")"
    printf '\''NZ_BUSYBOX_PATH=%s\n'\'' "$(compatibility_quote "${BUSYBOX_PATH:-}")"
    printf '\''%s\n'\'' '\''eval "$TOOL_COMPATIBILITY_CODE"'\'' '\''setup_busybox_fallbacks'\''
}

compatibility_shell() (
    if [ "${1:-}" = -c ] && [ "$#" -ge 2 ]; then
        shift
        compat_script="$1"
        shift
        set -- -c "$(compatibility_bootstrap)
$compat_script" "$@"
    fi
    if busybox_fallback_needed sh; then
        "$BUSYBOX_PATH" sh "$@"
    else
        "$(command -v sh)" "$@"
    fi
)

compatibility_env() (
    compat_env_prefix='\'''\''
    while [ "$#" -gt 0 ]; do
        case "$1" in
            -i|--ignore-environment|*=*)
                compat_env_prefix="$compat_env_prefix $(compatibility_quote "$1")"
                shift ;;
            -u|--unset)
                [ "$#" -ge 2 ] || return 2
                compat_env_prefix="$compat_env_prefix $(compatibility_quote "$1") $(compatibility_quote "$2")"
                shift 2 ;;
            --)
                compat_env_prefix="$compat_env_prefix --"
                shift
                break ;;
            *) break ;;
        esac
    done
    if [ "$#" -ge 3 ] && [ "$1" = sh ] && [ "$2" = -c ]; then
        shift 2
        compat_env_script="$1"
        shift
        set -- sh -c "$(compatibility_bootstrap)
$compat_env_script" "$@"
    fi
    if [ "$#" -gt 0 ] && busybox_fallback_needed "$1"; then
        set -- "$BUSYBOX_PATH" "$@"
    fi
    eval "env $compat_env_prefix \"\$@\""
)'
eval "$TOOL_COMPATIBILITY_CODE"
setup_busybox_fallbacks

run_as_root() (
    case "$1" in
        sh) shift; set -- compatibility_shell "$@" ;;
        env) shift; set -- compatibility_env "$@" ;;
    esac
    if [ "$(id -u 2>/dev/null || printf 1)" = 0 ]; then
        "$@"
    elif has_cmd sudo; then
        if busybox_fallback_needed "$1"; then
            sudo "$BUSYBOX_PATH" "$@"
        else
            root_command="$(command -v "$1" 2>/dev/null || true)"
            case "$root_command" in
                /*) shift; sudo "$root_command" "$@" ;;
                *)
                    root_bootstrap="$(compatibility_bootstrap)
\"\$@\""
                    if busybox_fallback_needed sh; then
                        sudo "$BUSYBOX_PATH" sh -c "$root_bootstrap" sh "$@"
                    else
                        sudo "$(command -v sh)" -c "$root_bootstrap" sh "$@"
                    fi ;;
            esac
        fi
    else
        err "Root privileges are required and sudo is not installed."
        return 1
    fi
)

require_tools() {
    missing_tools=''
    for required_tool in "$@"; do
        has_cmd "$required_tool" || missing_tools="$missing_tools $required_tool"
    done
    [ -z "$missing_tools" ] || die "Missing required tools:$missing_tools. Native tools or matching BusyBox applets are required; NZ_BUSYBOX_PATH can specify BusyBox."
}

require_install_tools() {
    require_tools awk sed grep cut tr head tail wc df mktemp mkdir rmdir cp mv rm chmod chown cat env uname id sh
    if ! has_cmd od && ! has_cmd hexdump; then
        die "Binary inspection requires od or hexdump, native or a BusyBox applet."
    fi
}

require_download_tool() {
    if ! has_cmd curl && ! has_cmd wget && ! has_cmd uclient-fetch && ! busybox_has_applet wget; then
        die "Downloading requires curl, wget, uclient-fetch or BusyBox wget with HTTPS support."
    fi
}

emit_tool_compatibility() {
    compatibility_bootstrap
    printf '%s\n' "NZ_TOOL_COMPATIBILITY_VERSION='1'"
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
    require_download_tool
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
            wget -T "$NZ_DOWNLOAD_TIMEOUT" --no-check-certificate -O "$destination" "$url" >> "$LOG_FILE" 2>&1 && return 0
        else
            wget -T "$NZ_DOWNLOAD_TIMEOUT" -O "$destination" "$url" >> "$LOG_FILE" 2>&1 && return 0
        fi
        if auto_insecure_tls; then
            append_log "WARN: certificate verification failed; retrying wget without certificate verification"
            rm -f "$destination"
            wget -T "$NZ_DOWNLOAD_TIMEOUT" --no-check-certificate -O "$destination" "$url" >> "$LOG_FILE" 2>&1 && return 0
        fi
        append_log "WARN: wget download failed; trying another available client"
    fi
    if has_cmd uclient-fetch; then
        client_found=1
        DOWNLOAD_TOOL="uclient-fetch"
        rm -f "$destination"
        uclient-fetch -T "$NZ_DOWNLOAD_TIMEOUT" -O "$destination" "$url" >> "$LOG_FILE" 2>&1 && return 0
        if auto_insecure_tls; then
            append_log "WARN: certificate verification failed; retrying uclient-fetch without certificate verification"
            rm -f "$destination"
            uclient-fetch -T "$NZ_DOWNLOAD_TIMEOUT" --no-check-certificate -O "$destination" "$url" >> "$LOG_FILE" 2>&1 && return 0
        fi
        append_log "WARN: uclient-fetch download failed; trying BusyBox wget"
    fi
    if busybox_has_applet wget; then
        client_found=1
        DOWNLOAD_TOOL="busybox wget"
        rm -f "$destination"
        "$BUSYBOX_PATH" wget -T "$NZ_DOWNLOAD_TIMEOUT" -O "$destination" "$url" >> "$LOG_FILE" 2>&1 && return 0
        if auto_insecure_tls && "$BUSYBOX_PATH" wget --help 2>&1 | grep -q -- '--no-check-certificate'; then
            append_log "WARN: certificate verification failed; retrying BusyBox wget without certificate verification"
            rm -f "$destination"
            "$BUSYBOX_PATH" wget -T "$NZ_DOWNLOAD_TIMEOUT" --no-check-certificate -O "$destination" "$url" >> "$LOG_FILE" 2>&1 && return 0
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
    if busybox_has_applet wget && "$BUSYBOX_PATH" wget --help 2>&1 | grep -q -- '--post-data'; then
        post_data="chat_id=$(url_encode "$TG_CHAT_ID")&text=$(url_encode "$message")&disable_web_page_preview=true"
        if "$BUSYBOX_PATH" wget -qO- --post-data="$post_data" "$api_url" >/dev/null 2>&1; then
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
    NZ_SANITIZE_SECRET="${NZ_CLIENT_SECRET:-}" NZ_SANITIZE_UUID="${NZ_UUID:-}" \
        NZ_SANITIZE_BOT_TOKEN="${TG_BOT_TOKEN:-}" awk '
        function redact(line, key,    position, result) {
            if (length(key) == 0) return line
            result = ""
            while ((position = index(line, key)) > 0) {
                result = result substr(line, 1, position - 1) "***"
                line = substr(line, position + length(key))
            }
            return result line
        }
        {
            line = redact($0, ENVIRON["NZ_SANITIZE_SECRET"])
            line = redact(line, ENVIRON["NZ_SANITIZE_UUID"])
            line = redact(line, ENVIRON["NZ_SANITIZE_BOT_TOKEN"])
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
        OpenBSD) DETECTED_OS=openbsd ;;
        *) die "Unsupported operating system: ${SYSTEM:-unknown}" ;;
    esac

    if [ -n "${NZ_ARCH:-}" ]; then
        case "$NZ_ARCH" in
            amd64|386|arm5|arm6|armv7_softfloat|armv7_hardfloat|arm64|mips|mipsle|mips64|mips64le|s390x|ppc|ppc64|ppc64le)
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
                mips64el|mips64le) DETECTED_ARCH=mips64le ;;
                mips64eb) DETECTED_ARCH=mips64 ;;
                mips64)
                    mips_endian="$(detect_mips_endian 2>/dev/null || true)"
                    case "$mips_endian" in
                        little) DETECTED_ARCH=mips64le ;;
                        big) DETECTED_ARCH=mips64 ;;
                        *) die "Could not determine MIPS64 byte order; set NZ_ARCH=mips64le or NZ_ARCH=mips64." ;;
                    esac
                    ;;
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
    [ -n "$available_kb" ] || die "Could not determine free space at $path."
    required_kb="$(required_space_kb "$required_bytes")"
    [ "$available_kb" -ge "$required_kb" ] || \
        die "Not enough free space at $path: need at least ${required_kb} KiB, have ${available_kb} KiB."
}

initialize_workspace() {
    workspace_parent=''
    for candidate in "${TMPDIR:-/tmp}" /var/tmp /tmp \
        "$NZ_AGENT_PATH" "$NZ_VOLATILE_CONFIG_DIR" /var/lib/nezha-agent; do
        [ -d "$candidate" ] && [ -w "$candidate" ] || continue
        has_free_space "$candidate" 65536 || continue
        WORK_DIR="$(mktemp -d "$candidate/nezha-installer.XXXXXX" 2>/dev/null)" || continue
        workspace_parent="$candidate"
        break
    done
    [ -n "$workspace_parent" ] || die "No writable workspace with enough free space. Set TMPDIR to a writable directory."
    TMPDIR="$WORK_DIR"
    LOG_FILE="$WORK_DIR/install.log"
    DEBUG_OUTPUT="$WORK_DIR/debug.log"
    DEBUG_COMMAND_FILE="$WORK_DIR/debug-command"
    : > "$LOG_FILE" || die "Could not create installer log."
    info "Installer workspace: $WORK_DIR"
}

prepare_download_directory() {
    download_parent="$1"
    run_as_root mkdir -p "$download_parent" || die "Could not create download directory $download_parent."
    DOWNLOAD_DIR="$(run_as_root mktemp -d "$download_parent/.nezha-download.XXXXXX")" ||
        die "Could not create download staging directory at $download_parent."
    if [ "$(id -u)" != 0 ]; then
        run_as_root chown "$(id -u):$(id -g)" "$DOWNLOAD_DIR" || die "Could not grant access to download staging directory."
    fi
    [ -w "$DOWNLOAD_DIR" ] || die "Download directory is not accessible. Run the installer as root."
    TEMP_BINARY="$DOWNLOAD_DIR/nezha-agent"
    info "Binary download staging: $DOWNLOAD_DIR (same filesystem as final binary)"
}

install_staged_binary() {
    staged_target="$1"
    [ "${DOWNLOAD_DIR%/*}" = "${staged_target%/*}" ] || die "Binary staging directory does not match installation target."
    run_as_root chown 0:0 "$TEMP_BINARY" && run_as_root chmod 755 "$TEMP_BINARY" &&
        run_as_root mv -f "$TEMP_BINARY" "$staged_target" || return 1
    TEMP_BINARY=''
    if run_as_root rmdir "$DOWNLOAD_DIR"; then DOWNLOAD_DIR=''; fi
    return 0
}

ensure_binary_backup_space() {
    if [ -n "$INSTALL_BACKUP_PATH" ]; then
        backup_bytes=0
        if run_as_root test -f "$INSTALL_BINARY_PATH"; then
            backup_bytes="$(run_as_root wc -c "$INSTALL_BINARY_PATH" | awk '{print $1}')"
        fi
        ensure_free_space "${INSTALL_BACKUP_PATH%/*}" "$backup_bytes"
    fi
}

validate_install_path() (
    case "$1" in
        /*) ;;
        *) return 1 ;;
    esac
    case "$1" in
        /|*'
'*|*"$(printf '\r')"*) return 1 ;;
    esac
    return 0
)

canonical_install_path() (
    validate_install_path "$1" || return 1
    candidate="${1%/}"
    suffix=''
    while ! run_as_root test -d "$candidate"; do
        run_as_root test -e "$candidate" && return 1
        suffix="/${candidate##*/}$suffix"
        candidate="${candidate%/*}"
        [ -n "$candidate" ] || candidate=/
    done
    canonical="$(run_as_root sh -c 'cd "$1" && pwd -P' sh "$candidate")" || return 1
    result="${canonical%/}$suffix"
    validate_install_path "$result" || return 1
    case "$result/" in
        */../*|*/./*) return 1 ;;
    esac
    printf '%s\n' "$result"
)

existing_parent_dir() (
    candidate="$1"
    while ! run_as_root test -d "$candidate"; do
        candidate="${candidate%/*}"
        [ -n "$candidate" ] || candidate=/
    done
    printf '%s\n' "$candidate"
)

directory_write_test() (
    run_as_root sh -c '
        probe="$(mktemp -d "$1/.nezha-write.XXXXXX")" || exit 1
        rmdir "$probe"
    ' sh "$1" >/dev/null 2>&1
)

mount_details() (
    mount_directory="$(existing_parent_dir "$1")" || return 1
    if [ -r /proc/self/mountinfo ]; then
        awk -v target="$mount_directory" '
            function decode(value) {
                gsub(/\\040/, " ", value)
                gsub(/\\011/, sprintf("%c", 9), value)
                gsub(/\\134/, sprintf("%c", 92), value)
                return value
            }
            {
                mount = decode($5)
                if (target != mount && mount != "/" && index(target, mount "/") != 1) next
                if (length(mount) < longest) next
                for (field = 7; field <= NF && $field != "-"; field++) {}
                longest = length(mount)
                kind = $(field + 1)
                options = $6 "," $(field + 3)
            }
            END { if (longest) print kind " " options }
        ' /proc/self/mountinfo
    elif has_cmd stat && stat -f -c %T "$mount_directory" >/dev/null 2>&1; then
        stat -f -c %T "$mount_directory"
    else
        df -PT "$mount_directory" 2>/dev/null | awk 'NR == 2 { print $2 }'
    fi
)

path_is_volatile() (
    case "$1/" in
        /tmp/*|/run/*|/var/run/*|/dev/shm/*) return 0 ;;
    esac
    case "$(mount_details "$1")" in
        tmpfs|tmpfs\ *|ramfs|ramfs\ *|devtmpfs|devtmpfs\ *) return 0 ;;
    esac
    return 1
)

path_is_noexec() (
    case ",$(mount_details "$1" | cut -d ' ' -f 2-)," in
        *,noexec,*) return 0 ;;
    esac
    return 1
)

classify_directory() (
    parent="$(existing_parent_dir "$1")" || return 1
    case ",$(mount_details "$parent" | cut -d ' ' -f 2-)," in
        *,ro,*) printf 'readonly\n'; return 0 ;;
    esac
    directory_write_test "$parent" || { printf 'readonly\n'; return 0; }
    if path_is_volatile "$1"; then printf 'volatile\n'; else printf 'persistent\n'; fi
)

literal_value() (
    literal_mode="$3"
    run_as_root awk -v key="$2" -v mode="$literal_mode" '
        function parse(value,    quote, output, index_value, character, next_character, tail) {
            sub(/\r$/, "", value)
            sub(/^[[:space:]]+/, "", value)
            quote = substr(value, 1, 1)
            if (quote != apostrophe && !(mode == "yaml" && quote == "\"")) {
                if (mode != "yaml") { invalid = 1; return "" }
                sub(/[[:space:]]+#.*$/, "", value)
                sub(/[[:space:]]+$/, "", value)
                first_character = substr(value, 1, 1)
                if (first_character == "[" || first_character == "]" ||
                    first_character == "{" || first_character == "}" ||
                    first_character == "&" || first_character == "*" ||
                    first_character == "!" || first_character == ">" ||
                    first_character == "|") invalid = 1
                return value
            }
            for (index_value = 2; index_value <= length(value); index_value++) {
                character = substr(value, index_value, 1)
                next_character = substr(value, index_value + 1, 1)
                if (character == quote) {
                    if (mode == "yaml" && quote == apostrophe && next_character == quote) {
                        output = output quote; index_value++; continue
                    }
                    if (mode == "shell" && substr(value, index_value + 1, 3) == "\\" apostrophe apostrophe) {
                        output = output apostrophe; index_value += 3; continue
                    }
                    tail = substr(value, index_value + 1)
                    if (mode == "yaml") sub(/^[[:space:]]+#.*$/, "", tail)
                    if (tail !~ /^[[:space:]]*$/) invalid = 1
                    return output
                }
                if (quote == "\"" && character == "\\") {
                    if (next_character == "\\" || next_character == "\"") character = next_character
                    else { invalid = 1; return "" }
                    index_value++
                }
                output = output character
            }
            invalid = 1
            return ""
        }
        BEGIN { apostrophe = sprintf("%c", 39) }
        {
            prefix = mode == "yaml" ? key ":[[:space:]]*" : key "="
            if ($0 !~ ("^" prefix)) next
            matched++
            value = $0
            sub("^" prefix, "", value)
            result = parse(value)
        }
        END {
            if (matched != 1 || invalid) exit 1
            print result
        }
    ' "$1" 2>/dev/null
)

metadata_value() {
    literal_value "$1" "$2" shell
}

yaml_value() {
    literal_value "$1" "$2" yaml
}

config_matches_requested() (
    config="$1"
    run_as_root test -f "$config" || return 1
    [ "$(yaml_value "$config" server)" = "$NZ_SERVER" ] || return 1
    [ "$(yaml_value "$config" client_secret)" = "$NZ_CLIENT_SECRET" ] || return 1
    [ -n "$(yaml_value "$config" uuid)" ] || return 1
    if [ -n "${NZ_UUID:-}" ]; then
        [ "$(yaml_value "$config" uuid)" = "$NZ_UUID" ] || return 1
    fi
    for key in tls disable_auto_update disable_force_update disable_command_execute skip_connection_count; do
        actual="$(yaml_value "$config" "$key")" || return 1
        actual="$(normalize_boolean "$actual" "$key" 2>/dev/null)" || return 1
        case "$key" in
            tls) expected="$TLS_VALUE" ;;
            disable_auto_update) expected="$DISABLE_AUTO_UPDATE_VALUE" ;;
            disable_force_update) expected="$DISABLE_FORCE_UPDATE_VALUE" ;;
            disable_command_execute) expected="$DISABLE_COMMAND_EXECUTE_VALUE" ;;
            skip_connection_count) expected="$SKIP_CONNECTION_COUNT_VALUE" ;;
        esac
        [ "$actual" = "$expected" ] || return 1
    done
)

append_discovery_path() (
    candidate="$(canonical_install_path "$1")" || return 0
    grep -Fxq "$candidate" "$DISCOVERY_FILE" 2>/dev/null || printf '%s\n' "$candidate" >> "$DISCOVERY_FILE"
)

service_referenced_paths() (
    run_as_root awk '
        function tokens(text,    position, character, quote, output, started) {
            for (position = 1; position <= length(text); position++) {
                character = substr(text, position, 1)
                if (character == "\\" && quote != apostrophe) {
                    output = output substr(text, ++position, 1); started = 1
                } else if (quote != "") {
                    if (character == quote) quote = ""
                    else output = output character
                } else if (character == apostrophe || character == "\"") {
                    quote = character; started = 1
                } else if (character ~ /[[:space:]]/) {
                    if (started) { emit(output); output = ""; started = 0 }
                } else { output = output character; started = 1 }
            }
            if (started && quote == "") emit(output)
        }
        function emit(value) {
            gsub(/%%/, "%", value)
            if (value ~ /^\/.*\/(run\.sh|supervise\.sh|nezha-agent|config[^\/]*\.yml)$/) print value
        }
        BEGIN { apostrophe = sprintf("%c", 39) }
        /^[[:space:]]*(ExecStart|command|SUPERVISOR)=/ {
            text = $0; sub(/^[^=]*=/, "", text); tokens(text)
        }
        /^[[:space:]]*procd_set_param[[:space:]]+command[[:space:]]/ {
            text = $0; sub(/^[[:space:]]*procd_set_param[[:space:]]+command[[:space:]]+/, "", text); tokens(text)
        }
        /^[[:space:]]*@reboot[[:space:]]/ || /exec .*supervise\.sh/ {
            tokens($0)
        }
    ' "$1" 2>/dev/null
)

discover_installations() {
    DISCOVERY_FILE="$WORK_DIR/discovered"
    : > "$DISCOVERY_FILE"
    for candidate in "$NZ_AGENT_PATH" "$NZ_VOLATILE_CONFIG_DIR" \
        /opt/nezha-rust/agent /etc/nezha-agent /usr/local/nezha-rust/agent \
        /usr/local/lib/nezha-rust/agent /var/lib/nezha-agent /data/nezha-agent; do
        append_discovery_path "$candidate"
    done
    if run_as_root test -f "$NZ_INSTALL_REGISTRY"; then
        run_as_root cat "$NZ_INSTALL_REGISTRY" > "$WORK_DIR/registry-read" || die "Could not read the installation registry."
        while IFS= read -r candidate; do append_discovery_path "$candidate"; done < "$WORK_DIR/registry-read"
    fi
    for service in "$NZ_SYSTEMD_DIR"/nezha-agent*.service /etc/systemd/system/nezha-agent*.service \
        "$NZ_INIT_DIR/nezha-agent" "$NZ_OPENWRT_INIT_DIR/nezha-agent" /etc/init.d/nezha-agent; do
        run_as_root test -f "$service" || continue
        service_referenced_paths "$service" > "$WORK_DIR/service-paths"
        while IFS= read -r reference; do
            append_discovery_path "${reference%/*}"
        done < "$WORK_DIR/service-paths"
    done
    for boot_hook in "$NZ_BUSYBOX_RCS_PATH" /etc/init.d/rcS /etc/rcS; do
        [ -n "$boot_hook" ] && run_as_root test -f "$boot_hook" || continue
        run_as_root awk '
            $0 == "# BEGIN nezha-agent-rust boot hook" { inside = 1; next }
            $0 == "# END nezha-agent-rust boot hook" { inside = 0 }
            inside { print }
        ' "$boot_hook" > "$WORK_DIR/boot-hook"
        service_referenced_paths "$WORK_DIR/boot-hook" > "$WORK_DIR/service-paths"
        while IFS= read -r reference; do append_discovery_path "${reference%/*}"; done < "$WORK_DIR/service-paths"
    done
    if has_cmd crontab; then
        run_as_root crontab -l > "$WORK_DIR/crontab" 2>/dev/null || :
        service_referenced_paths "$WORK_DIR/crontab" > "$WORK_DIR/service-paths"
        while IFS= read -r reference; do append_discovery_path "${reference%/*}"; done < "$WORK_DIR/service-paths"
    fi
}

read_installation_metadata() {
    INSTALL_METADATA="$1/runtime.env"
    INSTALL_KIND=runner
    if ! run_as_root test -f "$INSTALL_METADATA"; then
        INSTALL_METADATA="$1/install.env"
        INSTALL_KIND=native
    fi
    run_as_root test -f "$INSTALL_METADATA" || return 1
    KEEPALIVE_DIR="$1"
    INSTALL_CONFIG_PATH="$(metadata_value "$INSTALL_METADATA" NZ_RUNTIME_CONFIG)" || return 1
    INSTALL_BINARY_PATH="$(metadata_value "$INSTALL_METADATA" NZ_RUNTIME_BINARY)" || return 1
    [ "$(canonical_install_path "${INSTALL_CONFIG_PATH%/*}")" = "$KEEPALIVE_DIR" ] || return 1
    [ "${INSTALL_CONFIG_PATH##*/}" = config.yml ] || return 1
    validate_install_path "$INSTALL_BINARY_PATH" && [ "${INSTALL_BINARY_PATH##*/}" = nezha-agent ] || return 1
    STORAGE_MODE="$(metadata_value "$INSTALL_METADATA" NZ_STORAGE_MODE || printf volatile)"
    case "$STORAGE_MODE" in persistent|volatile) ;; *) return 1 ;; esac
    KEEPALIVE_METHOD="$(metadata_value "$INSTALL_METADATA" NZ_KEEPALIVE_METHOD)" || return 1
    case "$KEEPALIVE_METHOD" in systemd|openwrt|openrc|sysv|busybox-rcs|cron) ;; *) return 1 ;; esac
    saved="$(metadata_value "$INSTALL_METADATA" NZ_INSTALL_SYSTEMD_DIR || true)"
    [ -z "$saved" ] || NZ_SYSTEMD_DIR="$saved"
    saved="$(metadata_value "$INSTALL_METADATA" NZ_INSTALL_INIT_DIR || true)"
    [ -z "$saved" ] || NZ_INIT_DIR="$saved"
    saved="$(metadata_value "$INSTALL_METADATA" NZ_INSTALL_OPENWRT_INIT_DIR || true)"
    [ -z "$saved" ] || NZ_OPENWRT_INIT_DIR="$saved"
    saved="$(metadata_value "$INSTALL_METADATA" NZ_INSTALL_BUSYBOX_RCS_PATH || true)"
    [ -z "$saved" ] || NZ_BUSYBOX_RCS_PATH="$saved"
    if [ "$STORAGE_MODE" = persistent ]; then
        [ "$(canonical_install_path "${INSTALL_BINARY_PATH%/*}")" = "$KEEPALIVE_DIR" ] || return 1
    else
        INSTALL_BINARY_PATH="$(canonical_install_path "${INSTALL_BINARY_PATH%/*}")/nezha-agent" || return 1
        path_is_volatile "${INSTALL_BINARY_PATH%/*}" || return 1
    fi
    return 0
}

runner_service_owned() (
    case "$KEEPALIVE_METHOD" in
        systemd)
            service="$NZ_SYSTEMD_DIR/nezha-agent.service"
            expected="ExecStart=$(systemd_quote_path "$KEEPALIVE_DIR/run.sh")" ;;
        openwrt)
            service="$NZ_OPENWRT_INIT_DIR/nezha-agent"
            expected="    procd_set_param command $(shell_quote "$KEEPALIVE_DIR/run.sh")" ;;
        openrc)
            service="$NZ_INIT_DIR/nezha-agent"
            expected="command=$(shell_quote "$KEEPALIVE_DIR/run.sh")" ;;
        sysv)
            service="$NZ_INIT_DIR/nezha-agent"
            expected="SUPERVISOR=$(shell_quote "$KEEPALIVE_DIR/supervise.sh")" ;;
        busybox-rcs)
            hook="${NZ_BUSYBOX_RCS_PATH:-$(busybox_rcs_path || true)}"
            [ -n "$hook" ] || return 1
            run_as_root grep -Fq '# BEGIN nezha-agent-rust boot hook' "$hook" &&
                run_as_root grep -Fxq "$(busybox_rcs_hook)" "$hook"
            return $? ;;
        cron)
            run_as_root crontab -l > "$WORK_DIR/crontab-check" 2>/dev/null || return 1
            grep -Fxq "@reboot $(shell_quote "$KEEPALIVE_DIR/supervise.sh") >/dev/null 2>&1" "$WORK_DIR/crontab-check"
            return $? ;;
        *) return 1 ;;
    esac
    run_as_root test -f "$service" && ! run_as_root test -L "$service" || return 1
    assert_owned_service_file "$service" "$expected" >/dev/null 2>&1
)

installation_complete() (
    directory="$1"
    run_as_root test -f "$directory/config.yml" || return 1
    if run_as_root test -f "$directory/runtime.env"; then
        read_installation_metadata "$directory" || return 1
        run_as_root test -x "$directory/run.sh" || return 1
        run_as_root grep -Fxq "NZ_TOOL_COMPATIBILITY_VERSION='1'" "$directory/run.sh" || return 1
        if [ "$STORAGE_MODE" = persistent ]; then
            run_as_root test -x "$INSTALL_BINARY_PATH" || return 1
        else
            run_as_root grep -Fxq "NZ_RUNTIME_ALLOW_DOWNLOAD='1'" "$INSTALL_METADATA" || return 1
        fi
        case "$KEEPALIVE_METHOD" in
            cron|busybox-rcs|sysv)
                run_as_root test -x "$directory/supervise.sh" &&
                    run_as_root grep -Fxq "NZ_TOOL_COMPATIBILITY_VERSION='1'" "$directory/supervise.sh" || return 1 ;;
        esac
        if [ "$KEEPALIVE_METHOD" = sysv ]; then
            run_as_root grep -Fxq "NZ_TOOL_COMPATIBILITY_VERSION='1'" "$NZ_INIT_DIR/nezha-agent" || return 1
        fi
        runner_service_owned
    else
        NZ_AGENT_PATH="$directory"
        if run_as_root test -f "$directory/install.env"; then
            read_installation_metadata "$directory" || return 1
        fi
        run_as_root test -x "$directory/nezha-agent" || return 1
        service="$(persistent_unit_path nezha-agent.service)"
        run_as_root test -f "$service" || return 1
        assert_owned_service_file "$service" "$(persistent_unit_command)" >/dev/null 2>&1
    fi
)

select_install_directories() {
    NZ_AGENT_PATH="$(canonical_install_path "$NZ_AGENT_PATH")" || die "NZ_AGENT_PATH must be an absolute installation directory."
    NZ_VOLATILE_CONFIG_DIR="$(canonical_install_path "$NZ_VOLATILE_CONFIG_DIR")" || die "Invalid persistent configuration directory."
    NZ_VOLATILE_RUNTIME_DIR="$(canonical_install_path "$NZ_VOLATILE_RUNTIME_DIR")" || die "Invalid runtime directory."
    discover_installations
    existing_count=0
    while IFS= read -r candidate; do
        if run_as_root test -f "$candidate/runtime.env" || run_as_root test -f "$candidate/install.env" ||
            installation_complete "$candidate"; then
            if [ -n "$NZ_PATH_EXPLICIT" ] && [ "$candidate" != "$NZ_AGENT_PATH" ] &&
                [ "$candidate" != "$NZ_VOLATILE_CONFIG_DIR" ]; then continue; fi
            EXISTING_DIR="$candidate"
            existing_count=$((existing_count + 1))
        fi
    done < "$DISCOVERY_FILE"
    [ "$existing_count" -le 1 ] || die "Multiple installations were found. Set NZ_AGENT_PATH or run uninstall first."
    if [ -n "$EXISTING_DIR" ]; then
        if read_installation_metadata "$EXISTING_DIR"; then
            if [ "$STORAGE_MODE" = persistent ]; then
                NZ_AGENT_PATH="$EXISTING_DIR"
            else
                NZ_VOLATILE_CONFIG_DIR="$EXISTING_DIR"
                NZ_VOLATILE_RUNTIME_DIR="${INSTALL_BINARY_PATH%/*}"
                NZ_FORCE_VOLATILE=1
            fi
        elif run_as_root test -f "$EXISTING_DIR/runtime.env" || run_as_root test -f "$EXISTING_DIR/install.env"; then
            die "Invalid installation metadata at $EXISTING_DIR; refusing to replace it."
        else
            NZ_AGENT_PATH="$EXISTING_DIR"
        fi
        info "Discovered installation: $EXISTING_DIR"
        return 0
    fi
    if [ -z "$NZ_PATH_EXPLICIT" ]; then
        for candidate in /opt/nezha-rust/agent /usr/local/lib/nezha-rust/agent /var/lib/nezha-agent /data/nezha-agent; do
            candidate="$(canonical_install_path "$candidate")" || continue
            classification="$(classify_directory "$candidate")"
            info "Install directory check: $candidate ($classification)"
            [ "$classification" = persistent ] && ! path_is_noexec "$candidate" || continue
            NZ_AGENT_PATH="$candidate"
            break
        done
    fi
    info "Selected installation directory: $NZ_AGENT_PATH ($(classify_directory "$NZ_AGENT_PATH"))"
}

skip_if_same_configuration() {
    [ -n "$EXISTING_DIR" ] || return 1
    if ! run_as_root test -f "$EXISTING_DIR/runtime.env"; then
        info "Legacy installation requires migration to the unified startup entry."
        return 1
    fi
    if config_matches_requested "$EXISTING_DIR/config.yml" && installation_complete "$EXISTING_DIR"; then
        INSTALL_CONFIG_PATH="$EXISTING_DIR/config.yml"
        success "Matching configuration is already installed at $EXISTING_DIR; skipping installation and download."
        return 0
    fi
    info "Configuration differs or installation is incomplete; continuing installation."
    return 1
}

select_storage_mode() {
    required_bytes="$ASSET_SIZE"
    target_binary="$NZ_AGENT_PATH/nezha-agent"
    if run_as_root test -f "$target_binary"; then
        old_size="$(run_as_root wc -c "$target_binary" | awk '{print $1}')"
        required_bytes=$((required_bytes + old_size))
    fi
    classification="$(classify_directory "$NZ_AGENT_PATH")"
    info "Binary directory: $NZ_AGENT_PATH ($classification)"
    if [ "$NZ_FORCE_VOLATILE" != 1 ] && [ "$classification" = persistent ] &&
        ! path_is_noexec "$NZ_AGENT_PATH" && has_free_space "$(existing_parent_dir "$NZ_AGENT_PATH")" "$required_bytes"; then
        STORAGE_MODE=persistent
        run_as_root mkdir -p "$NZ_AGENT_PATH" || die "Could not create $NZ_AGENT_PATH."
        return 0
    fi
    if [ -n "$EXISTING_DIR" ] && [ "$NZ_FORCE_VOLATILE" != 1 ]; then
        die "Existing installation directory is read-only, volatile, noexec or full. Uninstall before changing storage modes."
    fi
    STORAGE_MODE=volatile
    if [ "$(classify_directory "$NZ_VOLATILE_CONFIG_DIR")" != persistent ] ||
        path_is_noexec "$NZ_VOLATILE_CONFIG_DIR"; then
        [ -z "$EXISTING_DIR" ] || die "Existing configuration directory is not writable persistent storage."
        selected=''
        for candidate in "$NZ_AGENT_PATH" /var/lib/nezha-agent /data/nezha-agent /usr/local/etc/nezha-agent; do
            [ "$(classify_directory "$candidate")" = persistent ] && ! path_is_noexec "$candidate" || continue
            NZ_VOLATILE_CONFIG_DIR="$candidate"
            selected=1
            break
        done
        [ -n "$selected" ] || die "No writable persistent config directory found. Set NZ_VOLATILE_CONFIG_DIR to a persistent mount."
    fi
    [ "$(classify_directory "$NZ_VOLATILE_RUNTIME_DIR")" != readonly ] &&
        path_is_volatile "$NZ_VOLATILE_RUNTIME_DIR" && ! path_is_noexec "$NZ_VOLATILE_RUNTIME_DIR" ||
        die "NZ_VOLATILE_RUNTIME_DIR must be writable, executable, volatile storage."
    ensure_free_space "$(existing_parent_dir "$NZ_VOLATILE_RUNTIME_DIR")" "$ASSET_SIZE"
    run_as_root mkdir -p "$NZ_VOLATILE_CONFIG_DIR" "$NZ_VOLATILE_RUNTIME_DIR" || die "Could not create volatile installation directories."
    info "Using volatile binary storage with persistent config at $NZ_VOLATILE_CONFIG_DIR."
}

write_location_metadata() {
    printf 'NZ_INSTALL_SYSTEMD_DIR=%s\n' "$(shell_quote "$NZ_SYSTEMD_DIR")"
    printf 'NZ_INSTALL_INIT_DIR=%s\n' "$(shell_quote "$NZ_INIT_DIR")"
    printf 'NZ_INSTALL_OPENWRT_INIT_DIR=%s\n' "$(shell_quote "$NZ_OPENWRT_INIT_DIR")"
    printf 'NZ_INSTALL_BUSYBOX_RCS_PATH=%s\n' "$(shell_quote "$NZ_BUSYBOX_RCS_PATH")"
}

register_installation() {
    registry_dir="${NZ_INSTALL_REGISTRY%/*}"
    if [ "$(classify_directory "$registry_dir")" != persistent ]; then
        info "Registry is not writable persistent storage; service files remain available for discovery."
        return 0
    fi
    registry_temp="$WORK_DIR/registry-new"
    : > "$registry_temp"
    if run_as_root test -f "$NZ_INSTALL_REGISTRY"; then
        run_as_root cat "$NZ_INSTALL_REGISTRY" > "$registry_temp" || die "Could not read installation registry."
    fi
    grep -Fxq "${INSTALL_CONFIG_PATH%/*}" "$registry_temp" || printf '%s\n' "${INSTALL_CONFIG_PATH%/*}" >> "$registry_temp"
    run_as_root mkdir -p "$registry_dir" && copy_root_file_atomic "$registry_temp" "$NZ_INSTALL_REGISTRY" 600 ||
        die "Could not save the installation registry."
}

unregister_installation() {
    run_as_root test -f "$NZ_INSTALL_REGISTRY" || return 0
    run_as_root cat "$NZ_INSTALL_REGISTRY" > "$WORK_DIR/registry-old" || die "Could not read installation registry."
    grep -Fxv "$1" "$WORK_DIR/registry-old" > "$WORK_DIR/registry-new" || true
    if [ -s "$WORK_DIR/registry-new" ]; then
        copy_root_file_atomic "$WORK_DIR/registry-new" "$NZ_INSTALL_REGISTRY" 600 || die "Could not update installation registry."
    else
        run_as_root rm -f "$NZ_INSTALL_REGISTRY" || die "Could not remove installation registry."
    fi
}


sha256_file() {
    file="$1"
    if has_cmd sha256sum; then
        sha256sum "$file" | awk '{print $1}'
    elif has_cmd shasum; then
        shasum -a 256 "$file" | awk '{print $1}'
    elif has_cmd openssl; then
        openssl dgst -sha256 "$file" | sed 's/^.*= //'
    elif busybox_has_applet sha256sum; then
        "$BUSYBOX_PATH" sha256sum "$file" | awk '{print $1}'
    else
        return 1
    fi
}

elf_header_hex() {
    input_file="$1"
    if has_cmd od; then
        od -An -tx1 -N20 "$input_file" 2>/dev/null |
            awk '{for (i = 1; i <= NF; i++) { if (length($i) == 1) printf "0%s", $i; else printf "%s", $i }}'
    elif has_cmd hexdump; then
        hexdump -C -n 20 "$input_file" 2>/dev/null |
            awk '{for (i = 2; i <= NF; i++) if ($i ~ /^[0-9A-Fa-f][0-9A-Fa-f]$/) printf "%s", $i}'
    else
        return 1
    fi
}

verify_binary_header() {
    file="$1"
    header="$(elf_header_hex "$file" 2>/dev/null)" || return 0

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
    [ -s "$TEMP_BINARY" ] || return 1
    header="$(elf_header_hex "$TEMP_BINARY" 2>/dev/null)" || return 1
    case "$header" in 7f454c46*) ;; *) return 1 ;; esac
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
    esac || return 1
    chmod +x "$TEMP_BINARY" || return 1
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
    [ -n "$DOWNLOAD_DIR" ] && [ -n "$TEMP_BINARY" ] || die "Download staging directory has not been prepared."
    download_url="${NZ_RELEASE_BASE}/${ASSET_NAME}"
    info "Downloading from: $download_url"
    rm -f "$TEMP_BINARY"
    ensure_free_space "$DOWNLOAD_DIR" "$ASSET_SIZE"
    if download_to "$download_url" "$TEMP_BINARY" 2>/dev/null && verify_rust_download; then
        return 0
    fi
    case "$ASSET_NAME" in
        UPX-*)
            ASSET_NAME="$RUST_ORIGINAL_ASSET"
            download_url="${NZ_RELEASE_BASE}/${ASSET_NAME}"
            info "UPX asset unavailable or incompatible; falling back to $download_url"
            rm -f "$TEMP_BINARY"
            ensure_free_space "$DOWNLOAD_DIR" "$ASSET_SIZE"
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
        *) err "$name must be true or false, got: $value"; return 1 ;;
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

copy_root_file_atomic() {
    source_file="$1"
    destination_file="$2"
    file_mode="$3"
    staged_file="${destination_file}.nezha-agent-rust.$$"
    if ! run_as_root cp -p "$source_file" "$staged_file"; then
        run_as_root rm -f "$staged_file" || true
        return 1
    fi
    if ! run_as_root chmod "$file_mode" "$staged_file" ||
        ! run_as_root mv -f "$staged_file" "$destination_file"; then
        run_as_root rm -f "$staged_file" || true
        return 1
    fi
}

assert_owned_service_file() {
    service_file="$1"
    expected_command="$2"
    if run_as_root test -e "$service_file" || run_as_root test -L "$service_file"; then
        case "$expected_command" in
            ExecStart=*) directive='^[[:space:]]*ExecStart[[:space:]]*=' ;;
            *'procd_set_param command '*) directive='^[[:space:]]*procd_set_param[[:space:]]+command[[:space:]]+' ;;
            command=*) directive='^[[:space:]]*command[[:space:]]*=' ;;
            SUPERVISOR=*) directive='^[[:space:]]*SUPERVISOR[[:space:]]*=' ;;
            *) die "Unsupported service ownership check: $service_file" ;;
        esac
        run_as_root test -f "$service_file" && ! run_as_root test -L "$service_file" &&
            run_as_root awk -v expected="$expected_command" -v directive="$directive" '
                $0 == expected { matching++ }
                $0 ~ directive { commands++ }
                END { exit !(matching == 1 && commands == 1) }
            ' "$service_file" ||
            die "Existing service is not managed by this installer: $service_file"
    fi
}

systemd_quote_path() {
    printf '"%s"' "$(printf '%s' "$1" | sed 's/%/%%/g; s/\\/\\\\/g; s/"/\\"/g')"
}

persistent_unit_command() {
    printf 'ExecStart=%s -c %s\n' \
        "$(systemd_quote_path "$NZ_AGENT_PATH/nezha-agent")" \
        "$(systemd_quote_path "${1:-$NZ_AGENT_PATH/config.yml}")"
}

persistent_unit_path() {
    printf '%s/%s\n' "$NZ_SYSTEMD_DIR" "$1"
}

validate_config_booleans() {
    TLS_VALUE="$(normalize_boolean "${NZ_TLS:-false}" NZ_TLS)" || die "Invalid NZ_TLS value."
    DISABLE_AUTO_UPDATE_VALUE="$(normalize_boolean "${NZ_DISABLE_AUTO_UPDATE:-true}" NZ_DISABLE_AUTO_UPDATE)" || die "Invalid NZ_DISABLE_AUTO_UPDATE value."
    DISABLE_FORCE_UPDATE_VALUE="$(normalize_boolean "${NZ_DISABLE_FORCE_UPDATE:-${DISABLE_FORCE_UPDATE:-false}}" NZ_DISABLE_FORCE_UPDATE)" || die "Invalid NZ_DISABLE_FORCE_UPDATE value."
    DISABLE_COMMAND_EXECUTE_VALUE="$(normalize_boolean "${NZ_DISABLE_COMMAND_EXECUTE:-false}" NZ_DISABLE_COMMAND_EXECUTE)" || die "Invalid NZ_DISABLE_COMMAND_EXECUTE value."
    SKIP_CONNECTION_COUNT_VALUE="$(normalize_boolean "${NZ_SKIP_CONNECTION_COUNT:-false}" NZ_SKIP_CONNECTION_COUNT)" || die "Invalid NZ_SKIP_CONNECTION_COUNT value."
}

write_agent_config() {
    config_path="$1"
    config_dir="${config_path%/*}"
    [ "$config_dir" != "$config_path" ] || config_dir='.'
    config_temp="${TMPDIR:-/tmp}/nezha-agent-config.$$"
    uuid_value="${NZ_UUID:-}"
    [ -n "$uuid_value" ] || uuid_value="$(generate_uuid)"
    [ -n "$uuid_value" ] || die "Could not generate an agent UUID. Set NZ_UUID explicitly."

    validate_config_booleans

    {
        printf 'server: %s\n' "$(yaml_quote "$NZ_SERVER")"
        printf 'client_secret: %s\n' "$(yaml_quote "$NZ_CLIENT_SECRET")"
        printf 'uuid: %s\n' "$(yaml_quote "$uuid_value")"
        printf 'tls: %s\n' "$TLS_VALUE"
        printf 'disable_auto_update: %s\n' "$DISABLE_AUTO_UPDATE_VALUE"
        printf 'disable_force_update: %s\n' "$DISABLE_FORCE_UPDATE_VALUE"
        printf 'disable_command_execute: %s\n' "$DISABLE_COMMAND_EXECUTE_VALUE"
        printf 'skip_connection_count: %s\n' "$SKIP_CONNECTION_COUNT_VALUE"
    } > "$config_temp" || die "Could not prepare agent configuration."

    run_as_root mkdir -p "$config_dir" || die "Could not create $config_dir."
    copy_root_file "$config_temp" "$config_path" 600 || die "Could not install $config_path."
    rm -f "$config_temp"
}

write_requested_config() {
    config_path="$1"
    if [ -z "${NZ_UUID:-}" ] && run_as_root test -f "$config_path"; then
        existing_uuid="$(yaml_value "$config_path" uuid)"
        [ -n "$existing_uuid" ] && NZ_UUID="$existing_uuid"
    fi
    write_agent_config "$config_path"
}

write_runtime_env() {
    case "$RUNTIME_ALLOW_DOWNLOAD" in
        0|1) ;;
        *) die "Runtime download policy has not been prepared." ;;
    esac
    env_path="$1"
    env_temp="${TMPDIR:-/tmp}/nezha-agent-runtime-env.$$"
    {
        printf 'NZ_RUNTIME_URL=%s\n' "$(shell_quote "$download_url")"
        printf 'NZ_RUNTIME_SIZE=%s\n' "$(shell_quote "$ASSET_SIZE")"
        printf 'NZ_RUNTIME_SHA256=%s\n' "$(shell_quote "")"
        printf 'NZ_RUNTIME_FALLBACK_URL=%s\n' "$(shell_quote "${NZ_RELEASE_BASE}/${RUST_ORIGINAL_ASSET}")"
        printf 'NZ_RUNTIME_DIR=%s\n' "$(shell_quote "${INSTALL_BINARY_PATH%/*}")"
        printf 'NZ_RUNTIME_BINARY=%s\n' "$(shell_quote "$INSTALL_BINARY_PATH")"
        printf 'NZ_RUNTIME_CONFIG=%s\n' "$(shell_quote "$INSTALL_CONFIG_PATH")"
        printf 'NZ_RUNTIME_VERSION=%s\n' "$(shell_quote 'nezha-agent-rust 2.1.0')"
        printf 'NZ_RUNTIME_DOWNLOAD_TIMEOUT=%s\n' "$(shell_quote "$NZ_DOWNLOAD_TIMEOUT")"
        printf 'NZ_RUNTIME_INSECURE_TLS=%s\n' "$(shell_quote "$NZ_INSECURE_TLS")"
        printf 'NZ_KEEPALIVE_METHOD=%s\n' "$(shell_quote "$KEEPALIVE_METHOD")"
        printf 'NZ_STORAGE_MODE=%s\n' "$(shell_quote "$STORAGE_MODE")"
        write_location_metadata
        printf 'NZ_RUNTIME_ALLOW_DOWNLOAD=%s\n' "$(shell_quote "$RUNTIME_ALLOW_DOWNLOAD")"
    } > "$env_temp" || die "Could not prepare volatile runtime metadata."
    copy_root_file "$env_temp" "$env_path" 600 || die "Could not install $env_path."
    rm -f "$env_temp"
}

write_runner() {
    runner_path="$1"
    runner_temp="${TMPDIR:-/tmp}/nezha-agent-runner.$$"
    {
        printf '%s\n' '#!/bin/sh' '' 'set -eu'
        emit_tool_compatibility
        cat <<'RUNNEREOF'
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
            wget -T "$NZ_RUNTIME_DOWNLOAD_TIMEOUT" --no-check-certificate -O "$destination" "$url" >> "$download_log" 2>&1 && { rm -f "$download_log"; return 0; }
        else
            wget -T "$NZ_RUNTIME_DOWNLOAD_TIMEOUT" -O "$destination" "$url" >> "$download_log" 2>&1 && { rm -f "$download_log"; return 0; }
        fi
        cat "$download_log" >&2
        if auto_insecure_tls "$download_log"; then
            echo 'certificate verification failed; retrying wget without certificate verification' >&2
            rm -f "$destination"
            wget -T "$NZ_RUNTIME_DOWNLOAD_TIMEOUT" --no-check-certificate -O "$destination" "$url" >> "$download_log" 2>&1 && { rm -f "$download_log"; return 0; }
            cat "$download_log" >&2
        fi
    fi
    if has_cmd uclient-fetch; then
        found=1
        rm -f "$destination"
        uclient-fetch -T "$NZ_RUNTIME_DOWNLOAD_TIMEOUT" -O "$destination" "$url" >> "$download_log" 2>&1 && { rm -f "$download_log"; return 0; }
        cat "$download_log" >&2
        if auto_insecure_tls "$download_log"; then
            echo 'certificate verification failed; retrying uclient-fetch without certificate verification' >&2
            rm -f "$destination"
            uclient-fetch -T "$NZ_RUNTIME_DOWNLOAD_TIMEOUT" --no-check-certificate -O "$destination" "$url" >> "$download_log" 2>&1 && { rm -f "$download_log"; return 0; }
            cat "$download_log" >&2
        fi
    fi
    if busybox_has_applet wget; then
        found=1
        rm -f "$destination"
        "$BUSYBOX_PATH" wget -T "$NZ_RUNTIME_DOWNLOAD_TIMEOUT" -O "$destination" "$url" >> "$download_log" 2>&1 && { rm -f "$download_log"; return 0; }
        cat "$download_log" >&2
        if auto_insecure_tls "$download_log" && "$BUSYBOX_PATH" wget --help 2>&1 | grep -q -- '--no-check-certificate'; then
            echo 'certificate verification failed; retrying BusyBox wget without certificate verification' >&2
            rm -f "$destination"
            "$BUSYBOX_PATH" wget -T "$NZ_RUNTIME_DOWNLOAD_TIMEOUT" --no-check-certificate -O "$destination" "$url" >> "$download_log" 2>&1 && { rm -f "$download_log"; return 0; }
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
    elif busybox_has_applet sha256sum; then
        "$BUSYBOX_PATH" sha256sum "$file" | awk '{print $1}'
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

    [ "${NZ_RUNTIME_ALLOW_DOWNLOAD:-1}" = 1 ] || {
        echo "persistent agent binary is missing or invalid: $NZ_RUNTIME_BINARY" >&2
        return 1
    }

    rm -f "$NZ_RUNTIME_BINARY"
    mkdir -p "$NZ_RUNTIME_DIR"
    ensure_space || {
        echo "not enough free space in $NZ_RUNTIME_DIR" >&2
        return 1
    }

    temporary_binary="${NZ_RUNTIME_BINARY}.download.$$"
    trap 'rm -f "$temporary_binary"' EXIT HUP INT TERM
    if ! download_to "$NZ_RUNTIME_URL" "$temporary_binary" || ! binary_ok "$temporary_binary"; then
        rm -f "$temporary_binary"
        download_to "$NZ_RUNTIME_FALLBACK_URL" "$temporary_binary" && binary_ok "$temporary_binary" || {
            echo 'downloaded Nezha Agent failed size or runtime verification' >&2
            return 1
        }
    fi
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
    } > "$runner_temp"
    copy_root_file "$runner_temp" "$runner_path" 700 || die "Could not install $runner_path."
    rm -f "$runner_temp"
}

write_supervisor() {
    supervisor_path="$1"
    supervisor_temp="${TMPDIR:-/tmp}/nezha-agent-supervisor.$$"
    runner_path="$KEEPALIVE_DIR/run.sh"
    {
        printf '%s\n' '#!/bin/sh' '' 'set -eu'
        emit_tool_compatibility
        printf '%s\n' 'child=' \
            "lock=$(shell_quote "$KEEPALIVE_DIR/supervise.lock")" \
            "supervisor=$(shell_quote "$supervisor_path")" \
            'supervisor_running() {' \
            '    pid="$1"' \
            '    case "$pid" in *[!0-9]*|"") return 1 ;; esac' \
            '    kill -0 "$pid" 2>/dev/null || return 1' \
            '    if [ -r "/proc/$pid/cmdline" ]; then' \
            '        tr "\000" " " < "/proc/$pid/cmdline" | grep -Fq "$supervisor"' \
            '    else' \
            '        ps -p "$pid" -o args= 2>/dev/null | grep -Fq "$supervisor"' \
            '    fi' \
            '}' \
            'if ! mkdir "$lock" 2>/dev/null; then' \
            '    old_pid="$(cat "$lock/pid" 2>/dev/null || true)"' \
            '    supervisor_running "$old_pid" && exit 0' \
            '    recovery="${lock}.recovery"' \
            '    mkdir "$recovery" 2>/dev/null || exit 1' \
            '    trap '\''rmdir "$recovery" 2>/dev/null || true'\'' EXIT' \
            '    trap "exit 1" HUP INT TERM' \
            '    old_pid="$(cat "$lock/pid" 2>/dev/null || true)"' \
            '    supervisor_running "$old_pid" && exit 0' \
            '    rm -rf "$lock" || exit 1' \
            '    mkdir "$lock" 2>/dev/null || exit 0' \
            '    rmdir "$recovery" 2>/dev/null || true' \
            '    trap - EXIT HUP INT TERM' \
            'fi' \
            'printf "%s\n" "$$" > "$lock/pid"' \
            'finish() {' \
            '    [ -z "$child" ] || { kill "$child" 2>/dev/null || true; wait "$child" 2>/dev/null || true; }' \
            '    [ "$(cat "$lock/pid" 2>/dev/null || true)" != "$$" ] || rm -rf "$lock"' \
            '}' \
            'trap finish EXIT' \
            'trap "exit 0" HUP INT TERM'
        printf 'while :; do\n    %s & child=$!\n    wait "$child" || true\n    child=\n    sleep 5\ndone\n' "$(shell_quote "$runner_path")"
    } > "$supervisor_temp"
    copy_root_file "$supervisor_temp" "$supervisor_path" 700 || die "Could not install $supervisor_path."
    rm -f "$supervisor_temp"
}

supervisor_process_running() {
    candidate_pid="$1"
    candidate_path="$2"
    case "$candidate_pid" in *[!0-9]*|'') return 1 ;; esac
    run_as_root kill -0 "$candidate_pid" 2>/dev/null || return 1
    if run_as_root test -r "/proc/$candidate_pid/cmdline"; then
        run_as_root cat "/proc/$candidate_pid/cmdline" |
            tr '\000' ' ' | grep -Fq "$candidate_path"
    else
        run_as_root ps -p "$candidate_pid" -o args= 2>/dev/null |
            grep -Fq "$candidate_path"
    fi
}

stop_supervisor() {
    lock="$KEEPALIVE_DIR/supervise.lock"
    if [ ! -f "$lock/pid" ]; then
        legacy_script="$KEEPALIVE_DIR/supervise.sh"
        if run_as_root test -f "$legacy_script" &&
            ! run_as_root grep -Fq 'supervise.lock' "$legacy_script"; then
            process_list="$(run_as_root ps -eo pid=,args= 2>/dev/null)" ||
                process_list="$(run_as_root ps -o pid,args 2>/dev/null)" || return 1
            legacy_pids="$(printf '%s\n' "$process_list" |
                NZ_SUPERVISOR_PATH="$legacy_script" awk '
                    $1 ~ /^[0-9]+$/ {
                        path = ENVIRON["NZ_SUPERVISOR_PATH"]
                        if (length($0) >= length(path) &&
                            substr($0, length($0) - length(path) + 1) == path)
                            print $1
                    }
                ')"
            for legacy_pid in $legacy_pids; do
                run_as_root kill "$legacy_pid" || return 1
            done
        fi
        return 0
    fi
    supervisor_pid="$(run_as_root cat "$lock/pid")" || return 1
    case "$supervisor_pid" in *[!0-9]*|'') return 1 ;; esac
    if ! run_as_root kill -0 "$supervisor_pid" 2>/dev/null; then
        run_as_root rm -rf "$lock"
        return $?
    fi
    supervisor_process_running "$supervisor_pid" "$KEEPALIVE_DIR/supervise.sh" || return 1
    run_as_root kill "$supervisor_pid" || return 1
    attempts=0
    while [ -d "$lock" ] && [ "$attempts" -lt 10 ]; do
        sleep 1
        attempts=$((attempts + 1))
    done
    [ ! -d "$lock" ]
}

start_supervisor() {
    supervisor_path="$KEEPALIVE_DIR/supervise.sh"
    run_as_root sh -c 'if has_cmd nohup; then nohup "$1" >/dev/null 2>&1 & else (trap "" HUP; exec "$1") >/dev/null 2>&1 & fi' sh "$supervisor_path" ||
        die "Could not launch the agent supervisor."
    attempts=0
    while [ "$attempts" -lt 10 ]; do
        supervisor_pid="$(run_as_root cat "$KEEPALIVE_DIR/supervise.lock/pid" 2>/dev/null || true)"
        if supervisor_process_running "$supervisor_pid" "$supervisor_path"; then
            return 0
        fi
        sleep 1
        attempts=$((attempts + 1))
    done
    die "The agent supervisor did not stay running."
}

detect_keepalive_method() {
    if [ -n "${NZ_INIT_SYSTEM:-}" ]; then
        case "$NZ_INIT_SYSTEM" in
            openwrt|systemd|openrc|sysv|busybox-rcs|cron) printf '%s\n' "$NZ_INIT_SYSTEM" ;;
            *) die "Unsupported NZ_INIT_SYSTEM: $NZ_INIT_SYSTEM" ;;
        esac
    elif [ -f /etc/openwrt_release ] || [ -x /sbin/procd ]; then
        printf '%s\n' openwrt
    elif has_cmd systemctl && [ -d /etc/systemd/system ] && [ -d /run/systemd/system ]; then
        printf '%s\n' systemd
    elif has_cmd rc-service && has_cmd rc-update && [ -d /etc/init.d ]; then
        printf '%s\n' openrc
    elif [ -r /etc/inittab ] && grep -q '::sysinit:.*rcS' /etc/inittab; then
        printf '%s\n' busybox-rcs
    elif [ -d /etc/init.d ]; then
        printf '%s\n' sysv
    elif has_cmd crontab; then
        printf '%s\n' cron
    else
        return 1
    fi
}

busybox_rcs_path() {
    rcs="$NZ_BUSYBOX_RCS_PATH"
    if [ -z "$rcs" ] && [ -r /etc/inittab ]; then
        rcs="$(sed -n 's/^.*::sysinit:\([^[:space:]]*rcS\).*$/\1/p' /etc/inittab | head -n 1)"
    fi
    [ -n "$rcs" ] || rcs=/etc/init.d/rcS
    printf '%s\n' "$rcs"
}

busybox_rcs_hook() (
    if busybox_fallback_needed sleep; then
        boot_sleep="$(shell_quote "$BUSYBOX_PATH") sleep"
    else
        boot_sleep="$(shell_quote "$(command -v sleep)")"
    fi
    printf '( while [ ! -x %s ]; do %s 2; done; exec %s ) >/dev/null 2>&1 &\n' \
        "$(shell_quote "$KEEPALIVE_DIR/supervise.sh")" "$boot_sleep" \
        "$(shell_quote "$KEEPALIVE_DIR/supervise.sh")"
)

install_busybox_rcs_keepalive() {
    rcs_path="$(busybox_rcs_path)"
    [ -f "$rcs_path" ] && [ ! -L "$rcs_path" ] || die "BusyBox rcS must be a regular file: $rcs_path"
    write_supervisor "$KEEPALIVE_DIR/supervise.sh"
    hook_marker='# BEGIN nezha-agent-rust boot hook'
    if grep -Fq "$hook_marker" "$rcs_path"; then
        grep -Fq "$(shell_quote "$KEEPALIVE_DIR/supervise.sh")" "$rcs_path" ||
            die "Existing BusyBox boot hook uses a different installation; uninstall it first."
        remove_busybox_rcs_hook
    fi
    if ! grep -Fq "$hook_marker" "$rcs_path"; then
        run_as_root cp -p "$rcs_path" "$rcs_path.nezha-agent-rust.bak" || die "Could not back up BusyBox rcS."
        hook_temp="${TMPDIR:-/tmp}/nezha-agent-rcs.$$"
        {
            IFS= read -r first || true
            printf '%s\n' "$first"
            printf '%s\n' "$hook_marker"
            busybox_rcs_hook
            printf '%s\n' '# END nezha-agent-rust boot hook'
            cat
        } < "$rcs_path" > "$hook_temp"
        copy_root_file_atomic "$hook_temp" "$rcs_path" 755 || die "Could not install the BusyBox rcS hook."
        rm -f "$hook_temp"
    fi
    if [ "$NZ_NO_START" != 1 ]; then
        start_supervisor
    fi
}

remove_busybox_rcs_hook() {
    rcs_path="$(busybox_rcs_path)"
    [ -f "$rcs_path" ] && [ ! -L "$rcs_path" ] || return 0
    grep -Fq '# BEGIN nezha-agent-rust boot hook' "$rcs_path" || return 0
    hook_temp="${TMPDIR:-/tmp}/nezha-agent-rcs.$$"
    awk '
        $0 == "# BEGIN nezha-agent-rust boot hook" {skip=1; next}
        $0 == "# END nezha-agent-rust boot hook" {skip=0; next}
        !skip {print}
    ' "$rcs_path" > "$hook_temp"
    copy_root_file_atomic "$hook_temp" "$rcs_path" 755 || die "Could not remove the BusyBox rcS hook."
    rm -f "$hook_temp"
}

install_openwrt_keepalive() {
    service_path="$NZ_OPENWRT_INIT_DIR/nezha-agent"
    service_temp="${TMPDIR:-/tmp}/nezha-agent-openwrt.$$"
    runner_path="$KEEPALIVE_DIR/run.sh"
    assert_owned_service_file "$service_path" "    procd_set_param command $(shell_quote "$runner_path")"
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
    runner_path="$KEEPALIVE_DIR/run.sh"
    if [ "$NATIVE_SYSTEMD_MIGRATION" = 1 ]; then
        assert_owned_service_file "$service_path" "$(persistent_unit_command "$INSTALL_CONFIG_PATH")"
    else
        assert_owned_service_file "$service_path" "ExecStart=$(systemd_quote_path "$runner_path")"
    fi
    run_as_root mkdir -p "$NZ_SYSTEMD_DIR" || die "Could not create $NZ_SYSTEMD_DIR."
    {
        printf '%s\n' '[Unit]' 'Description=Nezha monitoring agent' 'Wants=network-online.target' 'After=network-online.target' ''
        printf '%s\n' '[Service]' 'Type=simple' 'User=root'
        printf 'ExecStart=%s\n' "$(systemd_quote_path "$runner_path")"
        printf '%s\n' 'Restart=always' 'RestartSec=5' 'StartLimitIntervalSec=0' '' '[Install]' 'WantedBy=multi-user.target'
    } > "$service_temp"
    copy_root_file_atomic "$service_temp" "$service_path" 644 || die "Could not install $service_path."
    rm -f "$service_temp"
    run_as_root systemctl daemon-reload || die "systemctl daemon-reload failed."
    run_as_root systemctl enable nezha-agent || die "Could not enable the systemd nezha-agent service."
    if [ "$NZ_NO_START" != 1 ]; then
        run_as_root systemctl restart nezha-agent || die "Could not start the systemd nezha-agent service."
    fi
}

install_openrc_keepalive() {
    service_path="$NZ_INIT_DIR/nezha-agent"
    service_temp="${TMPDIR:-/tmp}/nezha-agent-openrc.$$"
    runner_path="$KEEPALIVE_DIR/run.sh"
    assert_owned_service_file "$service_path" "command=$(shell_quote "$runner_path")"
    {
        printf '%s\n' '#!/sbin/openrc-run' '' 'description="Nezha monitoring agent"'
        printf 'command=%s\n' "$(shell_quote "$runner_path")"
        printf '%s\n' 'command_background="no"' 'supervisor="supervise-daemon"' 'respawn_delay="5"' 'respawn_max="0"' '' 'depend() {' '    need net' '}'
    } > "$service_temp"
    run_as_root mkdir -p "$NZ_INIT_DIR" || die "Could not create $NZ_INIT_DIR."
    copy_root_file "$service_temp" "$service_path" 755 || die "Could not install $service_path."
    rm -f "$service_temp"
    run_as_root rc-update add nezha-agent default || die "Could not enable the OpenRC nezha-agent service."
    if [ "$NZ_NO_START" != 1 ]; then
        run_as_root rc-service nezha-agent restart || run_as_root rc-service nezha-agent start || \
            die "Could not start the OpenRC nezha-agent service."
    fi
}

install_sysv_keepalive() {
    supervisor_path="$KEEPALIVE_DIR/supervise.sh"
    write_supervisor "$supervisor_path"
    service_path="$NZ_INIT_DIR/nezha-agent"
    assert_owned_service_file "$service_path" "SUPERVISOR=$(shell_quote "$supervisor_path")"
    service_temp="${TMPDIR:-/tmp}/nezha-agent-sysv.$$"
    {
        printf '%s\n' '#!/bin/sh' '### BEGIN INIT INFO' '# Provides: nezha-agent' '# Required-Start: $network' '# Required-Stop: $network' '# Default-Start: 2 3 4 5' '# Default-Stop: 0 1 6' '# Short-Description: Nezha monitoring agent' '### END INIT INFO' ''
        emit_tool_compatibility
        printf 'LOCK=%s\n' "$(shell_quote "$KEEPALIVE_DIR/supervise.lock")"
        printf 'SUPERVISOR=%s\n\n' "$(shell_quote "$supervisor_path")"
        printf '%s\n' 'start() {' '    if has_cmd nohup; then' '        nohup "$SUPERVISOR" >/dev/null 2>&1 &' '    else' '        (trap "" HUP; exec "$SUPERVISOR") >/dev/null 2>&1 &' '    fi' '}' '' 'stop() {' '    [ -f "$LOCK/pid" ] || return 0' '    pid="$(cat "$LOCK/pid")"' '    case "$pid" in *[!0-9]*|"") return 1 ;; esac' '    kill "$pid"' '}' '' 'case "${1:-}" in' '    start) start ;;' '    stop) stop ;;' '    restart) stop; sleep 1; start ;;' '    *) echo "Usage: $0 {start|stop|restart}"; exit 1 ;;' 'esac'
    } > "$service_temp"
    run_as_root mkdir -p "$NZ_INIT_DIR" || die "Could not create $NZ_INIT_DIR."
    copy_root_file "$service_temp" "$service_path" 755 || die "Could not install $service_path."
    rm -f "$service_temp"
    if has_cmd update-rc.d; then run_as_root update-rc.d nezha-agent defaults || die "Could not enable nezha-agent."; fi
    if has_cmd chkconfig; then run_as_root chkconfig nezha-agent on || die "Could not enable nezha-agent."; fi
    if [ "$NZ_NO_START" != 1 ]; then run_as_root "$service_path" start || die "Could not start nezha-agent."; fi
}

install_cron_keepalive() {
    supervisor_path="$KEEPALIVE_DIR/supervise.sh"
    write_supervisor "$supervisor_path"
    cron_temp="${TMPDIR:-/tmp}/nezha-agent-cron.$$"
    existing_cron="${cron_temp}.existing"
    cron_error="${cron_temp}.error"
    if ! run_as_root env LC_ALL=C crontab -l > "$existing_cron" 2> "$cron_error"; then
        grep -Fqi 'no crontab' "$cron_error" || die "Could not read the existing root crontab."
        : > "$existing_cron"
    fi
    cron_entry="@reboot $(shell_quote "$supervisor_path") >/dev/null 2>&1"
    { grep -Fxv "$cron_entry" "$existing_cron" || true; printf '%s\n' "$cron_entry"; } > "$cron_temp"
    run_as_root crontab "$cron_temp" || die "Could not install the @reboot cron entry."
    rm -f "$existing_cron" "$cron_error"
    rm -f "$cron_temp"
    if [ "$NZ_NO_START" != 1 ]; then start_supervisor; fi
}

remove_cron_entry() {
    cron_owner="$1"
    cron_temp="${TMPDIR:-/tmp}/nezha-agent-cron-remove.$$.$cron_owner"
    if [ "$cron_owner" = root ]; then
        run_as_root env LC_ALL=C crontab -l > "$cron_temp" 2> "${cron_temp}.error" && cron_status=0 || cron_status=$?
    else
        compatibility_env LC_ALL=C crontab -l > "$cron_temp" 2> "${cron_temp}.error" && cron_status=0 || cron_status=$?
    fi
    if [ "$cron_status" -ne 0 ]; then
        if grep -Fqi 'no crontab' "${cron_temp}.error"; then
            rm -f "$cron_temp" "${cron_temp}.error"
            return 0
        fi
        die "Could not read the $cron_owner crontab."
    fi
    cron_entry="@reboot $(shell_quote "$KEEPALIVE_DIR/supervise.sh") >/dev/null 2>&1"
    if grep -Fxq "$cron_entry" "$cron_temp"; then
        grep -Fxv "$cron_entry" "$cron_temp" > "${cron_temp}.new" || true
        if [ "$cron_owner" = root ]; then
            run_as_root crontab "${cron_temp}.new" || die "Could not remove root cron entry."
        else
            crontab "${cron_temp}.new" || die "Could not remove user cron entry."
        fi
    fi
    rm -f "$cron_temp" "${cron_temp}.error" "${cron_temp}.new"
}

install_keepalive() {
    case "$KEEPALIVE_METHOD" in
        openwrt) install_openwrt_keepalive ;;
        systemd) install_systemd_keepalive ;;
        openrc) install_openrc_keepalive ;;
        sysv) install_sysv_keepalive ;;
        busybox-rcs) install_busybox_rcs_keepalive ;;
        cron) install_cron_keepalive ;;
        *) die "No supported boot-time service manager was detected." ;;
    esac
}

prepare_storage_layout() {
    if [ "$STORAGE_MODE" = volatile ]; then
        INSTALL_MODE='volatile binary (restored after reboot)'
        KEEPALIVE_DIR="$NZ_VOLATILE_CONFIG_DIR"
        INSTALL_BINARY_PATH="$NZ_VOLATILE_RUNTIME_DIR/nezha-agent"
        INSTALL_BACKUP_PATH=''
        RUNTIME_ALLOW_DOWNLOAD=1
        other_metadata="$NZ_AGENT_PATH/runtime.env"
    else
        INSTALL_MODE='persistent binary'
        KEEPALIVE_DIR="$NZ_AGENT_PATH"
        INSTALL_BINARY_PATH="$NZ_AGENT_PATH/nezha-agent"
        INSTALL_BACKUP_PATH="$NZ_AGENT_PATH/nezha-agent.backup"
        RUNTIME_ALLOW_DOWNLOAD=0
        other_metadata="$NZ_VOLATILE_CONFIG_DIR/runtime.env"
    fi
    INSTALL_CONFIG_PATH="$KEEPALIVE_DIR/config.yml"
    if [ "$other_metadata" != "$KEEPALIVE_DIR/runtime.env" ] && run_as_root test -f "$other_metadata"; then
        die "Another installation mode is present; uninstall it before changing binary storage."
    fi
    if run_as_root test -f "$KEEPALIVE_DIR/runtime.env"; then
        saved_storage="$(metadata_value "$KEEPALIVE_DIR/runtime.env" NZ_STORAGE_MODE || printf volatile)"
        [ "$saved_storage" = "$STORAGE_MODE" ] ||
            die "Existing installation uses different storage; uninstall it before changing storage modes."
    elif [ "$STORAGE_MODE" = volatile ] && run_as_root test -f "$INSTALL_CONFIG_PATH"; then
        die "Existing volatile configuration has no runtime metadata; refusing to replace it."
    fi
}

validate_keepalive_selection() {
    [ -n "$KEEPALIVE_METHOD" ] || die "No supported boot-time service manager was detected."
    [ -n "$EXISTING_DIR" ] || return 0
    for existing_metadata in "$EXISTING_DIR/runtime.env" "$EXISTING_DIR/install.env"; do
        run_as_root test -f "$existing_metadata" || continue
        saved_method="$(metadata_value "$existing_metadata" NZ_KEEPALIVE_METHOD)" ||
            die "Could not identify the existing service manager."
        [ "$saved_method" = "$KEEPALIVE_METHOD" ] ||
            die "Existing installation uses a different service manager; uninstall it before changing service managers."
        return 0
    done
}

validate_keepalive_update() {
    NATIVE_SYSTEMD_MIGRATION=0
    if [ "$KEEPALIVE_METHOD" != systemd ] && run_as_root test -e "$NZ_SYSTEMD_DIR/nezha-agent.service"; then
        die "An existing systemd installation must be removed before changing service managers."
    fi
    if ! run_as_root test -f "$KEEPALIVE_DIR/runtime.env"; then
        for legacy_config in "$KEEPALIVE_DIR"/config-*.yml; do
            run_as_root test -f "$legacy_config" || continue
            die "Legacy additional agent configuration found: $legacy_config. Uninstall the old instances before installing again."
        done
        if [ "$KEEPALIVE_METHOD" = systemd ] &&
            { run_as_root test -e "$NZ_SYSTEMD_DIR/nezha-agent.service" ||
                run_as_root test -L "$NZ_SYSTEMD_DIR/nezha-agent.service"; }; then
            assert_owned_service_file "$NZ_SYSTEMD_DIR/nezha-agent.service" "$(persistent_unit_command "$INSTALL_CONFIG_PATH")"
            NATIVE_SYSTEMD_MIGRATION=1
        fi
    fi
    case "$KEEPALIVE_METHOD" in
        systemd)
            if [ "$NATIVE_SYSTEMD_MIGRATION" != 1 ]; then
                assert_owned_service_file "$NZ_SYSTEMD_DIR/nezha-agent.service" "ExecStart=$(systemd_quote_path "$KEEPALIVE_DIR/run.sh")"
            fi ;;
        openwrt) assert_owned_service_file "$NZ_OPENWRT_INIT_DIR/nezha-agent" "    procd_set_param command $(shell_quote "$KEEPALIVE_DIR/run.sh")" ;;
        openrc) assert_owned_service_file "$NZ_INIT_DIR/nezha-agent" "command=$(shell_quote "$KEEPALIVE_DIR/run.sh")" ;;
        sysv) assert_owned_service_file "$NZ_INIT_DIR/nezha-agent" "SUPERVISOR=$(shell_quote "$KEEPALIVE_DIR/supervise.sh")" ;;
    esac
}

stop_existing_keepalive() {
    case "$KEEPALIVE_METHOD" in
        systemd)
            if run_as_root test -f "$NZ_SYSTEMD_DIR/nezha-agent.service"; then
                run_as_root systemctl stop nezha-agent.service || die "Could not stop the existing systemd service."
            fi ;;
        openwrt)
            if run_as_root test -f "$NZ_OPENWRT_INIT_DIR/nezha-agent"; then
                run_as_root "$NZ_OPENWRT_INIT_DIR/nezha-agent" stop || die "Could not stop the existing OpenWrt service."
            fi ;;
        openrc)
            if run_as_root test -f "$NZ_INIT_DIR/nezha-agent"; then
                run_as_root rc-service nezha-agent stop || die "Could not stop the existing OpenRC service."
            fi ;;
        sysv|busybox-rcs|cron) stop_supervisor || die "Could not stop the existing supervisor." ;;
    esac
}

apply_managed_installation() {
    if config_matches_requested "$INSTALL_CONFIG_PATH"; then
        info "Existing configuration matches requested parameters: $INSTALL_CONFIG_PATH"
    else
        info "Writing requested configuration: $INSTALL_CONFIG_PATH"
        write_requested_config "$INSTALL_CONFIG_PATH"
    fi
    write_runtime_env "$KEEPALIVE_DIR/runtime.env"
    write_runner "$KEEPALIVE_DIR/run.sh"

    install_staged_binary "$INSTALL_BINARY_PATH" || die "Could not install the agent binary."
    install_keepalive
}

save_legacy_systemd_state() {
    migration_snapshot="$WORK_DIR/legacy-systemd"
    migration_had_binary=0
    if run_as_root test -f "$INSTALL_BINARY_PATH"; then migration_had_binary=1; fi
    mkdir -p "$migration_snapshot" || die "Could not prepare legacy migration backup."
    for migration_file in config.yml run.sh runtime.env; do
        if run_as_root test -f "$KEEPALIVE_DIR/$migration_file"; then
            run_as_root cp -p "$KEEPALIVE_DIR/$migration_file" "$migration_snapshot/$migration_file" ||
                die "Could not back up legacy $migration_file."
        fi
    done
    run_as_root cp -p "$NZ_SYSTEMD_DIR/nezha-agent.service" "$migration_snapshot/nezha-agent.service" ||
        die "Could not back up the legacy systemd service."
}

restore_legacy_systemd_state() {
    for migration_file in config.yml run.sh runtime.env; do
        if run_as_root test -f "$migration_snapshot/$migration_file"; then
            run_as_root cp -p "$migration_snapshot/$migration_file" "$KEEPALIVE_DIR/$migration_file" || return 1
        else
            run_as_root rm -f "$KEEPALIVE_DIR/$migration_file" || return 1
        fi
    done
    if [ "$migration_had_binary" = 1 ]; then
        copy_root_file_atomic "$INSTALL_BACKUP_PATH" "$INSTALL_BINARY_PATH" 755 || return 1
    else
        run_as_root rm -f "$INSTALL_BINARY_PATH" || return 1
    fi
    run_as_root cp -p "$migration_snapshot/nezha-agent.service" "$NZ_SYSTEMD_DIR/nezha-agent.service" || return 1
    run_as_root systemctl daemon-reload || return 1
    if [ "$NZ_NO_START" != 1 ]; then
        run_as_root systemctl start nezha-agent.service || return 1
    fi
}

install_managed_agent() {
    info "Binary storage: $STORAGE_MODE ($INSTALL_BINARY_PATH)"
    info "Config directory: $KEEPALIVE_DIR"
    info "Boot keepalive method: $KEEPALIVE_METHOD"

    if [ -n "$INSTALL_BACKUP_PATH" ] && run_as_root test -f "$INSTALL_BINARY_PATH"; then
        run_as_root cp -f "$INSTALL_BINARY_PATH" "$INSTALL_BACKUP_PATH" ||
            die "Could not back up existing agent."
    fi
    if [ "$NATIVE_SYSTEMD_MIGRATION" = 1 ]; then
        save_legacy_systemd_state
    fi
    stop_existing_keepalive
    if [ "$NATIVE_SYSTEMD_MIGRATION" = 1 ]; then
        if (apply_managed_installation); then
            TEMP_BINARY=''
            DOWNLOAD_DIR=''
            NATIVE_SYSTEMD_MIGRATION=0
        else
            restore_legacy_systemd_state || die "Legacy migration failed and rollback needs manual recovery. Backups: $INSTALL_BACKUP_PATH"
            die "Legacy migration failed; previous systemd service, binary and configuration restored."
        fi
    else
        apply_managed_installation
    fi
    register_installation
    run_as_root rm -f "$KEEPALIVE_DIR/install.env" || die "Could not remove migrated legacy metadata."

    success "Nezha Agent installed with $STORAGE_MODE binary storage."
    if [ "$STORAGE_MODE" = volatile ]; then
        success "After each reboot, $KEEPALIVE_DIR/run.sh verifies or downloads the binary before starting it."
    fi
    notify_result success
    rm -f "$LOG_FILE"
}

install_agent() {
    require_install_tools
    [ -n "${NZ_SERVER:-}" ] || die "NZ_SERVER must not be empty."
    [ -n "${NZ_CLIENT_SECRET:-}" ] || die "NZ_CLIENT_SECRET must not be empty."
    validate_config_booleans

    select_install_directories
    KEEPALIVE_METHOD="$(detect_keepalive_method || true)"
    validate_keepalive_selection
    if skip_if_same_configuration; then
        return 0
    fi

    detect_platform
    select_storage_mode
    prepare_storage_layout
    validate_keepalive_update
    prepare_download_directory "${INSTALL_BINARY_PATH%/*}"
    download_rust_asset
    ensure_binary_backup_space
    install_managed_agent
}

check_download() {
    detect_platform
    check_download_parent=''
    for candidate in "${TMPDIR:-/tmp}" /var/tmp /tmp "$NZ_AGENT_PATH" "$NZ_VOLATILE_RUNTIME_DIR"; do
        [ -d "$candidate" ] && [ -w "$candidate" ] && ! path_is_noexec "$candidate" || continue
        has_free_space "$candidate" "$ASSET_SIZE" || continue
        check_download_parent="$candidate"
        break
    done
    [ -n "$check_download_parent" ] || die "No writable executable directory has enough space for download verification. Set TMPDIR."
    prepare_download_directory "$check_download_parent"
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

    compatibility_shell -c "$debug_command" > "$DEBUG_OUTPUT" 2>&1
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

persistent_unit_name() {
    config_file="$1"
    if [ "$config_file" = "$NZ_AGENT_PATH/config.yml" ]; then
        printf '%s\n' nezha-agent.service
        return 0
    fi
    if has_cmd md5sum; then
        digest="$(printf '%s' "$config_file" | md5sum | awk '{print $1}')"
    elif busybox_has_applet md5sum; then
        digest="$(printf '%s' "$config_file" | "$BUSYBOX_PATH" md5sum | awk '{print $1}')"
    elif has_cmd openssl; then
        digest="$(printf '%s' "$config_file" | openssl dgst -md5 | sed 's/^.*= //')"
    else
        return 1
    fi
    [ -n "$digest" ] || return 1
    printf 'nezha-agent-%s.service\n' "$(printf '%s' "$digest" | cut -c 1-7)"
}

uninstall_runner_agent() {
    metadata="$KEEPALIVE_DIR/runtime.env"
    run_as_root test -f "$metadata" || return 1
    if [ "$STORAGE_MODE" = persistent ]; then
        run_as_root grep -Fxq "NZ_STORAGE_MODE='persistent'" "$metadata" ||
            die "Persistent installation metadata is inconsistent."
    elif run_as_root grep -q '^NZ_STORAGE_MODE=' "$metadata"; then
        run_as_root grep -Fxq "NZ_STORAGE_MODE='volatile'" "$metadata" ||
            die "Volatile installation metadata is inconsistent."
    fi
    method=''
    for candidate in systemd openwrt openrc sysv busybox-rcs cron; do
        if run_as_root grep -Fxq "NZ_KEEPALIVE_METHOD='$candidate'" "$metadata"; then
            method="$candidate"
            break
        fi
    done
    [ -n "$method" ] || die "Could not identify the installation's service manager."

    case "$method" in
        systemd)
            service_path="$NZ_SYSTEMD_DIR/nezha-agent.service"
            assert_owned_service_file "$service_path" "ExecStart=$(systemd_quote_path "$KEEPALIVE_DIR/run.sh")"
            run_as_root test -f "$service_path" || die "Cannot verify the systemd service; configuration was retained."
            run_as_root systemctl stop nezha-agent.service || die "Could not stop systemd service."
            run_as_root systemctl disable nezha-agent.service || die "Could not disable systemd service."
            run_as_root rm -f "$service_path" || die "Could not remove systemd service."
            run_as_root systemctl daemon-reload || die "Could not reload systemd."
            ;;
        openwrt)
            service_path="$NZ_OPENWRT_INIT_DIR/nezha-agent"
            assert_owned_service_file "$service_path" "    procd_set_param command $(shell_quote "$KEEPALIVE_DIR/run.sh")"
            run_as_root test -f "$service_path" || die "Cannot verify the OpenWrt service; configuration was retained."
            run_as_root "$service_path" stop || die "Could not stop OpenWrt service."
            run_as_root "$service_path" disable || die "Could not disable OpenWrt service."
            run_as_root rm -f "$service_path" || die "Could not remove OpenWrt service."
            ;;
        openrc)
            service_path="$NZ_INIT_DIR/nezha-agent"
            assert_owned_service_file "$service_path" "command=$(shell_quote "$KEEPALIVE_DIR/run.sh")"
            run_as_root test -f "$service_path" || die "Cannot verify the OpenRC service; configuration was retained."
            run_as_root rc-service nezha-agent stop || die "Could not stop OpenRC service."
            run_as_root rc-update del nezha-agent default || die "Could not disable OpenRC service."
            run_as_root rm -f "$service_path" || die "Could not remove OpenRC service."
            ;;
        sysv)
            service_path="$NZ_INIT_DIR/nezha-agent"
            assert_owned_service_file "$service_path" "SUPERVISOR=$(shell_quote "$KEEPALIVE_DIR/supervise.sh")"
            run_as_root test -f "$service_path" || die "Cannot verify the SysV service; configuration was retained."
            stop_supervisor || die "Could not stop SysV supervisor."
            if has_cmd update-rc.d; then run_as_root update-rc.d -f nezha-agent remove || die "Could not disable SysV service."; fi
            if has_cmd chkconfig; then run_as_root chkconfig nezha-agent off || die "Could not disable SysV service."; fi
            run_as_root rm -f "$service_path" || die "Could not remove SysV service."
            run_as_root rm -f /var/run/nezha-agent.pid || die "Could not remove SysV PID file."
            ;;
        busybox-rcs)
            stop_supervisor || die "Could not stop BusyBox supervisor."
            remove_busybox_rcs_hook
            ;;
        cron)
            stop_supervisor || die "Could not stop cron supervisor."
            remove_cron_entry root
            if [ "$(id -u)" != 0 ]; then remove_cron_entry user; fi
            ;;
    esac
    run_as_root rm -f "$KEEPALIVE_DIR/config.yml" "$metadata" \
        "$KEEPALIVE_DIR/run.sh" "$KEEPALIVE_DIR/supervise.sh" ||
        die "Could not remove installation files."
    run_as_root rm -f "$INSTALL_BINARY_PATH" "$KEEPALIVE_DIR/nezha-agent.backup" || die "Could not remove installed binary."
    run_as_root rmdir "${INSTALL_BINARY_PATH%/*}" 2>/dev/null || true
    run_as_root rmdir "$KEEPALIVE_DIR" 2>/dev/null || true
    return 0
}

uninstall_agent() {
    discover_installations
    uninstall_found=0
    while IFS= read -r uninstall_directory; do
        if run_as_root test -f "$uninstall_directory/runtime.env" ||
            run_as_root test -f "$uninstall_directory/install.env" ||
            run_as_root test -f "$uninstall_directory/config.yml"; then
            uninstall_discovered_directory "$uninstall_directory" || die "Could not safely uninstall $uninstall_directory."
            uninstall_found=1
        fi
    done < "$DISCOVERY_FILE"
    [ "$uninstall_found" = 1 ] || info "No installer-managed installation was found."
    success "Uninstallation completed."
}

uninstall_discovered_directory() (
    uninstall_directory="$1"
    NZ_AGENT_PATH="$uninstall_directory"
    info "Discovered uninstall directory: $uninstall_directory"
    if run_as_root test -f "$uninstall_directory/runtime.env"; then
        read_installation_metadata "$uninstall_directory" || die "Invalid runtime metadata; installation retained."
        uninstall_runner_agent || return 1
        unregister_installation "$uninstall_directory"
        return 0
    fi
    if run_as_root test -f "$uninstall_directory/install.env"; then
        read_installation_metadata "$uninstall_directory" || die "Invalid installation metadata; installation retained."
    fi
    native_removed=0
    native_retained=0
    for native_config in "$uninstall_directory"/config*.yml; do
        run_as_root test -f "$native_config" || continue
        native_unit="$(persistent_unit_name "$native_config")" || die "Could not determine service name for $native_config."
        native_unit_path="$(persistent_unit_path "$native_unit")"
        if ! run_as_root test -f "$native_unit_path"; then
            info "No matching service for $native_config; unverified files retained."
            native_retained=1
            continue
        fi
        assert_owned_service_file "$native_unit_path" "$(persistent_unit_command "$native_config")"
        run_as_root systemctl stop "$native_unit" || die "Could not stop $native_unit; files retained."
        run_as_root systemctl disable "$native_unit" || die "Could not disable $native_unit; files retained."
        run_as_root rm -f "$native_unit_path" || die "Could not remove $native_unit."
        run_as_root systemctl daemon-reload || die "Could not reload systemd."
        run_as_root rm -f "$native_config" || die "Could not remove $native_config."
        native_removed=1
    done
    if [ "$native_removed" = 1 ] && [ "$native_retained" = 0 ]; then
        run_as_root rm -f "$uninstall_directory/nezha-agent" "$uninstall_directory/nezha-agent.backup" \
            "$uninstall_directory/install.env" || die "Could not remove installation files."
        unregister_installation "$uninstall_directory"
        run_as_root rmdir "$uninstall_directory" 2>/dev/null || true
    fi
)

: > "$LOG_FILE" 2>/dev/null || LOG_FILE="/dev/null"
trap cleanup EXIT
trap 'cleanup; exit 1' HUP INT TERM
case "${1:-}" in
    --help|-h) require_tools cat ;;
    *)
        require_tools awk sed grep cut tr head tail wc df mktemp mkdir rmdir cp mv rm chmod cat env uname id sh
        initialize_workspace ;;
esac

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
  NZ_VOLATILE_CONFIG_DIR, NZ_VOLATILE_RUNTIME_DIR, NZ_BUSYBOX_RCS_PATH
  NZ_INSTALL_REGISTRY (persistent installation discovery registry)
  TMPDIR (installer workspace preference; automatically falls back if full)
  NZ_BUSYBOX_PATH (BusyBox executable; automatically detected when unset)
  NZ_INIT_DIR (OpenRC and SysV service directory; default /etc/init.d)
  TG_BOT_TOKEN, TG_CHAT_ID

Command debug mode executes the trusted command through /bin/sh -c, sends its
combined output and exit code to Telegram, and does not install Nezha Agent.

Missing native utilities automatically use available BusyBox applets. The same
fallback is embedded in generated runner, supervisor and SysV scripts and works
through sudo and boot hooks without creating system-wide symlinks. BusyBox must
include the needed applets; HTTPS, root access and a boot service still matter.

Binary storage and boot keepalive are selected independently. Every backend
(systemd, OpenWrt, OpenRC, SysV, BusyBox rcS or cron) uses the same run.sh entry,
regardless of persistent or volatile binary storage. Only startup recovery
policy and binary paths depend on storage. Owned legacy direct-binary systemd
services are migrated to run.sh once, then matching installations can be skipped.

Installation directories are discovered from metadata, service files, boot
hooks, crontab and the persistent registry. Otherwise writable persistent
directories are selected automatically. Explicit NZ_AGENT_PATH/NZ_BASE_PATH
settings take precedence for new installations.

An intact installation with matching server, secret, TLS and feature flags
is skipped before downloading. NZ_UUID is compared only when supplied;
configuration updates preserve the existing UUID when NZ_UUID is unset.

Storage and free space are checked before downloading. The binary is staged
inside its final directory, verified, then atomically renamed without an extra
binary copy or a dependency on /tmp space. Existing binaries remain untouched
if the download fails. Small installer files use a workspace with free-space
checks and fallback locations; TMPDIR can select a writable workspace.

With NZ_FORCE_VOLATILE=1, a read-only/volatile binary directory, noexec mounts,
or insufficient persistent space, the installer keeps
configuration and runner files persistently, stores the binary under /tmp,
and verifies or downloads it after every reboot. Otherwise the binary stays
under NZ_AGENT_PATH, regardless of the detected init system.

Uninstall discovers managed installations without the original path options,
verifies service ownership, and removes services, configs, runners and binaries.
Unrelated files are retained. Persistent writable config storage is required;
fully read-only systems must provide NZ_VOLATILE_CONFIG_DIR on a persistent mount.

NZ_ARCH values: amd64, 386, arm5, arm6, armv7_softfloat,
                armv7_hardfloat, arm64, mips, mipsle, mips64,
                mips64le, ppc, ppc64, ppc64le, s390x
EOF
        ;;
    '')
        install_agent
        ;;
    *)
        die "Unknown option: $1"
        ;;
esac

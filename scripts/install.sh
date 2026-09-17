#!/bin/sh
# Keep the implementation inside one function so a partial download cannot
# begin installation before the shell has read the complete function.
sericon_install() {
    set -eu
    sericon_version=0.1.0
    sericon_prefix=${HOME:?HOME must be set}/.local
    sericon_work=
    sericon_binary_tmp=
    sericon_archive_tmp=

    sericon_fail() { printf 'sericon: %s\n' "$*" >&2; exit 1; }
    sericon_cleanup() {
        [ -z "$sericon_binary_tmp" ] || rm -f -- "$sericon_binary_tmp"
        [ -z "$sericon_archive_tmp" ] || rm -f -- "$sericon_archive_tmp"
        [ -z "$sericon_work" ] || rm -rf -- "$sericon_work"
    }
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --prefix)
                [ "$#" -ge 2 ] || sericon_fail '--prefix needs an absolute directory'
                sericon_prefix=$2
                shift 2
                ;;
            -h|--help)
                printf '%s\n' "Install Sericon $sericon_version for Linux." \
                    'Usage: sh install.sh [--prefix /absolute/directory]' \
                    'Default prefix: $HOME/.local' \
                    'Installs bin/sericon and keeps the release archive in share/sericon/.' \
                    'Verifies the pinned SHA-256. Does not use sudo or edit shell startup files.'
                return
                ;;
            *) sericon_fail "unknown option: $1 (use --help)" ;;
        esac
    done
    case "$sericon_prefix" in /*) ;; *) sericon_fail '--prefix must be an absolute directory' ;; esac
    [ "$(uname -s)" = Linux ] || sericon_fail 'only Linux is supported'
    sericon_machine=$(uname -m)
    # A 32-bit Raspberry Pi userspace can run on an ARM64 kernel.
    if [ "$sericon_machine" = aarch64 ] && command -v getconf >/dev/null 2>&1; then
        if [ "$(getconf LONG_BIT 2>/dev/null || :)" = 32 ]; then
            sericon_machine=armv7l
        fi
    fi
    case "$sericon_machine" in
        x86_64)
            sericon_arch=x86_64
            sericon_sha=aa26cabe93f390ba3928f3cb92b83073b8b4051f85f0712e6226c19d503173ba
            ;;
        aarch64)
            sericon_arch=aarch64
            sericon_sha=7189458c24702374f0add076ca042bf6a2211802aee3835387e13a69f4bb0587
            ;;
        armv6l|armv7l|armv8l)
            sericon_arch=armv6hf
            sericon_sha=c47ea950cd531432c149c57bea6d01fbed5614641ac32316b2e959235f555cb2
            ;;
        *) sericon_fail "unsupported Linux architecture: $sericon_machine" ;;
    esac
    for sericon_tool in curl sha256sum tar mktemp mkdir cp chmod mv rm; do
        command -v "$sericon_tool" >/dev/null 2>&1 || sericon_fail "required command not found: $sericon_tool"
    done
    sericon_name=sericon-$sericon_version-linux-$sericon_arch
    sericon_archive=$sericon_name.tar.gz
    sericon_url=https://github.com/digitalandrew/sericon/releases/download/v$sericon_version/$sericon_archive
    trap sericon_cleanup 0
    trap 'exit 1' 1 2 3 15
    umask 022
    sericon_work=$(mktemp -d "${TMPDIR:-/tmp}/sericon-install.XXXXXXXX")
    printf 'Downloading Sericon %s for Linux %s...\n' "$sericon_version" "$sericon_arch"
    curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
        --tlsv1.2 --connect-timeout 15 --max-time 180 --retry 3 \
        --output "$sericon_work/$sericon_archive" "$sericon_url" || sericon_fail 'download failed; installation unchanged'
    (cd "$sericon_work" && printf '%s  %s\n' "$sericon_sha" "$sericon_archive" | sha256sum --check --status -) \
        || sericon_fail 'checksum mismatch; installation unchanged'
    # Extract just the executable from the verified release archive.
    tar -xzf "$sericon_work/$sericon_archive" -C "$sericon_work" "$sericon_name/sericon"
    sericon_version_output=$("$sericon_work/$sericon_name/sericon" --version) \
        || sericon_fail 'this executable cannot run on this host; installation unchanged'
    [ "$sericon_version_output" = "sericon $sericon_version" ] || sericon_fail 'executable version mismatch'

    sericon_bin=$sericon_prefix/bin
    sericon_share=$sericon_prefix/share/sericon
    [ ! -d "$sericon_bin/sericon" ] || sericon_fail "$sericon_bin/sericon is a directory"
    [ ! -d "$sericon_share/$sericon_archive" ] || sericon_fail "$sericon_share/$sericon_archive is a directory"
    mkdir -p -- "$sericon_bin" "$sericon_share"
    # Stage in the destination filesystems. Renaming leaves a running broker
    # on its original executable and never exposes a partially copied binary.
    sericon_archive_tmp=$(mktemp "$sericon_share/.download.XXXXXXXX")
    cp -- "$sericon_work/$sericon_archive" "$sericon_archive_tmp"
    chmod 644 "$sericon_archive_tmp"
    mv -f -- "$sericon_archive_tmp" "$sericon_share/$sericon_archive"
    sericon_archive_tmp=
    sericon_binary_tmp=$(mktemp "$sericon_bin/.sericon.XXXXXXXX")
    cp -- "$sericon_work/$sericon_name/sericon" "$sericon_binary_tmp"
    chmod 755 "$sericon_binary_tmp"
    mv -f -- "$sericon_binary_tmp" "$sericon_bin/sericon"
    sericon_binary_tmp=
    printf 'Installed %s to %s/sericon\n' "$sericon_version_output" "$sericon_bin"
    printf 'Release archive, examples and license notices: %s/%s\n' "$sericon_share" "$sericon_archive"
    case :${PATH:-}: in
        *:"$sericon_bin":*) printf 'Run sericon to start; Ctrl-] then m opens the menu.\n' ;;
        *) printf 'Add %s to PATH, or run %s/sericon directly.\n' "$sericon_bin" "$sericon_bin" ;;
    esac
    printf 'Updates apply to new sessions; stop an existing session with Ctrl-] then q.\n'
}

sericon_install "$@"

#!/bin/sh
set -eu

fail() {
    printf 'pVisor installer: %s\n' "$*" >&2
    exit 1
}

download() {
    curl -q --proto '=https' --proto-redir '=https' -fsSL \
        --connect-timeout 15 --max-time 180 --output "$2" "$1" ||
        fail "could not download $1"
}

cleanup() {
    rm -rf "$work"
    if [ "$locked" = yes ]; then rmdir "$install_dir/.install-lock"; fi
}

main() {
    version=
    install_dir=
    channel=stable
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --nightly)
                channel=nightly
                shift ;;
            --version)
                [ "$#" -ge 2 ] && [ -n "$2" ] || fail "--version needs a value"
                version=${2#v}
                printf '%s\n' "$version" |
                    awk 'NR == 1 && /^[0-9]+\.[0-9]+\.[0-9]+$/ { valid = 1 } END { exit !(NR == 1 && valid) }' ||
                    fail "expected a stable version such as 0.4.0"
                shift 2 ;;
            --install-dir)
                [ "$#" -ge 2 ] && [ -n "$2" ] || fail "--install-dir needs a value"
                install_dir=$2
                shift 2 ;;
            -h|--help)
                printf '%s\n' 'Usage: sh install.sh [--nightly | --version X.Y.Z] [--install-dir /absolute/path]' \
                    'Stable (default): $HOME/.local/share/pvisor/cli.' \
                    'Nightly: $HOME/.local/share/pvisor/cli-nightly.'
                return ;;
            *) fail "unknown argument: $1" ;;
        esac
    done

    tag=
    slot=cli
    if [ "$channel" = nightly ]; then
        [ -z "$version" ] || fail "--nightly and --version cannot be combined"
        tag=nightly
        slot=cli-nightly
    elif [ -n "$version" ]; then
        tag=v$version
    fi

    host=$(uname -sm)
    case "$host" in
        'Linux x86_64') platform=linux-x86_64; replace_link=-fT ;;
        'Darwin arm64') platform=macos-arm64; replace_link=-fh ;;
        *) fail "no published native package for $host" ;;
    esac
    command -v curl >/dev/null 2>&1 || fail "curl is required"
    command -v tar >/dev/null 2>&1 || fail "tar is required"
    if command -v sha256sum >/dev/null 2>&1; then
        set -- sha256sum
    elif command -v shasum >/dev/null 2>&1; then
        set -- shasum -a 256
    else
        fail "sha256sum or shasum is required"
    fi

    install_dir=${install_dir:-"${HOME:?HOME is not set}/.local/share/pvisor/$slot"}
    case "$install_dir" in /*) ;; *) fail "--install-dir must be absolute" ;; esac
    umask 077
    mkdir -p "$install_dir"
    work=$(mktemp -d "$install_dir/.stage.XXXXXXXX")
    locked=no
    trap cleanup EXIT
    trap 'exit 1' HUP INT TERM

    releases=https://github.com/DeepLink-org/pvisor/releases
    release_path=${tag:+download/$tag}
    index="$releases/${release_path:-latest/download}/SHA256SUMS"
    download "$index" "$work/SHA256SUMS"
    record=$(awk -v platform="$platform" -v wanted="$version" -v channel="$channel" '
        function valid_version(v) {
            if (channel == "nightly") return v ~ /^[0-9]+\.[0-9]+\.[0-9]+\+g[0-9]+\.[0-9a-f]+$/
            return v ~ /^[0-9]+\.[0-9]+\.[0-9]+$/
        }
        {
            name = $2
            sub(/^\*/, "", name)
            candidate = name
            if (!sub(/^pvisor-/, "", candidate)) next
            sub(/-.*/, "", candidate)
            if (!valid_version(candidate) ||
                name != "pvisor-" candidate "-" platform ".tar.gz" ||
                (wanted != "" && candidate != wanted)) next
            count++
            valid = NF == 2 && length($1) == 64 && $1 ~ /^[0-9a-fA-F]+$/
            record = candidate " " name " " tolower($1)
        }
        END {
            if (count != 1 || !valid) exit 1
            print record
        }' "$work/SHA256SUMS") || fail "expected one valid $platform archive checksum in $index"
    IFS=' ' read -r version asset expected <<EOF
$record
EOF
    download "$releases/download/${tag:-v$version}/$asset" "$work/$asset"
    actual=$("$@" < "$work/$asset") || fail "could not hash $asset"
    [ "${actual%% *}" = "$expected" ] || fail "checksum mismatch for $asset"

    files='LICENSE NOTICE bin/libkrunfw.SOURCE bin/pvisor bin/pvisor-replay bin/pvisor-daemon'
    mkdir -p "$work/payload/bin"
    for file in $files; do
        tar -xOzf "$work/$asset" "$file" > "$work/payload/$file" &&
            [ -s "$work/payload/$file" ] || fail "archive is missing a nonempty $file"
    done
    chmod 755 "$work/payload/bin/pvisor" "$work/payload/bin/pvisor-replay" "$work/payload/bin/pvisor-daemon"

    mkdir "$install_dir/.install-lock" 2>/dev/null ||
        fail "could not lock $install_dir; check for an active installer"
    locked=yes
    [ -L "$install_dir/current" ] || [ ! -e "$install_dir/current" ] ||
        fail "refusing to replace $install_dir/current: it is not a symlink"
    [ ! -L "$install_dir/versions" ] || fail "refusing a symlinked versions directory"
    release="$install_dir/versions/$version"
    mkdir -p "$install_dir/versions"
    if [ -e "$release" ] || [ -L "$release" ]; then
        [ -d "$release" ] && [ ! -L "$release" ] && [ ! -L "$release/bin" ] ||
            fail "invalid installed release at $release"
        for file in $files; do
            [ -f "$release/$file" ] && [ ! -L "$release/$file" ] &&
                cmp -s "$work/payload/$file" "$release/$file" ||
                fail "installed $release differs from the verified archive; choose a new --install-dir"
        done
        for binary in pvisor pvisor-replay pvisor-daemon; do
            [ -x "$release/bin/$binary" ] || fail "installed $release/bin/$binary is not executable"
        done
    else
        mv "$work/payload" "$release"
    fi

    # Replace the link itself so the CLI and both companions change together.
    ln -s "versions/$version" "$work/current"
    mv "$replace_link" "$work/current" "$install_dir/current"
    bin_dir="$install_dir/current/bin"
    printf 'Installed pVisor %s in %s\n' "$version" "$bin_dir"
    case ":$PATH:" in
        *":$bin_dir:"*) ;;
        *) printf 'Add to PATH: %s\n' "$bin_dir" ;;
    esac
}

# A piped script must receive the whole invocation before it can install.
{
    main "$@"
}

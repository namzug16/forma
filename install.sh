#!/bin/sh
# Installs the latest forma CLI release:
#   curl -fsSL https://raw.githubusercontent.com/namzug16/forma/master/install.sh | sh
# Set FORMA_INSTALL_DIR to install somewhere other than ~/.local/bin.
set -eu

# Everything runs from main so a download cut off midway executes nothing.
main() {
    if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != arm64 ]; then
        echo "error: forma currently supports Apple silicon Macs only; Windows and Linux are coming soon" >&2
        exit 1
    fi

    install_directory="${FORMA_INSTALL_DIR:-$HOME/.local/bin}"
    download_directory="$(mktemp -d)"
    trap 'rm -rf "$download_directory"' EXIT

    curl -fsSL -o "$download_directory/forma.tar.gz" \
        https://github.com/namzug16/forma/releases/latest/download/forma-macos-arm64.tar.gz
    tar -xzf "$download_directory/forma.tar.gz" -C "$download_directory"
    mkdir -p "$install_directory"
    install -m 755 "$download_directory/forma" "$install_directory/forma"

    "$install_directory/forma" version
    echo "installed to $install_directory/forma"
    case ":$PATH:" in
        *":$install_directory:"*) ;;
        *) echo "add $install_directory to your PATH to run forma" ;;
    esac
}

main

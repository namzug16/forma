#!/bin/sh
# Prints the Homebrew formula for a released version.
# Usage: homebrew-formula.sh VERSION SHA256_OF_CLI_ARCHIVE
set -eu

version="$1"
archive_sha256="$2"

cat <<FORMULA
class Forma < Formula
  desc "Agent-first, headless 2D design engine"
  homepage "https://github.com/namzug16/forma"
  url "https://github.com/namzug16/forma/releases/download/v${version}/forma-macos-arm64.tar.gz"
  version "${version}"
  sha256 "${archive_sha256}"
  license "MIT"

  depends_on arch: :arm64
  depends_on :macos

  def install
    bin.install "forma"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/forma version")
  end
end
FORMULA

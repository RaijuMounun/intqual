#!/usr/bin/env bash
set -euo pipefail

# a) Senkronizasyon
echo "Syncing with remote..."
git pull origin HEAD

# b) Versiyon Tespiti
VERSION=$(grep '^version = ' crates/cli/Cargo.toml | awk -F'"' '{print $2}')
if [ -z "$VERSION" ]; then
    echo "Error: Could not determine version from Cargo.toml"
    exit 1
fi
echo "Detected version: $VERSION"

# c) Artifact İndirme
TMP_DIR="/tmp/intqual_release"
rm -rf "$TMP_DIR"
mkdir -p "$TMP_DIR"

echo "Downloading release artifacts for v$VERSION..."
# Using gh release download
gh release download "v$VERSION" -p 'intqual-macos-amd64' -p 'intqual-macos-aarch64' -p 'intqual-linux-amd64' -D "$TMP_DIR"

# d) Hashing
echo "Calculating SHA256 hashes..."
if command -v shasum >/dev/null 2>&1; then
    MAC_INTEL_SHA=$(shasum -a 256 "$TMP_DIR/intqual-macos-amd64" | awk '{print $1}')
    MAC_ARM_SHA=$(shasum -a 256 "$TMP_DIR/intqual-macos-aarch64" | awk '{print $1}')
    LINUX_SHA=$(shasum -a 256 "$TMP_DIR/intqual-linux-amd64" | awk '{print $1}')
else
    MAC_INTEL_SHA=$(sha256sum "$TMP_DIR/intqual-macos-amd64" | awk '{print $1}')
    MAC_ARM_SHA=$(sha256sum "$TMP_DIR/intqual-macos-aarch64" | awk '{print $1}')
    LINUX_SHA=$(sha256sum "$TMP_DIR/intqual-linux-amd64" | awk '{print $1}')
fi

echo "macOS Intel SHA: $MAC_INTEL_SHA"
echo "macOS ARM SHA: $MAC_ARM_SHA"
echo "Linux SHA: $LINUX_SHA"

# e) Homebrew Güncellemesi (Heredoc Template)
TAP_DIR="/tmp/homebrew-intqual-tmp"
rm -rf "$TAP_DIR"

echo "Cloning homebrew-intqual tap..."
git clone https://github.com/RaijuMounun/homebrew-intqual.git "$TAP_DIR"

echo "Updating Formula/intqual.rb..."
mkdir -p "$TAP_DIR/Formula"
cat << EOF > "$TAP_DIR/Formula/intqual.rb"
class Intqual < Formula
  desc "A network diagnostic tool"
  homepage "https://github.com/RaijuMounun/intqual"
  version "$VERSION"

  if OS.mac?
    if Hardware::CPU.arm?
      url "https://github.com/RaijuMounun/intqual/releases/download/v$VERSION/intqual-macos-aarch64"
      sha256 "$MAC_ARM_SHA"
    else
      url "https://github.com/RaijuMounun/intqual/releases/download/v$VERSION/intqual-macos-amd64"
      sha256 "$MAC_INTEL_SHA"
    end
  elsif OS.linux?
    url "https://github.com/RaijuMounun/intqual/releases/download/v$VERSION/intqual-linux-amd64"
    sha256 "$LINUX_SHA"
  end

  def install
    if OS.mac? && Hardware::CPU.arm?
      bin.install "intqual-macos-aarch64" => "intqual"
    elsif OS.mac? && Hardware::CPU.intel?
      bin.install "intqual-macos-amd64" => "intqual"
    elsif OS.linux?
      bin.install "intqual-linux-amd64" => "intqual"
    end
  end
end
EOF

echo "Committing and pushing to homebrew tap..."
cd "$TAP_DIR"
git add Formula/intqual.rb
if ! git diff --cached --quiet; then
    git commit -m "chore: release v$VERSION"
    git push origin HEAD
else
    echo "No changes in Formula/intqual.rb"
fi
cd - > /dev/null

# f) Temizlik
echo "Cleaning up..."
rm -rf "$TMP_DIR" "$TAP_DIR"

echo "Distribution process completed successfully!"

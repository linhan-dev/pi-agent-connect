#!/usr/bin/env bash
# Update Formula/pi-agent-connect.rb from the latest pi-agent-connect release.
# Triggered by repository_dispatch from the source repo's release workflow;
# no-op if already current.
set -euo pipefail

SRC_REPO="linhan-dev/pi-agent-connect"
FORMULA="Formula/pi-agent-connect.rb"

release_json="$(gh api "repos/${SRC_REPO}/releases/latest")"
tag="$(jq -r .tag_name <<<"$release_json")"
version="${tag#v}"
echo "latest release: ${tag}"

if [ -f "$FORMULA" ] && grep -q "version \"${version}\"" "$FORMULA"; then
  echo "formula already at ${version}; nothing to do"
  exit 0
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

arm_url="https://github.com/${SRC_REPO}/releases/download/${tag}/pi-agent-connect-aarch64-apple-darwin.tar.gz"
x64_url="https://github.com/${SRC_REPO}/releases/download/${tag}/pi-agent-connect-x86_64-apple-darwin.tar.gz"

curl -fsSL -o "$tmp/arm.tgz" "$arm_url"
curl -fsSL -o "$tmp/x64.tgz" "$x64_url"

arm_sha="$(shasum -a 256 "$tmp/arm.tgz" | cut -d' ' -f1)"
x64_sha="$(shasum -a 256 "$tmp/x64.tgz" | cut -d' ' -f1)"

mkdir -p "$(dirname "$FORMULA")"
cat > "$FORMULA" <<EOF
class PiAgentConnect < Formula
  desc "Minimal Discord DM gateway for the pi coding agent"
  homepage "https://github.com/linhan-dev/pi-agent-connect"
  license "MIT"
  version "${version}"

  on_macos do
    if Hardware::CPU.arm?
      url "${arm_url}"
      sha256 "${arm_sha}"
    else
      url "${x64_url}"
      sha256 "${x64_sha}"
    end
  end

  def install
    bin.install "pi-agent-connect"
  end

  test do
    system "#{bin}/pi-agent-connect", "--version"
  end
end
EOF

echo "wrote ${FORMULA} for ${tag}"

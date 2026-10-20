#!/bin/sh
# Install the latest bonsai release and its Agent Skill:
#   curl -fsSL https://raw.githubusercontent.com/aymericbeaumet/bonsai/main/install.sh | sh
set -eu

repo="aymericbeaumet/bonsai"
dir="${1:-$HOME/.local/bin}"

case "$(uname -s)" in
  Linux) os=linux ;;
  Darwin) os=darwin ;;
  *) echo "bonsai: unsupported OS $(uname -s); see https://github.com/$repo#install" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) arch=amd64 ;;
  arm64 | aarch64) arch=arm64 ;;
  *) echo "bonsai: unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac

asset="bonsai-$os-$arch.tar.gz"
url="https://github.com/$repo/releases/latest/download"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

curl -fsSL -o "$tmp/$asset" "$url/$asset"
curl -fsSL -o "$tmp/SHA256SUMS" "$url/SHA256SUMS"
(
  cd "$tmp"
  grep " $asset\$" SHA256SUMS > expected
  if command -v sha256sum > /dev/null; then sha256sum -c expected; else shasum -a 256 -c expected; fi
) > /dev/null

tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$dir"
mv "$tmp/bonsai" "$dir/bonsai"
echo "bonsai: installed $("$dir/bonsai" --version) to $dir/bonsai"

"$dir/bonsai" skill install

case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "bonsai: add $dir to your PATH" ;;
esac
echo "bonsai: enable the shell integration with eval \"\$(bonsai init zsh)\" (or bash, fish)"

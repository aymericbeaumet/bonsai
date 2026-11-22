#!/bin/sh
# Re-records docs/demo/demo.gif from this checkout. Everything runs in a
# container, so the recording is reproducible and never touches your own
# repositories or ~/.bonsai. Requires Colima on macOS or Docker Engine on Linux.
set -eu

dir=$(cd "$(dirname "$0")" && pwd)
docker-cli-plugin-docker-buildx build --load --file "$dir/Dockerfile" --tag bonsai-demo "$dir/../.."
recording=$(mktemp -d "$dir/.recording.XXXXXXXX")
trap 'rm -rf "$recording"' EXIT
cp "$dir/demo.tape" "$dir/setup.sh" "$recording/"
docker run --rm --volume "$recording:/vhs" bonsai-demo demo.tape
test -s "$recording/demo.gif"
mv "$recording/demo.gif" "$dir/demo.gif"

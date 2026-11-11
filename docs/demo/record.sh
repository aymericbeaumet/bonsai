#!/bin/sh
# Re-records docs/demo/demo.gif from this checkout. Everything runs in a
# container, so the recording is reproducible and never touches your own
# repositories or ~/.bonsai. Requires Colima on macOS or Docker Engine on Linux.
set -eu

dir=$(cd "$(dirname "$0")" && pwd)
docker build --file "$dir/Dockerfile" --tag bonsai-demo "$dir/../.."
docker run --rm --volume "$dir:/vhs" bonsai-demo demo.tape

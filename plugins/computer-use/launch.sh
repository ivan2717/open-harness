#!/bin/sh
set -eu
exec "${OPENHARNESS_NODE_BINARY:-node}" "$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)/server.mjs"

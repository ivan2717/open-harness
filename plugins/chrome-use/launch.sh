#!/bin/sh
set -eu
plugin_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
exec "${OPENHARNESS_NODE_BINARY:-node}" "$plugin_dir/server.mjs" "$@"

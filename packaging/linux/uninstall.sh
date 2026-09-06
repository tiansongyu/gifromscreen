#!/bin/sh
set -eu
bundle_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
exec python3 "$bundle_dir/installer.py" uninstall "$@"

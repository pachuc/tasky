#!/bin/sh
# Regenerate the agent skill's command reference from the CLI itself.
set -eu
cd "$(dirname "$0")/.."
cargo build --locked -q -p tasky-cli
./target/debug/tasky reference > skills/tasky/reference.md
echo "wrote skills/tasky/reference.md"

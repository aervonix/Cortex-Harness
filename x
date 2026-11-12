#!/bin/sh
# ./x <command> [args]: the developer commands, from any directory.
# It is `cargo xtask <command>`. Run ./x with no arguments for the list.
cd "$(dirname "$0")" || exit 1
exec cargo run --quiet --package xtask -- "$@"

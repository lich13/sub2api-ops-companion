#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
source_dir=server/protocol/src
config=server/protocol/typeshare.toml
ts=desktop/src/protocol.ts
typeshare "$source_dir" -c "$config" --lang=typescript --output-file="$ts"
typeshare "$source_dir" -c "$config" --lang=swift --output-file=desktop/macos/Sources/Bridge/Protocol.swift
(cd desktop && pnpm exec prettier --write src/protocol.ts)

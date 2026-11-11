#!/usr/bin/env bash
# Pin every nym-* crate resolved at 1.22.x back to 1.21.6: nym-sdk 1.21.6 (latest on crates.io) does not
# build against the 1.22.x inner crates Nym published without a matching nym-sdk. Run after `cargo update`.
set -u
for round in $(seq 1 20); do
  pk=$(python3 -I -c '
import re
lock=open("Cargo.lock").read()
print(" ".join(f"{m.group(1)}@{m.group(2)}" for m in re.finditer(r"name = \"(nym-[^\"]+)\"\nversion = \"(1\.22[^\"]*)\"",lock)))')
  [ -z "$pk" ] && { echo "all nym crates pinned (round $round)"; exit 0; }
  for p in $pk; do cargo update -q --offline -p "$p" --precise 1.21.6 2>/dev/null || cargo update -q -p "$p" --precise 1.21.6 2>&1 >/dev/null 2>&1; done
done
echo "still unpinned:"; grep -B1 'version = "1.22' Cargo.lock | grep name

#!/usr/bin/env bash
# 提交前的全量门禁（0.x/v1.x 一贯那套）
set -u
cd /d/Projects/Rust/ruyix || exit 1
echo "=== cargo fmt --check"; cargo fmt --check && echo FMT_OK || echo FMT_FAIL
echo "=== cargo clippy --all-targets（要 0 warning）"
cargo clippy --all-targets 2>&1 | grep -E "^(warning|error)" | sort | uniq -c | head -20
echo "=== cargo test -p harness-engine"; cargo test -p harness-engine 2>&1 | grep -E "^test result|^error" | head -5
echo "=== cargo test -p ruyix --bins"; rm -f target/debug/deps/ruyix-*.exe; cargo test -p ruyix --bins 2>&1 | grep -E "^test result|^error" | head -5
echo "=== node scripts/check-style.js"; node scripts/check-style.js 2>&1 | tail -3
echo "=== node scripts/ui-smoke.js"; node scripts/ui-smoke.js 2>&1 | tail -4
echo "=== 评测台自检"; target/debug/examples/rsi_bench.exe --self-check 2>&1 | tail -2
echo ALL_DONE

#!/bin/bash
# 从仓库里的 engine/ 源码编译原生引擎，写入 bin/ 并更新 integrity.json。
set -euo pipefail
bridge_package="$(cd -- "$(dirname -- "$0")/.." && pwd)"
if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != arm64 ]; then
  echo 'This release supports Apple Silicon macOS only.' >&2
  exit 1
fi
manifest="$bridge_package/engine/Cargo.toml"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$bridge_package/engine/target}"
cargo test --manifest-path "$manifest" --release --locked -p cursor-server --lib
cargo test --manifest-path "$manifest" --release --locked -p cursor-server --test lodestar_replay_repro
cargo build --manifest-path "$manifest" --release --locked -p cursor-server --bin cursor-magpie-bridge
mkdir -p "$bridge_package/bin"
cp "$CARGO_TARGET_DIR/release/cursor-magpie-bridge" "$bridge_package/bin/cursor-magpie-bridge.new"
chmod 755 "$bridge_package/bin/cursor-magpie-bridge.new"
codesign --force --sign - "$bridge_package/bin/cursor-magpie-bridge.new"
mv "$bridge_package/bin/cursor-magpie-bridge.new" "$bridge_package/bin/cursor-magpie-bridge"
python3 -I - "$bridge_package" <<'PY'
import hashlib,json,pathlib,subprocess,sys
p=pathlib.Path(sys.argv[1]);v=json.loads((p/'package.json').read_text())['version'];b=p/'bin/cursor-magpie-bridge'
engine_version=subprocess.check_output([str(b),'--version'],text=True).strip()
assert engine_version=='0.2.1'
(p/'integrity.json').write_text(json.dumps({'version':v,'engine_version':engine_version,'engine':hashlib.sha256(b.read_bytes()).hexdigest()},indent=2)+'\n')
PY

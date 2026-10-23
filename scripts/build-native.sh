#!/bin/bash
set -euo pipefail
bridge_package="$(cd -- "$(dirname -- "$0")/.." && pwd)"
bridge_build="$(mktemp -d "${TMPDIR:-/tmp}/cursor-bridge-build.XXXXXX")"
trap 'rm -rf -- "$bridge_build"' EXIT
if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != arm64 ]; then
  echo 'This release supports Apple Silicon macOS only.' >&2
  exit 1
fi
source_commit=31e9f5cbc98a7d28cffcb5efbb9323cd3e143354
git init "$bridge_build/source"
git -C "$bridge_build/source" remote add origin https://github.com/leookun/cursor-byok.git
git -C "$bridge_build/source" fetch --depth 1 origin "$source_commit"
git -C "$bridge_build/source" checkout --detach FETCH_HEAD
[ "$(git -C "$bridge_build/source" rev-parse HEAD)" = "$source_commit" ]
git -C "$bridge_build/source" apply "$bridge_package/source.patch"
CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}" cargo test --manifest-path "$bridge_build/source/Cargo.toml" --release --locked -p cursor-server --test lodestar_replay_repro
CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}" cargo build --manifest-path "$bridge_build/source/Cargo.toml" --release --locked -p cursor-server --bin cursor-magpie-bridge
mkdir -p "$bridge_package/bin"
cp "$bridge_build/source/target/release/cursor-magpie-bridge" "$bridge_package/bin/cursor-magpie-bridge.new"
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

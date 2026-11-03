#!/usr/bin/env bash
# Builds refrain's test plugins and packages them the way plugins are
# installed: bundles on macOS, single files on Windows and Linux.
#
#   scripts/build-test-plugins.sh [--release]
#
# Writes target/test-plugins/refrain-test-synth.clap and .vst3.
set -euo pipefail

profile=debug
cargo_flags=()
if [[ "${1:-}" == "--release" ]]; then
  profile=release
  cargo_flags=(--release)
fi

cargo build ${cargo_flags[@]+"${cargo_flags[@]}"} -p refrain-test-synth -p refrain-test-synth-vst3

out=target/test-plugins
rm -rf "$out"
mkdir -p "$out"

# Copies a built library into a macOS bundle.
bundle() {
  local lib=$1 bundle=$2
  local name
  name=$(basename "${bundle%.*}")
  mkdir -p "$bundle/Contents/MacOS"
  cp "target/$profile/lib$lib.dylib" "$bundle/Contents/MacOS/$name"
  cat > "$bundle/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key>
  <string>$name</string>
  <key>CFBundleIdentifier</key>
  <string>dev.refrain.$lib</string>
  <key>CFBundleName</key>
  <string>$name</string>
  <key>CFBundlePackageType</key>
  <string>BNDL</string>
</dict>
</plist>
PLIST
  printf 'BNDL????' > "$bundle/Contents/PkgInfo"
}

case "$(uname -s)" in
  Darwin)
    bundle refrain_test_synth "$out/refrain-test-synth.clap"
    bundle refrain_test_synth_vst3 "$out/refrain-test-synth.vst3"
    ;;
  MINGW* | MSYS* | CYGWIN*)
    cp "target/$profile/refrain_test_synth.dll" "$out/refrain-test-synth.clap"
    cp "target/$profile/refrain_test_synth_vst3.dll" "$out/refrain-test-synth.vst3"
    ;;
  *)
    cp "target/$profile/librefrain_test_synth.so" "$out/refrain-test-synth.clap"
    cp "target/$profile/librefrain_test_synth_vst3.so" "$out/refrain-test-synth.vst3"
    ;;
esac

ls -1 "$out"

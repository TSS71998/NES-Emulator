set -eu

cd "$(dirname "$0")/.."
ROM="${1:-roms/moonlight.nes}"
DIST=web/dist

if [ ! -f "$ROM" ]; then
  echo "can't find $ROM: copy moonlight.nes into roms/ or pass the ROM's path" >&2
  exit 1
fi

cargo build -p nes_web --release --target wasm32-unknown-unknown ${CARGO_FLAGS:-}

rm -rf "$DIST"
mkdir -p "$DIST"
cp web/www/* "$DIST"/
cp target/wasm32-unknown-unknown/release/nes_web.wasm "$DIST"/emulator.wasm
cp "$ROM" "$DIST"/moonlight.nes

if command -v wasm-opt >/dev/null 2>&1; then
  wasm-opt -O3 "$DIST"/emulator.wasm -o "$DIST"/emulator.wasm
fi

echo "built $DIST"
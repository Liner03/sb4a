#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Cross-compile sb4a for Android arm64 against the official NDK bionic sysroot.
#
# Google ships the NDK's clang only for x86_64 Linux hosts, but the linker work
# is done by `rust-lld`, which ships with the Rust toolchain and is
# architecture-agnostic. So nothing from the NDK is ever executed: lld is given
# the NDK sysroot directly instead of going through the clang driver.
set -euo pipefail

cd "$(dirname "$0")/.."

NDK="${ANDROID_NDK_HOME:-$PWD/.ndk/android-ndk-r27c}"
API="${ANDROID_API:-31}"
TARGET=aarch64-linux-android
OUT="${OUT_DIR:-target/android/arm64-v8a}"

PREBUILT="$NDK/toolchains/llvm/prebuilt/linux-x86_64"
SYSROOT="$PREBUILT/sysroot"
LIB="$SYSROOT/usr/lib/$TARGET"
if [ ! -d "$LIB/$API" ]; then
  echo "Android NDK sysroot not found at $API under $LIB (set ANDROID_NDK_HOME)" >&2
  exit 1
fi

# cargo invokes the linker directly with rustc's flags, which are written for a
# GCC driver: `-Wl,` groups and `-nodefaultlibs`. Unwrap them for lld.
RUST_LDD="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin/rust-lld"
WRAPPER="target/android/lld-$TARGET"
mkdir -p "$(dirname "$WRAPPER")"
cat > "$WRAPPER" <<EOF
#!/bin/bash
out=()
for a in "\$@"; do
  case "\$a" in
    -Wl,*) IFS=, read -r -a p <<< "\${a#-Wl,}"; out+=("\${p[@]}") ;;
    -nodefaultlibs) ;;
    *) out+=("\$a") ;;
  esac
done
exec "$RUST_LDD" -flavor gnu "\${out[@]}"
EOF
chmod +x "$WRAPPER"

CLANG_RT="$(find "$PREBUILT/lib/clang" -type d -path "*/lib/linux/aarch64" | head -1)"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$PWD/$WRAPPER"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS="\
-C link-arg=--sysroot=$SYSROOT \
-C link-arg=--dynamic-linker=/system/bin/linker64 \
-C link-arg=-L$LIB/$API -C link-arg=-L$CLANG_RT \
-C link-arg=$LIB/$API/crtbegin_dynamic.o -C link-arg=$LIB/$API/crtend_android.o \
-C link-arg=-z -C link-arg=max-page-size=16384"

mkdir -p "$OUT"
cargo build --release --target "$TARGET" -p sb4a-cli
cp "target/$TARGET/release/sb4a" "$OUT/sb4a"

# Without PT_INTERP the kernel execs it but Android cannot load it.
grep -aq '/system/bin/linker64' "$OUT/sb4a" || {
  echo "missing PT_INTERP /system/bin/linker64" >&2
  exit 1
}
file "$OUT/sb4a"
ls -lh "$OUT/sb4a"

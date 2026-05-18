#!/bin/bash
set -e

# This script sets up the environment for cross-compilation on macOS.

HOST_UNAME=$(uname)
TARGET_TRIPLE="$1"
LINUX_OPENSSL_SYSROOT="$HOME/opt/openssl_for_cross"

if [[ -z "$TARGET_TRIPLE" ]]; then
  echo "No target specified. Defaulting to x86_64-unknown-linux-gnu"
  TARGET_TRIPLE="x86_64-unknown-linux-gnu"
fi

## check_target
check_target() {
  local target="$1"
  if ! rustup target list | grep -q "^$target (installed)"; then
    echo "Target $target not installed. Installing..."
    rustup target add "$target"

    # Verify installation was successful
    if ! rustup target list | grep -q "^$target (installed)"; then
      echo "Error: Failed to install target $target" >&2
      return 1
    fi

    echo "Successfully installed target $target"
  else
    echo "Target $target is already installed."
  fi
  return 0
}

echo "Building for target: $TARGET_TRIPLE on host: $HOST_UNAME"

if [[ "$HOST_UNAME" == "Darwin" ]]; then
  case "$TARGET_TRIPLE" in
    x86_64-unknown-linux-gnu)
      if ! command -v x86_64-unknown-linux-gnu-gcc >/dev/null 2>&1; then
        echo "Error: x86_64-unknown-linux-gnu-gcc not found in PATH!" >&2
        echo "Please install with:" >&2
        echo "  brew tap messense/macos-cross-toolchains" >&2
        echo "  brew install x86_64-unknown-linux-gnu" >&2
        exit 1
      fi
      if [ ! -d "$LINUX_OPENSSL_SYSROOT/lib" ] || [ ! -d "$LINUX_OPENSSL_SYSROOT/include" ]; then
        echo "Error: Linux OpenSSL sysroot not found at $LINUX_OPENSSL_SYSROOT" >&2
        echo "Please copy the OpenSSL headers and libraries from your Linux machine to that location." >&2
        exit 1
      fi

      # Disable sccache and mold for cross-compilation
      export SCCACHE_DISABLE="1"
      export RUSTC_WRAPPER=""

      export CC="x86_64-unknown-linux-gnu-gcc"
      export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER="x86_64-unknown-linux-gnu-gcc"
      export LDFLAGS_x86_64_unknown_linux_gnu="-L$LINUX_OPENSSL_SYSROOT/lib"

      # Point to the OpenSSL sysroot for the target
      export OPENSSL_DIR="$LINUX_OPENSSL_SYSROOT"
      export OPENSSL_STATIC=1
      export PKG_CONFIG_ALLOW_CROSS=1

      # Prevent macOS-specific flags from being passed to the Linux compiler
      unset MACOSX_DEPLOYMENT_TARGET
      export CFLAGS_x86_64_unknown_linux_gnu=""
      export CXXFLAGS_x86_64_unknown_linux_gnu=""
      export TARGET_ARCH="x86_64"

      # Match Linux build optimization settings and strip symbols
      export RUSTFLAGS="-C opt-level=3 -C lto=fat -C codegen-units=1 -C strip=symbols"
      
            LINUX_SQLITE_SYSROOT="$HOME/opt/sqlite_for_cross"

      # Disable sccache and mold for cross-compilation
      export SCCACHE_DISABLE="1"
      export RUSTC_WRAPPER=""

      # Use target-specific CC/AR to prevent macOS flags leaking
      export CC_x86_64_unknown_linux_gnu="x86_64-unknown-linux-gnu-gcc"
      export AR_x86_64_unknown_linux_gnu="x86_64-unknown-linux-gnu-ar"
      export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER="x86_64-unknown-linux-gnu-gcc"
      
      # Unset global CC to force target-specific
      unset CC

      # OpenSSL
      export OPENSSL_DIR="$LINUX_OPENSSL_SYSROOT"
      export OPENSSL_STATIC=1
      export PKG_CONFIG_ALLOW_CROSS=1

      # SQLite - use prebuilt libs, don't compile from source
      export SQLITE3_LIB_DIR="$LINUX_SQLITE_SYSROOT/lib"
      export SQLITE3_INCLUDE_DIR="$LINUX_SQLITE_SYSROOT/include"

      # Prevent macOS flags
      unset MACOSX_DEPLOYMENT_TARGET
      export CFLAGS_x86_64_unknown_linux_gnu=""
      export CXXFLAGS_x86_64_unknown_linux_gnu=""
      
      ;;

    x86_64-unknown-freebsd)
      if ! command -v x86_64-unknown-freebsd13-clang >/dev/null 2>&1; then
        echo "Error: x86_64-unknown-freebsd13-clang not found in PATH!" >&2
        echo "Please install with:" >&2
        echo "  brew tap messense/macos-cross-toolchains" >&2
        echo "  brew install x86_64-unknown-freebsd13" >&2
        exit 1
      fi
      if ! command -v ld.lld >/dev/null 2>&1; then
        echo "Error: ld.lld not found! Required for FreeBSD cross-compilation." >&2
        echo "Please install with:" >&2
        echo "  brew install llvm lld" >&2
        exit 1
      fi
      SYSROOT_PATH="$HOME/opt/freebsd-sysroot"
      if [[ ! -d "$SYSROOT_PATH" ]]; then
        echo "Error: FreeBSD sysroot not found at $SYSROOT_PATH" >&2
        echo "Please create it with:" >&2
        echo "  mkdir -p ~/opt/freebsd-sysroot" >&2
        echo "  curl -LO https://download.freebsd.org/ftp/releases/amd64/13.3-RELEASE/base.txz" >&2
        echo "  tar -xf base.txz -C ~/opt/freebsd-sysroot" >&2
        exit 1
      fi
      echo "Using FreeBSD sysroot at $SYSROOT_PATH"

      # Disable sccache and mold for cross-compilation
      export SCCACHE_DISABLE="1"
      export RUSTC_WRAPPER=""

      # Create temporary clang wrapper
      CLANG_WRAPPER="$(pwd)/clang_wrapper_for_cross_freebsd.sh"
      echo '#!/bin/bash' > "$CLANG_WRAPPER"
      echo 'exec x86_64-unknown-freebsd13-clang -fuse-ld=lld "$@"' >> "$CLANG_WRAPPER"
      chmod +x "$CLANG_WRAPPER"

      export CC="x86_64-unknown-freebsd13-clang"
      export CARGO_TARGET_X86_64_UNKNOWN_FREEBSD_LINKER="$CLANG_WRAPPER"
      export RUSTFLAGS="-C link-arg=--sysroot=$SYSROOT_PATH -C opt-level=3 -C lto=fat -C codegen-units=1 -C strip=symbols"

      # Optionally: remove the wrapper on script exit
      trap 'rm -f "$CLANG_WRAPPER"' EXIT
      ;;

    aarch64-unknown-linux-gnu)
  if ! command -v aarch64-unknown-linux-gnu-gcc >/dev/null 2>&1; then
    echo "Error: aarch64-unknown-linux-gnu-gcc not found in PATH!" >&2
    echo "Please install with:" >&2
    echo "  brew tap messense/macos-cross-toolchains" >&2
    echo "  brew install aarch64-unknown-linux-gnu" >&2
    exit 1
  fi

  AARCH64_OPENSSL_SYSROOT="$HOME/opt/openssl_for_cross_aarch64"
  AARCH64_SQLITE_SYSROOT="$HOME/opt/sqlite_for_cross_aarch64"

  if [ ! -d "$AARCH64_OPENSSL_SYSROOT/lib" ] || [ ! -d "$AARCH64_OPENSSL_SYSROOT/include" ]; then
    echo "Error: aarch64 OpenSSL sysroot not found at $AARCH64_OPENSSL_SYSROOT" >&2
    echo "Extract it from your aarch64 Linux VM (libssl-dev installed) into that path." >&2
    exit 1
  fi

  # Disable sccache and mold for cross-compilation
  export SCCACHE_DISABLE="1"
  export RUSTC_WRAPPER=""

  # Target-specific CC/AR (prevents macOS flags from leaking)
  export CC_aarch64_unknown_linux_gnu="aarch64-unknown-linux-gnu-gcc"
  export AR_aarch64_unknown_linux_gnu="aarch64-unknown-linux-gnu-ar"
  export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="aarch64-unknown-linux-gnu-gcc"
  unset CC

  # OpenSSL
  export OPENSSL_DIR="$AARCH64_OPENSSL_SYSROOT"
  export OPENSSL_LIB_DIR="$AARCH64_OPENSSL_SYSROOT/lib"
  export OPENSSL_INCLUDE_DIR="$AARCH64_OPENSSL_SYSROOT/include"
  export OPENSSL_STATIC=1
  export PKG_CONFIG_ALLOW_CROSS=1
  export PKG_CONFIG_PATH="$AARCH64_OPENSSL_SYSROOT/lib/pkgconfig"
  export PKG_CONFIG_SYSROOT_DIR="$AARCH64_OPENSSL_SYSROOT"

  # SQLite
  export SQLITE3_LIB_DIR="$AARCH64_SQLITE_SYSROOT/lib"
  export SQLITE3_INCLUDE_DIR="$AARCH64_SQLITE_SYSROOT/include"

  # Prevent macOS-specific flags from being passed to the Linux compiler
  unset MACOSX_DEPLOYMENT_TARGET
  export CFLAGS_aarch64_unknown_linux_gnu=""
  export CXXFLAGS_aarch64_unknown_linux_gnu=""
  export TARGET_ARCH="aarch64"

  # Match Linux build optimization settings
  export RUSTFLAGS="-C opt-level=3 -C lto=fat -C codegen-units=1 -C strip=symbols"

  # C23-symbol shim: our aarch64 OpenSSL sysroot was built on a host
  # with glibc ≥ 2.38 and pulls in `__isoc23_strtol` etc. The brew
  # cross-toolchain ships glibc 2.17 (CentOS 7 era) which only has
  # plain `strtol`. We build a tiny shim object that defines the
  # missing symbols as wrappers around the older ones and link it
  # into the final binary. CARGO_TARGET_…_RUSTFLAGS only applies to
  # the aarch64 target, not the wasm lib, and survives the
  # `unset RUSTFLAGS` further down. Drop once libcrypto.a is rebuilt
  # on a glibc-≤-2.37 host.
  ISOC23_SHIM_OBJ="$(pwd)/target/cross-shims/isoc23_shim.aarch64.o"
  mkdir -p "$(dirname "$ISOC23_SHIM_OBJ")"
  if [[ ! -f "$ISOC23_SHIM_OBJ" \
        || "scripts/cross-shims/isoc23_shim.c" -nt "$ISOC23_SHIM_OBJ" ]]; then
    aarch64-unknown-linux-gnu-gcc -c -O2 -fPIC \
      -o "$ISOC23_SHIM_OBJ" scripts/cross-shims/isoc23_shim.c
  fi
  export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=$ISOC23_SHIM_OBJ"
  ;;

    x86_64-pc-windows-gnu)
      if ! command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then
        echo "Error: x86_64-w64-mingw32-gcc not found in PATH!" >&2
        echo "Please install with:" >&2
        echo "  brew install mingw-w64" >&2
        echo "  rustup target add x86_64-pc-windows-gnu" >&2
        exit 1
      fi

      # Disable sccache and mold for cross-compilation
      export SCCACHE_DISABLE="1"
      export RUSTC_WRAPPER=""

      export CC=x86_64-w64-mingw32-gcc
      export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=$CC
      export RUSTFLAGS="-C opt-level=3 -C lto=fat -C codegen-units=1 -C strip=symbols"
      ;;

    aarch64-pc-windows-msvc)
      if ! command -v lib.exe >/dev/null 2>&1; then
              echo "Error: lib.exe special shell script not found in PATH" >&2
              echo "The \`aarch64-pc-windows-msvc\` target requires a lib.exe wrapper for LLVM." >&2
              echo "" >&2
              echo "❶ Ensure LLVM is available (needed for llvm-ar)" >&2
              echo "   brew install llvm" >&2
              echo "" >&2
              echo "❷ Install the wrapper at /opt/homebrew/bin/lib.exe:" >&2
              cat <<'EOF'
#!/bin/bash
# Minimal shim translating "lib.exe" arguments to llvm‑ar.
OUT=""
OBJS=()

for arg in "$@"; do
  case "$arg" in
    -nologo) ;;                       # ignore
    -out:*)  OUT="${arg#-out:}" ;;    # strip "-out:"
    *)       OBJS+=("$arg") ;;        # collect object files
  esac
done

if [[ -z "$OUT" ]]; then
  echo "lib.exe wrapper error: -out:<file> missing" >&2
  exit 1
fi

exec /opt/homebrew/opt/llvm/bin/llvm-ar crs "$OUT" "${OBJS[@]}"
EOF
        echo "" >&2
        echo "❸ Make it executable:" >&2
        echo "   chmod +x /opt/homebrew/bin/lib.exe" >&2
        exit 1
      fi




# Configure Windows ARM64 sysroot and paths
SYSROOT="$HOME/opt/aarch64-windows"

# Disable sccache completely for cross-compilation
export SCCACHE_DISABLE="1"
export RUSTC_WRAPPER=""
export CARGO_BUILD_RUSTC_WRAPPER=""

# Create a temporary .cargo/config.toml that overrides workspace settings
mkdir -p .cargo
TEMP_CONFIG=".cargo/config.toml.temp"
cat > "$TEMP_CONFIG" << 'CONFIGEOF'
[build]
# Explicitly disable rustc-wrapper for cross-compilation
rustc-wrapper = ""

[env]
RUSTC_WRAPPER = ""
CARGO_INCREMENTAL = "0"
SCCACHE_DISABLE = "1"

[target.aarch64-pc-windows-msvc]
linker = "lld-link"
CONFIGEOF

# Backup original config and use temp one
if [ -f ".cargo/config.toml" ]; then
  mv ".cargo/config.toml" ".cargo/config.toml.backup"
  mv "$TEMP_CONFIG" ".cargo/config.toml"
  trap 'mv .cargo/config.toml.backup .cargo/config.toml 2>/dev/null' EXIT
else
  mv "$TEMP_CONFIG" ".cargo/config.toml"
  trap 'rm -f .cargo/config.toml' EXIT
fi




# Set target-specific compilers (these take precedence in cc-rs)
# Note: Use underscores in variable names (bash requirement), cc-rs will map them correctly
export CC_aarch64_pc_windows_msvc="clang-cl"
export CXX_aarch64_pc_windows_msvc="clang-cl"
export AR_aarch64_pc_windows_msvc="llvm-lib"
export CARGO_TARGET_AARCH64_PC_WINDOWS_MSVC_LINKER="lld-link"

# For clang-cl, we need to use Unix-style -I flags, not MSVC /I flags
# cc-rs will handle the conversion when it detects clang-cl
export CFLAGS_aarch64_pc_windows_msvc="-I$SYSROOT/include -I$SYSROOT/include/ucrt"
export CXXFLAGS_aarch64_pc_windows_msvc="-I$SYSROOT/include -I$SYSROOT/include/ucrt"

# Unset global CC/CXX/AR to force cc-rs to use target-specific variables
unset CC
unset CXX
unset AR
# CRITICAL: Set DYLD_LIBRARY_PATH so lld can find LLVM libraries
export DYLD_LIBRARY_PATH="/opt/homebrew/opt/llvm/lib:/opt/homebrew/opt/zstd/lib:${DYLD_LIBRARY_PATH:-}"
# Also set the target architecture explicitly for clang-cl
export CFLAGS_aarch64_pc_windows_msvc="--target=aarch64-pc-windows-msvc -I$SYSROOT/include -I$SYSROOT/include/ucrt"
export CXXFLAGS_aarch64_pc_windows_msvc="--target=aarch64-pc-windows-msvc -I$SYSROOT/include -I$SYSROOT/include/ucrt"

# Set rustflags directly instead of using config.toml
# Note: Windows doesn't use -C strip=symbols, use /DEBUG:NONE instead
export RUSTFLAGS="-C linker=lld-link \
                  -C link-arg=/LIBPATH:$SYSROOT/lib/um/arm64 \
                  -C link-arg=/LIBPATH:$SYSROOT/lib/ucrt/arm64 \
                  -C link-arg=/LIBPATH:$SYSROOT/lib/arm64 \
                  -Lnative=$SYSROOT/lib/ucrt/arm64 \
                  -C opt-level=3 -C lto=fat -C codegen-units=1 \
                  -C link-arg=/DEBUG:NONE"

echo "Configured Windows ARM64 cross-compilation environment"
echo "Using CC: clang-cl (via CC_aarch64_pc_windows_msvc)"
echo "Using AR: llvm-lib (via AR_aarch64_pc_windows_msvc)"
echo "Using Linker: lld-link"
echo "CFLAGS: $CFLAGS_aarch64_pc_windows_msvc"
# Force blake3 to use pure Rust implementation (no NEON)
export CARGO_FEATURE_PURE=1
export BLAKE3_NO_NEON=1

      ;;
    x86_64-apple-darwin)
          if ! check_target "$TARGET_TRIPLE"; then
            echo "Failed to set up target $TARGET_TRIPLE. Aborting build." >&2
            exit 1
          fi
          ;;
    synology-x86_64)
      TARGET_TRIPLE="x86_64-unknown-linux-gnu"
      if ! command -v x86_64-unknown-linux-gnu-gcc >/dev/null 2>&1; then
        echo "Error: x86_64-unknown-linux-gnu-gcc not found in PATH!" >&2
        echo "Please install with:" >&2
        echo "  brew tap messense/macos-cross-toolchains" >&2
        echo "  brew install x86_64-unknown-linux-gnu" >&2
        exit 1
      fi
      export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER="x86_64-unknown-linux-gnu-gcc"
      check_target "$TARGET_TRIPLE"
      cargo build --release --target "$TARGET_TRIPLE" --all-features
      # create_synology_package "$TARGET_TRIPLE"
      exit 0
      ;;
    synology-aarch64)
      TARGET_TRIPLE="aarch64-unknown-linux-gnu"
      if ! command -v aarch64-unknown-linux-gnu-gcc >/dev/null 2>&1; then
        echo "Error: aarch64-unknown-linux-gnu-gcc not found in PATH!" >&2
        echo "Please install with:" >&2
        echo "  brew tap messense/macos-cross-toolchains" >&2
        echo "  brew install aarch64-unknown-linux-gnu" >&2
        exit 1
      fi
      export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="aarch64-unknown-linux-gnu-gcc"
      check_target "$TARGET_TRIPLE"
      cargo build --release --target "$TARGET_TRIPLE" --all-features
      # create_synology_package "$TARGET_TRIPLE"
      exit 0
      ;;
    *)
      echo "Error: Unsupported target: $TARGET_TRIPLE" >&2
      echo "Supported targets on macOS:" >&2
      echo "  - x86_64-unknown-linux-gnu" >&2
      echo "  - aarch64-unknown-linux-gnu" >&2
      echo "  - x86_64-unknown-freebsd" >&2
      echo "  - x86_64-pc-windows-gnu" >&2
      echo "  - aarch64-pc-windows-msvc" >&2
      echo "  - x86_64-apple-darwin" >&2
      echo "  - synology-x86_64" >&2
      echo "  - synology-aarch64" >&2
      exit 1
      ;;
  esac
else
  echo "Warning: Running on non-macOS host ($HOST_UNAME). Cross-compilation setup may not be necessary." >&2
fi

check_target "$TARGET_TRIPLE"

# Clean the target directory to ensure we're starting fresh
#echo "Cleaning target directory for $TARGET_TRIPLE..."
#cargo clean --target "$TARGET_TRIPLE"

# Build the target binary
echo "Starting build for $TARGET_TRIPLE..."

# Clear RUSTFLAGS for leptos build - the release profile handles optimization
# LTO and other flags conflict with WASM build and cross-compilation
unset RUSTFLAGS

# Single profile: release-prod (fat LTO, codegen-units=1). The metadata
# in workspace Cargo.toml binds bin-profile-release / lib-profile-release
# to "release-prod" so cargo-leptos's wasm-bindgen lookup path matches
# the actual cargo output dir. The legacy --milestone flag is accepted
# but no longer toggles anything.
PROFILE="release-prod"

# Global incremental is off by default. The bin enables it via
# --bin-cargo-args below; the lib (wasm) stays non-incremental because
# cargo-leptos's wasm validator chokes on incremental bulk-memory ops.
export CARGO_INCREMENTAL=0

echo "Using profile: $PROFILE"

LEPTOS_BIN_TARGET_TRIPLE=$TARGET_TRIPLE \
  cargo leptos build --release \
    --bin-cargo-args="--config=build.incremental=true" \
    -v

# Check if the build was successful
LEPTOS_RC=$?
if [ $LEPTOS_RC -eq 0 ]; then
  echo "Server build completed successfully!"
  # $BIN_NAME comes from .env (exported by deploy_to_server.sh via
  # `set -a` before sourcing). Fallback matches our [[bin]] entry.
  BINARY_PATH="target/$TARGET_TRIPLE/$PROFILE/${BIN_NAME:-gldf-search-server}"
  if [ -f "$BINARY_PATH" ]; then
    echo "Server binary size: $(du -h "$BINARY_PATH" | cut -f1)"
  fi

  # Cross-build the Pi binary (rusterando-printer) for aarch64 targets.
  # The crate is excluded from the main workspace (see top-level
  # Cargo.toml) so it needs its own `cargo build` invocation. It
  # inherits the same cross-toolchain envs (CC_…, AR_…, LINKER, the
  # C23 shim via CARGO_TARGET_AARCH64_…_RUSTFLAGS) that the aarch64
  # branch above set up.
  #
  # Only fires for aarch64 targets — the Pi is the only consumer.
  # Skipped silently when the crate dir is missing so older checkouts
  # without the kitchen-printer subsystem still build.
  PI_MANIFEST="crates/rusterando-printer/Cargo.toml"
  case "$TARGET_TRIPLE" in
    aarch64-unknown-linux-gnu)
      if [[ -f "$PI_MANIFEST" ]]; then
        echo ""
        echo "=== Cross-building rusterando-printer (Pi binary) ==="
        cargo build --release \
          --target "$TARGET_TRIPLE" \
          --manifest-path "$PI_MANIFEST"
        PI_RC=$?
        if [ $PI_RC -eq 0 ]; then
          PI_BINARY="crates/rusterando-printer/target/$TARGET_TRIPLE/release/rusterando-printer"
          if [[ -f "$PI_BINARY" ]]; then
            echo "Pi binary size: $(du -h "$PI_BINARY" | cut -f1)"
            echo "Pi binary path: $PI_BINARY"
          fi
        else
          echo "rusterando-printer build failed (rc=$PI_RC)."
          exit 1
        fi
      fi
      ;;
  esac

  echo ""
  echo "Build completed successfully!"
else
  echo "Build failed. Please check the error messages above."
  exit 1
fi

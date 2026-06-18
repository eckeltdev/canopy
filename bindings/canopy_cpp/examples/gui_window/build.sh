#!/usr/bin/env bash
# Build the live AppKit window demo (Objective-C++ + the canopy_cpp binding + libcanopy_abi.a).
# Standalone (not a cpp-doctor/CMake target) because it is macOS-only ObjC++ glue. macOS only.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIND="$(cd "${HERE}/../.." && pwd)"   # bindings/canopy_cpp
REPO="$(cd "${BIND}/../.." && pwd)"   # repo root
SDK="$(xcrun --show-sdk-path)"
CXX="$(xcrun --find clang++)"

echo "==> cargo build -p canopy-abi (libcanopy_abi.a)"
( cd "${REPO}" && cargo build -p canopy-abi )

echo "==> compiling canopy_cpp_window"
"${CXX}" -std=c++23 -fobjc-arc -O2 -isysroot "${SDK}" \
	-I"${BIND}/include" -I"${REPO}/crates/canopy-abi/include" \
	"${HERE}/main.mm" \
	"${BIND}/src/build_context.cpp" "${BIND}/src/host.cpp" "${BIND}/src/reactive.cpp" \
	-L"${REPO}/target/debug" -lcanopy_abi -framework Cocoa \
	-o "${HERE}/canopy_cpp_window"

echo "==> built ${HERE}/canopy_cpp_window"
echo "    run:            ${HERE}/canopy_cpp_window"
echo "    headless proof: ${HERE}/canopy_cpp_window --selftest   (writes frame_before/after.ppm)"

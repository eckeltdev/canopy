#!/usr/bin/env bash
# frt M5 — the std-on-freestanding go/no-go symbol audit.
#
# THE THESIS UNDER TEST: Canopy's core C++ (the canopy_cpp binding) writes normal
# std::vector / map / string / string_view / unique_ptr / std::function, routed through
# frt's global operator new/delete over the platform seam. M1-M4 proved this *compiles and
# runs on the host*. M5 asks the load-bearing question that gates every device milestone:
#
#   When those std-using translation units are compiled for a FREESTANDING target
#   (-ffreestanding -fno-exceptions -fno-rtti), do they leave behind only undefined symbols
#   we can PROVIDE on a bare-metal aarch64 device, or do they drag in the C++ exception
#   unwinder / RTTI machinery that a no_std target cannot link?
#
# PROVIDABLE (a GO): operator new/delete (frt already defines these); the std::__throw_*
#   terminate shims + __libcpp_verbose_abort (forward to frt_platform_panic); __cxa_guard_*
#   for function-local statics (trivial single-threaded bodies); the few libc++ extern-template
#   library members (std::string's char specialization — supply a target libc++.a OR build with
#   -D_LIBCPP_DISABLE_EXTERN_TEMPLATE to inline them); the libc CRT floor (memcpy/memset/...);
#   and our own cross-TU / engine-ABI references (resolve when the binding is fully linked).
#   None of these need an unwinder; all are M6 floor / link-line work.
# FATAL (a NO-GO): __cxa_throw / __cxa_allocate_exception / __cxa_begin_catch / _Unwind_* /
#   __gxx_personality_v0 (the exception unwinder) or typeinfo / __dynamic_cast / __cxa_bad_*
#   (RTTI). If any of these are referenced, "std works freestanding" is false as built.
#
# METHOD
#   1. Compile each shipping std-heavy TU to an object (NO link) at -O0 and -O2 — an optimizer
#      both elides and exposes shims, so we audit the union.
#   2. Enumerate undefined symbols (nm -u), demangle, and bucket EVERY one. Verdict = GO iff
#      the FATAL buckets are empty and every referenced operator new/delete is defined by frt.
#   3. Independent closure proof: a real `-nostdlib++` link (drops libc++ / libc++abi = no
#      unwinder, no operator new, no throw shims) of the subjects + frt's providers + a
#      throwaway floor, with -D_LIBCPP_DISABLE_EXTERN_TEMPLATE so no libc++.a is needed. If the
#      image closes, the binding provably needs no C++ runtime support library at all.
#      (-lSystem still supplies the libc floor and *would* supply an unwinder, so the link alone
#      can't disprove EH — the undefined-symbol classifier is the authority there. Together they
#      are conclusive.)
#
# HOST NOTE: the repo's toolchain is Apple clang / arm64 (Mach-O, std::__1). A literal
# aarch64-unknown-none ELF reproduction needs a cross-configured libc++ (no vendor availability,
# single-threaded) and is M6 toolchain-provisioning work — see runtime/frt/docs/m5-nm-audit.md.
# The EH/RTTI codegen question this script answers is a property of -fno-exceptions, not of the
# object format, so the verdict transfers to aarch64-none.
#
# EXIT: 0 = GO, 1 = NO-GO (a FATAL bucket non-empty / an operator new-delete unsatisfied),
#       2 = harness/build error. CI-gateable.

set -u
set -o pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FRT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
REPO_DIR="$(cd "${FRT_DIR}/../.." && pwd)"
BINDING_DIR="${REPO_DIR}/bindings/canopy_cpp"
ABI_INC="${REPO_DIR}/crates/canopy-abi/include"

CXX="${CXX:-$(xcrun --find clang++ 2>/dev/null || echo clang++)}"
NM="${NM:-$(xcrun --find nm 2>/dev/null || echo nm)}"
FILT="${FILT:-$(xcrun --find c++filt 2>/dev/null || echo c++filt)}"

# Apple clang outside CMake needs the SDK sysroot to resolve libc++'s C backing (<cstdint> ->
# <stdint.h>). CMake injects this via -isysroot; replicate it. Harmless if absent.
SYSROOT_FLAGS=()
if _sdk="$(xcrun --show-sdk-path 2>/dev/null)" && [ -n "${_sdk}" ]; then
	SYSROOT_FLAGS=(-isysroot "${_sdk}")
fi

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

# The freestanding gate's flags, hardened to the DEVICE contract. -ffreestanding (no hosted
# assumptions); -fno-exceptions/-fno-rtti (the EH/RTTI policy under test); -fno-stack-protector
# (the device disables the stack guard); and -fno-threadsafe-statics, which runtime/frt/include/
# frt/config.hpp explicitly MANDATES — without it a function-local static would emit
# __cxa_guard_acquire/release (atomic guards) that a single-core no_std target has no runtime for.
# Auditing without it would certify a different configuration than the device ships.
FREESTANDING_FLAGS=(-std=c++23 -ffreestanding -fno-exceptions -fno-rtti -fno-stack-protector -fno-threadsafe-statics)
INCLUDES=(-I"${BINDING_DIR}/include" -I"${ABI_INC}" -I"${FRT_DIR}/include")

# The shipping std-heavy translation units (the real std consumers — NOT test/example code).
SUBJECT_TUS=(
	"${BINDING_DIR}/src/build_context.cpp"
	"${BINDING_DIR}/src/reactive.cpp"
	"${BINDING_DIR}/src/host.cpp"
)
# frt's providers: what DEFINES operator new/delete + the platform seam on a real image.
PROVIDER_TUS=(
	"${FRT_DIR}/src/new_delete.cpp"
	"${FRT_DIR}/src/platform.cpp"
	"${FRT_DIR}/src/backend_host_posix.cpp"
)

say()  { printf '%s\n' "$*"; }
rule() { printf '%s\n' "------------------------------------------------------------------------"; }

say "frt M5 — std-on-freestanding symbol audit"
rule
say "compiler : ${CXX}"
"${CXX}" --version 2>/dev/null | head -1 | sed 's/^/           /'
say "target   : $("${CXX}" -dumpmachine 2>/dev/null) (host proxy; aarch64-none ELF is M6)"
say "flags    : ${FREESTANDING_FLAGS[*]}"
rule

compile_obj() {
	local src="$1" opt="$2" out="$3" extra="${4:-}"
	# shellcheck disable=SC2086
	"${CXX}" "${FREESTANDING_FLAGS[@]}" "${SYSROOT_FLAGS[@]}" "${opt}" ${extra} "${INCLUDES[@]}" -c "${src}" -o "${out}"
}

# --- 1. compile subjects at -O0 and -O2 (no link) --------------------------------------------
ALL_OBJ=()
for tu in "${SUBJECT_TUS[@]}"; do
	base="$(basename "${tu}" .cpp)"
	for opt in -O0 -O2; do
		obj="${WORK}/${base}${opt}.o"
		if ! compile_obj "${tu}" "${opt}" "${obj}"; then
			say "BUILD ERROR: ${tu} failed to compile at ${opt} under freestanding flags."; exit 2
		fi
		ALL_OBJ+=("${obj}")
	done
done

# A REPRESENTATIVE CONSUMER TU: the three library TUs above are the binding's internals, but a real
# app performs std operations they don't — most notably std::to_string(int) and a by-value
# std::string copy. Audit those too, so the verdict reflects the consumer surface, not just the
# library. (Verified independently: this surface stays EH/RTTI-clean; it only enriches the
# providable floor — more __throw_*/basic_string members.)
cat > "${WORK}/representative.cpp" <<'REP'
#include <string>

#include "canopy_cpp/dsl.hpp"
// A stand-in for application code over the binding. extern "C" so -ffreestanding doesn't mangle a
// stray `main`; no namespace-scope non-trivial global (that would be a static-init signal).
extern "C" unsigned long represent(int n) {
	canopy::build_context ctx;
	std::string label = std::to_string(n);  // integer formatting -> basic_string library members
	std::string copy = label;                // by-value basic_string copy-ctor
	canopy::mount(ctx, canopy::div(canopy::cls("card"),
	                               canopy::button(canopy::on_click([] {}), copy.c_str())));
	const auto bytes = ctx.take_batch(0);
	return bytes.size() + label.size() + copy.size();
}
REP
for opt in -O0 -O2; do
	obj="${WORK}/representative${opt}.o"
	if ! compile_obj "${WORK}/representative.cpp" "${opt}" "${obj}"; then
		say "BUILD ERROR: representative consumer TU failed to compile at ${opt}."; exit 2
	fi
	ALL_OBJ+=("${obj}")
done
say "compiled ${#SUBJECT_TUS[@]} subject TU(s) + 1 representative consumer x2 opt levels = ${#ALL_OBJ[@]} objects."

# frt providers: compile new_delete + platform + the host backend. new_delete/platform are
# device-relevant frt code and ALSO get classified below (platform.cpp holds the one
# function-local-static atomic seam — exactly the kind of code a guard/atomics regression hides
# in); the host backend is host-only (the device replaces it) so it is NOT folded into the
# classified union, only used for the provider defined-symbol set.
PROVIDER_OBJ=()
PROV_NEWDELETE=""; PROV_PLATFORM=""
for tu in "${PROVIDER_TUS[@]}"; do
	base="$(basename "${tu}" .cpp)"; obj="${WORK}/prov_${base}.o"; extra=""
	[ "${base}" = "new_delete" ] && extra="-DFRT_OWN_NEW_DELETE"
	if compile_obj "${tu}" "-O2" "${obj}" "${extra}"; then
		PROVIDER_OBJ+=("${obj}")
		[ "${base}" = "new_delete" ] && PROV_NEWDELETE="${obj}"
		[ "${base}" = "platform" ]   && PROV_PLATFORM="${obj}"
	fi
done

# --- 2. collect undefined symbols, demangled -------------------------------------------------
# nm -u prints undefined symbols, indented, with multi-file "path:" headers. Drop the headers
# and blank lines, trim leading whitespace, and demangle. Apple c++filt expects the FULL Mach-O
# name (both leading underscores on a C++ mangled "__Z..."), so DO NOT strip the underscore:
# C++ names demangle; plain C symbols simply keep one leading '_' (matched underscore-tolerantly).
undef_demangled() {
	"${NM}" -u "$@" 2>/dev/null | grep -vE '(^|/)[^[:space:]]*:[[:space:]]*$' \
		| sed -e 's/^[[:space:]]*//' -e '/^$/d' | "${FILT}"
}
# Classify the subjects + the representative consumer + the device-relevant frt providers.
CLASSIFY_OBJ=("${ALL_OBJ[@]}")
[ -n "${PROV_NEWDELETE}" ] && CLASSIFY_OBJ+=("${PROV_NEWDELETE}")
[ -n "${PROV_PLATFORM}" ]  && CLASSIFY_OBJ+=("${PROV_PLATFORM}")
UNDEF=()
while IFS= read -r _line; do [ -n "${_line}" ] && UNDEF+=("${_line}"); done \
	< <(undef_demangled "${CLASSIFY_OBJ[@]}" | sort -u)
provider_defines() {
	"${NM}" -gjU "${PROVIDER_OBJ[@]}" 2>/dev/null | sed -e 's/^[[:space:]]*//' -e '/^$/d' | "${FILT}" | sort -u
}
PROVIDED=()
while IFS= read -r _line; do [ -n "${_line}" ] && PROVIDED+=("${_line}"); done < <(provider_defines)
provided_has() { printf '%s\n' "${PROVIDED[@]+"${PROVIDED[@]}"}" | grep -qxF "$1"; }

# --- 3. classify every undefined symbol ------------------------------------------------------
# classify_one echoes a bucket key for a demangled symbol. ORDER MATTERS: the two FATAL arms run
# FIRST so a std::-namespaced exception symbol (e.g. std::rethrow_exception, std::__1::exception_ptr)
# is caught as FATAL and can never be silently absorbed by the broad `std::*` catch-all below it.
# The harness must fail CLOSED — verified by the EH negative control further down.
classify_one() {
	case "$1" in
		*__cxa_throw*|*__cxa_allocate_exception*|*__cxa_free_exception*|*__cxa_begin_catch*|\
*__cxa_end_catch*|*__cxa_rethrow*|*__cxa_call_terminate*|*__cxa_call_unexpected*|\
*__clang_call_terminate*|*_Unwind_*|*__gxx_personality*|\
*"std::terminate"*|*"std::__terminate"*|*"std::rethrow_exception"*|*"std::current_exception"*|\
*"std::make_exception_ptr"*|*exception_ptr*|*"std::set_terminate"*|*"std::get_terminate"*|\
*"std::uncaught_exception"*|*"std::nested_exception"*|*"std::unexpected"*|*"std::rethrow_if_nested"*)
			echo EH ;;
		"typeinfo for "*|"typeinfo name for "*|"vtable for "*|*__dynamic_cast*|*__cxa_bad_typeid*|\
*__cxa_bad_cast*|*"std::type_info"*|*"std::bad_cast"*|*"std::bad_typeid"*)
			echo RTTI ;;
		"operator new"*|"operator delete"*)              echo NEWDEL ;;
		*"std::__1::__throw_"*|*"std::__throw_"*|*__throw_*)    echo THROW ;;
		*__libcpp_verbose_abort*|*__libcpp_assertion_handler*) echo VABORT ;;
		*__cxa_guard_*)                                  echo GUARD ;;
		*__cxa_atexit*|*__cxa_finalize*|*__dso_handle*|*__cxa_pure_virtual*|*__cxa_deleted_virtual*)
			echo CXA ;;
		*__aarch64_*|*__udivti3*|*__divti3*|*__umodti3*|*__modti3*|*__multi3*|*__udivdi3*|*__umoddi3*|\
*__cxa_thread_atexit*|*__tls_get_addr*|*emutls*|*__aeabi_*)  echo ELFRT ;;  # ELF/AArch64 RT helpers (M6)
		std::*)                                          echo LIBCPP ;;   # libc++ extern-template lib members
		_memcpy|memcpy|_memset|memset|_memmove|memmove|_memcmp|memcmp|_bzero|bzero|_memchr|memchr|\
_strlen|strlen|_strcmp|strcmp|_strncmp|strncmp)  echo CRT ;;
		_abort|abort|___stack_chk_fail|__stack_chk_fail|___stack_chk_guard) echo LIBC ;;
		_frt_*|frt_*|"frt::"*)                           echo FRT ;;       # frt seam / cross-TU (frt provides)
		_canopy_host_*|canopy_host_*|_canopy_*)          echo ABI ;;       # the Rust engine FFI (libcanopy_abi)
		canopy::*)                                       echo INTERNAL ;;  # intra-binding cross-TU refs
		*)                                               echo OTHER ;;
	esac
}

# Self-test (EH negative control): a TU that references the libc++abi exception runtime — which
# COMPILES under -fno-exceptions, since std::current_exception/rethrow_exception are ordinary
# library calls — MUST classify as FATAL EH. If it doesn't, the classifier fails OPEN and the whole
# GO verdict is untrustworthy. (This is the exact hole an earlier review demonstrated end-to-end.)
cat > "${WORK}/ctrl_eh.cpp" <<'CTRLEH'
#include <exception>
extern "C" void* ctrl_eh() {
	std::exception_ptr p = std::current_exception();  // -> std::current_exception / exception_ptr
	if (p) { std::rethrow_exception(p); }             // -> std::rethrow_exception (EH runtime)
	return nullptr;
}
CTRLEH
if compile_obj "${WORK}/ctrl_eh.cpp" "-O0" "${WORK}/ctrl_eh.o"; then
	eh_ctrl_hit=0
	while IFS= read -r _cs; do
		[ -n "${_cs}" ] && [ "$(classify_one "${_cs}")" = "EH" ] && eh_ctrl_hit=$((eh_ctrl_hit+1))
	done < <(undef_demangled "${WORK}/ctrl_eh.o" | sort -u)
	if [ "${eh_ctrl_hit}" -eq 0 ]; then
		say "HARNESS ERROR: the classifier did NOT flag the EH negative control as FATAL — it fails OPEN."
		exit 2
	fi
else
	say "HARNESS ERROR: EH negative-control TU failed to compile."; exit 2
fi

declare -a B_FATAL_EH B_FATAL_RTTI                                # NO-GO buckets
declare -a B_NEWDEL B_THROW B_VABORT B_GUARD B_CXA B_LIBCPP B_CRT B_LIBC B_ELFRT B_FRT B_ABI B_INTERNAL B_OTHER
for s in "${UNDEF[@]+"${UNDEF[@]}"}"; do
	[ -z "${s}" ] && continue
	case "$(classify_one "${s}")" in
		EH)       B_FATAL_EH+=("${s}") ;;
		RTTI)     B_FATAL_RTTI+=("${s}") ;;
		NEWDEL)   B_NEWDEL+=("${s}") ;;
		THROW)    B_THROW+=("${s}") ;;
		VABORT)   B_VABORT+=("${s}") ;;
		GUARD)    B_GUARD+=("${s}") ;;
		CXA)      B_CXA+=("${s}") ;;
		ELFRT)    B_ELFRT+=("${s}") ;;
		LIBCPP)   B_LIBCPP+=("${s}") ;;
		CRT)      B_CRT+=("${s}") ;;
		LIBC)     B_LIBC+=("${s}") ;;
		FRT)      B_FRT+=("${s}") ;;
		ABI)      B_ABI+=("${s}") ;;
		INTERNAL) B_INTERNAL+=("${s}") ;;
		*)        B_OTHER+=("${s}") ;;
	esac
done

print_bucket() {
	local title="$1"; shift
	if [ "$#" -eq 0 ]; then say ""; say "${title}: (none)"; return; fi
	say ""; say "${title}: $#"; printf '    %s\n' "$@"
}

say ""; rule
say "UNDEFINED SYMBOL CLASSIFICATION  (subjects + representative consumer + frt providers @ -O0,-O2)"
rule
say "  classifier self-test: EH negative control classified FATAL (${eh_ctrl_hit} EH symbol(s)) — fails closed."
print_bucket "[FATAL]   EXCEPTION UNWINDER / EH RUNTIME (NO-GO if present)" "${B_FATAL_EH[@]+"${B_FATAL_EH[@]}"}"
print_bucket "[FATAL]   RTTI (NO-GO if present)"                    "${B_FATAL_RTTI[@]+"${B_FATAL_RTTI[@]}"}"
print_bucket "[provide] operator new/delete (frt new_delete.cpp)"   "${B_NEWDEL[@]+"${B_NEWDEL[@]}"}"
print_bucket "[provide] std::__throw_* terminate shims -> panic"    "${B_THROW[@]+"${B_THROW[@]}"}"
print_bucket "[provide] libc++ verbose-abort/assert -> panic"       "${B_VABORT[@]+"${B_VABORT[@]}"}"
print_bucket "[provide] __cxa_guard_* (should be EMPTY under -fno-threadsafe-statics)" "${B_GUARD[@]+"${B_GUARD[@]}"}"
print_bucket "[provide] other __cxa_* (atexit/dso_handle)"          "${B_CXA[@]+"${B_CXA[@]}"}"
print_bucket "[provide] libc++ lib members (libc++.a / -D_LIBCPP_DISABLE_EXTERN_TEMPLATE)" "${B_LIBCPP[@]+"${B_LIBCPP[@]}"}"
print_bucket "[provide] ELF/AArch64 RT helpers (compiler-rt / outline-atomics / TLS — M6)" "${B_ELFRT[@]+"${B_ELFRT[@]}"}"
print_bucket "[floor]   libc CRT (memcpy/memset/...)"               "${B_CRT[@]+"${B_CRT[@]}"}"
print_bucket "[floor]   libc misc (abort/...)"                      "${B_LIBC[@]+"${B_LIBC[@]}"}"
print_bucket "[frt]     frt seam / cross-TU (frt provides)"         "${B_FRT[@]+"${B_FRT[@]}"}"
print_bucket "[link]    engine ABI (resolves from libcanopy_abi)"   "${B_ABI[@]+"${B_ABI[@]}"}"
print_bucket "[link]    intra-binding cross-TU refs"                "${B_INTERNAL[@]+"${B_INTERNAL[@]}"}"
print_bucket "[REVIEW]  unclassified — INSPECT THESE"               "${B_OTHER[@]+"${B_OTHER[@]}"}"

# --- 4. operator new/delete closure: each referenced variant must be DEFINED by frt ----------
say ""; rule; say "OPERATOR NEW/DELETE CLOSURE (frt must DEFINE every referenced variant)"; rule
NEWDEL_MISSING=0
if [ "${#B_NEWDEL[@]:-0}" -eq 0 ]; then
	say "  (no operator new/delete referenced)"
else
	for s in "${B_NEWDEL[@]}"; do
		if provided_has "${s}"; then say "  OK   frt defines: ${s}"
		else say "  MISS not in frt:  ${s}"; NEWDEL_MISSING=$((NEWDEL_MISSING+1)); fi
	done
fi

# --- 5. closure link proof: -nostdlib++ against frt + throwaway floor -------------------------
say ""; rule; say "CLOSURE LINK PROOF  (-nostdlib++: no libc++/libc++abi; -D_LIBCPP_DISABLE_EXTERN_TEMPLATE)"; rule
# Floor: the M6 providables that -nostdlib++ removes (libc++/libc++abi). libSystem still supplies
# abort/memcpy/__cxa_atexit/__dso_handle, so we DON'T redefine those. platform.cpp supplies the
# frt_platform_* seam (so we don't redefine frt_platform_panic either). Preview only — not shipped.
cat > "${WORK}/floor.cpp" <<'FLOOR'
extern "C" [[noreturn]] void frt_platform_panic(const char*);   // defined in frt platform.cpp
namespace { [[noreturn]] void boom(const char* w) { frt_platform_panic(w); } }
namespace std { inline namespace __1 {
	[[noreturn]] void __throw_length_error(const char*)     { boom("length_error"); }
	[[noreturn]] void __throw_out_of_range(const char*)     { boom("out_of_range"); }
	[[noreturn]] void __throw_bad_alloc()                   { boom("bad_alloc"); }
	[[noreturn]] void __throw_bad_array_new_length()        { boom("bad_array_new_length"); }
	[[noreturn]] void __throw_bad_function_call()           { boom("bad_function_call"); }
}}
extern "C" {
	void __libcpp_verbose_abort(const char*, ...) { boom("verbose_abort"); }
	int  __cxa_guard_acquire(void*) { return 1; }   // single-threaded: first caller wins
	void __cxa_guard_release(void*) {}
	void __cxa_guard_abort(void*)   {}
}
FLOOR
cat > "${WORK}/closure_main.cpp" <<'MAIN'
#include "canopy_cpp/dsl.hpp"
// extern "C": under -ffreestanding the compiler stops treating `main` as the special hosted
// entry and mangles it (__Z4mainv), but the host crt startup references the unmangled `_main`.
// Forcing C linkage emits `_main` so this closure link (against the host crt) resolves. A device
// has no crt/_main — it jumps to its own entry — so this only matters for the host-side proof.
extern "C" int main() {
	canopy::build_context ctx;
	canopy::mount(ctx, canopy::div(canopy::cls("card"),
	                               canopy::button(canopy::on_click([] {}), "Click")));
	const auto bytes = ctx.take_batch(0);
	return static_cast<int>(bytes.size() & 0x7f);
}
MAIN
# Engine-ABI stubs: host.cpp calls into the Rust engine (canopy_host_*), normally provided by
# libcanopy_abi. That FFI boundary is orthogonal to the std-closure question, so we stand it in
# with trivial stubs (including canopy.h guarantees the signatures match). On a device these are
# the linked Rust staticlib's real symbols.
cat > "${WORK}/engine_stub.cpp" <<'STUB'
#include "canopy.h"
extern "C" {
	int32_t canopy_host_apply(CanopyHost*, const uint8_t*, size_t) { return 0; }
	size_t  canopy_host_node_count(const CanopyHost*) { return 0; }
	int32_t canopy_host_resize(CanopyHost*, float, float) { return 0; }
	int32_t canopy_host_pointer(CanopyHost*, float, float, uint8_t, uint16_t) { return 0; }
	int32_t canopy_host_poll_events(CanopyHost*, uint8_t*, size_t, size_t*) { return 0; }
}
STUB
# A device-style backend: defines frt::host_ops() with no <chrono>. The real host backend
# (backend_host_posix.cpp) pulls std::chrono::steady_clock — which a bare-metal target never
# links; the HAL installs its own ops at startup. This stands in for that device backend so the
# closure proof reflects the device link line, not the host one.
cat > "${WORK}/device_backend.cpp" <<'DEV'
#include "frt/platform.hpp"
namespace {
	void* dev_alloc(std::size_t, std::size_t) { return nullptr; }
	void  dev_free(void*, std::size_t, std::size_t) {}
	void  dev_panic(const char*) { __builtin_trap(); }
	void  dev_log(const char*, std::size_t) {}
	std::uint64_t dev_ticks() { return 0; }
	std::uint64_t dev_tps() { return 1; }
	constexpr frt::platform_ops k_dev_ops{dev_alloc, dev_free, dev_panic, dev_log, dev_ticks, dev_tps};
} // namespace
namespace frt { auto host_ops() -> const platform_ops& { return k_dev_ops; } }
DEV

EXTERN_OFF="-D_LIBCPP_DISABLE_EXTERN_TEMPLATE"
LINK_INPUTS=()
ok=1
for tu in "${SUBJECT_TUS[@]}"; do
	base="$(basename "${tu}" .cpp)"; o="${WORK}/cl_${base}.o"
	compile_obj "${tu}" "-O2" "${o}" "${EXTERN_OFF}" || ok=0; LINK_INPUTS+=("${o}")
done
compile_obj "${WORK}/floor.cpp"          "-O2" "${WORK}/floor.o"          "${EXTERN_OFF}" || ok=0
compile_obj "${WORK}/closure_main.cpp"   "-O2" "${WORK}/closure_main.o"   "${EXTERN_OFF}" || ok=0
compile_obj "${WORK}/engine_stub.cpp"    "-O2" "${WORK}/engine_stub.o"    "${EXTERN_OFF}" || ok=0
compile_obj "${WORK}/device_backend.cpp" "-O2" "${WORK}/device_backend.o" "${EXTERN_OFF}" || ok=0
# frt's allocator providers (operator new/delete + the seam forwarders), but the DEVICE backend
# rather than the host one (which would drag std::chrono::steady_clock).
LINK_INPUTS+=("${WORK}/prov_new_delete.o" "${WORK}/prov_platform.o" "${WORK}/device_backend.o")
LINK_INPUTS+=("${WORK}/floor.o" "${WORK}/closure_main.o" "${WORK}/engine_stub.o")

CLOSURE_OK=0
LINK_LOG="${WORK}/link.log"
if [ "${ok}" -eq 1 ] && "${CXX}" "${SYSROOT_FLAGS[@]}" -nostdlib++ -fno-exceptions -fno-rtti \
		"${LINK_INPUTS[@]}" -o "${WORK}/closed_image" 2>"${LINK_LOG}"; then
	CLOSURE_OK=1
	say "  LINKED: a freestanding image closed with NO libc++/libc++abi."
	say "  -> the binding's std code needs no C++ runtime support library; only frt + the floor + libc."
	if "${NM}" -u "${WORK}/closed_image" 2>/dev/null | "${FILT}" | grep -Eq '__gxx_personality|_Unwind_|__cxa_throw'; then
		say "  NOTE: residual unwinder symbol in the linked image — see classifier (authoritative)."
	else
		say "  the linked image's undefined set contains no __gxx_personality / _Unwind_ / __cxa_throw."
	fi
else
	say "  link did NOT close. Unresolved beyond frt+floor+libc:"
	grep -E '"|Undefined|symbol\(s\) not found' "${LINK_LOG}" | sed 's/^/    /' | head -40
fi

# --- 6. static-initializer / global-constructor check ----------------------------------------
# nm -u (the classifier above) is blind to static-init machinery, which lives in DEFINED symbols
# and SECTIONS, not undefined refs: a global with a runtime constructor emits a defined
# __GLOBAL__sub_I_* / __cxx_global_var_init symbol plus a __mod_init_func (Mach-O) / .init_array
# (ELF) section. If that global's dtor is trivial/elided it registers NO __cxa_atexit, so it would
# slip the undefined-symbol buckets entirely while STILL requiring C++ runtime startup
# (.init_array iteration) that a bare-metal no_std target must implement itself. So we scan DEFINED
# symbols + init sections and FAIL on any. (Constant-initialized globals — constexpr / constinit /
# zero-init .bss — need no runtime ctor and do not trip this; that is the whole point.)
STATIC_INIT_HITS=0
scan_static_init() { # args: label obj...
	local label="$1"; shift
	local found=0 o syms
	for o in "$@"; do
		[ -f "${o}" ] || continue
		syms="$("${NM}" "${o}" 2>/dev/null | grep -v ' U ' \
			| grep -E 'GLOBAL__sub_I|GLOBAL__I_|cxx_global_var_init' | awk '{print $NF}' | sort -u)"
		if [ -n "${syms}" ]; then
			found=$((found+1)); say "  CTOR  ${o##*/}:"; printf '        %s\n' ${syms}
		fi
		if command -v otool >/dev/null 2>&1 && otool -l "${o}" 2>/dev/null | grep -qi 'mod_init_func'; then
			found=$((found+1)); say "  SECT  ${o##*/}: __mod_init_func (Mach-O static-init section)"
		fi
		if command -v llvm-readelf >/dev/null 2>&1 && llvm-readelf -S "${o}" 2>/dev/null | grep -qi 'init_array'; then
			found=$((found+1)); say "  SECT  ${o##*/}: .init_array (ELF static-init section)"
		elif command -v readelf >/dev/null 2>&1 && readelf -S "${o}" 2>/dev/null | grep -qi 'init_array'; then
			found=$((found+1)); say "  SECT  ${o##*/}: .init_array (ELF static-init section)"
		fi
	done
	STATIC_INIT_HITS=$((STATIC_INIT_HITS + found))
}

say ""; rule; say "STATIC-INITIALIZER / GLOBAL-CONSTRUCTOR CHECK (defined symbols + init sections)"; rule
# Self-test (negative control): a TU with a global of non-trivial ctor + TRIVIAL dtor (registers no
# __cxa_atexit, so it slips the undefined classifier) plus a function-local static MUST trip the
# check. If it doesn't, the check is broken and the whole audit is untrustworthy -> harness error.
cat > "${WORK}/ctrl_static_init.cpp" <<'CTRL'
extern "C" void ctrl_sink(int);                         // undefined: keeps the ctor un-elided
struct nt { nt(); int v; };                             // non-trivial ctor, TRIVIAL dtor (no atexit)
nt::nt() : v(1) { ctrl_sink(v); }
nt ctrl_global;                                         // runtime ctor -> __GLOBAL__sub_I + init section
int ctrl_touch() { static nt local; return local.v; }  // function-local static -> guard
CTRL
if compile_obj "${WORK}/ctrl_static_init.cpp" "-O2" "${WORK}/ctrl_static_init.o"; then
	scan_static_init "negative-control" "${WORK}/ctrl_static_init.o" >/dev/null
	if [ "${STATIC_INIT_HITS}" -eq 0 ]; then
		say "  HARNESS ERROR: the static-init check did NOT trip on the negative control — check is broken."
		exit 2
	fi
	say "  self-test OK: the negative control tripped the check (${STATIC_INIT_HITS} signal(s) seen)."
else
	say "  HARNESS ERROR: negative-control TU failed to compile."; exit 2
fi

# Real scan: the verdict requires ZERO static-init machinery in any subject or provider object.
STATIC_INIT_HITS=0
scan_static_init "subjects+providers" "${ALL_OBJ[@]}" "${PROVIDER_OBJ[@]}"
N_INIT="${STATIC_INIT_HITS}"
if [ "${N_INIT}" -eq 0 ]; then
	say "  clean: no __GLOBAL__sub_I / __cxx_global_var_init / __mod_init_func / .init_array in any"
	say "         subject or provider object — no C++ runtime startup (.init_array iteration) needed."
else
	say "  FOUND ${N_INIT} static-init signal(s) above — a no_std device would need .init_array startup."
fi

# --- 7. verdict ------------------------------------------------------------------------------
say ""; rule
N_EH="${#B_FATAL_EH[@]:-0}"; N_RTTI="${#B_FATAL_RTTI[@]:-0}"; N_OTHER="${#B_OTHER[@]:-0}"
say "VERDICT"; rule
say "  exception-unwinder symbols  : ${N_EH}   (must be 0)"
say "  RTTI symbols                : ${N_RTTI}   (must be 0)"
say "  operator new/del unsatisfied : ${NEWDEL_MISSING}   (must be 0)"
say "  static-init machinery       : ${N_INIT}   (must be 0)"
say "  unclassified (review)       : ${N_OTHER}   (must be 0)"
say "  closure link (-nostdlib++)  : $([ "${CLOSURE_OK}" -eq 1 ] && echo CLOSED || echo open)   (must be CLOSED)"
say ""
# The closure link is a BACKSTOP, not decoration: if the classifier ever misses a fatal dependency,
# the -nostdlib++ link fails to close and flips the verdict to NO-GO. So GO requires it CLOSED.
if [ "${N_EH}" -eq 0 ] && [ "${N_RTTI}" -eq 0 ] && [ "${NEWDEL_MISSING}" -eq 0 ] \
		&& [ "${N_INIT}" -eq 0 ] && [ "${N_OTHER}" -eq 0 ] && [ "${CLOSURE_OK}" -eq 1 ]; then
	say "  RESULT: GO — freestanding std drags only PROVIDABLE symbols. No exception unwinder,"
	say "          no RTTI, no global-constructor startup. operator new/delete are satisfied by frt;"
	say "          __throw_*/verbose_abort/guards by the M6 floor; std::string's extern-template"
	say "          members by a target libc++.a (or -D_LIBCPP_DISABLE_EXTERN_TEMPLATE); memcpy by libc."
	say "          NOTE: host-proxy GO — the aarch64-none ELF re-run (M6) confirms the ELF-only floor."
	exit 0
else
	say "  RESULT: NO-GO — the audit found machinery a bare-metal target cannot link, or the closure"
	say "          backstop did not close, or a symbol is unclassified. See the buckets above."
	exit 1
fi

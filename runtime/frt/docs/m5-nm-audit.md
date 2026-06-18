# frt M5 — the std-on-freestanding go/no-go audit

**Verdict: GO (host-proxy).** Canopy's core C++ binding, compiled freestanding, drags only
symbols we can provide on a bare-metal aarch64 device. It references **no exception unwinder, no
RTTI, and runs no global-constructor startup**. The one open item before the GO is *unconditional
for the device* is the M6 ELF re-run (see [§6](#6-limitations--residual-risk)) — the EH/RTTI core of
the verdict was, however, verified directly on the real `aarch64-none` triple and holds.

This is the load-bearing milestone the whole "write normal `std::vector`/`map`/`string`, route it
through frt" bet rests on. Every later device milestone (M6 CRT bodies, M7 HAL, M8 shared backend)
was provisional until this was settled.

Run it: `bash runtime/frt/tools/nm_audit.sh` (exit 0 = GO, 1 = NO-GO, 2 = harness error).
The verdict was adversarially reviewed by a 9-agent sweep; the harness was hardened against every
fail-open hole it found (§7).

---

## 1. The question

The binding (`bindings/canopy_cpp`) deliberately writes ordinary STL — `std::vector`, `std::map`,
`std::string`, `std::string_view`, `std::unique_ptr`, `std::function`, `std::to_string` — and routes
every allocation through frt's global `operator new`/`delete` over the `frt_platform_*` seam. M1–M4
proved this **compiles and runs on the host**. M5 asks whether the same code, compiled freestanding,
**links on a target with no OS, no libc++abi, and no exception runtime**:

- **Providable (a GO):** `operator new`/`delete` (frt defines these); `std::__throw_*` terminate
  shims + `__libcpp_verbose_abort` (→ `frt_platform_panic`); the libc++ extern-template library
  members (`std::string`'s char specialization, `std::to_string` — a target `libc++.a`, or
  `-D_LIBCPP_DISABLE_EXTERN_TEMPLATE` to inline them); the libc CRT floor (`memcpy`/`memset`/…); and
  on ELF, the compiler-rt / outline-atomic / TLS helpers.
- **Fatal (a NO-GO):** the exception unwinder (`__cxa_throw`, `_Unwind_*`, `__gxx_personality_v0`,
  `std::terminate`/`std::rethrow_exception`/`exception_ptr` & friends, `__clang_call_terminate`) or
  RTTI (`typeinfo`, `__dynamic_cast`, `__cxa_bad_*`, `std::type_info`).

## 2. Method

`runtime/frt/tools/nm_audit.sh` does four things, under the **device's own freestanding flags**
(`-std=c++23 -ffreestanding -fno-exceptions -fno-rtti -fno-stack-protector -fno-threadsafe-statics`
— the last is mandated by `frt/config.hpp` and removes the `__cxa_guard_*` atomic guards a single-core
target cannot host):

1. **Compile, no link**, at **both `-O0` and `-O2`** (an optimizer both elides and exposes shims, so
   the audit takes the union), the three shipping std-heavy TUs (`build_context`/`reactive`/`host`),
   **plus a representative consumer TU** (exercises `std::to_string` + a by-value `std::string` copy —
   std operations a real app performs that the library TUs don't), **plus the device-relevant frt
   providers** (`new_delete.cpp`, `platform.cpp` — the latter holds the one function-local-static
   atomic seam, exactly where a guard/atomics regression would hide).
2. **Classify** every undefined symbol (`nm -u`, demangled) into buckets. GO requires the two FATAL
   buckets empty, every `operator new`/`delete` defined by frt, and the REVIEW bucket empty.
3. **Static-initializer check** (`nm` defined symbols + `otool`/`readelf` sections): `nm -u` is blind
   to global-constructor machinery, which lives in *defined* `__GLOBAL__sub_I_*`/`__cxx_global_var_init`
   symbols and `__mod_init_func`/`.init_array` sections. A global with a non-trivial ctor but trivial
   dtor registers no `__cxa_atexit`, so it slips the undefined classifier while still needing
   `.init_array` startup. The check NO-GOs on any such machinery.
4. **Closure link backstop**: a real `-nostdlib++` link (drops libc++ **and** libc++abi) of the
   subjects + frt providers + a throwaway floor, with `-D_LIBCPP_DISABLE_EXTERN_TEMPLATE`. If it
   **closes**, the binding needs no C++ runtime support library for the EH/operator-new question. GO
   requires it CLOSED, so a classifier miss the link catches flips the verdict to NO-GO.

Two **self-validating negative controls** keep the gate honest: an EH control (a TU referencing
`std::current_exception`/`rethrow_exception`, which compiles under `-fno-exceptions`) must classify
FATAL, and a static-init control (a global with a non-trivial ctor) must trip the section check. If
either fails to fire, the harness aborts (exit 2) rather than report a false GO.

## 3. Result (current run)

```
[FATAL]  EXCEPTION UNWINDER / EH RUNTIME : (none)     classifier self-test: EH control -> FATAL ✓
[FATAL]  RTTI                            : (none)
[provide] operator new/delete            : 4   (all defined by frt new_delete.cpp ✓)
[provide] std::__throw_*                  : (none, at these instantiations)
[provide] __cxa_guard_*                   : (none — empty under -fno-threadsafe-statics ✓)
[provide] libc++ lib members             : 4   (basic_string __init / copy-ctor / dtor; to_string(int))
[provide] ELF/AArch64 RT helpers          : (none on host; bucket ready for the M6 ELF run)
[floor]  libc CRT                        : 5   (memcpy memset memmove memcmp strlen)
[floor]  libc misc                       : 1   (abort)
[frt]    frt seam / cross-TU             : 4   (frt_platform_alloc/free/panic, frt::host_ops())
[link]   engine ABI / intra-binding      : 19  (canopy_host_*, canopy:: cross-TU — resolve at link)
[REVIEW] unclassified                    : 0
static-init machinery: 0 (control tripped ✓)   closure link: CLOSED ✓
VERDICT: GO
```

## 4. What M6 must provide (candidate floor — config-dependent, verify on ELF)

The GO is conditional on supplying these — none need an unwinder. **This table is a candidate floor
derived on the host; the exact symbol set is libc++-config-dependent and is confirmed only when M6
compiles against the real cross libc++ and re-runs the audit on ELF objects.**

| Symbol class | Who provides it | Notes |
|---|---|---|
| `operator new`/`delete` (sized + aligned) | **frt** `new_delete.cpp` | done; gated by `FRT_OWN_NEW_DELETE` |
| `std::__throw_*` family | **M6 floor** → `frt_platform_panic` | route libc++'s own `__throw_*` to panic in the library build, not by redefining `std::` internals |
| `__libcpp_verbose_abort` | **M6 floor** → `frt_platform_panic` | only referenced in libc++ DEBUG hardening; FAST mode traps inline |
| `std::string` extern-template members, `std::to_string` | **target `libc++.a`** *or* `-D_LIBCPP_DISABLE_EXTERN_TEMPLATE` | the `libc++.a` branch is **unverified** until M6 links a real cross libc++ |
| `memcpy`/`memset`/`memmove`/`memcmp`/`strlen`, `abort` | **libc CRT** (M6) | the standard freestanding-libc floor |
| compiler-rt builtins / AArch64 outline atomics / TLS | **compiler-rt + flags** | see §6.1 — pin `-mcpu=cortex-a76` so atomics inline to LSE |

The harness's closure proof contains a working ~20-line preview of the throw/guard floor. It is a
**non-ODR-correct stand-in** (it defines `std::__1::__throw_*` directly); the shipping device build
routes libc++'s own shims to panic instead.

## 5. Why a host proxy answers the EH/RTTI question

The audit runs on `arm64-apple-darwin` (Apple clang 17, Mach-O, libc++ `std::__1`), not the literal
`aarch64-unknown-none` ELF target, because a faithful `-none` reproduction needs a **cross-configured
libc++** (no vendor-availability markup, single-threaded) and the only cross-capable libc++ here
(Homebrew LLVM 22) has an **Apple-baked `__config_site`** that refuses every non-Apple triple — M6
toolchain work. **Whether `-fno-exceptions` code emits a personality routine is a property of the
compiler's codegen mode, not the object format or triple**, so the EH/RTTI verdict transfers. The
adversarial sweep confirmed this **directly on `aarch64-unknown-none-elf`**: the same throwing-capable
snippet emits zero unwinder symbols under `-fno-exceptions` and the full `_Unwind_Resume` /
`__cxa_begin_catch` / `__gxx_personality_v0` set under `-fexceptions`. It is invariant across `-O0`
… `-O3`, `-Os`, `-Oz`, and ThinLTO/full-LTO.

## 6. Limitations & residual risk

1. **ELF-only target symbols are not covered by a Mach-O host audit.** `aarch64-unknown-none` ELF can
   mandate symbols this audit never sees: **AArch64 outline atomics** (`__aarch64_swp8_acq_rel`, …),
   compiler-rt builtins (`__udivti3`), TLS (`__cxa_thread_atexit`, `__tls_get_addr`). These are
   providable and not an unwinder; the classifier now has a bucket for them so they won't trip a false
   REVIEW on the M6 run. **The RK3588S is Cortex-A76/A55 = ARMv8.2-A with mandatory LSE: with
   `-mcpu=cortex-a76`/`-march=armv8.2-a` the atomics inline to native LSE and no outline-atomic symbol
   appears** (verified) — so the device build must pin that `-mcpu`. **Confirming the complete ELF
   floor is the one open item before the GO is unconditional for the device.**
2. **The `target libc++.a` provider branch is unverified.** Only the `-D_LIBCPP_DISABLE_EXTERN_TEMPLATE`
   (inline) branch is exercised by the closure link. M6 must link a real cross-built `libc++.a` and
   confirm *that* closes with no `_Unwind_`/`__cxa_throw`/personality.
3. **The closure link's "needs no C++ runtime library" is scoped to the EH/operator-new question.**
   It closes because `-O2` + extern-template-disable inline away the libc++ library members; the
   `std::to_string`/`basic_string` members are still real libc++ *library* symbols (bucketed
   `[provide] libc++ lib members`) that a device satisfies via the target `libc++.a`. The closure
   does **not** claim those vanish — it claims no *unwinder/EH/operator-new* dependency beyond frt.
4. **M8 (shared allocator with Rust) is untested here.** This GO covers the C++ std-closure only.
   Before building a shared backend, audit a lane that links the real `canopy_abi` Rust staticlib
   (`panic=abort`) against frt and confirms one allocator symbol set and Rust's panic routed to frt.
5. **`host_ops()` is a strong symbol** the device must override by replacing the backend TU, and the
   contract that no allocation occurs before `install_platform()` is documented, not yet tested.
6. **`-ffreestanding` mangles `main`**; the closure proof forces `extern "C"` to satisfy the host crt.
   A device has its own entry and never hits this — noted so nobody re-discovers it.

## 7. What the adversarial sweep changed

A 9-agent sweep attacked the GO from 8 failure modes + a completeness critic. The EH/RTTI core
**could not be broken** (confirmed on the real ELF triple, all opt levels, LTO, and a representative
app TU). It found **fail-open harness defects**, all now fixed:

- the `std::*` catch-all swallowed std-namespaced EH symbols (`std::terminate`, `exception_ptr`, …)
  and `__clang_call_terminate` → added a FATAL arm before the catch-all, **proven by an EH negative
  control** that aborts the harness if the classifier ever fails open;
- the closure link didn't gate the verdict → **GO now requires CLOSED**;
- the audit omitted `-fno-threadsafe-statics` (the device contract) → **added**, which also empties
  the `__cxa_guard_*` bucket;
- providers weren't classified and the consumer surface was under-covered → **`platform.cpp`/
  `new_delete.cpp` and a representative `to_string`+string-copy TU are now in the audited union**;
- the static-init blind spot (this milestone's namesake catch) → **defined-symbol + section check
  with its own negative control**.

## 8. Reproducibility

`runtime/frt/tools/nm_audit.sh` is self-contained (Apple clang via `xcrun`, bash 3.2 compatible, no
build dir), compiles nothing into the shipping libraries, writes no files outside a temp dir, and is
CI-gateable by exit code. Re-run it after any change to the binding's std usage or frt's allocator to
catch a regression that newly drags the unwinder, RTTI, a global constructor, or an unclassified
symbol.

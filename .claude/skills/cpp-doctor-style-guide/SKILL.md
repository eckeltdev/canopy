---
name: cpp-doctor-style-guide
description: Write C++ that conforms to cpp-doctor's rules, naming, and formatting up front, so the after-check finds fewer issues.
license: MIT
compatibility: "claude-code, cursor"
---
# cpp-doctor C++ Style Guide

Write conforming C++ the first time. This guide is **generated** from cpp-doctor's rule registry and this project's configuration, so it always matches what `cpp-doctor check` enforces. Each rule below leads with a good example; run `cpp-doctor explain <id>` for the full rationale, a bad example, and the suggested fix.

- **C++ standard:** `c++23` — prefer its modern facilities (RAII, `std::span`, `std::expected`, concepts) over legacy idioms.
- **Scope:** the rules shown are the families enabled for this project. A finding you cannot avoid can be suppressed inline (see *Suppressing a finding*).

## Naming & formatting

These come from the project's `.clang-format` and `.clang-tidy`; `cpp-doctor format` and `cpp-doctor fix --safe` will reformat, but write it right the first time:

- **Formatting:** LLVM base style, 4-space indent (never tabs), 100-column limit.
- **Braces & pointers:** attach braces (`if (c) {`); left-aligned `*`/`&` (`int* p`, `const T& r`); keep `#include`s sorted.
- **Types:** `snake_case` for classes and structs (e.g. `widget_pool`), not `PascalCase`.
- **Functions & variables:** `snake_case` (e.g. `compute_total`, `item_count`).
- **Concepts:** `CamelCase` (e.g. `Hashable`, `RandomAccessRange`).
- **Private members:** trailing underscore (e.g. `count_`, `buffer_`); no leading underscore.
- **Type aliases / values:** follow the standard library's `_t` / `_v` convention (`value_type`, `is_trivial_v`).

## Rules by family

Each line is a rule id, its severity, and a conforming example. Run `cpp-doctor explain <id>` for the rationale and fix.

### Ownership & lifetimes

- `ownership.no-raw-delete` (error) — Avoid raw `delete` → `// no delete: the std::unique_ptr<Widget> frees it on scope exit`
- `ownership.no-raw-new` (error) — Avoid raw owning `new` → `auto w = std::make_unique<Widget>(args);`

### Concurrency

- `concurrency.no-detached-thread` (warning) — Avoid detached threads → `std::jthread t(worker); // joined automatically on scope exit`
- `concurrency.no-volatile-sync` (error) — `volatile` is not synchronization → `std::atomic<bool> done_{false};`
- `concurrency.require-relaxed-comment` (warning) — Explain relaxed memory order → `// relaxed: counter is independent, no data depends on its value count_.fetch_add(1, std::memory_order_relaxed);`

### Dangerous patterns

- `dangerous.no-c-style-cast` (warning) — Avoid C-style casts → `int n = static_cast<int>(floating_value);`
- `dangerous.no-memcpy-nontrivial-warning` (warning) — Verify `memcpy` targets are trivially copyable → `dst = src; // or std::copy for ranges of trivial types`

### Header hygiene

- `headers.no-relative-parent-include` (warning) — No `../` parent-relative includes → `#include "core/widget.hpp" // rooted at an include directory`
- `headers.no-using-namespace` (error) — No `using namespace` in headers → `// widget.hpp using std::string; // or fully qualify at use sites`
- `headers.require-include-guard` (warning) — Header needs an include guard → `// widget.hpp #pragma once struct Widget { int id; };`

### Layout

- `layout.abi-struct-needs-size-assert` (warning) — Packed struct needs a size assertion → `#pragma pack(push, 1) struct Header { uint32_t magic; uint16_t len; }; #pragma pack(pop) static_assert(sizeof(Header) == 6);`
- `layout.file-too-large` (warning) — File is too large → `// parser.cpp, codegen.cpp, io.cpp — one responsibility each`
- `layout.function-too-large` (warning) — Function is too large → `void process() { parse(); transform(); emit(); }`

### Comments

- `comments.no-todo-without-owner` (note) — TODO/FIXME needs an owner → `// TODO(alice): handle the timeout case (or TODO(JIRA-1234))`

### Build

- `build.require-compile-commands` (error) — Project needs compile_commands.json → `set(CMAKE_EXPORT_COMPILE_COMMANDS ON) # in CMakeLists.txt`

Keep files under 500 lines and function bodies under 80 lines; split along natural seams before you hit the limit.

## Suppressing a finding

When a finding is a deliberate, reviewed exception, silence it with a comment directive — never delete the rule or weaken the config:

```cpp
auto* p = new Foo();  // cpp-doctor: allow ownership.no-raw-new
// cpp-doctor: allow-next-line dangerous
int x = (int)y;
// cpp-doctor: allow-file comments
```

`allow` targets the directive's own line, `allow-next-line` the next line, and `allow-file` the whole file. The optional spec list takes rule ids (`ownership.no-raw-new`) or whole families (`ownership`).

## Verify loop

After writing or editing C++, drive it to zero findings:

1. Write code per this guide.
2. `cpp-doctor check --json` — snapshot the findings.
3. `cpp-doctor fix --safe` — let the tool absorb the mechanical fixes.
4. Fix what remains by family; run `cpp-doctor explain <id>` for any rule you are unsure about.
5. Re-run `cpp-doctor check` and repeat until it is clean.

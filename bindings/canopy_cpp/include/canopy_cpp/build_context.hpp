#pragma once

#include <cstdint>
#include <functional>
#include <map>
#include <memory>
#include <string>
#include <string_view>
#include <vector>

#include "canopy_cpp/inplace_function.hpp"
#include "canopy_cpp/protocol.hpp"

namespace canopy {

    // The reactive runtime lives in reactive.hpp (it pulls <vector>-backed registries); here it is
    // only forward-declared so build_context can OWN one without dragging the runtime header (and
    // its include cost) into every consumer. `build_context.cpp` includes reactive.hpp to define
    // the runtime accessors. The custom deleter lets the unique_ptr hold an incomplete type.
    class reactive_runtime;

    // Opaque, author-minted handles — the C++ mirror of the protocol's id newtypes.
    struct node_id {
        std::uint64_t raw = 0;
    };
    struct str_id {
        std::uint32_t raw = 0;
    };
    struct handler_id {
        std::uint32_t raw = 0;
    };

    // The callable an `on_click`-style listener runs — stored INLINE (no heap), so handlers
    // work on the freestanding target and compile under -fno-exceptions. A capture larger
    // than the inline budget is a compile error, never a silent heap allocation.
    using click_handler = inplace_function<void()>;

    // The implicit host root every top-level node mounts under.
    inline constexpr node_id root{wire::node_root};

    // Accumulates Canopy op bytes the way `canopy-core::Emitter` does: author-minted
    // node ids (monotonic from 1), interned-once strings, and a pending op buffer that
    // `take_batch` wraps in BeginBatch/EndBatch. The string table and the id counters
    // persist across batches; only the pending bytes drain. This is the freestanding
    // encoder the DSL builds on — no Rust, no FFI; it just fills a byte buffer.
    class build_context {
    public:
        build_context();
        build_context(const build_context&) = delete;
        auto operator=(const build_context&) -> build_context& = delete;
        build_context(build_context&&) = delete;
        // The `&&` move-assignment trips the c-style-cast heuristic; deleted special member.
        // cpp-doctor: allow-next-line dangerous.no-c-style-cast
        auto operator=(build_context&&) -> build_context& = delete;
        // Declared (defaulted in the .cpp) so the unique_ptr<reactive_runtime> can hold an
        // incomplete type at this header's point of definition.
        ~build_context();

        // Create a host element of `tag` (see `wire::el_*`); returns its handle.
        auto create_element(std::uint16_t tag) -> node_id;
        // Create a text leaf holding `text`; returns its handle.
        auto create_text(std::string_view text) -> node_id;
        // Append `child` as the last child of `parent`.
        void append(node_id parent, node_id child);
        // Insert `child` under `parent` before `anchor` (`node_null` anchor = append).
        void insert_before(node_id parent, node_id child, node_id anchor);

        void set_class(node_id node, std::string_view name);
        void set_tag_name(node_id node, std::string_view name);
        void set_attribute(node_id node, std::uint16_t attr, std::string_view value);
        void set_inline_style(node_id node, std::uint16_t prop, std::string_view value);
        void set_text(node_id node, std::string_view text);

        // Register a listener for `event` (see `wire::event_*`); returns the handler id
        // the host echoes back in a DispatchEvent. This raw form mints an id with no stored
        // callable (protocol-only use).
        auto add_listener(node_id node, std::uint16_t event) -> handler_id;

        // Register a listener that runs `handler` when `event` fires on `node`. The callable
        // is parked in the context's handler table, keyed by the returned id; draining real
        // events into it is P3. `invoke_handler` runs it directly for now (tests / P3 drain).
        auto add_listener(node_id node, std::uint16_t event, click_handler handler) -> handler_id;

        // Invoke the stored handler for `handler` if one is present; returns whether it fired.
        auto invoke_handler(handler_id handler) -> bool;

        // Intern `text`, emitting an InternString op only the first time it is seen.
        auto intern(std::string_view text) -> str_id;

        // Wrap the pending ops in BeginBatch(version, seq)/EndBatch and return the bytes,
        // draining the pending buffer. The intern table and id counters persist.
        auto take_batch(std::uint32_t seq) -> std::vector<std::uint8_t>;

        // The reactive runtime this context owns (the dirty-set + flush engine). The DSL's
        // reactive `text(λ)` overload registers a binding here when the runtime is active; a click
        // handler's `signal.set` marks that binding dirty.
        [[nodiscard]] auto runtime() noexcept -> reactive_runtime&;

        // Re-run every dirty effect once, each emitting one targeted op into this context's pending
        // buffer (e.g. a reactive text binding emits exactly one `SetText`). Installs this
        // context's runtime as active for the duration. Call after a `signal.set` and before
        // `take_batch` to collect the surgical update batch. A no-op when nothing is dirty.
        void flush();

    private:
        auto alloc_node() -> node_id;

        std::vector<std::uint8_t> ops_;
        // `std::less<>` is transparent, so a `string_view` looks up without allocating.
        std::map<std::string, str_id, std::less<>> interned_;
        // Parked listener callables, indexed by handler id (ids are minted monotonically, so
        // the id IS the index). P3 drains real events into these.
        std::vector<click_handler> handlers_;
        std::uint64_t next_node_ = 1;
        std::uint32_t next_str_ = 0;
        std::uint32_t next_handler_ = 0;
        // The owned reactive runtime (incomplete here; constructed in the .cpp). Held by pointer so
        // this header need not include the <vector>-backed reactive registry — and so a context
        // that never goes reactive pays only one allocation, not the whole engine inline.
        std::unique_ptr<reactive_runtime> runtime_;
    };

} // namespace canopy

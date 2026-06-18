#pragma once

#include <concepts>
#include <cstdint>
#include <memory>
#include <string>
#include <string_view>
#include <tuple>
#include <type_traits>
#include <utility>

#include "canopy_cpp/build_context.hpp"
#include "canopy_cpp/reactive.hpp"
#include "canopy_cpp/signal.hpp"

// A Leptos-builder-like authoring DSL that compiles to the Canopy op-stream. The factories
// (div/row/button/text/cls/on_click/...) build lightweight VALUE descriptions — no context,
// no emission — and `mount` walks that description ONCE in source order, interning strings
// and emitting ops deterministically. C++ argument evaluation is unsequenced, so emission is
// driven by the sequenced fold here, never by argument construction: the bytes are byte-for-
// byte identical to the equivalent hand-written build_context calls (the P1 parity gate).
namespace canopy {

    // ---- description nodes (allocator-free value types) ------------------------------------

    // A host element of `tag` (see wire::el_*) with its children captured by value.
    template <class... Children> struct element {
        std::uint16_t tag;
        std::tuple<Children...> children;
    };

    // A class name to add to the enclosing element (the host-side cascade resolves it).
    struct class_modifier {
        std::string name;
    };

    // A static text leaf.
    struct text_node {
        std::string text;
    };

    // A reactive text leaf: `func` is resolved during mount. In P1 it resolves ONCE (byte-
    // identical to static text); P4 re-runs it on signal change and emits a targeted SetText.
    template <class Fn> struct dynamic_text {
        Fn func;
    };

    // An event listener bound to the enclosing element; `handler` runs when it fires.
    struct click_listener {
        std::uint16_t event;
        click_handler handler;
    };

    // The element's CSS id (the host's reserved AttrId::ID slot).
    struct id_attribute {
        std::string value;
    };

    // An arbitrary host attribute (a host-minted AttrId) with a string value.
    struct attribute {
        std::uint16_t attr;
        std::string value;
    };

    // An inline style declaration: a PropId (e.g. `wire::prop_bg`) with a string value.
    struct inline_style {
        std::uint16_t prop;
        std::string value;
    };

    // An overriding tag name the host retains for tag/attribute selectors.
    struct tag_name {
        std::string name;
    };

    // ---- concepts --------------------------------------------------------------------------

    namespace detail {
        template <class T> struct is_element : std::false_type {};
        template <class... Children> struct is_element<element<Children...>> : std::true_type {};
    } // namespace detail

    template <class T>
    concept Element = detail::is_element<std::remove_cvref_t<T>>::value;

    // A user component: it knows how to splice its own subtree under a parent node. Author it
    // as `void build(build_context&, node_id parent) const`.
    template <class T>
    concept Component =
        requires(const T comp, build_context& ctx, node_id parent) { comp.build(ctx, parent); };

    // A string-like child (a bare literal becomes a text leaf), excluding our own node types
    // so a marker/element/component never mis-routes to text.
    template <class T>
    concept StringLike =
        std::convertible_to<const T&, std::string_view> && !Element<T> && !Component<T>;

    // ---- mount (description -> ops) --------------------------------------------------------

    namespace detail {

        template <class... Children>
        auto apply(build_context& ctx, node_id parent, const element<Children...>& node) -> node_id;

        inline void apply(build_context& ctx, node_id parent, const class_modifier& mod) {
            ctx.set_class(parent, mod.name);
        }

        inline void apply(build_context& ctx, node_id parent, const text_node& leaf) {
            ctx.append(parent, ctx.create_text(leaf.text));
        }

        // The heap-owned state a reactive text binding carries across runs: the resolver closure,
        // the build_context to emit into, and the text node it owns (filled after creation). It
        // outlives mount because the runtime keeps re-running the effect on later flushes.
        template <class Fn> struct text_binding {
            Fn func;
            build_context* ctx;
            node_id node{};
            bool node_created = false;
        };

        // The effect body for a reactive text binding. First run (node_created == false) only
        // resolves the closure — under the runtime's `running` it subscribes the signals it reads —
        // and stashes nothing extra; the caller reads the value via `func()` again to seed the
        // CreateText, so this run emits NO op (preserving byte-parity with the static path).
        // Subsequent runs (after the node exists) re-resolve and emit exactly ONE targeted SetText.
        template <class Fn> void run_text_binding(void* ctx_data, effect_id /*self*/) {
            auto* binding = static_cast<text_binding<Fn>*>(ctx_data);
            const std::string value{binding->func()};
            if (binding->node_created) {
                binding->ctx->set_text(binding->node, value); // surgical update: one SetText op
            }
        }

        template <class Fn>
        void apply(build_context& ctx, node_id parent, const dynamic_text<Fn>& leaf) {
            // Force an OWNED string: `auto` would deduce the closure's exact return type, and a
            // closure returning a string_view into a temporary would dangle before create_text
            // reads it. std::string copies the bytes regardless of what func returns.
            reactive_runtime* runtime = active_runtime();
            if (runtime == nullptr) {
                // STATIC PATH: no runtime installed → resolve once, byte-identical to static text.
                const std::string value{leaf.func()};
                ctx.append(parent, ctx.create_text(value));
                return;
            }

            // REACTIVE PATH: register a binding effect (runs once now, subscribing the signals the
            // closure reads), then create the text node from the SAME resolved value. The effect
            // re-runs on flush, each run emitting one SetText for `node`. The structural ops here
            // (CreateText + InsertBefore) are byte-identical to the static path; only the dirty-set
            // bookkeeping differs, which emits nothing until a signal changes.
            auto binding = std::make_unique<text_binding<Fn>>(
                text_binding<Fn>{.func = leaf.func, .ctx = &ctx});
            auto* raw = binding.get();
            const auto free_ctx = [](void* ctx_data) noexcept {
                // Reclaim ownership into a unique_ptr so teardown is RAII — no raw delete.
                std::unique_ptr<text_binding<Fn>>{static_cast<text_binding<Fn>*>(ctx_data)};
            };
            // The runtime takes ownership of the binding box; release the unique_ptr to it.
            runtime->register_effect(&run_text_binding<Fn>, binding.release(), free_ctx);

            // The first effect run above subscribed the signals; resolve once more for the initial
            // CreateText content (the same value), then hand the node to the binding so later runs
            // target it with SetText.
            const std::string value{raw->func()};
            const node_id node = ctx.create_text(value);
            ctx.append(parent, node);
            raw->node = node;
            raw->node_created = true;
        }

        inline void apply(build_context& ctx, node_id parent, const click_listener& listener) {
            ctx.add_listener(parent, listener.event, listener.handler);
        }

        inline void apply(build_context& ctx, node_id parent, const id_attribute& mod) {
            ctx.set_attribute(parent, wire::attr_id, mod.value);
        }

        inline void apply(build_context& ctx, node_id parent, const attribute& mod) {
            ctx.set_attribute(parent, mod.attr, mod.value);
        }

        inline void apply(build_context& ctx, node_id parent, const inline_style& mod) {
            ctx.set_inline_style(parent, mod.prop, mod.value);
        }

        inline void apply(build_context& ctx, node_id parent, const tag_name& mod) {
            ctx.set_tag_name(parent, mod.name);
        }

        template <Component C> void apply(build_context& ctx, node_id parent, const C& comp) {
            comp.build(ctx, parent);
        }

        template <class... Children>
        auto apply(build_context& ctx, node_id parent, const element<Children...>& node)
            -> node_id {
            const node_id self = ctx.create_element(node.tag);
            std::apply([&](const auto&... child) { (apply(ctx, self, child), ...); },
                       node.children);
            ctx.append(parent, self);
            return self;
        }

    } // namespace detail

    // Mount `tree` under `parent` (defaults to the host root); returns the new element node.
    //
    // AUTO-DISCOVERY: the build pass installs this context's reactive runtime as active for the
    // walk, so a reactive `text(λ)` resolves a runtime through the seam and registers a binding —
    // no component wires a runtime in by hand. A tree with no reactive slot installs the runtime
    // but never touches it, so its bytes are byte-identical to the non-reactive path.
    template <class... Children>
    auto mount(build_context& ctx, node_id parent, const element<Children...>& tree) -> node_id {
        const active_runtime_scope scope(&ctx.runtime());
        return detail::apply(ctx, parent, tree);
    }
    template <class... Children>
    auto mount(build_context& ctx, const element<Children...>& tree) -> node_id {
        return mount(ctx, root, tree);
    }

    // ---- factories -------------------------------------------------------------------------

    namespace detail {
        // Owned-ify a string-like child so the stack-resident description never parks a view:
        // a string_view backed by a temporary would dangle before mount reads it. The copy
        // happens HERE, while the backing is still alive (inside the factory full-expression).
        // Non-string children pass through, decayed by value.
        template <class T> auto normalize_child(T&& child) {
            if constexpr (StringLike<std::remove_cvref_t<T>>) {
                return text_node{.text = std::string{std::string_view{child}}};
            } else {
                return std::forward<T>(child);
            }
        }
    } // namespace detail

    template <class... Children> auto make_element(std::uint16_t tag, Children&&... children) {
        return element{.tag = tag,
                       .children = std::make_tuple(
                           detail::normalize_child(std::forward<Children>(children))...)};
    }

    // `div` maps to the host COLUMN element — there is no `el_div` in the parity-checked
    // header, and a C++-only tag value must never be minted. Use `el(tag)` for any other tag.
    template <class... Children> auto div(Children&&... children) {
        return make_element(wire::el_column, std::forward<Children>(children)...);
    }
    template <class... Children> auto row(Children&&... children) {
        return make_element(wire::el_row, std::forward<Children>(children)...);
    }
    template <class... Children> auto button(Children&&... children) {
        return make_element(wire::el_button, std::forward<Children>(children)...);
    }
    template <class... Children> auto input(Children&&... children) {
        return make_element(wire::el_input, std::forward<Children>(children)...);
    }

    // Escape hatch for any host tag id (which must come from the parity-checked header, never
    // a C++-only value): `el(my_tag)(child, ...)`.
    inline auto el(std::uint16_t tag) {
        return [tag](auto&&... children) {
            return make_element(tag, std::forward<decltype(children)>(children)...);
        };
    }

    inline auto cls(std::string_view name) -> class_modifier {
        return {.name = std::string(name)};
    }

    // The element's CSS id (`id("main")` -> SetAttribute on the reserved id slot).
    inline auto id(std::string_view value) -> id_attribute {
        return {.value = std::string(value)};
    }

    // An arbitrary attribute (`attr(some_attr_id, "v")` -> SetAttribute).
    inline auto attr(std::uint16_t kind, std::string_view value) -> attribute {
        return {.attr = kind, .value = std::string(value)};
    }

    // An inline style (`style(wire::prop_bg, "#fff")` -> SetInlineStyle).
    inline auto style(std::uint16_t prop, std::string_view value) -> inline_style {
        return {.prop = prop, .value = std::string(value)};
    }

    // An overriding tag name (`tag("section")` -> SetTagName).
    inline auto tag(std::string_view name) -> tag_name {
        return {.name = std::string(name)};
    }

    inline auto text(std::string_view value) -> text_node {
        return {.text = std::string(value)};
    }
    template <class Fn>
        requires std::invocable<Fn>
    auto text(Fn&& func) -> dynamic_text<std::decay_t<Fn>> {
        return {.func = std::forward<Fn>(func)};
    }

    template <class Fn>
        requires std::invocable<Fn>
    auto on_click(Fn&& func) -> click_listener {
        return {.event = wire::event_click, .handler = click_handler{std::forward<Fn>(func)}};
    }

} // namespace canopy

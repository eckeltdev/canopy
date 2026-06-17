#pragma once

#include <concepts>
#include <cstdint>
#include <string>
#include <string_view>
#include <tuple>
#include <type_traits>
#include <utility>

#include "canopy_cpp/build_context.hpp"

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

        template <class Fn>
        void apply(build_context& ctx, node_id parent, const dynamic_text<Fn>& leaf) {
            // Force an OWNED string: `auto` would deduce the closure's exact return type, and a
            // closure returning a string_view into a temporary would dangle before create_text
            // reads it. std::string copies the bytes regardless of what func returns.
            const std::string value{leaf.func()}; // P4: re-run on change; P1 resolves once
            ctx.append(parent, ctx.create_text(value));
        }

        inline void apply(build_context& ctx, node_id parent, const click_listener& listener) {
            ctx.add_listener(parent, listener.event, listener.handler);
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
    template <class... Children>
    auto mount(build_context& ctx, node_id parent, const element<Children...>& tree) -> node_id {
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

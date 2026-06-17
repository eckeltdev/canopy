#include <array>
#include <cstdint>
#include <iostream>
#include <string>
#include <vector>

#include "canopy_cpp/dsl.hpp"
#include "canopy_cpp/signal.hpp"

namespace {

    using bytes = std::vector<std::uint8_t>;

    void dump(const char* label, const bytes& data) {
        std::cerr << "  " << label << " (" << data.size() << "):" << std::hex;
        for (std::uint8_t value : data) {
            std::cerr << ' ' << static_cast<int>(value);
        }
        std::cerr << std::dec << '\n';
    }

    bool same(const bytes& got, const bytes& want, const char* what) {
        if (got == want) {
            return true;
        }
        std::cerr << "FAIL: " << what << '\n';
        dump("got ", got);
        dump("want", want);
        return false;
    }

    // ---- byte anchor: pins the DSL output to an INDEPENDENT hand-authored byte array, so a
    // bug that is symmetric across the DSL and the raw encoder (which the parity tests below
    // cannot catch) still fails here.
    bool dsl_tree_is_byte_exact() {
        canopy::build_context ctx;
        canopy::mount(ctx, canopy::div(canopy::cls("a"), canopy::text("x")));
        const bytes want = {
            0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,                   // BeginBatch v1 seq0
            0x10, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, // CreateElement n1 COLUMN
            0x1b, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x61, // InternString s0 "a"
            0x17, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // SetClass n1 s0
            0x1b, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x78, // InternString s1 "x"
            0x11, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, // CreateText n2 s1
            0x13, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // InsertBefore(n1,n2,NULL)
            0x13, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // InsertBefore(root,n1,NULL)
            0x02,                                                       // EndBatch
        };
        return same(ctx.take_batch(0), want, "dsl tree byte-exact");
    }

    // ---- parity helper: the DSL must be exact sugar over build_context, so every DSL tree
    // emits identical bytes to the equivalent raw encoder calls. (The raw side is byte-pinned
    // by encoder_test, so parity transitively pins the DSL.)

    bool sugar_over_encoder() {
        canopy::build_context dsl;
        canopy::mount(dsl, canopy::button(canopy::cls("primary"), "Click"));

        canopy::build_context raw;
        const canopy::node_id btn = raw.create_element(canopy::wire::el_button);
        raw.set_class(btn, "primary");
        raw.append(btn, raw.create_text("Click"));
        raw.append(canopy::root, btn);

        return same(dsl.take_batch(0), raw.take_batch(0), "dsl == raw encoder");
    }

    // Multiple text children pin the left-to-right SEQUENCED fold (the core safety claim).
    bool multiple_children_emit_in_source_order() {
        canopy::build_context dsl;
        canopy::mount(dsl, canopy::div(canopy::text("a"), canopy::text("b"), canopy::text("c")));

        canopy::build_context raw;
        const canopy::node_id col = raw.create_element(canopy::wire::el_column);
        raw.append(col, raw.create_text("a"));
        raw.append(col, raw.create_text("b"));
        raw.append(col, raw.create_text("c"));
        raw.append(canopy::root, col);

        return same(dsl.take_batch(0), raw.take_batch(0), "children in source order");
    }

    // Nested elements: children fully emitted and appended before the next sibling.
    bool nested_elements_post_order() {
        canopy::build_context dsl;
        canopy::mount(dsl, canopy::div(canopy::div(canopy::text("inner")), canopy::text("outer")));

        canopy::build_context raw;
        const canopy::node_id outer = raw.create_element(canopy::wire::el_column);
        const canopy::node_id inner = raw.create_element(canopy::wire::el_column);
        raw.append(inner, raw.create_text("inner"));
        raw.append(outer, inner);
        raw.append(outer, raw.create_text("outer"));
        raw.append(canopy::root, outer);

        return same(dsl.take_batch(0), raw.take_batch(0), "nested post-order");
    }

    // A literal, a std::string, and an explicit text() leaf all route identically (owned).
    bool every_string_child_kind_routes_to_text() {
        canopy::build_context dsl;
        canopy::mount(dsl, canopy::div(std::string("sa"), "lit", canopy::text("tn")));

        canopy::build_context raw;
        const canopy::node_id col = raw.create_element(canopy::wire::el_column);
        raw.append(col, raw.create_text("sa"));
        raw.append(col, raw.create_text("lit"));
        raw.append(col, raw.create_text("tn"));
        raw.append(canopy::root, col);

        return same(dsl.take_batch(0), raw.take_batch(0), "all string kinds -> text");
    }

    // The same string used as a class AND as text interns exactly once across roles.
    bool a_string_used_in_two_roles_interns_once() {
        canopy::build_context dsl;
        canopy::mount(dsl, canopy::div(canopy::cls("dup"), canopy::text("dup")));

        canopy::build_context raw;
        const canopy::node_id col = raw.create_element(canopy::wire::el_column);
        raw.set_class(col, "dup");
        raw.append(col, raw.create_text("dup"));
        raw.append(canopy::root, col);

        return same(dsl.take_batch(0), raw.take_batch(0), "intern reuse across roles");
    }

    // An empty element emits CreateElement + InsertBefore and nothing else.
    bool empty_element_has_no_children() {
        canopy::build_context dsl;
        canopy::mount(dsl, canopy::div());

        canopy::build_context raw;
        raw.append(canopy::root, raw.create_element(canopy::wire::el_column));

        return same(dsl.take_batch(0), raw.take_batch(0), "empty div");
    }

    // A Component spliced BETWEEN static siblings keeps source order.
    struct counter {
        int initial_value = 0;
        void build(canopy::build_context& ctx, canopy::node_id parent) const {
            ctx.append(parent, ctx.create_text(std::to_string(initial_value)));
        }
    };

    bool component_interleaved_keeps_order() {
        canopy::build_context dsl;
        canopy::mount(dsl,
                      canopy::div(canopy::text("a"), counter{.initial_value = 2}, canopy::text("b")));

        canopy::build_context raw;
        const canopy::node_id col = raw.create_element(canopy::wire::el_column);
        raw.append(col, raw.create_text("a"));
        raw.append(col, raw.create_text("2"));
        raw.append(col, raw.create_text("b"));
        raw.append(canopy::root, col);

        return same(dsl.take_batch(0), raw.take_batch(0), "component interleaved order");
    }

    bool single_child_component_splices_in_place() {
        canopy::build_context dsl;
        canopy::mount(dsl, canopy::div(counter{.initial_value = 3}));

        canopy::build_context raw;
        const canopy::node_id col = raw.create_element(canopy::wire::el_column);
        raw.append(col, raw.create_text("3"));
        raw.append(canopy::root, col);

        return same(dsl.take_batch(0), raw.take_batch(0), "component splice");
    }

    bool reactive_text_resolves_once() {
        canopy::signal<int> count{7};
        canopy::build_context dsl;
        canopy::mount(dsl, canopy::div(canopy::text([&] { return std::to_string(count.get()); })));

        canopy::build_context raw;
        const canopy::node_id col = raw.create_element(canopy::wire::el_column);
        raw.append(col, raw.create_text("7"));
        raw.append(canopy::root, col);

        return same(dsl.take_batch(0), raw.take_batch(0), "reactive text resolves once");
    }

    // The on_click emission path (AddListener op) must be byte-identical to a raw add_listener
    // on the SAME node — this pins the op bytes AND that the listener landed on the button.
    bool on_click_emits_add_listener_on_the_right_node() {
        int clicks = 0;
        canopy::build_context dsl;
        canopy::mount(dsl, canopy::button(canopy::on_click([&] { ++clicks; }), "Go"));

        canopy::build_context raw;
        const canopy::node_id btn = raw.create_element(canopy::wire::el_button);
        raw.add_listener(btn, canopy::wire::event_click);
        raw.append(btn, raw.create_text("Go"));
        raw.append(canopy::root, btn);

        return same(dsl.take_batch(0), raw.take_batch(0), "on_click AddListener parity");
    }

    // Two listeners must route to their OWN closures: handler N runs the Nth closure.
    bool handlers_route_to_their_own_closures() {
        int first = 0;
        int second = 0;
        canopy::build_context ctx;
        canopy::mount(ctx, canopy::button(canopy::on_click([&] { first += 1; }), "a"));
        canopy::mount(ctx, canopy::button(canopy::on_click([&] { second += 10; }), "b"));

        const bool fired0 = ctx.invoke_handler(canopy::handler_id{0});
        const bool fired1 = ctx.invoke_handler(canopy::handler_id{1});
        if (!fired0 || !fired1 || first != 1 || second != 10) {
            std::cerr << "FAIL: handler routing (first=" << first << " second=" << second << ")\n";
            return false;
        }
        return true;
    }

    // The raw (callable-less) add_listener mints a slot with no callable: invoking it is a
    // no-op-false, and it does not shift the ids of stored handlers around it.
    bool a_callable_less_listener_does_not_misroute_ids() {
        int clicks = 0;
        canopy::build_context ctx;
        const canopy::node_id btn = ctx.create_element(canopy::wire::el_button);
        ctx.add_listener(btn, canopy::wire::event_click);                  // id 0: no callable
        ctx.add_listener(btn, canopy::wire::event_click, [&] { ++clicks; }); // id 1: stored

        if (ctx.invoke_handler(canopy::handler_id{0})) {
            std::cerr << "FAIL: empty handler 0 fired\n";
            return false;
        }
        if (!ctx.invoke_handler(canopy::handler_id{1}) || clicks != 1) {
            std::cerr << "FAIL: stored handler 1 (clicks=" << clicks << ")\n";
            return false;
        }
        return true;
    }

} // namespace

int main() {
    const std::array results = {
        dsl_tree_is_byte_exact(),
        sugar_over_encoder(),
        multiple_children_emit_in_source_order(),
        nested_elements_post_order(),
        every_string_child_kind_routes_to_text(),
        a_string_used_in_two_roles_interns_once(),
        empty_element_has_no_children(),
        component_interleaved_keeps_order(),
        single_child_component_splices_in_place(),
        reactive_text_resolves_once(),
        on_click_emits_add_listener_on_the_right_node(),
        handlers_route_to_their_own_closures(),
        a_callable_less_listener_does_not_misroute_ids(),
    };
    for (const bool passed : results) {
        if (!passed) {
            return 1;
        }
    }
    std::cerr << "ok: all DSL tests passed\n";
    return 0;
}

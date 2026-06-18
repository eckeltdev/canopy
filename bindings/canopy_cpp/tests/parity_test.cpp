#include <cstddef>
#include <cstdint>
#include <iostream>
#include <string>
#include <vector>

#include "canopy.h" // the Rust C ABI (canopy-abi)

#include "canopy_cpp/dsl.hpp"

// Cross-producer parity SWEEP. Each case authors a tree with the C++ builder DSL, applies its
// op bytes through the REAL Rust engine (canopy_host_apply, from libcanopy_abi.a), and asserts
// canopy_host_debug_snapshot equals the exact string the Rust Emitter would yield for the
// equivalent authoring. The snapshot is the cross-producer oracle: same bytes -> same Dom ->
// same snapshot, so a passing case proves the C++ emitter and the Rust Emitter are byte-for-byte
// equivalent for that shape. The sweep covers deep nesting, multi-class nodes, every inline-style
// PropId, id + attr, an overriding tag name, multiple listeners, plain + reactive text, the
// row/column/button/input widgets, an empty element, and sibling ordering.
//
// Snapshot format (see canopy-abi::CanopyHost::debug_snapshot): pre-order DFS, two spaces of
// indent per depth; a text node is `text=<content>`; an element is `el tag=<n>` then, when
// present, ` name=` ` class=` (comma-joined, op order) ` style=` (PropId ascending, ';'-joined)
// ` attr=` (AttrId ascending, ';'-joined) ` on=` (op order, ','-joined, `event:handler`). Handler
// ids mint monotonically from 0 within one build_context.
namespace {

    // Apply `batch` through a fresh host and return its debug snapshot (or a marker on error).
    std::string snapshot_of(const std::vector<std::uint8_t>& batch) {
        CanopyHost* host = canopy_host_new();
        const int apply_rc = canopy_host_apply(host, batch.data(), batch.size());
        if (apply_rc != CANOPY_OK) {
            canopy_host_free(host);
            std::cerr << "FAIL: canopy_host_apply returned " << apply_rc << '\n';
            return "<apply-error>";
        }

        std::vector<std::uint8_t> buf(256);
        std::size_t len = 0;
        int snap_rc = canopy_host_debug_snapshot(host, buf.data(), buf.size(), &len);
        if (snap_rc == CANOPY_ERR_TOO_LARGE) { // grow once to the reported size and retry
            buf.resize(len);
            snap_rc = canopy_host_debug_snapshot(host, buf.data(), buf.size(), &len);
        }
        canopy_host_free(host);
        if (snap_rc != CANOPY_OK) {
            std::cerr << "FAIL: canopy_host_debug_snapshot returned " << snap_rc << '\n';
            return "<snapshot-error>";
        }
        using diff = std::vector<std::uint8_t>::difference_type;
        return {buf.begin(), buf.begin() + static_cast<diff>(len)};
    }

    // Run one parity case: apply `ctx`'s batch through the engine and compare the snapshot against
    // `want`. Prints a labelled got/want diff on mismatch and returns false. Each case is named so
    // a failure points straight at the offending tree.
    bool check_case(const char* name, canopy::build_context& ctx, const std::string& want) {
        const std::string got = snapshot_of(ctx.take_batch(0));
        if (got == want) {
            return true;
        }
        std::cerr << "FAIL[" << name << "]: snapshot mismatch\n--- got ---\n"
                  << got << "--- want ---\n"
                  << want << "-----------\n";
        return false;
    }

    // 1. Deep nesting: column > row > button > text, four levels, indent grows two spaces a level.
    bool deep_nesting() {
        canopy::build_context ctx;
        canopy::mount(ctx, canopy::div(canopy::row(canopy::button(canopy::text("Go")))));
        return check_case("deep_nesting", ctx,
                          "el tag=1\n  el tag=2\n    el tag=3\n      text=Go\n");
    }

    // 2. Multiple classes on one node: deduped, kept in authored (op) order, comma-joined.
    bool multiple_classes() {
        canopy::build_context ctx;
        canopy::mount(ctx, canopy::div(canopy::cls("card"), canopy::cls("elevated"),
                                       canopy::cls("dark")));
        return check_case("multiple_classes", ctx, "el tag=1 class=card,elevated,dark\n");
    }

    // 3. Every inline-style PropId on one node. Authored deliberately OUT of id order; the host
    //    re-sorts styles by ascending PropId (BTreeMap), so the snapshot is always id-ordered:
    //    bg=1 fg=2 width=3 height=4 gap=5 padding=6 direction=7 radius=8 align=12 justify=13.
    bool every_style_prop() {
        canopy::build_context ctx;
        namespace w = canopy::wire;
        canopy::mount(ctx, canopy::div(canopy::style(w::prop_justify, "center"),
                                       canopy::style(w::prop_align, "start"),
                                       canopy::style(w::prop_radius, "4"),
                                       canopy::style(w::prop_direction, "row"),
                                       canopy::style(w::prop_padding, "8"),
                                       canopy::style(w::prop_gap, "2"),
                                       canopy::style(w::prop_height, "20"),
                                       canopy::style(w::prop_width, "10"),
                                       canopy::style(w::prop_fg, "#000"),
                                       canopy::style(w::prop_bg, "#fff")));
        return check_case(
            "every_style_prop", ctx,
            "el tag=1 style=1:#fff;2:#000;3:10;4:20;5:2;6:8;7:row;8:4;12:start;13:center\n");
    }

    // 4. id + arbitrary attr. The id maps to the reserved AttrId 1; a second attr (id 2) sorts
    //    after it. Attrs render in ascending AttrId order regardless of authored order.
    bool id_and_attr() {
        canopy::build_context ctx;
        // attr(2) authored before id(1); the host re-sorts by AttrId.
        canopy::mount(ctx, canopy::div(canopy::attr(2, "main"), canopy::id("hero")));
        return check_case("id_and_attr", ctx, "el tag=1 attr=1:hero;2:main\n");
    }

    // 5. Overriding tag name: `name=` appears right after `el tag=`, before class/style/attr/on.
    bool set_tag_name() {
        canopy::build_context ctx;
        canopy::mount(ctx, canopy::div(canopy::tag("section"), canopy::cls("wrap")));
        return check_case("set_tag_name", ctx, "el tag=1 name=section class=wrap\n");
    }

    // 6. Multiple listeners on one tree: two buttons each with one click listener. Handler ids
    //    mint monotonically from 0 across the whole context, so the first button is on=1:0 and the
    //    second is on=1:1 (event_click is 1).
    bool multiple_listeners() {
        canopy::build_context ctx;
        canopy::mount(ctx, canopy::div(canopy::button(canopy::on_click([] {}), "A"),
                                       canopy::button(canopy::on_click([] {}), "B")));
        return check_case("multiple_listeners", ctx,
                          "el tag=1\n  el tag=3 on=1:0\n    text=A\n"
                          "  el tag=3 on=1:1\n    text=B\n");
    }

    // 7. Two listeners on a SINGLE node: two on_click bindings on one button. The host does NOT
    //    dedup on (node,event) the way classes dedup, so both are retained in op order and comma-
    //    joined as `event:handler`; handler ids 0 and 1 mint in authoring order (event_click is 1).
    bool two_listeners_one_node() {
        canopy::build_context ctx;
        canopy::mount(ctx, canopy::button(canopy::on_click([] {}), canopy::on_click([] {}), "X"));
        return check_case("two_listeners_one_node", ctx, "el tag=3 on=1:0,1:1\n  text=X\n");
    }

    // 8. Plain static text leaf under a column.
    bool plain_text() {
        canopy::build_context ctx;
        canopy::mount(ctx, canopy::div(canopy::text("Hello")));
        return check_case("plain_text", ctx, "el tag=1\n  text=Hello\n");
    }

    // 9. Reactive text(λ): with no runtime driving updates the closure resolves once at mount,
    //    byte-identical to a static text leaf (the structural ops match exactly).
    bool reactive_text() {
        canopy::build_context ctx;
        const int count = 7;
        canopy::mount(ctx,
                      canopy::div(canopy::text([count] { return "n=" + std::to_string(count); })));
        return check_case("reactive_text", ctx, "el tag=1\n  text=n=7\n");
    }

    // 10. A row widget (tag=2) with two text children, in sibling order.
    bool row_widget() {
        canopy::build_context ctx;
        canopy::mount(ctx, canopy::row(canopy::text("L"), canopy::text("R")));
        return check_case("row_widget", ctx, "el tag=2\n  text=L\n  text=R\n");
    }

    // 11. A button widget (tag=3) holding a label.
    bool button_widget() {
        canopy::build_context ctx;
        canopy::mount(ctx, canopy::button(canopy::text("Submit")));
        return check_case("button_widget", ctx, "el tag=3\n  text=Submit\n");
    }

    // 12. An input widget (tag=4) carrying an inline style and an id, no children.
    bool input_widget() {
        canopy::build_context ctx;
        canopy::mount(ctx, canopy::input(canopy::id("email"),
                                         canopy::style(canopy::wire::prop_width, "200")));
        return check_case("input_widget", ctx, "el tag=4 style=3:200 attr=1:email\n");
    }

    // 13. An empty element: no children, no modifiers — just the bare `el tag=` line.
    bool empty_element() {
        canopy::build_context ctx;
        canopy::mount(ctx, canopy::div());
        return check_case("empty_element", ctx, "el tag=1\n");
    }

    // 14. Sibling ordering across mixed child kinds: text, then a row, then a button, all direct
    //     children of one column, must appear in exactly the authored order.
    bool sibling_ordering() {
        canopy::build_context ctx;
        canopy::mount(ctx, canopy::div(canopy::text("top"), canopy::row(canopy::text("mid")),
                                       canopy::button(canopy::text("bot"))));
        return check_case("sibling_ordering", ctx,
                          "el tag=1\n  text=top\n  el tag=2\n    text=mid\n"
                          "  el tag=3\n    text=bot\n");
    }

    // 15. A "kitchen sink" node combining every field at once: tag name, two classes, two styles
    //     (re-sorted), id + attr (re-sorted), a listener, and a text child — exercising the full
    //     field ordering in one line.
    bool combined_fields() {
        canopy::build_context ctx;
        namespace w = canopy::wire;
        canopy::mount(ctx, canopy::button(canopy::tag("cta"), canopy::cls("primary"),
                                          canopy::cls("lg"), canopy::style(w::prop_width, "120"),
                                          canopy::style(w::prop_bg, "#09f"),
                                          canopy::attr(2, "x"), canopy::id("buy"),
                                          canopy::on_click([] {}), canopy::text("Buy")));
        return check_case(
            "combined_fields", ctx,
            "el tag=3 name=cta class=primary,lg style=1:#09f;3:120 attr=1:buy;2:x on=1:0\n"
            "  text=Buy\n");
    }

} // namespace

int main() {
    struct parity_case {
        const char* name;
        bool (*run)();
    };
    const std::vector<parity_case> cases = {
        {"deep_nesting", deep_nesting},
        {"multiple_classes", multiple_classes},
        {"every_style_prop", every_style_prop},
        {"id_and_attr", id_and_attr},
        {"set_tag_name", set_tag_name},
        {"multiple_listeners", multiple_listeners},
        {"two_listeners_one_node", two_listeners_one_node},
        {"plain_text", plain_text},
        {"reactive_text", reactive_text},
        {"row_widget", row_widget},
        {"button_widget", button_widget},
        {"input_widget", input_widget},
        {"empty_element", empty_element},
        {"sibling_ordering", sibling_ordering},
        {"combined_fields", combined_fields},
    };

    int failures = 0;
    for (const parity_case& one : cases) {
        if (!one.run()) {
            ++failures;
        }
    }

    if (failures == 0) {
        std::cerr << "ok: all " << cases.size()
                  << " DSL trees match the Rust-Emitter snapshot through the real engine\n";
        return 0;
    }
    std::cerr << "FAIL: " << failures << " of " << cases.size() << " parity cases mismatched\n";
    return 1;
}

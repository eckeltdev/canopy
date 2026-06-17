#include <cstdint>
#include <iostream>
#include <vector>

#include "canopy_cpp/build_context.hpp"

namespace {

    using bytes = std::vector<std::uint8_t>;

    void dump(const char* label, const bytes& data) {
        std::cerr << "  " << label << " (" << data.size() << "):" << std::hex;
        for (std::uint8_t value : data) {
            std::cerr << ' ' << static_cast<int>(value);
        }
        std::cerr << std::dec << '\n';
    }

    bool check(const bytes& got, const bytes& want, const char* what) {
        if (got == want) {
            return true;
        }
        std::cerr << "FAIL: " << what << '\n';
        dump("got ", got);
        dump("want", want);
        return false;
    }

    // A minimal tree (a column appended under ROOT) must encode to exactly these bytes —
    // the byte-for-byte contract a host's OpReader decodes.
    bool minimal_tree_is_byte_exact() {
        canopy::build_context ctx;
        canopy::node_id col = ctx.create_element(canopy::wire::el_column);
        ctx.append(canopy::root, col);

        const bytes want = {
            // BeginBatch(version = 1, seq = 0)
            0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
            // CreateElement(node = 1, tag = COLUMN = 1)
            0x10, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00,
            // InsertBefore(parent = ROOT = 0, child = 1, anchor = NULL)
            0x13, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            // EndBatch
            0x02,
        };
        return check(ctx.take_batch(0), want, "minimal tree");
    }

    // A repeated string is interned exactly once: the second create_text reuses StrId 0
    // and emits no second InternString.
    bool strings_are_interned_once() {
        canopy::build_context ctx;
        ctx.create_text("x"); // interns "x" -> StrId 0, node 1
        ctx.create_text("x"); // reuses StrId 0, node 2; NO new InternString

        const bytes want = {
            0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,                   // BeginBatch
            0x1b, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x78, // InternString(0, "x")
            0x11, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // CreateText n1
            0x11, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // CreateText n2
            0x02,                                                       // EndBatch
        };
        return check(ctx.take_batch(0), want, "intern once");
    }

    // The intern table and the node counter persist across take_batch: a string interned
    // in one batch is not re-interned in the next, and node ids keep counting up.
    bool state_persists_across_batches() {
        canopy::build_context ctx;
        ctx.create_text("x"); // batch 0: interns "x", node 1
        ctx.take_batch(0);
        ctx.create_text("x"); // batch 1: reuses StrId 0 (no InternString), node 2

        const bytes want = {
            0x01, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00,                   // BeginBatch(seq = 1)
            0x11, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // CreateText n2
            0x02,                                                       // EndBatch
        };
        return check(ctx.take_batch(1), want, "state persists across batches");
    }

    // Pin the exact bytes of the ops the DSL leans on but the minimal tree never exercised:
    // a non-COLUMN element tag (BUTTON), SetClass, AddListener (tag + node + u16 event + the
    // monotonic u32 handler id), and SetText. The DSL parity tests trust these encodings.
    bool button_ops_are_byte_exact() {
        canopy::build_context ctx;
        const canopy::node_id btn = ctx.create_element(canopy::wire::el_button);
        ctx.set_class(btn, "c");
        ctx.add_listener(btn, canopy::wire::event_click);
        ctx.set_text(btn, "t");

        const bytes want = {
            0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,                   // BeginBatch v1 seq0
            0x10, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, // CreateElement n1 BUTTON(3)
            0x1b, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x63, // InternString s0 "c"
            0x17, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // SetClass n1 s0
            0x19, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
            0x00, 0x00,                                                 // AddListener n1 event=1 handler=0
            0x1b, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x74, // InternString s1 "t"
            0x14, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, // SetText n1 s1
            0x02,                                                       // EndBatch
        };
        return check(ctx.take_batch(0), want, "button ops byte-exact");
    }

} // namespace

int main() {
    const bool tree_ok = minimal_tree_is_byte_exact();
    const bool intern_ok = strings_are_interned_once();
    const bool persist_ok = state_persists_across_batches();
    if (tree_ok && intern_ok && persist_ok) {
        std::cerr << "ok: all encoder byte-oracle tests passed\n";
        return 0;
    }
    return 1;
}

#include <cstddef>
#include <cstdint>
#include <iostream>
#include <string>
#include <vector>

#include "canopy.h" // the Rust C ABI (canopy-abi)

#include "canopy_cpp/build_context.hpp"
#include "canopy_cpp/dsl.hpp"
#include "canopy_cpp/protocol.hpp"
#include "canopy_cpp/reactive.hpp"
#include "canopy_cpp/signal.hpp"

// The P4 fine-grained-reactivity proof. The dirty-set + flush runtime must turn a `count.set(9)`
// into exactly ONE targeted `SetText` — no rebuild — and that surgical batch, applied to the SAME
// live host the initial tree was applied to, must move the rendered text from 7 to 9.
//
// This is the end-to-end claim: mount a reactive `text([&]{ to_string(count.get()) })` with
// count=7, apply through the real engine, snapshot (text=7); set count=9, flush, take_batch, and
// assert the batch is one SetText (tag 0x14) with ZERO structural ops (no 0x10 CreateElement /
// 0x11 CreateText / 0x13 InsertBefore); apply it to the same host; snapshot (text=9).
namespace {

    using bytes = std::vector<std::uint8_t>;

    // Drain the live host's deterministic tree dump (grow-once on a too-small buffer).
    std::string snapshot_of(CanopyHost* host) {
        std::vector<std::uint8_t> buf(256);
        std::size_t len = 0;
        int snap_rc = canopy_host_debug_snapshot(host, buf.data(), buf.size(), &len);
        if (snap_rc == CANOPY_ERR_TOO_LARGE) {
            buf.resize(len);
            snap_rc = canopy_host_debug_snapshot(host, buf.data(), buf.size(), &len);
        }
        if (snap_rc != CANOPY_OK) {
            std::cerr << "FAIL: canopy_host_debug_snapshot returned " << snap_rc << '\n';
            return "<snapshot-error>";
        }
        using diff = std::vector<std::uint8_t>::difference_type;
        return {buf.begin(), buf.begin() + static_cast<diff>(len)};
    }

    // Count occurrences of an op tag byte in a decoded batch. The batch is BeginBatch then a flat
    // op stream; for the surgical update we only need to confirm WHICH op tags are present, and the
    // op set here (BeginBatch, optional InternString, SetText, EndBatch) is unambiguous on the tag
    // byte because the value StrId of a fresh "9" is interned first. We therefore assert on the
    // structural tags directly: their absence is the whole point.
    bool tag_absent(const bytes& batch, std::uint8_t tag, const char* name) {
        for (const std::uint8_t value : batch) {
            if (value == tag) {
                std::cerr << "FAIL: structural op " << name << " (tag 0x" << std::hex
                          << static_cast<int>(tag) << std::dec << ") present in update batch\n";
                return false;
            }
        }
        return true;
    }

    // Count how many times `tag` appears as an OP tag by walking the batch with the field layout
    // of the ops a flush can emit: InternString (variable length) and SetText (fixed). This is a
    // precise op-level count, not a raw byte scan, so a StrId/len byte that happens to equal 0x14
    // cannot inflate the SetText count.
    int count_set_text_ops(const bytes& batch) {
        int set_text = 0;
        std::size_t pos = 0;
        const std::size_t len = batch.size();
        while (pos < len) {
            const std::uint8_t op = batch[pos];
            pos += 1;
            if (op == canopy::wire::op_begin_batch) {
                pos += 2 + 4; // version:u16, seq:u32
            } else if (op == canopy::wire::op_end_batch) {
                break;
            } else if (op == canopy::wire::op_intern_string) {
                // id:u32, len:u32, bytes[len]
                if (pos + 8 > len) {
                    break;
                }
                std::uint32_t str_len = 0;
                for (std::size_t shift = 0; shift < 4; ++shift) {
                    str_len |= static_cast<std::uint32_t>(batch[pos + 4 + shift]) << (8 * shift);
                }
                pos += 8 + str_len;
            } else if (op == canopy::wire::op_set_text) {
                set_text += 1;
                pos += 8 + 4; // node:u64, text:StrId(u32)
            } else {
                // Any other op in a flush batch is unexpected; bail so a miscount is loud.
                std::cerr << "FAIL: unexpected op 0x" << std::hex << static_cast<int>(op)
                          << std::dec << " in flush batch\n";
                return -1;
            }
        }
        return set_text;
    }

    bool reactive_set_flush_emits_one_set_text_and_no_structural_ops() {
        canopy::signal<int> count{7};
        canopy::build_context ctx;

        // Mount the reactive tree. The build pass installs ctx's runtime as active, so the reactive
        // `text` overload registers a binding (an effect bound to the created text node).
        canopy::mount(ctx, canopy::div(canopy::text([&] { return std::to_string(count.get()); })));

        CanopyHost* host = canopy_host_new();
        const bytes initial = ctx.take_batch(0);
        if (canopy_host_apply(host, initial.data(), initial.size()) != CANOPY_OK) {
            std::cerr << "FAIL: initial apply\n";
            canopy_host_free(host);
            return false;
        }

        const std::string snap_before = snapshot_of(host);
        const std::string want_before = "el tag=1\n  text=7\n";
        if (snap_before != want_before) {
            std::cerr << "FAIL: initial snapshot\n--- got ---\n"
                      << snap_before << "--- want ---\n"
                      << want_before;
            canopy_host_free(host);
            return false;
        }

        // The reactive update. `set` runs under the active runtime (installed here exactly as the
        // event loop's pump does around a handler) so it marks the bound effect dirty; `flush`
        // re-runs that one effect, which emits one SetText into ctx's pending buffer.
        {
            const canopy::active_runtime_scope scope(&ctx.runtime());
            count.set(9);
            ctx.flush();
        }

        const bytes update = ctx.take_batch(1);

        const int set_text_ops = count_set_text_ops(update);
        if (set_text_ops != 1) {
            std::cerr << "FAIL: expected exactly one SetText, got " << set_text_ops << '\n';
            canopy_host_free(host);
            return false;
        }
        const bool no_structural =
            tag_absent(update, canopy::wire::op_create_element, "CreateElement") &&
            tag_absent(update, canopy::wire::op_create_text, "CreateText") &&
            tag_absent(update, canopy::wire::op_insert_before, "InsertBefore");
        if (!no_structural) {
            canopy_host_free(host);
            return false;
        }

        // Apply the surgical batch to the SAME host and confirm the text moved 7 -> 9 with no
        // structural change to the tree.
        if (canopy_host_apply(host, update.data(), update.size()) != CANOPY_OK) {
            std::cerr << "FAIL: update apply\n";
            canopy_host_free(host);
            return false;
        }
        const std::string snap_after = snapshot_of(host);
        const std::string want_after = "el tag=1\n  text=9\n";
        const bool ok = snap_after == want_after;
        if (!ok) {
            std::cerr << "FAIL: post-update snapshot\n--- got ---\n"
                      << snap_after << "--- want ---\n"
                      << want_after;
        }
        canopy_host_free(host);
        return ok;
    }

    // A second signal that NOTHING reads must not dirty the text binding: setting it and flushing
    // emits an empty batch (no SetText). This pins that subscriptions are per-signal, not global.
    bool an_unrelated_signal_does_not_dirty_the_binding() {
        canopy::signal<int> shown{1};
        canopy::signal<int> other{0};
        canopy::build_context ctx;
        canopy::mount(ctx, canopy::div(canopy::text([&] { return std::to_string(shown.get()); })));
        (void)ctx.take_batch(0); // drop the mount batch

        {
            const canopy::active_runtime_scope scope(&ctx.runtime());
            other.set(42); // no effect reads `other`
            ctx.flush();
        }
        const int set_text_ops = count_set_text_ops(ctx.take_batch(1));
        if (set_text_ops != 0) {
            std::cerr << "FAIL: unrelated signal emitted " << set_text_ops << " SetText op(s)\n";
            return false;
        }
        return true;
    }

    // Two sequential updates each emit exactly one SetText, and the runtime keeps re-subscribing
    // across flushes (the effect re-reads the signal each run), so the second set still fires.
    bool successive_sets_each_emit_one_set_text() {
        canopy::signal<int> count{0};
        canopy::build_context ctx;
        canopy::mount(ctx, canopy::div(canopy::text([&] { return std::to_string(count.get()); })));
        (void)ctx.take_batch(0);

        for (int step = 1; step <= 2; ++step) {
            {
                const canopy::active_runtime_scope scope(&ctx.runtime());
                count.set(step);
                ctx.flush();
            }
            const int set_text_ops = count_set_text_ops(ctx.take_batch(static_cast<std::uint32_t>(step)));
            if (set_text_ops != 1) {
                std::cerr << "FAIL: step " << step << " emitted " << set_text_ops << " SetText op(s)\n";
                return false;
            }
        }
        return true;
    }

    // The static path is untouched: mounting WITHOUT ever setting a signal and taking the batch
    // emits the same bytes a plain static text child would — the zero-overhead guarantee, asserted
    // here as well as by the unchanged dsl_test byte anchors.
    bool static_path_is_byte_identical_to_plain_text() {
        canopy::signal<int> count{7};
        canopy::build_context reactive;
        canopy::mount(reactive,
                      canopy::div(canopy::text([&] { return std::to_string(count.get()); })));

        canopy::build_context plain;
        canopy::mount(plain, canopy::div(canopy::text("7")));

        const bool ok = reactive.take_batch(0) == plain.take_batch(0);
        if (!ok) {
            std::cerr << "FAIL: reactive mount bytes diverged from static text\n";
        }
        return ok;
    }

} // namespace

int main() {
    const bool all_passed = reactive_set_flush_emits_one_set_text_and_no_structural_ops() &&
                            an_unrelated_signal_does_not_dirty_the_binding() &&
                            successive_sets_each_emit_one_set_text() &&
                            static_path_is_byte_identical_to_plain_text();
    if (all_passed) {
        std::cerr << "ok: reactive runtime tests passed\n";
        return 0;
    }
    return 1;
}

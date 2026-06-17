#include "canopy_cpp/build_context.hpp"

#include <cstdint>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "canopy_cpp/protocol.hpp"

namespace canopy {

    namespace {

        // Little-endian byte appenders — the encoding the protocol uses for every field.
        void push_u8(std::vector<std::uint8_t>& out, std::uint8_t value) {
            out.push_back(value);
        }

        void push_u16(std::vector<std::uint8_t>& out, std::uint16_t value) {
            out.push_back(static_cast<std::uint8_t>(value));
            out.push_back(static_cast<std::uint8_t>(value >> 8));
        }

        void push_u32(std::vector<std::uint8_t>& out, std::uint32_t value) {
            out.push_back(static_cast<std::uint8_t>(value));
            out.push_back(static_cast<std::uint8_t>(value >> 8));
            out.push_back(static_cast<std::uint8_t>(value >> 16));
            out.push_back(static_cast<std::uint8_t>(value >> 24));
        }

        void push_u64(std::vector<std::uint8_t>& out, std::uint64_t value) {
            for (unsigned shift = 0; shift < 64; shift += 8) {
                out.push_back(static_cast<std::uint8_t>(value >> shift));
            }
        }

    } // namespace

    auto build_context::alloc_node() -> node_id {
        return node_id{next_node_++};
    }

    auto build_context::intern(std::string_view text) -> str_id {
        if (auto found = interned_.find(text); found != interned_.end()) {
            return found->second;
        }
        const str_id new_id{next_str_++};
        interned_.emplace(std::string(text), new_id);
        push_u8(ops_, wire::op_intern_string);
        push_u32(ops_, new_id.raw);
        push_u32(ops_, static_cast<std::uint32_t>(text.size()));
        for (const char byte : text) {
            push_u8(ops_, static_cast<std::uint8_t>(byte));
        }
        return new_id;
    }

    auto build_context::create_element(std::uint16_t tag) -> node_id {
        node_id node = alloc_node();
        push_u8(ops_, wire::op_create_element);
        push_u64(ops_, node.raw);
        push_u16(ops_, tag);
        return node;
    }

    auto build_context::create_text(std::string_view text) -> node_id {
        const str_id str = intern(text);
        node_id node = alloc_node();
        push_u8(ops_, wire::op_create_text);
        push_u64(ops_, node.raw);
        push_u32(ops_, str.raw);
        return node;
    }

    void build_context::insert_before(node_id parent, node_id child, node_id anchor) {
        push_u8(ops_, wire::op_insert_before);
        push_u64(ops_, parent.raw);
        push_u64(ops_, child.raw);
        push_u64(ops_, anchor.raw);
    }

    void build_context::append(node_id parent, node_id child) {
        insert_before(parent, child, node_id{wire::node_null});
    }

    void build_context::set_class(node_id node, std::string_view name) {
        const str_id str = intern(name);
        push_u8(ops_, wire::op_set_class);
        push_u64(ops_, node.raw);
        push_u32(ops_, str.raw);
    }

    void build_context::set_tag_name(node_id node, std::string_view name) {
        const str_id str = intern(name);
        push_u8(ops_, wire::op_set_tag_name);
        push_u64(ops_, node.raw);
        push_u32(ops_, str.raw);
    }

    void build_context::set_attribute(node_id node, std::uint16_t attr, std::string_view value) {
        const str_id str = intern(value);
        push_u8(ops_, wire::op_set_attribute);
        push_u64(ops_, node.raw);
        push_u16(ops_, attr);
        push_u32(ops_, str.raw);
    }

    void build_context::set_inline_style(node_id node, std::uint16_t prop, std::string_view value) {
        const str_id str = intern(value);
        push_u8(ops_, wire::op_set_inline_style);
        push_u64(ops_, node.raw);
        push_u16(ops_, prop);
        push_u32(ops_, str.raw);
    }

    void build_context::set_text(node_id node, std::string_view text) {
        const str_id str = intern(text);
        push_u8(ops_, wire::op_set_text);
        push_u64(ops_, node.raw);
        push_u32(ops_, str.raw);
    }

    auto build_context::add_listener(node_id node, std::uint16_t event) -> handler_id {
        const handler_id handler{next_handler_++};
        push_u8(ops_, wire::op_add_listener);
        push_u64(ops_, node.raw);
        push_u16(ops_, event);
        push_u32(ops_, handler.raw);
        handlers_.emplace_back(); // keep handlers_[id] aligned with the minted id
        return handler;
    }

    auto build_context::add_listener(node_id node, std::uint16_t event, click_handler handler)
        -> handler_id {
        const handler_id listener_id = add_listener(node, event);
        // listener_id was just minted by add_listener, which emplace_back'd its slot — so the
        // id IS the last index and is in bounds by construction.
        // NOLINTNEXTLINE(cppcoreguidelines-pro-bounds-avoid-unchecked-container-access)
        handlers_[listener_id.raw] = std::move(handler);
        return listener_id;
    }

    auto build_context::invoke_handler(handler_id handler) -> bool {
        if (handler.raw >= handlers_.size()) {
            return false;
        }
        // The guard above proves handler.raw is in bounds.
        // NOLINTNEXTLINE(cppcoreguidelines-pro-bounds-avoid-unchecked-container-access)
        const click_handler& stored = handlers_[handler.raw];
        if (!stored) {
            return false;
        }
        stored();
        return true;
    }

    auto build_context::take_batch(std::uint32_t seq) -> std::vector<std::uint8_t> {
        std::vector<std::uint8_t> batch;
        push_u8(batch, wire::op_begin_batch);
        push_u16(batch, wire::protocol_version);
        push_u32(batch, seq);
        batch.insert(batch.end(), ops_.begin(), ops_.end());
        push_u8(batch, wire::op_end_batch);
        ops_.clear();
        return batch;
    }

} // namespace canopy

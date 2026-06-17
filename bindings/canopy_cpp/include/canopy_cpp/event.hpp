#pragma once

#include <bit> // std::bit_cast (f32 from wire bits)
#include <cstddef>
#include <cstdint>

#include "canopy_cpp/build_context.hpp" // node_id, handler_id
#include "canopy_cpp/protocol.hpp"      // wire op/payload tags

// A header-only, allocator-free decoder for the host -> guest event batch drained from
// canopy_host_poll_events (BeginBatch DispatchEvent* EndBatch). DispatchEvent is the only
// host -> guest op and is the one place the wire writes handler(u32) BEFORE node(u64) — the
// field-order anomaly is honored here. The op-stream carries no in-stream length, so an unknown
// top-level tag is a hard decode error (it cannot be skipped), matching the Rust OpReader.
namespace canopy {

    // A pointer payload (the only event data the C++ path surfaces today).
    struct pointer_payload {
        float pos_x = 0.0F;
        float pos_y = 0.0F;
        std::uint8_t button = 0;
    };

    // One decoded host -> guest event. `pointer` is meaningful only when
    // `payload_kind == wire::payload_pointer`.
    struct dispatch_event {
        handler_id handler;
        node_id node;
        std::uint8_t payload_kind = 0;
        pointer_payload pointer;
    };

    enum class decode_status : std::uint8_t { ok, truncated, unknown_tag };

    namespace detail {

        // Little-endian byte reader over a borrowed span; every read is bounds-checked.
        class byte_reader {
        public:
            byte_reader(const std::uint8_t* data, std::size_t len) : data_(data), len_(len) {}

            [[nodiscard]] auto done() const noexcept -> bool {
                return pos_ >= len_;
            }

            auto read_u8(std::uint8_t& out) -> bool {
                if (len_ - pos_ < 1) {
                    return false;
                }
                out = data_[pos_];
                pos_ += 1;
                return true;
            }
            auto read_u16(std::uint16_t& out) -> bool {
                return read_le(out);
            }
            auto read_u32(std::uint32_t& out) -> bool {
                return read_le(out);
            }
            auto read_u64(std::uint64_t& out) -> bool {
                return read_le(out);
            }
            auto read_f32(float& out) -> bool {
                std::uint32_t bits = 0;
                if (!read_le(bits)) {
                    return false;
                }
                out = std::bit_cast<float>(bits);
                return true;
            }

        private:
            template <class T> auto read_le(T& out) -> bool {
                if (len_ - pos_ < sizeof(T)) {
                    return false;
                }
                T value = 0;
                for (std::size_t shift = 0; shift < sizeof(T); ++shift) {
                    value = static_cast<T>(
                        value | static_cast<T>(static_cast<T>(data_[pos_ + shift]) << (8 * shift)));
                }
                pos_ += sizeof(T);
                out = value;
                return true;
            }

            const std::uint8_t* data_;
            std::size_t len_;
            std::size_t pos_ = 0;
        };

        // Consume a payload whose sub-tag is `kind`, filling `out` for a pointer payload.
        inline auto read_payload(byte_reader& reader, std::uint8_t kind, pointer_payload& out)
            -> decode_status {
            if (kind == wire::payload_none) {
                return decode_status::ok;
            }
            if (kind == wire::payload_pointer) {
                const bool got = reader.read_f32(out.pos_x) && reader.read_f32(out.pos_y) &&
                                 reader.read_u8(out.button);
                return got ? decode_status::ok : decode_status::truncated;
            }
            if (kind == wire::payload_key) {
                std::uint32_t code = 0;
                std::uint8_t mods = 0;
                return (reader.read_u32(code) && reader.read_u8(mods)) ? decode_status::ok
                                                                       : decode_status::truncated;
            }
            if (kind == wire::payload_text) {
                std::uint32_t str = 0;
                return reader.read_u32(str) ? decode_status::ok : decode_status::truncated;
            }
            return decode_status::unknown_tag;
        }

    } // namespace detail

    // Decode an event batch, invoking `on_dispatch(const dispatch_event&)` for each
    // DispatchEvent. Returns ok at EndBatch / clean end, or the first error encountered.
    template <class OnDispatch>
    auto decode_event_batch(const std::uint8_t* data, std::size_t len, OnDispatch on_dispatch)
        -> decode_status {
        detail::byte_reader reader(data, len);
        while (!reader.done()) {
            std::uint8_t op_tag = 0;
            if (!reader.read_u8(op_tag)) {
                return decode_status::truncated;
            }
            if (op_tag == wire::op_begin_batch) {
                std::uint16_t version = 0;
                std::uint32_t seq = 0;
                if (!reader.read_u16(version) || !reader.read_u32(seq)) {
                    return decode_status::truncated;
                }
            } else if (op_tag == wire::op_end_batch) {
                return decode_status::ok;
            } else if (op_tag == wire::op_dispatch_event) {
                std::uint32_t handler_raw = 0;
                std::uint64_t node_raw = 0;
                std::uint8_t kind = 0;
                if (!reader.read_u32(handler_raw) || !reader.read_u64(node_raw) ||
                    !reader.read_u8(kind)) {
                    return decode_status::truncated;
                }
                dispatch_event event;
                const decode_status payload = detail::read_payload(reader, kind, event.pointer);
                if (payload != decode_status::ok) {
                    return payload;
                }
                event.handler = handler_id{handler_raw};
                event.node = node_id{node_raw};
                event.payload_kind = kind;
                on_dispatch(event);
            } else {
                return decode_status::unknown_tag;
            }
        }
        return decode_status::ok;
    }

} // namespace canopy

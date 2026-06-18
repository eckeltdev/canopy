#pragma once

#include <array>
#include <cstddef>
#include <memory>
#include <span>

#include "canopy_rt/config.hpp"

// containers.hpp — heap-free inline containers for the canopy-rt hot path.
//
// Both types here allocate NOTHING: fixed_vector keeps its elements in an inline std::array and
// the arena hands out slices of a caller-owned buffer. Neither ever throws (the whole surface
// compiles under -fno-exceptions): overflow is a `false` return / a nullptr, observable by the
// caller, never an exception. That makes them safe to use on the bare-metal device and in tight
// loops where an allocation or an unwind would be unacceptable.
namespace canopy::rt {

    // A vector with a compile-time capacity and inline storage. No heap, no exceptions.
    //
    // push_back returns false (and is a no-op) when full instead of throwing or reallocating, so
    // a hot path can ignore overflow or branch on it cheaply. Elements live in an inline
    // std::array, so ElementType must be default-constructible; clear() resets the logical size
    // without destroying/reconstructing (fine for the trivial value types this is built for).
    template <typename ElementType, std::size_t Capacity> class fixed_vector {
    public:
        using value_type = ElementType;
        using size_type = std::size_t;
        using iterator = ElementType*;
        using const_iterator = const ElementType*;

        // Append a copy of `value`. Returns false and does nothing if already at capacity.
        [[nodiscard]] auto push_back(const ElementType& value) -> bool {
            if (size_ >= Capacity) {
                return false;
            }
            storage_[size_] = value;
            size_ += 1;
            return true;
        }

        // Drop all elements (logical size only; storage is untouched).
        void clear() {
            size_ = 0;
        }

        [[nodiscard]] auto size() const -> size_type {
            return size_;
        }
        [[nodiscard]] static constexpr auto capacity() -> size_type {
            return Capacity;
        }
        [[nodiscard]] auto empty() const -> bool {
            return size_ == 0;
        }

        // Unchecked element access. Out-of-range index is a programmer error; in a freestanding
        // debug build the CANOPY_RT_PANIC in back() catches the empty case.
        [[nodiscard]] auto operator[](size_type index) -> ElementType& {
            return storage_[index];
        }
        [[nodiscard]] auto operator[](size_type index) const -> const ElementType& {
            return storage_[index];
        }

        // Last element. Trapping on empty keeps a misuse from reading garbage on the device.
        [[nodiscard]] auto back() -> ElementType& {
            if (size_ == 0) {
                CANOPY_RT_PANIC("fixed_vector::back() on empty container");
            }
            return storage_[size_ - 1];
        }
        [[nodiscard]] auto back() const -> const ElementType& {
            if (size_ == 0) {
                CANOPY_RT_PANIC("fixed_vector::back() on empty container");
            }
            return storage_[size_ - 1];
        }

        [[nodiscard]] auto data() -> ElementType* {
            return storage_.data();
        }
        [[nodiscard]] auto data() const -> const ElementType* {
            return storage_.data();
        }

        [[nodiscard]] auto begin() -> iterator {
            return storage_.data();
        }
        [[nodiscard]] auto begin() const -> const_iterator {
            return storage_.data();
        }
        [[nodiscard]] auto end() -> iterator {
            return storage_.data() + size_;
        }
        [[nodiscard]] auto end() const -> const_iterator {
            return storage_.data() + size_;
        }

    private:
        std::array<ElementType, Capacity> storage_{};
        size_type size_{0};
    };

    // A bump (linear) allocator over a caller-provided byte buffer. No heap.
    //
    // allocate(size, align) carves an aligned slice off the front of the remaining buffer and
    // bumps a cursor; it returns nullptr when the request does not fit (never throws). reset()
    // rewinds the cursor so the whole buffer can be reused for the next frame. The arena does
    // not own its storage and runs no destructors — it is for trivially-destructible scratch
    // data with a frame/phase lifetime.
    class arena {
    public:
        // Construct over an existing byte span (e.g. a fixed buffer the caller owns).
        explicit arena(std::span<std::byte> buffer) : buffer_(buffer) {}

        // Carve `size` bytes aligned to `align` (a power of two). Returns nullptr if it does not
        // fit in what remains. A zero-size request yields the current (aligned) cursor.
        [[nodiscard]] auto allocate(std::size_t size, std::size_t align) -> void* {
            void* cursor = buffer_.data() + offset_;
            std::size_t remaining = buffer_.size() - offset_;
            // std::align bumps `cursor` up to `align` and shrinks `remaining` by the padding it
            // skipped, or returns nullptr without touching either if the aligned block will not
            // fit. After it succeeds, `remaining` is the space left AT the aligned cursor, so the
            // aligned cursor's offset is (total - remaining); consuming `size` advances past it.
            // This derives the new offset from the size_t `remaining` alone — no pointer
            // subtraction, so no cast is needed.
            void* aligned = std::align(align, size, cursor, remaining);
            if (aligned == nullptr) {
                return nullptr;
            }
            offset_ = (buffer_.size() - remaining) + size;
            return aligned;
        }

        // Rewind to empty; the backing buffer can be reused.
        void reset() {
            offset_ = 0;
        }

        // Bytes handed out since construction or the last reset().
        [[nodiscard]] auto used() const -> std::size_t {
            return offset_;
        }
        // Total backing-buffer size.
        [[nodiscard]] auto capacity() const -> std::size_t {
            return buffer_.size();
        }

    private:
        std::span<std::byte> buffer_;
        std::size_t offset_{0};
    };

} // namespace canopy::rt

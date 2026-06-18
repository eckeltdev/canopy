#pragma once

#include <array>
#include <cstddef>
#include <expected>
#include <memory>
#include <optional>
#include <span>
#include <string_view>
#include <utility>

// stl.hpp — the freestanding-safe std vocabulary, re-exported under canopy::rt.
//
// These are PLAIN ALIASES of the standard types, not reimplementations. Every one of them is
// usable under -fno-exceptions / -fno-rtti and pulls in no allocator, no thread runtime, and no
// I/O: they are pure value/view/ownership vocabulary. Consumer code spells `canopy::rt::span`
// etc. so the surface reads as one namespace and so a future embedded std-shim could be swapped
// in behind these names without touching call sites.
//
// Deliberately ABSENT (heap- or exception-bound, so not freestanding-safe): std::vector,
// std::string, std::map, std::function, std::shared_ptr. For inline, heap-free storage use the
// containers in containers.hpp (fixed_vector, arena).
namespace canopy::rt {

    // Views and fixed storage.
    template <typename ElementType, std::size_t Extent = std::dynamic_extent>
    using span = std::span<ElementType, Extent>;

    using string_view = std::string_view;

    template <typename ElementType, std::size_t Size> using array = std::array<ElementType, Size>;

    // Sum / product vocabulary.
    template <typename ValueType> using optional = std::optional<ValueType>;

    template <typename ValueType, typename ErrorType>
    using expected = std::expected<ValueType, ErrorType>;

    template <typename ErrorType> using unexpected = std::unexpected<ErrorType>;

    template <typename FirstType, typename SecondType>
    using pair = std::pair<FirstType, SecondType>;

    // Single-owner heap handle. The default deleter calls global operator delete, which in this
    // runtime routes through the canopy platform seam (see new_delete.cpp); it is allowed here
    // because freeing on scope exit never throws. There is no shared_ptr alias on purpose.
    template <typename PointeeType, typename DeleterType = std::default_delete<PointeeType>>
    using unique_ptr = std::unique_ptr<PointeeType, DeleterType>;

    // The byte vocabulary used by the arena and any raw-storage code.
    using byte = std::byte;

} // namespace canopy::rt

#ifdef CANOPY_RT_OWN_NEW_DELETE

#include <cstddef>
#include <new>

#include "canopy_rt/canopy_platform.h"

// The COMPLETE replaceable global allocation-function set, routing every C++ heap allocation
// through the canopy-rt platform seam. Replaceable operator new/delete must have exactly ONE
// definition program-wide, so this file is gated by CANOPY_RT_OWN_NEW_DELETE and added by-source
// only to the single target that owns allocation (never compiled into the canopy_rt static lib).
// Placement new (operator new(size_t, void*)) is deliberately NOT overridden — it must remain
// the standard no-op that constructs in caller-provided storage.
namespace {

    constexpr std::size_t default_align = __STDCPP_DEFAULT_NEW_ALIGNMENT__;

    auto alloc_or_panic(std::size_t size, std::size_t align) -> void* {
        void* ptr = canopy_platform_alloc(size, align);
        if (ptr == nullptr) {
            // Cannot throw std::bad_alloc under -fno-exceptions; the contract is to panic.
            canopy_platform_panic("canopy::rt operator new: out of memory");
        }
        return ptr;
    }

} // namespace

// ---- throwing new (panic on OOM) ----
void* operator new(std::size_t size) {
    return alloc_or_panic(size, default_align);
}
void* operator new[](std::size_t size) {
    return alloc_or_panic(size, default_align);
}
void* operator new(std::size_t size, std::align_val_t align) {
    return alloc_or_panic(size, static_cast<std::size_t>(align));
}
void* operator new[](std::size_t size, std::align_val_t align) {
    return alloc_or_panic(size, static_cast<std::size_t>(align));
}

// ---- nothrow new (return null on OOM) ----
void* operator new(std::size_t size, const std::nothrow_t& /*tag*/) noexcept {
    return canopy_platform_alloc(size, default_align);
}
void* operator new[](std::size_t size, const std::nothrow_t& /*tag*/) noexcept {
    return canopy_platform_alloc(size, default_align);
}
void* operator new(std::size_t size, std::align_val_t align,
                   const std::nothrow_t& /*tag*/) noexcept {
    return canopy_platform_alloc(size, static_cast<std::size_t>(align));
}
void* operator new[](std::size_t size, std::align_val_t align,
                     const std::nothrow_t& /*tag*/) noexcept {
    return canopy_platform_alloc(size, static_cast<std::size_t>(align));
}

// ---- delete (size/align are advisory hints; 0/default where the form doesn't carry them) ----
void operator delete(void* ptr) noexcept {
    canopy_platform_free(ptr, 0, default_align);
}
void operator delete[](void* ptr) noexcept {
    canopy_platform_free(ptr, 0, default_align);
}
void operator delete(void* ptr, std::size_t size) noexcept {
    canopy_platform_free(ptr, size, default_align);
}
void operator delete[](void* ptr, std::size_t size) noexcept {
    canopy_platform_free(ptr, size, default_align);
}
void operator delete(void* ptr, std::align_val_t align) noexcept {
    canopy_platform_free(ptr, 0, static_cast<std::size_t>(align));
}
void operator delete[](void* ptr, std::align_val_t align) noexcept {
    canopy_platform_free(ptr, 0, static_cast<std::size_t>(align));
}
void operator delete(void* ptr, std::size_t size, std::align_val_t align) noexcept {
    canopy_platform_free(ptr, size, static_cast<std::size_t>(align));
}
void operator delete[](void* ptr, std::size_t size, std::align_val_t align) noexcept {
    canopy_platform_free(ptr, size, static_cast<std::size_t>(align));
}

// ---- nothrow delete (chosen when a nothrow-new'd object's constructor fails) ----
void operator delete(void* ptr, const std::nothrow_t& /*tag*/) noexcept {
    canopy_platform_free(ptr, 0, default_align);
}
void operator delete[](void* ptr, const std::nothrow_t& /*tag*/) noexcept {
    canopy_platform_free(ptr, 0, default_align);
}
void operator delete(void* ptr, std::align_val_t align, const std::nothrow_t& /*tag*/) noexcept {
    canopy_platform_free(ptr, 0, static_cast<std::size_t>(align));
}
void operator delete[](void* ptr, std::align_val_t align, const std::nothrow_t& /*tag*/) noexcept {
    canopy_platform_free(ptr, 0, static_cast<std::size_t>(align));
}

#endif // CANOPY_RT_OWN_NEW_DELETE

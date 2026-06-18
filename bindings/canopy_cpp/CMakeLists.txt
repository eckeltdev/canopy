# Generated from cpp-doctor.toml - DO NOT EDIT
# Edit cpp-doctor.toml and re-run `cpp-doctor gen` instead.

cmake_minimum_required(VERSION 3.15...3.31)

if(CMAKE_SOURCE_DIR STREQUAL CMAKE_BINARY_DIR)
	message(
		FATAL_ERROR
		"In-tree builds are not supported. Run CMake from a separate directory: cmake -B build"
	)
endif()

set(CPPDOCTOR_ROOT_PROJECT OFF)
if(CMAKE_CURRENT_SOURCE_DIR STREQUAL CMAKE_SOURCE_DIR)
	set(CPPDOCTOR_ROOT_PROJECT ON)

	# Bootstrap cpp-doctor and automatically regenerate CMakeLists.txt
	include("cmake/cpp-doctor.cmake" OPTIONAL RESULT_VARIABLE CPPDOCTOR_INCLUDE_RESULT)
	if(CPPDOCTOR_INCLUDE_RESULT)
		cpp_doctor()
	endif()

	# Enable IDE folder support
	set_property(GLOBAL PROPERTY USE_FOLDERS ON)

	# Export a compile database for cpp-doctor, clang-tidy, and IDEs
	set(CMAKE_EXPORT_COMPILE_COMMANDS ON)

	# Re-run when cpp-doctor.toml changes (IDE dependency)
	set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS cpp-doctor.toml)
endif()

project(canopy_cpp LANGUAGES CXX VERSION 0.1.0)

# Target: canopy_cpp
add_library(canopy_cpp STATIC)
target_sources(canopy_cpp PRIVATE "src/build_context.cpp" "src/reactive.cpp")
target_include_directories(canopy_cpp PUBLIC "../../crates/canopy-abi/include" include)
target_compile_features(canopy_cpp PUBLIC cxx_std_23)

# Target: canopy_cpp_counter_example
add_executable(canopy_cpp_counter_example)
target_sources(
	canopy_cpp_counter_example
	PRIVATE
	"examples/counter_card/main.cpp"
	"src/build_context.cpp"
	"src/reactive.cpp"
)
target_include_directories(
	canopy_cpp_counter_example
	PRIVATE
	"../../crates/canopy-abi/include"
	include
)
target_compile_features(canopy_cpp_counter_example PRIVATE cxx_std_23)
target_link_options(canopy_cpp_counter_example PRIVATE "-L../../../target/debug" -lcanopy_abi)

# Target: canopy_cpp_dsl_test
add_executable(canopy_cpp_dsl_test)
target_sources(canopy_cpp_dsl_test PRIVATE "tests/dsl_test.cpp")
target_link_libraries(canopy_cpp_dsl_test PRIVATE canopy_cpp)
target_include_directories(canopy_cpp_dsl_test PRIVATE "../../crates/canopy-abi/include" include)
target_compile_features(canopy_cpp_dsl_test PRIVATE cxx_std_23)

# Target: canopy_cpp_event_test
add_executable(canopy_cpp_event_test)
target_sources(
	canopy_cpp_event_test
	PRIVATE
	"src/build_context.cpp"
	"src/host.cpp"
	"src/reactive.cpp"
	"tests/event_loop_test.cpp"
)
target_include_directories(canopy_cpp_event_test PRIVATE "../../crates/canopy-abi/include" include)
target_compile_features(canopy_cpp_event_test PRIVATE cxx_std_23)
target_link_options(canopy_cpp_event_test PRIVATE "-L../../../target/debug" -lcanopy_abi)

# Target: canopy_cpp_freestanding_smoke
add_executable(canopy_cpp_freestanding_smoke)
target_sources(
	canopy_cpp_freestanding_smoke
	PRIVATE
	"src/build_context.cpp"
	"src/reactive.cpp"
	"tests/freestanding_smoke.cpp"
)
target_include_directories(
	canopy_cpp_freestanding_smoke
	PRIVATE
	"../../crates/canopy-abi/include"
	include
)
target_compile_features(canopy_cpp_freestanding_smoke PRIVATE cxx_std_23)
target_compile_options(canopy_cpp_freestanding_smoke PRIVATE -fno-exceptions -fno-rtti)

# Target: canopy_cpp_gui_example
add_executable(canopy_cpp_gui_example)
target_sources(
	canopy_cpp_gui_example
	PRIVATE
	"examples/gui_render/main.cpp"
	"src/build_context.cpp"
	"src/host.cpp"
	"src/reactive.cpp"
)
target_include_directories(canopy_cpp_gui_example PRIVATE "../../crates/canopy-abi/include" include)
target_compile_features(canopy_cpp_gui_example PRIVATE cxx_std_23)
target_link_options(canopy_cpp_gui_example PRIVATE "-L../../../target/debug" -lcanopy_abi)

# Target: canopy_cpp_inplace_test
add_executable(canopy_cpp_inplace_test)
target_sources(canopy_cpp_inplace_test PRIVATE "tests/inplace_function_test.cpp")
target_include_directories(
	canopy_cpp_inplace_test
	PRIVATE
	"../../crates/canopy-abi/include"
	include
)
target_compile_features(canopy_cpp_inplace_test PRIVATE cxx_std_23)

# Target: canopy_cpp_on_frt_test
add_executable(canopy_cpp_on_frt_test)
target_sources(
	canopy_cpp_on_frt_test
	PRIVATE
	"../../runtime/frt/src/backend_debug_tracking.cpp"
	"../../runtime/frt/src/backend_host_posix.cpp"
	"../../runtime/frt/src/new_delete.cpp"
	"../../runtime/frt/src/platform.cpp"
	"src/build_context.cpp"
	"src/reactive.cpp"
	"tests/on_frt_test.cpp"
)
target_include_directories(
	canopy_cpp_on_frt_test
	PRIVATE
	"../../crates/canopy-abi/include"
	"../../runtime/frt/include"
	include
)
target_compile_features(canopy_cpp_on_frt_test PRIVATE cxx_std_23)
target_compile_options(canopy_cpp_on_frt_test PRIVATE -DFRT_OWN_NEW_DELETE)

# Target: canopy_cpp_parity_test
add_executable(canopy_cpp_parity_test)
target_sources(
	canopy_cpp_parity_test
	PRIVATE
	"src/build_context.cpp"
	"src/reactive.cpp"
	"tests/parity_test.cpp"
)
target_include_directories(canopy_cpp_parity_test PRIVATE "../../crates/canopy-abi/include" include)
target_compile_features(canopy_cpp_parity_test PRIVATE cxx_std_23)
target_link_options(canopy_cpp_parity_test PRIVATE "-L../../../target/debug" -lcanopy_abi)

# Target: canopy_cpp_reactive_test
add_executable(canopy_cpp_reactive_test)
target_sources(
	canopy_cpp_reactive_test
	PRIVATE
	"src/build_context.cpp"
	"src/host.cpp"
	"src/reactive.cpp"
	"tests/reactive_test.cpp"
)
target_include_directories(
	canopy_cpp_reactive_test
	PRIVATE
	"../../crates/canopy-abi/include"
	include
)
target_compile_features(canopy_cpp_reactive_test PRIVATE cxx_std_23)
target_link_options(canopy_cpp_reactive_test PRIVATE "-L../../../target/debug" -lcanopy_abi)

# Target: canopy_cpp_roundtrip_test
add_executable(canopy_cpp_roundtrip_test)
target_sources(
	canopy_cpp_roundtrip_test
	PRIVATE
	"src/build_context.cpp"
	"src/reactive.cpp"
	"tests/roundtrip_test.cpp"
)
target_include_directories(
	canopy_cpp_roundtrip_test
	PRIVATE
	"../../crates/canopy-abi/include"
	include
)
target_compile_features(canopy_cpp_roundtrip_test PRIVATE cxx_std_23)
target_link_options(canopy_cpp_roundtrip_test PRIVATE "-L../../../target/debug" -lcanopy_abi)

# Target: canopy_cpp_test
add_executable(canopy_cpp_test)
target_sources(canopy_cpp_test PRIVATE "tests/encoder_test.cpp")
target_link_libraries(canopy_cpp_test PRIVATE canopy_cpp)
target_include_directories(canopy_cpp_test PRIVATE "../../crates/canopy-abi/include" include)
target_compile_features(canopy_cpp_test PRIVATE cxx_std_23)

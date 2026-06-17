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
target_sources(canopy_cpp PRIVATE "src/build_context.cpp")
target_include_directories(canopy_cpp PUBLIC "../../crates/canopy-abi/include" include)
target_compile_features(canopy_cpp PUBLIC cxx_std_23)

# Target: canopy_cpp_dsl_test
add_executable(canopy_cpp_dsl_test)
target_sources(canopy_cpp_dsl_test PRIVATE "tests/dsl_test.cpp")
target_link_libraries(canopy_cpp_dsl_test PRIVATE canopy_cpp)
target_include_directories(canopy_cpp_dsl_test PRIVATE "../../crates/canopy-abi/include" include)
target_compile_features(canopy_cpp_dsl_test PRIVATE cxx_std_23)

# Target: canopy_cpp_freestanding_smoke
add_executable(canopy_cpp_freestanding_smoke)
target_sources(
	canopy_cpp_freestanding_smoke
	PRIVATE
	"src/build_context.cpp"
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

# Target: canopy_cpp_test
add_executable(canopy_cpp_test)
target_sources(canopy_cpp_test PRIVATE "tests/encoder_test.cpp")
target_link_libraries(canopy_cpp_test PRIVATE canopy_cpp)
target_include_directories(canopy_cpp_test PRIVATE "../../crates/canopy-abi/include" include)
target_compile_features(canopy_cpp_test PRIVATE cxx_std_23)

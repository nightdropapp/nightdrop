# Wrapper toolchain for cross-compiling BoringSSL (via boring-sys) to Android.
# Forces BoringSSL's test/benchmark tree off — google/benchmark's regex-backend detection
# runs a target binary, which cannot work when cross-compiling; we only need libcrypto/libssl.
set(BUILD_TESTING OFF CACHE BOOL "" FORCE)
if(NOT DEFINED ANDROID_ABI)
  set(ANDROID_ABI "$ENV{ND_ANDROID_ABI}")
endif()
set(ANDROID_PLATFORM "android-24")
# Link libc++ STATICALLY into the one native lib. BoringSSL has C++; the NDK toolchain otherwise
# defaults to the shared STL, and libnightdrop.so would need libc++_shared.so bundled in the APK
# — which cargokit does not add, so the app fails dlopen at startup ("library libc++_shared.so
# not found"). c++_static makes the .so self-contained (it is the only C++ native lib here, so
# there is no multiple-libc++ hazard).
set(ANDROID_STL c++_static)
include("$ENV{ANDROID_NDK_ROOT}/build/cmake/android.toolchain.cmake")
# Normalise the build path out of the objects, for F-Droid's reproducible-build check.
#
# BoringSSL is C/C++, so the recipe's --remap-path-prefix (which is a rustc flag) does not reach
# it. Its error macros embed __FILE__, and those land in .rodata — ~190 absolute paths that
# SURVIVE stripping and ship inside libnightdrop.so, naming both the build directory and cargo's
# boring-sys metadata hash.
#
# F-Droid rebuilds at a fixed /build/nightdrop, so they would ordinarily match the APK we publish
# from the same path. But that makes reproducibility depend on the build location, which it never
# did while the native code was pure Rust, and the failure mode is a "not reproducible" verdict
# published in their repo under our name. Remapping removes the dependency instead of relying on
# it holding.
#
# Set AFTER the include: the NDK toolchain resets the *_FLAGS_INIT variables.
if(DEFINED ENV{OUT_DIR})
  foreach(_nd_lang C CXX ASM)
    set(CMAKE_${_nd_lang}_FLAGS
        "${CMAKE_${_nd_lang}_FLAGS} -ffile-prefix-map=$ENV{OUT_DIR}=/nd-boringssl")
  endforeach()
endif()

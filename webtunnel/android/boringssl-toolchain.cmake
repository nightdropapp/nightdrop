# Wrapper toolchain for cross-compiling BoringSSL (via boring-sys) to Android.
# Forces BoringSSL's test/benchmark tree off — google/benchmark's regex-backend detection
# runs a target binary, which cannot work when cross-compiling; we only need libcrypto/libssl.
set(BUILD_TESTING OFF CACHE BOOL "" FORCE)
if(NOT DEFINED ANDROID_ABI)
  set(ANDROID_ABI "$ENV{ND_ANDROID_ABI}")
endif()
set(ANDROID_PLATFORM "android-24")
include("$ENV{ANDROID_NDK_ROOT}/build/cmake/android.toolchain.cmake")

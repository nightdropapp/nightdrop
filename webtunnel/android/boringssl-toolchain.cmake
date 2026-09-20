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

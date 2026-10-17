#include <flutter/dart_project.h>
#include <flutter/flutter_view_controller.h>
#include <windows.h>
#include <werapi.h>

#include "flutter_window.h"
#include "utils.h"

int APIENTRY wWinMain(_In_ HINSTANCE instance, _In_opt_ HINSTANCE prev,
                      _In_ wchar_t *command_line, _In_ int show_command) {
  // A crash must not leave this process's memory with Windows Error Reporting: it holds the store
  // key and decrypted messages, and WER can keep or upload a dump of it (self-audit 2026-10-17; the
  // Linux runner sets a zero core limit for the same reason). FALSE = this user only, no admin
  // rights needed; the name is our own executable's.
  {
    wchar_t exe[MAX_PATH];
    DWORD n = ::GetModuleFileNameW(nullptr, exe, MAX_PATH);
    if (n > 0 && n < MAX_PATH) {
      const wchar_t* name = wcsrchr(exe, L'\\');
      ::WerAddExcludedApplication(name ? name + 1 : exe, FALSE);
    }
  }

  // Attach to console when present (e.g., 'flutter run') or create a
  // new console when running with a debugger.
  if (!::AttachConsole(ATTACH_PARENT_PROCESS) && ::IsDebuggerPresent()) {
    CreateAndAttachConsole();
  }

  // Initialize COM, so that it is available for use in the library and/or
  // plugins.
  ::CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);

  // One instance per user session. Two would run two Tor cores against the same identity and
  // state directory, which arti refuses to share ("State already locked") — and with the window
  // able to hide in the tray, "it isn't open, launch it again" is the natural thing to try. The
  // second launch hands over to the first, bringing it forward from the tray if need be, and
  // ends. Held until exit; Windows releases it with the process.
  HANDLE single_instance =
      ::CreateMutexW(nullptr, TRUE, L"app.nightdrop.single-instance");
  if (single_instance != nullptr && ::GetLastError() == ERROR_ALREADY_EXISTS) {
    HWND running = ::FindWindowW(L"FLUTTER_RUNNER_WIN32_WINDOW", L"Night Drop");
    if (running != nullptr) {
      // Let the running copy take the foreground: only the process the user just started may.
      ::AllowSetForegroundWindow(ASFW_ANY);
      ::PostMessageW(running, FlutterWindow::ShowMessage(), 0, 0);
    }
    ::CloseHandle(single_instance);
    return EXIT_SUCCESS;
  }

  flutter::DartProject project(L"data");

  std::vector<std::string> command_line_arguments =
      GetCommandLineArguments();
  bool start_hidden = false;
  for (const std::string& arg : command_line_arguments) {
    if (arg == "--hidden") {
      start_hidden = true;
    }
  }

  project.set_dart_entrypoint_arguments(std::move(command_line_arguments));

  FlutterWindow window(project, start_hidden);
  Win32Window::Point origin(10, 10);
  Win32Window::Size size(1280, 720);
  if (!window.Create(L"Night Drop", origin, size)) {
    return EXIT_FAILURE;
  }
  window.SetQuitOnClose(true);

  ::MSG msg;
  while (::GetMessage(&msg, nullptr, 0, 0)) {
    ::TranslateMessage(&msg);
    ::DispatchMessage(&msg);
  }

  ::CoUninitialize();
  return EXIT_SUCCESS;
}

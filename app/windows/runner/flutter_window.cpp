#include "flutter_window.h"

#include <flutter/standard_method_codec.h>

#include <optional>

#include "flutter/generated_plugin_registrant.h"
#include "resource.h"

namespace {

// Tray icon callback message, and the ids of its menu items.
constexpr UINT kTrayMessage = WM_APP + 1;
constexpr UINT kMenuOpen = 1;
constexpr UINT kMenuQuit = 2;

// Sent to every top-level window when Explorer (and with it the notification area) restarts; the
// icon has to be added again.
UINT TaskbarCreatedMessage() {
  static const UINT message = ::RegisterWindowMessageW(L"TaskbarCreated");
  return message;
}

}  // namespace

FlutterWindow::FlutterWindow(const flutter::DartProject& project,
                             bool start_hidden)
    : project_(project), start_hidden_(start_hidden) {}

UINT FlutterWindow::ShowMessage() {
  static const UINT message = ::RegisterWindowMessageW(L"app.nightdrop.show");
  return message;
}

FlutterWindow::~FlutterWindow() {}

bool FlutterWindow::OnCreate() {
  if (!Win32Window::OnCreate()) {
    return false;
  }

  RECT frame = GetClientArea();

  // The size here must match the window dimensions to avoid unnecessary surface
  // creation / destruction in the startup path.
  flutter_controller_ = std::make_unique<flutter::FlutterViewController>(
      frame.right - frame.left, frame.bottom - frame.top, project_);
  // Ensure that basic setup of the controller was successful.
  if (!flutter_controller_->engine() || !flutter_controller_->view()) {
    return false;
  }
  RegisterPlugins(flutter_controller_->engine());
  SetChildContent(flutter_controller_->view()->GetNativeWindow());

  window_channel_ =
      std::make_unique<flutter::MethodChannel<flutter::EncodableValue>>(
          flutter_controller_->engine()->messenger(), "app.nightdrop/window",
          &flutter::StandardMethodCodec::GetInstance());
  window_channel_->SetMethodCallHandler(
      [this](const flutter::MethodCall<flutter::EncodableValue>& call,
             std::unique_ptr<flutter::MethodResult<flutter::EncodableValue>>
                 result) {
        const std::string& method = call.method_name();
        if (method == "setCloseToTray") {
          const auto* on = std::get_if<bool>(call.arguments());
          close_to_tray_ = on != nullptr && *on;
          if (close_to_tray_) {
            AddTrayIcon();
          } else {
            RemoveTrayIcon();
          }
          result->Success();
        } else if (method == "show") {
          ShowFromTray();
          result->Success();
        } else if (method == "hide") {
          ::ShowWindow(GetHandle(), SW_HIDE);
          result->Success();
        } else if (method == "quit") {
          result->Success();
          ::DestroyWindow(GetHandle());
        } else {
          result->NotImplemented();
        }
      });

  flutter_controller_->engine()->SetNextFrameCallback([&]() {
    // Started at login into the tray: stay hidden. Dart shows the window itself if the tray
    // turns out not to be wanted (desktop_window.dart), so it can never be left with no way in.
    if (!start_hidden_) {
      this->Show();
    }
  });

  // Flutter can complete the first frame before the "show window" callback is
  // registered. The following call ensures a frame is pending to ensure the
  // window is shown. It is a no-op if the first frame hasn't completed yet.
  flutter_controller_->ForceRedraw();

  return true;
}

void FlutterWindow::OnDestroy() {
  RemoveTrayIcon();
  window_channel_ = nullptr;
  if (flutter_controller_) {
    flutter_controller_ = nullptr;
  }

  Win32Window::OnDestroy();
}

LRESULT
FlutterWindow::MessageHandler(HWND hwnd, UINT const message,
                              WPARAM const wparam,
                              LPARAM const lparam) noexcept {
  // Give Flutter, including plugins, an opportunity to handle window messages.
  if (flutter_controller_) {
    std::optional<LRESULT> result =
        flutter_controller_->HandleTopLevelWindowProc(hwnd, message, wparam,
                                                      lparam);
    if (result) {
      return *result;
    }
  }

  if (message == ShowMessage()) {
    ShowFromTray();
    return 0;
  }
  if (message == TaskbarCreatedMessage() && tray_added_) {
    tray_added_ = false;
    AddTrayIcon();
    return 0;
  }

  switch (message) {
    case WM_FONTCHANGE:
      flutter_controller_->engine()->ReloadSystemFonts();
      break;
    case WM_CLOSE:
      // Close-to-tray: the window goes, the app (and its Tor core) keeps running.
      if (close_to_tray_) {
        ::ShowWindow(hwnd, SW_HIDE);
        return 0;
      }
      break;
    case WM_ENDSESSION:
      // Logging off, or an installer's Restart Manager closing us for an update: really quit,
      // whatever close-to-tray says, or the update would wait on a window nobody can see.
      if (wparam) {
        ::DestroyWindow(hwnd);
        return 0;
      }
      break;
    case kTrayMessage:
      switch (LOWORD(lparam)) {
        case NIN_SELECT:
        case NIN_KEYSELECT:
        case WM_LBUTTONUP:
          ShowFromTray();
          return 0;
        case WM_CONTEXTMENU:
        case WM_RBUTTONUP:
          ShowTrayMenu();
          return 0;
      }
      return 0;
  }

  return Win32Window::MessageHandler(hwnd, message, wparam, lparam);
}

void FlutterWindow::ShowFromTray() {
  HWND hwnd = GetHandle();
  ::ShowWindow(hwnd, ::IsIconic(hwnd) ? SW_RESTORE : SW_SHOW);
  ::SetForegroundWindow(hwnd);
}

void FlutterWindow::AddTrayIcon() {
  if (tray_added_) {
    return;
  }
  tray_ = {};
  tray_.cbSize = sizeof(tray_);
  tray_.hWnd = GetHandle();
  tray_.uID = 1;
  tray_.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
  tray_.uCallbackMessage = kTrayMessage;
  tray_.hIcon = ::LoadIconW(::GetModuleHandleW(nullptr),
                            MAKEINTRESOURCEW(IDI_APP_ICON));
  wcscpy_s(tray_.szTip, L"Night Drop");
  if (::Shell_NotifyIconW(NIM_ADD, &tray_)) {
    tray_.uVersion = NOTIFYICON_VERSION_4;
    ::Shell_NotifyIconW(NIM_SETVERSION, &tray_);
    tray_added_ = true;
  }
}

void FlutterWindow::RemoveTrayIcon() {
  if (!tray_added_) {
    return;
  }
  ::Shell_NotifyIconW(NIM_DELETE, &tray_);
  tray_added_ = false;
}

void FlutterWindow::ShowTrayMenu() {
  HMENU menu = ::CreatePopupMenu();
  if (menu == nullptr) {
    return;
  }
  ::AppendMenuW(menu, MF_STRING, kMenuOpen, L"Open Night Drop");
  ::AppendMenuW(menu, MF_STRING, kMenuQuit, L"Quit Night Drop");
  POINT at;
  ::GetCursorPos(&at);
  // Required for the menu to close when the user clicks elsewhere (documented TrackPopupMenu quirk).
  ::SetForegroundWindow(GetHandle());
  const UINT chosen = ::TrackPopupMenu(
      menu, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON, at.x, at.y, 0,
      GetHandle(), nullptr);
  ::DestroyMenu(menu);
  if (chosen == kMenuOpen) {
    ShowFromTray();
  } else if (chosen == kMenuQuit) {
    ::DestroyWindow(GetHandle());
  }
}

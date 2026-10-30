#ifndef RUNNER_FLUTTER_WINDOW_H_
#define RUNNER_FLUTTER_WINDOW_H_

#include <flutter/dart_project.h>
#include <flutter/encodable_value.h>
#include <flutter/flutter_view_controller.h>
#include <flutter/method_channel.h>
#include <shellapi.h>

#include <memory>

#include "win32_window.h"

// A window that does nothing but host a Flutter view.
class FlutterWindow : public Win32Window {
 public:
  // Creates a new FlutterWindow hosting a Flutter view running |project|.
  // |start_hidden|: launched at login into the tray (`--hidden`), so the window is not shown
  // when Flutter draws its first frame.
  FlutterWindow(const flutter::DartProject& project, bool start_hidden);
  virtual ~FlutterWindow();

  // Posted by a second launch (see main.cpp) to bring this one forward, from the tray if need be.
  static UINT ShowMessage();

 protected:
  // Win32Window:
  bool OnCreate() override;
  void OnDestroy() override;
  LRESULT MessageHandler(HWND window, UINT const message, WPARAM const wparam,
                         LPARAM const lparam) noexcept override;

 private:
  // The project to run.
  flutter::DartProject project_;

  // The Flutter instance hosted by this window.
  std::unique_ptr<flutter::FlutterViewController> flutter_controller_;

  // `app.nightdrop/window`: close-to-tray on/off, show, hide and quit, driven from Dart
  // (lib/src/desktop/desktop_window.dart).
  std::unique_ptr<flutter::MethodChannel<flutter::EncodableValue>> window_channel_;

  // `app.nightdrop/screenshots`: "Block screenshots" (lib/src/core/screenshot_detector.dart).
  std::unique_ptr<flutter::MethodChannel<flutter::EncodableValue>> screenshots_channel_;

  // Leave the window out of every capture while [BlockCapture] or a chat holds the block.
  void ApplyCaptureBlock();
  static bool BlockCapture();
  static void SetBlockCapture(bool on);
  // A chat whose contact has "Block screenshots" on is open (Dart's `holdBlock`).
  bool held_by_chat_ = false;

  void ShowFromTray();
  void AddTrayIcon();
  void RemoveTrayIcon();
  void ShowTrayMenu();

  bool start_hidden_;
  // While set, closing the window hides it to the tray instead of ending the app.
  bool close_to_tray_ = false;
  bool tray_added_ = false;
  NOTIFYICONDATAW tray_{};
};

#endif  // RUNNER_FLUTTER_WINDOW_H_

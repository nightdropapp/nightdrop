import 'package:flutter/material.dart';

/// Night Drop's dark, low-key theme.
ThemeData ghostTheme() {
  final scheme = ColorScheme.fromSeed(
    seedColor: const Color(0xFF7C83FD),
    brightness: Brightness.dark,
  );
  return ThemeData(
    useMaterial3: true,
    colorScheme: scheme,
    scaffoldBackgroundColor: const Color(0xFF0E0F14),
    appBarTheme: const AppBarTheme(centerTitle: true),
    // Render color emoji everywhere (Flutter's desktop builds ship no emoji font, so emoji
    // otherwise appear as missing-glyph boxes).
    fontFamilyFallback: const ['NotoColorEmoji'],
  );
}

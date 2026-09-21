/// This is copied from Cargokit (which is the official way to use it currently)
/// Details: https://fzyzcjy.github.io/flutter_rust_bridge/manual/integrate/builtin

import 'dart:io';

import 'package:collection/collection.dart';
import 'package:logging/logging.dart';
import 'package:path/path.dart' as path;

import 'android_environment.dart';
import 'cargo.dart';
import 'environment.dart';
import 'options.dart';
import 'rustup.dart';
import 'target.dart';
import 'util.dart';

final _log = Logger('builder');

enum BuildConfiguration {
  debug,
  release,
  profile,
}

extension on BuildConfiguration {
  bool get isDebug => this == BuildConfiguration.debug;
  String get rustName => switch (this) {
        BuildConfiguration.debug => 'debug',
        BuildConfiguration.release => 'release',
        BuildConfiguration.profile => 'release',
      };
}

class BuildException implements Exception {
  final String message;

  BuildException(this.message);

  @override
  String toString() {
    return 'BuildException: $message';
  }
}

class BuildEnvironment {
  final BuildConfiguration configuration;
  final CargokitCrateOptions crateOptions;
  final String targetTempDir;
  final String manifestDir;
  final CrateInfo crateInfo;

  final bool isAndroid;
  final String? androidSdkPath;
  final String? androidNdkVersion;
  final int? androidMinSdkVersion;
  final String? javaHome;

  final String? glibcVersion;

  BuildEnvironment({
    required this.configuration,
    required this.crateOptions,
    required this.targetTempDir,
    required this.manifestDir,
    required this.crateInfo,
    required this.isAndroid,
    this.androidSdkPath,
    this.androidNdkVersion,
    this.androidMinSdkVersion,
    this.javaHome,
    this.glibcVersion,
  });

  static BuildConfiguration parseBuildConfiguration(String value) {
    // XCode configuration adds the flavor to configuration name.
    final firstSegment = value.split('-').first;
    final buildConfiguration = BuildConfiguration.values.firstWhereOrNull(
      (e) => e.name == firstSegment,
    );
    if (buildConfiguration == null) {
      _log.warning('Unknown build configuraiton $value, will assume release');
      return BuildConfiguration.release;
    }
    return buildConfiguration;
  }

  static BuildEnvironment fromEnvironment({
    required bool isAndroid,
  }) {
    final buildConfiguration =
        parseBuildConfiguration(Environment.configuration);
    final manifestDir = Environment.manifestDir;
    final crateOptions = CargokitCrateOptions.load(
      manifestDir: manifestDir,
    );
    final crateInfo = CrateInfo.load(manifestDir);
    return BuildEnvironment(
      configuration: buildConfiguration,
      crateOptions: crateOptions,
      targetTempDir: Environment.targetTempDir,
      manifestDir: manifestDir,
      crateInfo: crateInfo,
      isAndroid: isAndroid,
      androidSdkPath: isAndroid ? Environment.sdkPath : null,
      androidNdkVersion: isAndroid ? Environment.ndkVersion : null,
      androidMinSdkVersion:
          isAndroid ? int.parse(Environment.minSdkVersion) : null,
      javaHome: isAndroid ? Environment.javaHome : null,
    );
  }
}

class RustBuilder {
  final Target target;
  final BuildEnvironment environment;

  RustBuilder({
    required this.target,
    required this.environment,
  });

  void prepare(
    Rustup rustup,
  ) {
    final toolchain = _toolchain;
    if (rustup.installedTargets(toolchain) == null) {
      rustup.installToolchain(toolchain);
    }
    if (toolchain == 'nightly') {
      rustup.installRustSrcForNightly();
    }
    if (!rustup.installedTargets(toolchain)!.contains(target.rust)) {
      rustup.installTarget(target.rust, toolchain: toolchain);
    }
    if (environment.glibcVersion != null) {
      rustup.installZigBuild(toolchain);
    }
  }

  CargoBuildOptions? get _buildOptions =>
      environment.crateOptions.cargo[environment.configuration];

  String get _toolchain => _buildOptions?.toolchain.name ?? 'stable';

  /// Night Drop customization: build the core with the in-process WebTunnel transport
  /// (BoringSSL). Off unless `NIGHTDROP_WEBTUNNEL=1`, so default and F-Droid builds are
  /// untouched. See `webtunnel/android/README.md`.
  bool get _webtunnelEnabled =>
      Platform.environment['NIGHTDROP_WEBTUNNEL'] == '1';

  /// Returns the path of directory containing build artifacts.
  Future<String> build() async {
    final extraArgs = [...?_buildOptions?.flags];
    if (_webtunnelEnabled) {
      extraArgs.addAll(['--features', 'webtunnel']);
    }
    final manifestPath = path.join(environment.manifestDir, 'Cargo.toml');
    runCommand(
      'rustup',
      [
        'run',
        _toolchain,
        'cargo',
        (target.android == null && environment.glibcVersion != null)
            ? 'zigbuild'
            : 'build',
        ...extraArgs,
        '--manifest-path',
        manifestPath,
        '-p',
        environment.crateInfo.packageName,
        if (!environment.configuration.isDebug) '--release',
        '--target',
        target.rust +
            ((target.android == null && environment.glibcVersion != null)
                ? '.${environment.glibcVersion!}'
                : ""),
        '--target-dir',
        environment.targetTempDir,
      ],
      environment: await _buildEnvironment(),
    );
    return path.join(
      environment.targetTempDir,
      target.rust,
      environment.configuration.rustName,
    );
  }

  Future<Map<String, String>> _buildEnvironment() async {
    if (target.android == null) {
      return {};
    } else {
      final sdkPath = environment.androidSdkPath;
      final ndkVersion = environment.androidNdkVersion;
      final minSdkVersion = environment.androidMinSdkVersion;
      if (sdkPath == null) {
        throw BuildException('androidSdkPath is not set');
      }
      if (ndkVersion == null) {
        throw BuildException('androidNdkVersion is not set');
      }
      if (minSdkVersion == null) {
        throw BuildException('androidMinSdkVersion is not set');
      }
      final env = AndroidEnvironment(
        sdkPath: sdkPath,
        ndkVersion: ndkVersion,
        minSdkVersion: minSdkVersion,
        targetTempDir: environment.targetTempDir,
        target: target,
      );
      if (!env.ndkIsInstalled() && environment.javaHome != null) {
        env.installNdk(javaHome: environment.javaHome!);
      }
      final result = await env.buildEnvironment();
      // Night Drop: BoringSSL (chrome-proto) needs a CMake toolchain that disables BoringSSL's
      // test tree (google/benchmark can't cross-compile) and points at the NDK, plus per-ABI
      // ND_ANDROID_ABI. Only when opted in, so nothing changes for the default build.
      if (_webtunnelEnabled) {
        final ndkPath = path.join(sdkPath, 'ndk', ndkVersion);
        final repoRoot = path.normalize(path.join(environment.manifestDir, '..'));
        result['ANDROID_NDK_ROOT'] = ndkPath;
        result['ANDROID_NDK_HOME'] = ndkPath;
        result['CMAKE_TOOLCHAIN_FILE'] =
            path.join(repoRoot, 'webtunnel', 'android', 'boringssl-toolchain.cmake');
        result['ND_ANDROID_ABI'] = target.android!;
        // boring-sys otherwise links `-lc++`, the shared libc++_shared.so — not bundled in the
        // APK, so the app crashes at dlopen ("library libc++_shared.so not found"). Link the
        // static libc++ instead (so the one native lib is self-contained), plus libc++abi for
        // the C++ ABI/exception symbols (`__gxx_personality_v0`) that libc++_static.a leaves
        // undefined. (Paired with `ANDROID_STL c++_static` in the toolchain, which builds
        // BoringSSL's own objects against the same static STL.)
        result['BORING_BSSL_RUST_CPPLIB'] = 'c++_static';
        const rustFlagsKey = 'CARGO_ENCODED_RUSTFLAGS';
        const cxxAbi = '-Clink-arg=-lc++abi';
        final rf = result[rustFlagsKey];
        result[rustFlagsKey] =
            (rf == null || rf.isEmpty) ? cxxAbi : '$rf\u001f$cxxAbi';
      }
      return result;
    }
  }
}

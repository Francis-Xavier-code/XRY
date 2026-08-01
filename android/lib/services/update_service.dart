// 更新检查（update.json，与 Windows 端同源）：
// 多加速源轮询 → Ed25519 验签（内置公钥）→ 版本对比 → 提示下载 APK。

import 'dart:convert';
import 'package:cryptography/cryptography.dart';
import 'package:http/http.dart' as http;
import 'package:url_launcher/url_launcher.dart';
import '../app_config.dart';

class ApkUpdate {
  ApkUpdate({required this.version, required this.urls, required this.sha256});

  final String version;
  final List<String> urls;
  final String sha256;
}

class UpdateCheckResult {
  UpdateCheckResult({
    required this.currentVersion,
    required this.latestVersion,
    required this.hasUpdate,
    required this.notes,
    this.apk,
  });

  final String currentVersion;
  final String latestVersion;
  final bool hasUpdate;
  final String notes;
  final ApkUpdate? apk;
}

class UpdateService {
  /// 拉取 + 验签 update.json；失败抛异常。
  static Future<Map<String, dynamic>> fetchVerifiedUpdateJson({
    String? upstreamUrl,
    List<String>? mirrors,
  }) async {
    final base = upstreamUrl ?? AppConfig.defaultUpdateUrl;
    final mirrorList = mirrors ?? AppConfig.mirrors;
    final candidates = <String>[base, ...mirrorList.map((m) => '$m$base')];

    Object? lastError;
    for (final url in candidates) {
      try {
        final res = await http
            .get(Uri.parse(url))
            .timeout(const Duration(seconds: 15));
        if (res.statusCode != 200) {
          lastError = 'HTTP ${res.statusCode}';
          continue;
        }
        final data = jsonDecode(utf8.decode(res.bodyBytes)) as Map<String, dynamic>;
        final signature = data['signature'] as String? ?? '';
        if (signature.isEmpty) {
          throw Exception('update.json 缺少签名');
        }
        final payload = Map<String, dynamic>.from(data)..remove('signature');
        final canonical = utf8.encode(jsonEncode(payload));
        if (!await _verifySignature(canonical, signature)) {
          throw Exception('update.json 签名验证失败');
        }
        return data;
      } catch (e) {
        lastError = e;
      }
    }
    throw Exception('所有更新源不可用：$lastError');
  }

  static Future<bool> _verifySignature(List<int> message, String signatureB64) async {
    try {
      final keyBytes = base64Decode(AppConfig.builtinPublicKeyB64());
      final sigBytes = base64Decode(signatureB64);
      final ed25519 = Ed25519();
      final publicKey = SimplePublicKey(keyBytes, type: KeyPairType.ed25519);
      final signature = Signature(sigBytes, publicKey: publicKey);
      return await ed25519.verify(message, signature: signature);
    } catch (_) {
      return false;
    }
  }

  /// 检查更新：返回结果；网络失败抛异常。
  /// APK 段缺失时回退拉取同目录的 update-apk.json（由 Android CI 单独签名发布）。
  static Future<UpdateCheckResult> check({String? upstreamUrl}) async {
    final base = upstreamUrl ?? AppConfig.defaultUpdateUrl;
    final data = await fetchVerifiedUpdateJson(upstreamUrl: base);
    final latest = (data['version'] as String?) ?? '';
    final current = '0.6.0';
    final hasUpdate = _compareVersions(latest, current) > 0;
    ApkUpdate? apk;
    final apkRaw = data['apk'];
    if (apkRaw is Map<String, dynamic>) {
      apk = ApkUpdate(
        version: (apkRaw['version'] as String?) ?? '',
        urls: ((apkRaw['urls'] as List?) ?? const [])
            .whereType<String>()
            .toList(),
        sha256: (apkRaw['sha256'] as String?) ?? '',
      );
    }
    if (apk == null || apk.urls.isEmpty) {
      // 回退：同目录 update-apk.json
      try {
        final dir = base.substring(0, base.lastIndexOf('/') + 1);
        final fallback = await fetchVerifiedUpdateJson(upstreamUrl: '$dir' 'update-apk.json');
        final fallbackApk = fallback['apk'];
        if (fallbackApk is Map<String, dynamic>) {
          apk = ApkUpdate(
            version: (fallbackApk['version'] as String?) ?? '',
            urls: ((fallbackApk['urls'] as List?) ?? const [])
                .whereType<String>()
                .toList(),
            sha256: (fallbackApk['sha256'] as String?) ?? '',
          );
        }
      } catch (_) {
        // 无独立 apk 更新源，忽略
      }
    }
    return UpdateCheckResult(
      currentVersion: current,
      latestVersion: latest,
      hasUpdate: hasUpdate,
      notes: (data['notes'] as String?) ?? '',
      apk: apk,
    );
  }

  /// 打开下载页面（应用商店/浏览器）。
  static Future<void> openDownload(ApkUpdate apk) async {
    for (final url in apk.urls) {
      final uri = Uri.parse(url);
      if (await canLaunchUrl(uri)) {
        await launchUrl(uri, mode: LaunchMode.externalApplication);
        return;
      }
    }
  }

  static int _compareVersions(String a, String b) {
    List<int> parse(String v) {
      final cleaned = v.trim().replaceFirst(RegExp(r'^v'), '');
      final parts = cleaned.split(RegExp(r'[-+]')).first.split('.');
      return [
        parts.isNotEmpty ? int.tryParse(parts[0]) ?? 0 : 0,
        parts.length > 1 ? int.tryParse(parts[1]) ?? 0 : 0,
        parts.length > 2 ? int.tryParse(parts[2]) ?? 0 : 0,
      ];
    }

    final av = parse(a);
    final bv = parse(b);
    for (var i = 0; i < 3; i++) {
      if (av[i] != bv[i]) return av[i].compareTo(bv[i]);
    }
    return 0;
  }
}

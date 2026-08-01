// 本地存储：配对状态 / 中继配置 / 激活状态。

import 'dart:convert';
import 'package:shared_preferences/shared_preferences.dart';

class PairingStore {
  static const _kToken = 'pairing_token';
  static const _kDeviceId = 'pairing_device_id';
  static const _kDesktopId = 'pairing_desktop_id';
  static const _kRelayUrl = 'relay_url';
  static const _kDirectUrl = 'direct_url';
  static const _kLabel = 'device_label';

  static Future<PairingStore> load() async {
    final prefs = await SharedPreferences.getInstance();
    return PairingStore._(prefs);
  }

  PairingStore._(this._prefs);

  final SharedPreferences _prefs;

  String get token => _prefs.getString(_kToken) ?? '';
  String get deviceId => _prefs.getString(_kDeviceId) ?? '';
  String get desktopId => _prefs.getString(_kDesktopId) ?? '';
  String get relayUrl => _prefs.getString(_kRelayUrl) ?? '';
  String get directUrl => _prefs.getString(_kDirectUrl) ?? '';
  String get label => _prefs.getString(_kLabel) ?? '';

  bool get isPaired => token.isNotEmpty && desktopId.isNotEmpty;

  Future<void> saveSession({
    required String token,
    required String deviceId,
    required String desktopId,
    String? relayUrl,
    String? directUrl,
  }) async {
    await _prefs.setString(_kToken, token);
    await _prefs.setString(_kDeviceId, deviceId);
    await _prefs.setString(_kDesktopId, desktopId);
    if (relayUrl != null && relayUrl.isNotEmpty) {
      await _prefs.setString(_kRelayUrl, relayUrl);
    }
    if (directUrl != null && directUrl.isNotEmpty) {
      await _prefs.setString(_kDirectUrl, directUrl);
    }
  }

  Future<void> setRelayUrl(String url) => _prefs.setString(_kRelayUrl, url);

  Future<void> clear() async {
    await _prefs.remove(_kToken);
    await _prefs.remove(_kDeviceId);
    await _prefs.remove(_kDesktopId);
  }

  // ── 激活状态（预留付费能力） ──
  static const _kLicenseCode = 'license_code';
  static const _kLicensePlan = 'license_plan';
  static const _kLicenseExpires = 'license_expires';
  static const _kAdminConfirmed = 'admin_confirmed';

  Future<void> saveLicense({
    required String code,
    required String plan,
    required String expiresAt,
  }) async {
    await _prefs.setString(_kLicenseCode, code);
    await _prefs.setString(_kLicensePlan, plan);
    await _prefs.setString(_kLicenseExpires, expiresAt);
  }

  String get licenseCode => _prefs.getString(_kLicenseCode) ?? '';
  String get licensePlan => _prefs.getString(_kLicensePlan) ?? '';
  String get licenseExpires => _prefs.getString(_kLicenseExpires) ?? '';

  /// 是否已向 Windows 端确认过辅导员身份（管理员激活码验签通过后）。
  bool get adminConfirmed => _prefs.getBool(_kAdminConfirmed) ?? false;

  Future<void> setAdminConfirmed(bool value) =>
      _prefs.setBool(_kAdminConfirmed, value);
}

/// 简单的键值 JSON 工具（配对码解析等）。
Map<String, String> parseQuery(String query) {
  final result = <String, String>{};
  if (query.isEmpty) return result;
  for (final part in query.split('&')) {
    final pair = part.split('=');
    if (pair.length == 2) {
      result[Uri.decodeComponent(pair[0])] = Uri.decodeComponent(pair[1]);
    }
  }
  return result;
}

/// 解析扫码内容：hilia://pair?relay=wss://...&code=xxx&direct=ws://...&dcode=xxx
/// （direct/dcode 为局域网直连通道，APK 优先尝试；relay/code 为公网中继备选）
class PairQrPayload {
  PairQrPayload({
    required this.relay,
    required this.code,
    this.direct,
    this.dcode,
  });

  final String relay;
  final String code;
  final String? direct;
  final String? dcode;

  static PairQrPayload? tryParse(String raw) {
    try {
      final uri = Uri.parse(raw);
      if (uri.scheme != 'hilia' || uri.host != 'pair') return null;
      final query = parseQuery(uri.query);
      final relay = query['relay'] ?? '';
      final code = query['code'] ?? '';
      final direct = query['direct'];
      final dcode = query['dcode'];
      // 直连通道或中继通道至少一个可用
      final hasRelay = relay.isNotEmpty && code.isNotEmpty;
      final hasDirect = (direct != null && direct.isNotEmpty) &&
          (dcode != null && dcode.isNotEmpty);
      if (!hasRelay && !hasDirect) return null;
      return PairQrPayload(
        relay: relay,
        code: code,
        direct: hasDirect ? direct : null,
        dcode: hasDirect ? dcode : null,
      );
    } catch (_) {
      return null;
    }
  }

  String toJson() => jsonEncode({
        'relay': relay,
        'code': code,
        'direct': direct,
        'dcode': dcode,
      });
}

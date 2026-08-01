// 激活码验证（预留付费能力）：与 Windows 端同规格
// HILIA1.<payload_b64>.<sig_b64>，Ed25519 验签 + 有效期检查。

import 'dart:convert';
import 'package:cryptography/cryptography.dart';
import '../app_config.dart';

class LicenseStatus {
  LicenseStatus({
    required this.activated,
    required this.plan,
    required this.user,
    required this.expiresAt,
  });

  final bool activated;
  final String plan;
  final String user;
  final String expiresAt;

  bool get expired {
    if (!activated || expiresAt.isEmpty) return false;
    final parsed = DateTime.tryParse(expiresAt);
    return parsed != null && parsed.isBefore(DateTime.now().toUtc());
  }

  String get summary {
    if (!activated) return '未激活（免费版）';
    final planText = plan.isEmpty ? '基础' : plan;
    if (expiresAt.isEmpty) return '已激活：$planText（永久）';
    return '已激活：$planText（至 $expiresAt）';
  }
}

class LicenseService {
  /// 解析并验证激活码，返回 payload；失败抛异常。
  static Future<Map<String, dynamic>> verify(String code) async {
    final trimmed = code.trim();
    final parts = trimmed.split('.');
    if (parts.length != 3 || parts[0] != 'HILIA1') {
      throw Exception('激活码格式错误（应为 HILIA1.<数据>.<签名>）');
    }
    final payloadBytes = base64Decode(parts[1]);
    final signatureB64 = parts[2];

    // 验签
    final keyBytes = base64Decode(AppConfig.builtinPublicKeyB64());
    final sigBytes = base64Decode(signatureB64);
    final ed25519 = Ed25519();
    final publicKey = SimplePublicKey(keyBytes, type: KeyPairType.ed25519);
    final signature = Signature(sigBytes, publicKey: publicKey);
    final ok = await ed25519.verify(payloadBytes, signature: signature);
    if (!ok) {
      throw Exception('激活码签名验证失败');
    }

    final payload = jsonDecode(utf8.decode(payloadBytes)) as Map<String, dynamic>;
    final expiresAt = (payload['expires_at'] as num?)?.toInt() ?? 0;
    if (expiresAt > 0) {
      final expires = DateTime.fromMillisecondsSinceEpoch(expiresAt * 1000, isUtc: true);
      if (expires.isBefore(DateTime.now().toUtc())) {
        throw Exception('激活码已过期（${expires.toIso8601String()}）');
      }
    }
    if ((payload['plan'] as String? ?? '').isEmpty ||
        (payload['user'] as String? ?? '').isEmpty) {
      throw Exception('激活码缺少 plan 或 user 字段');
    }
    return payload;
  }

  /// 从存储读取状态。
  static LicenseStatus statusFromStore({
    required String code,
    required String plan,
    required String expires,
  }) {
    if (code.isEmpty) {
      return LicenseStatus(activated: false, plan: '', user: '', expiresAt: '');
    }
    return LicenseStatus(
      activated: true,
      plan: plan,
      user: '',
      expiresAt: expires,
    );
  }
}

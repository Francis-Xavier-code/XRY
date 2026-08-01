// 希尔娅 App 全局配置与安全常量。
//
// 防逆向说明：内置公钥/默认中继地址经简单 XOR 运行时混淆（防 strings 直读）；
// 关键信任（update.json 签名、激活码）用 Ed25519 公钥验证，私钥不在客户端。

class AppConfig {
  // 更新源（与 Windows 端 update.json 同源；可被设置页覆盖）
  static const String defaultUpdateUrl =
      'https://raw.githubusercontent.com/Francis-Xavier-code/XRY/main/update.json';

  // GitHub 加速前缀（轮询）
  static const List<String> mirrors = [
    'https://ghproxy.net/',
    'https://gh-proxy.com/',
    'https://ghfast.top/',
  ];

  // 内置 Ed25519 公钥（base64，XOR 混淆；与 Windows 端 src/security.rs 同密钥对）
  static String builtinPublicKeyB64() => _xor(
        '\u0018*-&\u0016i\u0004 \u000d\u0010\u005c\u0018\u000f\u000e_\u0007])\u000f\u001d#\u000a\u0008\u001d?=\u0011\u00053\u000d\u0019\u0011C\u000e5\u001b\u00043-Y\u0002\u001b\u0008X',
        'hilia-key',
      );

  // 默认中继地址（空 = 未配置，扫码时自动带上中继地址）
  static String defaultRelayUrl() => '';

  // 应用名
  static const String appName = '希尔娅';
  static const String developer = '2101497063@qq.com';

  // 简单 XOR 混淆（仅防静态字符串提取，非加密）
  static String _xor(String input, String key) {
    final bytes = input.codeUnits;
    final keyBytes = key.codeUnits;
    final result = StringBuffer();
    for (var i = 0; i < bytes.length; i++) {
      result.writeCharCode(bytes[i] ^ keyBytes[i % keyBytes.length]);
    }
    return result.toString();
  }
}

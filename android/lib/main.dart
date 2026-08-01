// 希尔娅 App 入口。
// 未配对 → 扫码配对页；已配对 → 主页（对话 + 学分 + 设置）。

import 'package:flutter/material.dart';
import 'pages/home_page.dart';
import 'pages/scan_page.dart';
import 'services/pairing_store.dart';
import 'app_config.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  final store = await PairingStore.load();
  runApp(HiliaApp(store: store));
}

class HiliaApp extends StatelessWidget {
  const HiliaApp({super.key, required this.store});

  final PairingStore store;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: AppConfig.appName,
      debugShowCheckedModeBanner: false,
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(
          seedColor: const Color(0xFF6AA8FE),
          brightness: Brightness.dark,
        ),
        useMaterial3: true,
      ),
      home: store.isPaired
          ? HomePage(store: store)
          : ScanPage(store: store),
    );
  }
}

// 扫码配对页：扫描 Windows 面板生成的二维码 → 经中继配对。

import 'package:flutter/material.dart';
import 'package:mobile_scanner/mobile_scanner.dart';
import '../app_config.dart';
import '../services/pairing_store.dart';
import '../services/relay_client.dart';
import 'home_page.dart';

class ScanPage extends StatefulWidget {
  const ScanPage({super.key, required this.store});

  final PairingStore store;

  @override
  State<ScanPage> createState() => _ScanPageState();
}

class _ScanPageState extends State<ScanPage> {
  RelayClient? _relay;
  String _statusText = '请扫描 Windows 面板上的配对二维码';
  bool _pairing = false;

  @override
  void dispose() {
    _relay?.dispose();
    super.dispose();
  }

  Future<void> _onDetect(BarcodeCapture capture) async {
    if (_pairing) return;
    final raw = capture.barcodes
        .map((b) => b.rawValue ?? '')
        .firstWhere((v) => v.isNotEmpty, orElse: () => '');
    if (raw.isEmpty) return;
    final payload = PairQrPayload.tryParse(raw);
    if (payload == null) {
      setState(() => _statusText = '不是希尔娅配对二维码');
      return;
    }
    _pairing = true;
    setState(() => _statusText = '正在连接中继并等待辅导员确认…');

    final relay = RelayClient();
    _relay = relay;
    relay.addListener(() {
      if (!mounted) return;
      switch (relay.status) {
        case RelayStatus.pairPending:
          setState(() => _statusText = '已连接，等待辅导员在 Windows 面板确认…');
          break;
        case RelayStatus.paired:
          _onPaired(relay);
          break;
        case RelayStatus.rejected:
          setState(() {
            _pairing = false;
            _statusText = '配对被拒绝，请重新扫码';
          });
          break;
        case RelayStatus.error:
          setState(() {
            _pairing = false;
            _statusText = '错误：${relay.error}';
          });
          break;
        default:
          break;
      }
    });
    await relay.pairWithCode(payload.relay, payload.code);
  }

  Future<void> _onPaired(RelayClient relay) async {
    await widget.store.saveSession(
      token: relay.pendingToken ?? '',
      deviceId: relay.pendingDeviceId ?? '',
      desktopId: relay.desktopId,
      relayUrl: '',
    );
    if (!mounted) return;
    Navigator.of(context).pushReplacement(
      MaterialPageRoute(
        builder: (_) => HomePage(store: widget.store, relay: relay),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('扫码配对')),
      body: Column(
        children: [
          const SizedBox(height: 12),
          const Text(AppConfig.appName,
              style: TextStyle(fontSize: 22, fontWeight: FontWeight.bold)),
          const Text('扫描 Windows 面板上的配对二维码',
              style: TextStyle(fontSize: 13, color: Colors.grey)),
          const SizedBox(height: 16),
          Expanded(
            child: Padding(
              padding: const EdgeInsets.symmetric(horizontal: 32),
              child: ClipRRect(
                borderRadius: BorderRadius.circular(16),
                child: MobileScanner(
                  onDetect: _onDetect,
                  errorBuilder: (context, error, child) => Center(
                    child: Text('相机不可用：$error',
                        style: const TextStyle(color: Colors.redAccent)),
                  ),
                ),
              ),
            ),
          ),
          Padding(
            padding: const EdgeInsets.all(20),
            child: Text(_statusText,
                textAlign: TextAlign.center,
                style: const TextStyle(fontSize: 14)),
          ),
          const Padding(
            padding: EdgeInsets.only(bottom: 16),
            child: Text('开发者：2101497063@qq.com',
                style: TextStyle(fontSize: 11, color: Colors.grey)),
          ),
        ],
      ),
    );
  }
}

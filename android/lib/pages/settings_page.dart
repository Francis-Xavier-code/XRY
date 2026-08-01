// 设置页：配对状态 / 中继地址 / 激活（预留）/ 更新检查 / 关于。

import 'package:flutter/material.dart';
import '../app_config.dart';
import '../services/license_service.dart';
import '../services/pairing_store.dart';
import '../services/relay_client.dart';
import '../services/update_service.dart';
import 'scan_page.dart';

class SettingsPage extends StatefulWidget {
  const SettingsPage({super.key, required this.store, required this.relay});

  final PairingStore store;
  final RelayClient relay;

  @override
  State<SettingsPage> createState() => _SettingsPageState();
}

class _SettingsPageState extends State<SettingsPage> {
  String _updateText = '';
  bool _checking = false;

  Future<void> _checkUpdate() async {
    setState(() {
      _checking = true;
      _updateText = '检查中…';
    });
    try {
      final result = await UpdateService.check();
      setState(() {
        _checking = false;
        _updateText = result.hasUpdate
            ? '发现新版本 v${result.latestVersion}（当前 v${result.currentVersion}）'
            : '已是最新版本（v${result.currentVersion}）';
      });
      if (result.hasUpdate && result.apk != null) {
        final download = await showDialog<bool>(
          context: context,
          builder: (context) => AlertDialog(
            title: Text('发现新版本 v${result.latestVersion}'),
            content: Text(result.notes.isNotEmpty
                ? result.notes
                : '是否前往下载新版本？'),
            actions: [
              TextButton(
                onPressed: () => Navigator.pop(context, false),
                child: const Text('稍后'),
              ),
              FilledButton(
                onPressed: () => Navigator.pop(context, true),
                child: const Text('去下载'),
              ),
            ],
          ),
        );
        if (download == true) {
          await UpdateService.openDownload(result.apk!);
        }
      }
    } catch (e) {
      setState(() {
        _checking = false;
        _updateText = '检查失败：$e';
      });
    }
  }

  Future<void> _activate() async {
    final controller = TextEditingController();
    final code = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('输入激活码'),
        content: TextField(
          controller: controller,
          decoration: const InputDecoration(hintText: 'HILIA1.xxx.yyy'),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context),
            child: const Text('取消'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, controller.text.trim()),
            child: const Text('激活'),
          ),
        ],
      ),
    );
    if (code == null || code.isEmpty) return;
    try {
      final payload = await LicenseService.verify(code);
      final expiresAt = (payload['expires_at'] as num?)?.toInt() ?? 0;
      final expires = expiresAt > 0
          ? DateTime.fromMillisecondsSinceEpoch(expiresAt * 1000, isUtc: true)
              .toIso8601String()
          : '';
      await widget.store.saveLicense(
        code: code,
        plan: payload['plan'] as String? ?? '',
        expiresAt: expires,
      );
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('激活成功：${payload['plan']}')),
      );
      setState(() {});
    } catch (e) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('激活失败：$e')),
      );
    }
  }

  Future<void> _unpair() async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('解除配对？'),
        content: const Text('解除后需要重新扫码配对才能继续使用。'),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('取消'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('解除'),
          ),
        ],
      ),
    );
    if (confirmed != true) return;
    await widget.relay.disconnect();
    await widget.store.clear();
    if (!mounted) return;
    Navigator.of(context).pushAndRemoveUntil(
      MaterialPageRoute(builder: (_) => ScanPage(store: widget.store)),
      (route) => false,
    );
  }

  @override
  Widget build(BuildContext context) {
    final store = widget.store;
    final license = LicenseService.statusFromStore(
      code: store.licenseCode,
      plan: store.licensePlan,
      expires: store.licenseExpires,
    );
    final statusText = switch (widget.relay.status) {
      RelayStatus.paired => '已连接',
      RelayStatus.connecting => '连接中…',
      RelayStatus.pairPending => '等待确认',
      RelayStatus.rejected => '配对被拒绝',
      RelayStatus.error => '错误：${widget.relay.error}',
      RelayStatus.disconnected => '未连接',
    };

    return Scaffold(
      appBar: AppBar(title: const Text('设置')),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: [
          ListTile(
            leading: const Icon(Icons.link),
            title: const Text('配对状态'),
            subtitle: Text('设备：${store.deviceId.isEmpty ? '未配对' : store.deviceId}\n中继：$statusText'),
            trailing: TextButton(onPressed: _unpair, child: const Text('解除配对')),
          ),
          const Divider(),
          ListTile(
            leading: const Icon(Icons.card_membership),
            title: const Text('激活状态'),
            subtitle: Text(license.summary),
            trailing: TextButton(onPressed: _activate, child: const Text('激活')),
          ),
          const Divider(),
          ListTile(
            leading: const Icon(Icons.update),
            title: const Text('版本更新'),
            subtitle: Text(_updateText.isEmpty ? 'v0.6.0' : _updateText),
            trailing: TextButton(
              onPressed: _checking ? null : _checkUpdate,
              child: Text(_checking ? '检查中…' : '检查更新'),
            ),
          ),
          const Divider(),
          const ListTile(
            leading: Icon(Icons.info_outline),
            title: Text('关于'),
            subtitle: Text('${AppConfig.appName} v0.6.0\n开发者：${AppConfig.developer}'),
          ),
        ],
      ),
    );
  }
}

// 主页：底部导航（对话 / 学分 / 设置）。

import 'package:flutter/material.dart';
import '../services/pairing_store.dart';
import '../services/relay_client.dart';
import 'chat_page.dart';
import 'credits_page.dart';
import 'settings_page.dart';

class HomePage extends StatefulWidget {
  const HomePage({super.key, required this.store, this.relay});

  final PairingStore store;
  final RelayClient? relay;

  @override
  State<HomePage> createState() => _HomePageState();
}

class _HomePageState extends State<HomePage> {
  late final RelayClient _relay;
  int _index = 0;

  @override
  void initState() {
    super.initState();
    _relay = widget.relay ?? RelayClient();
    if (widget.relay == null && widget.store.token.isNotEmpty) {
      // 重连已配对会话
      _relay.connectWithToken(widget.store.relayUrl, widget.store.token);
    }
  }

  @override
  void dispose() {
    if (widget.relay == null) {
      _relay.dispose();
    }
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final pages = [
      ChatPage(relay: _relay),
      CreditsPage(relay: _relay),
      SettingsPage(store: widget.store, relay: _relay),
    ];
    return Scaffold(
      body: IndexedStack(index: _index, children: pages),
      bottomNavigationBar: NavigationBar(
        selectedIndex: _index,
        onDestinationSelected: (value) => setState(() => _index = value),
        destinations: const [
          NavigationDestination(
            icon: Icon(Icons.chat_bubble_outline),
            selectedIcon: Icon(Icons.chat_bubble),
            label: '对话',
          ),
          NavigationDestination(
            icon: Icon(Icons.grade_outlined),
            selectedIcon: Icon(Icons.grade),
            label: '学分',
          ),
          NavigationDestination(
            icon: Icon(Icons.settings_outlined),
            selectedIcon: Icon(Icons.settings),
            label: '设置',
          ),
        ],
      ),
    );
  }
}

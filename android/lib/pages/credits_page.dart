// 学分页：一键查学分 / 汇总（消息经中继由 Windows 端学分工具处理）。

import 'package:flutter/material.dart';
import '../services/relay_client.dart';

class CreditsPage extends StatefulWidget {
  const CreditsPage({super.key, required this.relay});

  final RelayClient relay;

  @override
  State<CreditsPage> createState() => _CreditsPageState();
}

class _CreditsPageState extends State<CreditsPage> {
  final List<_CreditCard> _cards = [];
  bool _loading = false;
  int _lastInboxLength = 0;

  @override
  void initState() {
    super.initState();
    widget.relay.addListener(_onRelayChanged);
  }

  @override
  void dispose() {
    widget.relay.removeListener(_onRelayChanged);
    super.dispose();
  }

  void _onRelayChanged() {
    if (!mounted) return;
    final inbox = widget.relay.inbox;
    if (inbox.length == _lastInboxLength) return;
    _lastInboxLength = inbox.length;
    // 学分页的回复通过「查学分」快捷操作触发，直接显示最新回复
    final latest = inbox.isNotEmpty ? inbox.last : null;
    if (latest != null) {
      final reply = latest.body['reply'] as String? ?? '';
      if (reply.isNotEmpty) {
        setState(() {
          _loading = false;
          _cards.insert(
            0,
            _CreditCard(
              title: '学分信息',
              content: reply,
              time: DateTime.now(),
            ),
          );
        });
      }
    }
  }

  Future<void> _ask(String question) async {
    if (_loading) return;
    setState(() => _loading = true);
    widget.relay.sendText(question);
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('我的学分')),
      body: Column(
        children: [
          Padding(
            padding: const EdgeInsets.all(12),
            child: Row(
              children: [
                Expanded(
                  child: FilledButton.icon(
                    onPressed: _loading ? null : () => _ask('查学分'),
                    icon: const Icon(Icons.grade),
                    label: Text(_loading ? '查询中…' : '查学分'),
                  ),
                ),
                const SizedBox(width: 8),
                Expanded(
                  child: OutlinedButton.icon(
                    onPressed: _loading ? null : () => _ask('我的学分明细'),
                    icon: const Icon(Icons.receipt_long),
                    label: const Text('学分明细'),
                  ),
                ),
              ],
            ),
          ),
          Expanded(
            child: _cards.isEmpty
                ? const Center(
                    child: Text('点击「查学分」查看你的学分汇总',
                        style: TextStyle(color: Colors.grey)),
                  )
                : ListView.builder(
                    padding: const EdgeInsets.all(12),
                    itemCount: _cards.length,
                    itemBuilder: (context, index) {
                      final card = _cards[index];
                      return Card(
                        margin: const EdgeInsets.only(bottom: 10),
                        child: Padding(
                          padding: const EdgeInsets.all(14),
                          child: Column(
                            crossAxisAlignment: CrossAxisAlignment.start,
                            children: [
                              Row(
                                children: [
                                  const Icon(Icons.grade,
                                      size: 18, color: Colors.amber),
                                  const SizedBox(width: 6),
                                  Text(card.title,
                                      style: const TextStyle(
                                          fontWeight: FontWeight.bold)),
                                  const Spacer(),
                                  Text(
                                    '${card.time.hour.toString().padLeft(2, '0')}:${card.time.minute.toString().padLeft(2, '0')}',
                                    style: const TextStyle(
                                        fontSize: 12, color: Colors.grey),
                                  ),
                                ],
                              ),
                              const SizedBox(height: 8),
                              SelectableText(card.content,
                                  style: const TextStyle(
                                      fontSize: 14, height: 1.5)),
                            ],
                          ),
                        ),
                      );
                    },
                  ),
          ),
        ],
      ),
    );
  }
}

class _CreditCard {
  _CreditCard({
    required this.title,
    required this.content,
    required this.time,
  });

  final String title;
  final String content;
  final DateTime time;
}

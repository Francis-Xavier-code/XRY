// 对话页：与 Windows 端希尔娅对话（消息经中继转发）。
// 首次使用提示绑定学号：发「绑定 学号 姓名」给希尔娅。

import 'package:flutter/material.dart';
import '../services/relay_client.dart';

class ChatPage extends StatefulWidget {
  const ChatPage({super.key, required this.relay});

  final RelayClient relay;

  @override
  State<ChatPage> createState() => _ChatPageState();
}

class _ChatPageState extends State<ChatPage> {
  final TextEditingController _input = TextEditingController();
  final ScrollController _scroll = ScrollController();
  final List<_ChatItem> _items = [];
  bool _sending = false;

  @override
  void initState() {
    super.initState();
    widget.relay.addListener(_onRelayChanged);
    _items.add(_ChatItem(
      text: '你好，我是希尔娅。首次使用请先绑定学号：发送「绑定 学号 姓名」，例如「绑定 2023010101 张三」。之后直接发「查学分」就能看到自己的记录。',
      fromMe: false,
    ));
  }

  @override
  void dispose() {
    widget.relay.removeListener(_onRelayChanged);
    _input.dispose();
    _scroll.dispose();
    super.dispose();
  }

  void _onRelayChanged() {
    if (!mounted) return;
    final newMessages = widget.relay.inbox
        .where((m) => !_items.any((item) => item.msgId == m.msgId))
        .toList();
    if (newMessages.isEmpty) return;
    setState(() {
      for (final message in newMessages) {
        final reply = message.body['reply'] as String? ?? '';
        if (reply.isNotEmpty) {
          _items.add(_ChatItem(text: reply, fromMe: false, msgId: message.msgId));
        }
      }
    });
    _scrollToBottom();
  }

  void _send() {
    final text = _input.text.trim();
    if (text.isEmpty || _sending) return;
    setState(() {
      _items.add(_ChatItem(text: text, fromMe: true));
      _sending = true;
    });
    _input.clear();
    widget.relay.sendText(text);
    // 无回复兜底：重置发送状态由收到消息时处理
    Future.delayed(const Duration(seconds: 1), () {
      if (mounted) setState(() => _sending = false);
    });
    _scrollToBottom();
  }

  void _scrollToBottom() {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (_scroll.hasClients) {
        _scroll.animateTo(
          _scroll.position.maxScrollExtent,
          duration: const Duration(milliseconds: 200),
          curve: Curves.easeOut,
        );
      }
    });
  }

  void _quickAsk(String text) {
    _input.text = text;
    _send();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('希尔娅')),
      body: Column(
        children: [
          // 快捷操作
          SizedBox(
            height: 44,
            child: ListView(
              scrollDirection: Axis.horizontal,
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
              children: [
                _chip('查学分', () => _quickAsk('查学分')),
                _chip('学分明细', () => _quickAsk('我的学分明细')),
                _chip('我的信息', () => _quickAsk('我的信息')),
                _chip('绑定学号', () => _quickAsk('绑定 ')),
              ],
            ),
          ),
          Expanded(
            child: ListView.builder(
              controller: _scroll,
              padding: const EdgeInsets.all(12),
              itemCount: _items.length,
              itemBuilder: (context, index) {
                final item = _items[index];
                return _bubble(item);
              },
            ),
          ),
          SafeArea(
            child: Padding(
              padding: const EdgeInsets.fromLTRB(12, 4, 12, 12),
              child: Row(
                children: [
                  Expanded(
                    child: TextField(
                      controller: _input,
                      minLines: 1,
                      maxLines: 4,
                      decoration: InputDecoration(
                        hintText: '给希尔娅发消息…',
                        filled: true,
                        border: OutlineInputBorder(
                          borderRadius: BorderRadius.circular(20),
                          borderSide: BorderSide.none,
                        ),
                        contentPadding: const EdgeInsets.symmetric(
                            horizontal: 16, vertical: 10),
                      ),
                      onSubmitted: (_) => _send(),
                    ),
                  ),
                  const SizedBox(width: 8),
                  IconButton.filled(
                    onPressed: _send,
                    icon: const Icon(Icons.send),
                    tooltip: '发送',
                  ),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }

  Widget _chip(String label, VoidCallback onTap) {
    return Padding(
      padding: const EdgeInsets.only(right: 8),
      child: ActionChip(label: Text(label), onPressed: onTap),
    );
  }

  Widget _bubble(_ChatItem item) {
    final alignment = item.fromMe ? Alignment.centerRight : Alignment.centerLeft;
    final color = item.fromMe
        ? Theme.of(context).colorScheme.primary
        : Theme.of(context).colorScheme.surfaceContainerHighest;
    final textColor = item.fromMe
        ? Theme.of(context).colorScheme.onPrimary
        : Theme.of(context).colorScheme.onSurface;
    return Align(
      alignment: alignment,
      child: Container(
        margin: const EdgeInsets.symmetric(vertical: 4),
        padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
        constraints: BoxConstraints(
          maxWidth: MediaQuery.of(context).size.width * 0.78,
        ),
        decoration: BoxDecoration(
          color: color,
          borderRadius: BorderRadius.circular(14),
        ),
        child: SelectableText(
          item.text,
          style: TextStyle(color: textColor, fontSize: 15, height: 1.4),
        ),
      ),
    );
  }
}

class _ChatItem {
  _ChatItem({required this.text, required this.fromMe, this.msgId});

  final String text;
  final bool fromMe;
  final String? msgId;
}

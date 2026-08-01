// 学分申报页：班级职位人员（班长/学委等）填写问卷申报学分 + 证据照片。
// 提交后由辅导员在 Windows 面板审批，通过后计入正式学分。

import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:image_picker/image_picker.dart';
import '../services/pairing_store.dart';
import '../services/relay_client.dart';

class ApplyPage extends StatefulWidget {
  const ApplyPage({super.key, required this.relay, required this.store});

  final RelayClient relay;
  final PairingStore store;

  @override
  State<ApplyPage> createState() => _ApplyPageState();
}

class _ApplyPageState extends State<ApplyPage> {
  final _formKey = GlobalKey<FormState>();
  final _pointsController = TextEditingController();
  final _descriptionController = TextEditingController();
  final List<Map<String, String>> _photos = [];
  List<Map<String, dynamic>> _types = [];
  String? _selectedTypeId;
  bool _loadingTypes = true;
  bool _submitting = false;
  String _resultText = '';
  int _lastInboxLength = 0;

  @override
  void initState() {
    super.initState();
    widget.relay.addListener(_onRelayChanged);
    _loadTypes();
  }

  @override
  void dispose() {
    widget.relay.removeListener(_onRelayChanged);
    _pointsController.dispose();
    _descriptionController.dispose();
    super.dispose();
  }

  void _onRelayChanged() {
    if (!mounted) return;
    final inbox = widget.relay.inbox;
    if (inbox.length == _lastInboxLength) return;
    _lastInboxLength = inbox.length;
    final latest = inbox.isNotEmpty ? inbox.last : null;
    if (latest != null) {
      final reply = latest.body['reply'] as String? ?? '';
      if (reply.isNotEmpty) {
        setState(() {
          _submitting = false;
          _resultText = reply;
        });
      }
    }
  }

  Future<void> _loadTypes() async {
    setState(() => _loadingTypes = true);
    widget.relay.sendAction('credit_types', {});
    // 回复会通过 _onRelayChanged 到达；解析 JSON 更新类型列表
    await Future.delayed(const Duration(milliseconds: 1200));
    // 尝试从收件箱最新结构化回复解析学分类型
    final inbox = widget.relay.inbox;
    for (final message in inbox.reversed) {
      final reply = message.body['reply'] as String? ?? '';
      final decoded = _tryDecode(reply);
      if (decoded != null && decoded['ok'] == true && decoded['types'] != null) {
        final types = (decoded['types'] as List)
            .map((item) => Map<String, dynamic>.from(item as Map))
            .toList();
        setState(() {
          _types = types;
          _loadingTypes = false;
          if (_selectedTypeId == null && types.isNotEmpty) {
            _selectedTypeId = types.first['id'].toString();
          }
        });
        return;
      }
    }
    setState(() => _loadingTypes = false);
  }

  Map<String, dynamic>? _tryDecode(String text) {
    try {
      final decoded = jsonDecode(text);
      if (decoded is Map<String, dynamic>) return decoded;
    } catch (_) {}
    return null;
  }

  Future<void> _pickPhoto() async {
    if (_photos.length >= 3) {
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(content: Text('最多 3 张证据照片')),
      );
      return;
    }
    final picker = ImagePicker();
    final picked = await picker.pickImage(
      source: ImageSource.gallery,
      maxWidth: 1280,
      imageQuality: 80,
    );
    if (picked == null) return;
    final bytes = await picked.readAsBytes();
    final name = picked.name.split('/').last.split('\\').last;
    setState(() {
      _photos.add({
        'name': name,
        'data': base64Encode(bytes),
      });
    });
  }

  Future<void> _submit() async {
    if (!_formKey.currentState!.validate()) return;
    if (_selectedTypeId == null) {
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(content: Text('请选择学分类型')),
      );
      return;
    }
    setState(() {
      _submitting = true;
      _resultText = '';
    });
    widget.relay.sendAction('credit_apply', {
      'type_id': int.tryParse(_selectedTypeId!),
      'points': double.tryParse(_pointsController.text) ?? 0,
      'description': _descriptionController.text.trim(),
      'evidence': _photos,
    });
  }

  @override
  Widget build(BuildContext context) {
    final isAdmin = widget.store.adminConfirmed;
    return Scaffold(
      appBar: AppBar(title: const Text('学分申报')),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: [
          if (isAdmin)
            Card(
              color: Colors.amber.withOpacity(0.15),
              child: const Padding(
                padding: EdgeInsets.all(12),
                child: Text(
                  '辅导员模式：你可以在对话中让 AI 总结申报情况、或直接在对话里审批（发"审批通过 #id"）。',
                  style: TextStyle(fontSize: 13),
                ),
              ),
            ),
          const SizedBox(height: 8),
          Text(
            '班级职位人员（班长/学委等）可通过问卷申报学分，辅导员审批后生效。',
            style: TextStyle(fontSize: 13, color: Colors.grey.shade400),
          ),
          const SizedBox(height: 16),
          Form(
            key: _formKey,
            child: Column(
              children: [
                DropdownButtonFormField<String>(
                  initialValue: _selectedTypeId,
                  decoration: const InputDecoration(
                    labelText: '学分类型',
                    border: OutlineInputBorder(),
                  ),
                  items: _types
                      .map((type) => DropdownMenuItem(
                            value: type['id'].toString(),
                            child: Text(
                              '${type['name']}（上限 ${type['max_points']} 分）',
                            ),
                          ))
                      .toList(),
                  onChanged: (value) =>
                      setState(() => _selectedTypeId = value),
                ),
                const SizedBox(height: 12),
                TextFormField(
                  controller: _pointsController,
                  keyboardType: TextInputType.number,
                  decoration: const InputDecoration(
                    labelText: '申报分值（加分，正数）',
                    border: OutlineInputBorder(),
                  ),
                  validator: (value) {
                    final points = double.tryParse(value ?? '');
                    if (points == null || points <= 0) {
                      return '请输入大于 0 的分值';
                    }
                    return null;
                  },
                ),
                const SizedBox(height: 12),
                TextFormField(
                  controller: _descriptionController,
                  maxLines: 3,
                  decoration: const InputDecoration(
                    labelText: '事项说明（活动内容、时间、组织方等）',
                    border: OutlineInputBorder(),
                  ),
                ),
                const SizedBox(height: 16),
                Row(
                  children: [
                    const Text('证据照片（最多 3 张）',
                        style: TextStyle(fontWeight: FontWeight.bold)),
                    const Spacer(),
                    TextButton.icon(
                      onPressed: _photos.length >= 3 ? null : _pickPhoto,
                      icon: const Icon(Icons.add_photo_alternate_outlined),
                      label: const Text('添加照片'),
                    ),
                  ],
                ),
                if (_photos.isNotEmpty)
                  SizedBox(
                    height: 72,
                    child: ListView.separated(
                      scrollDirection: Axis.horizontal,
                      itemCount: _photos.length,
                      separatorBuilder: (_, __) => const SizedBox(width: 8),
                      itemBuilder: (context, index) {
                        final bytes =
                            base64Decode(_photos[index]['data']!);
                        return Stack(
                          children: [
                            ClipRRect(
                              borderRadius: BorderRadius.circular(8),
                              child: Image.memory(
                                bytes,
                                width: 72,
                                height: 72,
                                fit: BoxFit.cover,
                              ),
                            ),
                            Positioned(
                              top: 0,
                              right: 0,
                              child: InkWell(
                                onTap: () => setState(
                                    () => _photos.removeAt(index)),
                                child: const CircleAvatar(
                                  radius: 10,
                                  backgroundColor: Colors.black54,
                                  child: Icon(Icons.close, size: 14),
                                ),
                              ),
                            ),
                          ],
                        );
                      },
                    ),
                  ),
                const SizedBox(height: 20),
                FilledButton.icon(
                  onPressed: _submitting ? null : _submit,
                  icon: _submitting
                      ? const SizedBox(
                          width: 16,
                          height: 16,
                          child: CircularProgressIndicator(strokeWidth: 2),
                        )
                      : const Icon(Icons.send),
                  label: Text(_submitting ? '提交中…' : '提交申报'),
                  style: FilledButton.styleFrom(
                    minimumSize: const Size.fromHeight(48),
                  ),
                ),
                if (_resultText.isNotEmpty) ...[
                  const SizedBox(height: 12),
                  Card(
                    color: Colors.green.withOpacity(0.1),
                    child: Padding(
                      padding: const EdgeInsets.all(12),
                      child: SelectableText(
                        _resultText,
                        style: const TextStyle(fontSize: 13),
                      ),
                    ),
                  ),
                ],
              ],
            ),
          ),
        ],
      ),
    );
  }
}

// 中继 WebSocket 客户端：配对握手 + 消息收发。
//
// 协议（与 relay-server/index.js 一致）：
//   连接后第一条消息 auth：
//     {type:"auth", code:"配对码"}       扫码配对中
//     {type:"auth", token:"session"}     已配对重连
//   服务端回复：auth_ok / auth_error / pair_pending / pair_result
//   消息：{type:"message", to:"<desktop_id>", body:{text:...}, msg_id:"uuid"}
//   心跳：每 30s {type:"ping"}

import 'dart:async';
import 'dart:convert';
import 'package:flutter/foundation.dart';
import 'package:web_socket_channel/web_socket_channel.dart';

enum RelayStatus { disconnected, connecting, paired, pairPending, rejected, error }

/// 连接候选：URL + 认证消息。
class _ConnTarget {
  _ConnTarget(this.url, this.auth);

  final String url;
  final Map<String, dynamic> auth;
}

class RelayMessage {
  RelayMessage({required this.from, required this.body, this.msgId});

  final String from;
  final Map<String, dynamic> body;
  final String? msgId;
}

class RelayClient extends ChangeNotifier {
  RelayClient();

  WebSocketChannel? _channel;
  StreamSubscription? _subscription;
  Timer? _heartbeat;
  RelayStatus _status = RelayStatus.disconnected;
  String _error = '';
  String _desktopId = '';
  String _pairCode = '';
  String? _pendingToken;
  String? _pendingDeviceId;

  final List<RelayMessage> _inbox = [];

  RelayStatus get status => _status;
  String get error => _error;
  String get desktopId => _desktopId;
  List<RelayMessage> get inbox => List.unmodifiable(_inbox);

  /// 当前连接通道（direct = 局域网直连 Win 端；relay = 公网中继）。
  String get channelName => _channelName;
  String _channelName = 'relay';

  /// 扫码配对：优先直连（同 WiFi），失败自动切公网中继。
  Future<void> pairWithCode({
    required String code,
    String? relayUrl,
    String? directUrl,
    String? dcode,
  }) async {
    _pairCode = code;
    await _tryConnect([
      if (directUrl != null && directUrl.isNotEmpty)
        _ConnTarget(directUrl, {'type': 'auth', 'code': dcode ?? code}),
      if (relayUrl != null && relayUrl.isNotEmpty)
        _ConnTarget(relayUrl, {'type': 'auth', 'code': code}),
    ]);
  }

  /// 已配对重连：优先直连（断线重连时直连通常可用），失败切中继。
  Future<void> connectWithToken(
    String relayUrl,
    String token, {
    String? directUrl,
  }) async {
    await _tryConnect([
      if (directUrl != null && directUrl.isNotEmpty)
        _ConnTarget(directUrl, {'type': 'auth', 'token': token}),
      if (relayUrl.isNotEmpty)
        _ConnTarget(relayUrl, {'type': 'auth', 'token': token}),
    ]);
  }

  Future<void> _tryConnect(List<_ConnTarget> targets) async {
    if (targets.isEmpty) {
      _status = RelayStatus.error;
      _error = '没有可用的连接地址';
      notifyListeners();
      return;
    }
    for (final target in targets) {
      if (await _connectOnce(target.url, auth: target.auth)) {
        return;
      }
    }
    _status = RelayStatus.error;
    _error = '所有连接方式都失败了，请检查网络或重新扫码配对';
    notifyListeners();
  }

  Future<bool> _connectOnce(
    String relayUrl, {
    required Map<String, dynamic> auth,
  }) async {
    await disconnect();
    _status = RelayStatus.connecting;
    _error = '';
    notifyListeners();
    try {
      final uri = Uri.parse(relayUrl);
      final channel = WebSocketChannel.connect(uri);
      _channel = channel;
      channel.sink.add(jsonEncode(auth));
      final completer = Completer<bool>();
      var settled = false;
      _subscription = channel.stream.listen(
        (raw) {
          final type = _rawType(raw);
          if (!settled &&
              (type == 'auth_ok' ||
                  type == 'pair_pending' ||
                  type == 'auth_error' ||
                  type == 'pair_result')) {
            settled = true;
            completer.complete(true);
          }
          _onMessage(raw);
        },
        onError: (Object error) {
          if (!settled) {
            settled = true;
            completer.complete(false);
          }
          _status = RelayStatus.error;
          _error = '连接错误：$error';
          notifyListeners();
        },
        onDone: () {
          if (!settled) {
            settled = true;
            completer.complete(false);
          }
          if (_status != RelayStatus.paired &&
              _status != RelayStatus.pairPending) {
            _status = RelayStatus.disconnected;
            notifyListeners();
          }
        },
      );
      _heartbeat?.cancel();
      _heartbeat = Timer.periodic(const Duration(seconds: 30), (_) {
        if (_channel != null) {
          _channel!.sink.add(jsonEncode({'type': 'ping'}));
        }
      });
      final ok = await completer.future
          .timeout(const Duration(seconds: 6), onTimeout: () => false);
      if (!ok) {
        await disconnect();
        return false;
      }
      // 连接成功：标记通道名（局域网直连地址不含 127.0.0.1）
      _channelName = relayUrl.startsWith('ws://') &&
              !relayUrl.contains('127.0.0.1') &&
              !relayUrl.contains('localhost')
          ? 'direct'
          : 'relay';
      return true;
    } catch (_) {
      await disconnect();
      return false;
    }
  }

  String _rawType(dynamic raw) {
    try {
      final map = jsonDecode(raw as String) as Map<String, dynamic>;
      return (map['type'] as String?) ?? '';
    } catch (_) {
      return '';
    }
  }

  void _onMessage(dynamic raw) {
    Map<String, dynamic> msg;
    try {
      msg = jsonDecode(raw as String) as Map<String, dynamic>;
    } catch (_) {
      return;
    }
    switch (msg['type']) {
      case 'auth_ok':
        _status = RelayStatus.paired;
        _desktopId = (msg['device_id'] as String?) ?? '';
        _error = '';
        notifyListeners();
        break;
      case 'auth_error':
        _status = RelayStatus.error;
        _error = '认证失败：${msg['error']}';
        notifyListeners();
        break;
      case 'pair_pending':
        _status = RelayStatus.pairPending;
        _pendingToken = (msg['token'] as String?) ?? '';
        _pendingDeviceId = (msg['device_id'] as String?) ?? '';
        notifyListeners();
        break;
      case 'pair_result':
        final accepted = msg['accepted'] == true;
        if (accepted) {
          _status = RelayStatus.paired;
          _desktopId = (msg['desktop_id'] as String?) ?? '';
          _pendingToken = (msg['token'] as String?) ?? _pendingToken;
          _pendingDeviceId = (msg['device_id'] as String?) ?? _pendingDeviceId;
        } else {
          _status = RelayStatus.rejected;
          _error = '配对被拒绝';
        }
        notifyListeners();
        break;
      case 'message':
        final from = (msg['from'] as String?) ?? '';
        final body = (msg['body'] as Map?)?.cast<String, dynamic>() ?? {};
        _inbox.add(RelayMessage(from: from, body: body, msgId: msg['msg_id'] as String?));
        notifyListeners();
        break;
      case 'pong':
        break;
      default:
        break;
    }
  }

  /// 发送消息给 Windows 端（desktop_id）。
  void sendText(String text) {
    if (_channel == null || _desktopId.isEmpty) return;
    final msgId = DateTime.now().microsecondsSinceEpoch.toString();
    _channel!.sink.add(jsonEncode({
      'type': 'message',
      'to': _desktopId,
      'body': {'text': text},
      'msg_id': msgId,
    }));
  }

  /// 发送结构化消息（申报/审批/管理员确认等），Windows 端按 body.kind 分流。
  void sendAction(String kind, Map<String, dynamic> payload) {
    if (_channel == null || _desktopId.isEmpty) return;
    final msgId = DateTime.now().microsecondsSinceEpoch.toString();
    _channel!.sink.add(jsonEncode({
      'type': 'message',
      'to': _desktopId,
      'body': {'kind': kind, ...payload},
      'msg_id': msgId,
    }));
  }

  /// 清除收件箱（页面切换时）。
  void clearInbox() {
    _inbox.clear();
    notifyListeners();
  }

  String? get pendingToken => _pendingToken;
  String? get pendingDeviceId => _pendingDeviceId;

  Future<void> disconnect() async {
    _heartbeat?.cancel();
    _heartbeat = null;
    await _subscription?.cancel();
    _subscription = null;
    await _channel?.sink.close();
    _channel = null;
    if (_status != RelayStatus.pairPending && _status != RelayStatus.paired) {
      _status = RelayStatus.disconnected;
    }
  }
}

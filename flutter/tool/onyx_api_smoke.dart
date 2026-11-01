// ONYX 原生端 API 冒烟验收 —— 直接跑 UI 底层那份 onyx_api.dart。
//
// 为什么要有它：Flutter Windows 的文本框不接收 SendKeys / 剪贴板合成输入
// （见 ONYX_IMPL_PLAN.md §18.11.4），所以「ONYX 页能不能用」不能靠点界面回答。
// 这个脚本走的是 onyx_page.dart 调的同一套静态方法，能验的就是 UI 能拿到的数据。
//
// 用法：dart run tool/onyx_api_smoke.dart <管理员密码>
// 只做读操作，不下发任何任务、不封禁、不改数据。

import 'dart:io';

import '../lib/desktop/pages/onyx/onyx_api.dart';

int _pass = 0;
int _fail = 0;

void ok(String name, [String detail = '']) {
  _pass++;
  stdout.writeln('  [PASS] $name${detail.isEmpty ? '' : '  ->  $detail'}');
}

void bad(String name, Object e) {
  _fail++;
  stdout.writeln('  [FAIL] $name  ->  $e');
}

Future<void> check(String name, Future<void> Function() body) async {
  try {
    await body();
  } catch (e) {
    bad(name, e);
  }
}

Future<void> main(List<String> args) async {
  if (args.isEmpty) {
    stderr.writeln('用法: dart run tool/onyx_api_smoke.dart <管理员密码>');
    exit(2);
  }
  final password = args.first;
  stdout.writeln('== ONYX 原生端 API 冒烟（${OnyxApi.baseUrl}）==');

  await check('login', () async {
    await OnyxApi.login(password);
    if (!OnyxApi.hasToken) throw '没有拿到 token';
    ok('login', 'token 长度 ${OnyxApi.token.length}');
  });
  if (!OnyxApi.hasToken) {
    stdout.writeln('\n登录失败，后续项无法验证。pass=$_pass fail=$_fail');
    exit(1);
  }

  await check('summary', () async {
    final s = await OnyxApi.summary();
    ok('summary',
        '在线 ${s['online_devices']} / 设备 ${s['devices']} / 账号 ${s['accounts']} / 今日任务 ${s['tasks_today']} / 风险 ${s['risk_today']} / 文件 ${s['files']}');
  });

  List<Map<String, dynamic>> devices = const [];
  await check('devices', () async {
    devices = await OnyxApi.devices();
    ok('devices', '${devices.length} 条');
  });

  if (devices.isNotEmpty) {
    // 挑一台 password_ready 的：只有它的 vol_serial 已上报，服务端才算得出密码。
    final target = devices.firstWhere(
      (d) => d['password_ready'] == true,
      orElse: () => devices.first,
    );
    final hwid = (target['hwid'] ?? '').toString();
    await check('device', () async {
      final raw = await OnyxApi.device(hwid);
      // 服务端把设备塞在 data.device 里，data 同级还有 tasks / files / accounts。
      // onyx_page.dart:109 就是这么解的，这里保持一致。
      final d = (raw['device'] as Map).cast<String, dynamic>();
      final pw = (d['password'] ?? '').toString();
      ok('device',
          'hwid=$hwid password_ready=${d['password_ready']} password=${pw.isEmpty ? '(空)' : pw} remote_id=${d['remote_device_id']} can_remote=${d['can_remote']}');
      ok('device 附带集合',
          'tasks=${(raw['tasks'] as List?)?.length} files=${(raw['files'] as List?)?.length} accounts=${(raw['accounts'] as List?)?.length}');
      if (d['password_ready'] == true && pw.isEmpty) {
        throw 'password_ready=true 却没下发密码';
      }
      if (d['password_ready'] == true && !pw.startsWith('Bridge@')) {
        throw '密码格式不对: $pw';
      }
    });

    await check('devices 不含密码', () async {
      final leaked =
          devices.where((d) => (d['password'] ?? '').toString().isNotEmpty);
      if (leaked.isNotEmpty) {
        throw '列表接口泄露了 ${leaked.length} 条密码';
      }
      ok('devices 不含密码', '${devices.length} 条全部只有 password_ready');
    });
  } else {
    stdout.writeln('  [SKIP] device —— 设备列表为空');
  }

  await check('exposures', () async {
    final list = await OnyxApi.exposures();
    ok('exposures', '${list.length} 条');
  });

  await check('artifactUrl 拼装', () async {
    final u = OnyxApi.artifactUrl(1, download: true);
    if (!u.contains('token=') || !u.contains('dl=1')) throw 'URL 拼装不对: $u';
    ok('artifactUrl 拼装', '含 token 与 dl=1');
  });

  await check('logout 后 token 失效', () async {
    OnyxApi.logout();
    if (OnyxApi.hasToken) throw 'logout 没有清 token';
    try {
      await OnyxApi.summary();
      throw '吊销后仍然能取 summary';
    } on Exception catch (e) {
      ok('logout 后 token 失效', e.toString().replaceFirst('Exception: ', ''));
    }
  });

  stdout.writeln('\n== 结果: pass=$_pass fail=$_fail ==');
  exit(_fail == 0 ? 0 : 1);
}

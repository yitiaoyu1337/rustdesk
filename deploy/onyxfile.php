<?php
// ONYX 取证文件下载/预览（需要 admin 登录态）
// 用法: onyxfile.php?id=<auth_agent_files.id>[&dl=1]
require __DIR__ . '/config.php';
session_start();
header('X-Robots-Tag: noindex');

if (empty($_SESSION['admin_logged_in'])) {
    http_response_code(403);
    exit('未登录');
}

$id = (int)($_GET['id'] ?? 0);
if ($id <= 0) {
    http_response_code(400);
    exit('缺少 id');
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

$st = $pdo->prepare("SELECT stored_path, src_path, bytes, sha256 FROM auth_agent_files WHERE id = ?");
$st->execute([$id]);
$row = $st->fetch();
if (!$row) {
    http_response_code(404);
    exit('记录不存在');
}

// 只允许读 uploads 目录里的文件
$root = realpath('/srv/onyx-auth/uploads');
$real = realpath($row['stored_path']);
if ($root === false || $real === false || strncmp($real, $root . DIRECTORY_SEPARATOR, strlen($root) + 1) !== 0) {
    http_response_code(403);
    exit('路径不在允许范围内');
}
if (!is_file($real) || !is_readable($real)) {
    http_response_code(404);
    exit('文件不存在');
}

$name = basename($row['src_path'] !== '' ? str_replace('\\', '/', $row['src_path']) : $real);
$ext = strtolower(pathinfo($name, PATHINFO_EXTENSION));
$inlineTypes = ['jpg' => 'image/jpeg', 'jpeg' => 'image/jpeg', 'png' => 'image/png',
                'gif' => 'image/gif', 'webp' => 'image/webp'];
$preview = isset($inlineTypes[$ext]) && !isset($_GET['dl']);
$mime = $preview ? $inlineTypes[$ext] : 'application/octet-stream';

while (ob_get_level()) { ob_end_clean(); }
header('Content-Type: ' . $mime);
header('Content-Length: ' . filesize($real));
header('X-Content-Type-Options: nosniff');
header('Cache-Control: private, max-age=60');
header('Content-Disposition: ' . ($preview ? 'inline' : 'attachment') . '; filename="' . rawurlencode($name) . '"');
readfile($real);

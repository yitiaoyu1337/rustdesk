<?php
// ONYX 取证包 —— 把一台机器的身份、事件、任务、取证文件打成一个 zip
// 不用 php-zip 扩展：store-only 的 zip 手写即可，从零恢复的新机器上也能直接跑
require __DIR__ . '/config.php';
session_start();
header('X-Robots-Tag: noindex');

if (empty($_SESSION['admin_logged_in'])) {
    http_response_code(403);
    exit('forbidden');
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

$hwid = trim((string)($_GET['hwid'] ?? ''));
if ($hwid === '') {
    http_response_code(400);
    exit('missing hwid');
}
$hwidHash = hash('sha256', $hwid);

/**
 * 最小 zip 写入器：只做 stored（不压缩），够用且零依赖。
 * 支持 UTF-8 文件名（flag 0x0800），中文路径不会乱码。
 */
class OnyxZip
{
    private $fh;
    private $entries = [];
    private $offset = 0;

    public function __construct(string $path)
    {
        $this->fh = fopen($path, 'wb');
        if (!$this->fh) throw new RuntimeException('cannot open temp file');
    }

    private static function dosTime(int $ts): array
    {
        $d = getdate($ts);
        $time = ($d['hours'] << 11) | ($d['minutes'] << 5) | intdiv($d['seconds'], 2);
        $date = (($d['year'] - 1980) << 9) | ($d['mon'] << 5) | $d['mday'];
        return [$time & 0xFFFF, $date & 0xFFFF];
    }

    public function add(string $name, string $data): void
    {
        $name = str_replace('\\', '/', $name);
        [$t, $d] = self::dosTime(time());
        $crc = crc32($data);
        $len = strlen($data);
        $header = pack('V', 0x04034b50)
            . pack('v', 20) . pack('v', 0x0800) . pack('v', 0)
            . pack('v', $t) . pack('v', $d)
            . pack('V', $crc) . pack('V', $len) . pack('V', $len)
            . pack('v', strlen($name)) . pack('v', 0)
            . $name;
        fwrite($this->fh, $header);
        fwrite($this->fh, $data);
        $this->entries[] = [
            'name' => $name, 'crc' => $crc, 'len' => $len,
            'offset' => $this->offset, 't' => $t, 'd' => $d,
        ];
        $this->offset += strlen($header) + $len;
    }

    public function addFile(string $name, string $path): bool
    {
        if (!is_file($path) || !is_readable($path)) return false;
        $data = file_get_contents($path);
        if ($data === false) return false;
        $this->add($name, $data);
        return true;
    }

    public function close(): void
    {
        $cdStart = $this->offset;
        $cd = '';
        foreach ($this->entries as $e) {
            $cd .= pack('V', 0x02014b50)
                . pack('v', 20) . pack('v', 20) . pack('v', 0x0800) . pack('v', 0)
                . pack('v', $e['t']) . pack('v', $e['d'])
                . pack('V', $e['crc']) . pack('V', $e['len']) . pack('V', $e['len'])
                . pack('v', strlen($e['name'])) . pack('v', 0) . pack('v', 0)
                . pack('v', 0) . pack('v', 0) . pack('V', 0)
                . pack('V', $e['offset'])
                . $e['name'];
        }
        fwrite($this->fh, $cd);
        $n = count($this->entries);
        fwrite($this->fh, pack('V', 0x06054b50)
            . pack('v', 0) . pack('v', 0)
            . pack('v', $n) . pack('v', $n)
            . pack('V', strlen($cd)) . pack('V', $cdStart)
            . pack('v', 0));
        fclose($this->fh);
    }
}

// ── 数据 ──
$st = $pdo->prepare("SELECT username, computer_name, system_version, host_computer_name,
                            host_system_version, ip, first_seen, last_seen, seen_count
                     FROM user_usage_identities WHERE identity_key = ? AND identity_type = 'hwid' LIMIT 1");
$st->execute([$hwidHash]);
$ident = $st->fetch() ?: [];

$st = $pdo->prepare("SELECT id, event_type, risk_level, detail, ip, created_at
                     FROM auth_security_events WHERE hwid_hash = ? ORDER BY id ASC");
$st->execute([$hwidHash]);
$events = $st->fetchAll();

$st = $pdo->prepare("SELECT task_id, type, status, attempts, created_at, delivered_at, result_at,
                            result_code, result_message
                     FROM auth_agent_tasks WHERE hwid = ? ORDER BY id ASC");
$st->execute([$hwid]);
$tasks = $st->fetchAll();

$st = $pdo->prepare("SELECT id, task_id, src_path, stored_path, bytes, sha256, created_at
                     FROM auth_agent_files WHERE hwid = ? ORDER BY id ASC");
$st->execute([$hwid]);
$files = $st->fetchAll();

$st = $pdo->prepare("SELECT headline, note, published, created_at, updated_at
                     FROM auth_exposures WHERE hwid_hash = ?");
$st->execute([$hwidHash]);
$exposure = $st->fetch() ?: [];

$stamp = date('Ymd_His');
$safe = preg_replace('/[^A-Za-z0-9_.-]/', '_', substr($hwid, 0, 32));
$tmp = tempnam(sys_get_temp_dir(), 'onyxz');
$zip = new OnyxZip($tmp);

// manifest
$man = [];
$man[] = 'ONYX 取证包';
$man[] = '生成时间: ' . date('Y-m-d H:i:s');
$man[] = 'HWID: ' . $hwid;
$man[] = 'HWID-SHA256: ' . $hwidHash;
$man[] = '';
$man[] = '[身份]';
foreach (['username' => '用户名', 'computer_name' => '计算机名', 'host_computer_name' => '主机计算机名',
          'system_version' => '系统版本', 'host_system_version' => '主机系统版本',
          'ip' => 'IP', 'first_seen' => '首次出现', 'last_seen' => '最后出现', 'seen_count' => '登录次数'] as $k => $label) {
    $v = $ident[$k] ?? '';
    if ($v !== '' && $v !== null) $man[] = $label . ': ' . $v;
}
$man[] = '';
$man[] = '[公示]';
$man[] = '状态: ' . (empty($exposure) ? '未上墙' : ((int)$exposure['published'] === 1 ? '已公示' : '已下墙'));
if (!empty($exposure['headline'])) $man[] = '标题: ' . $exposure['headline'];
$man[] = '';
$man[] = '[统计]';
$man[] = '事件总数: ' . count($events);
$high = 0;
foreach ($events as $e) if (in_array($e['risk_level'], ['high', 'critical'], true)) $high++;
$man[] = '其中高风险: ' . $high;
$man[] = '任务总数: ' . count($tasks);
$man[] = '取证文件: ' . count($files);
$man[] = '';
$man[] = '[文件清单]';
foreach ($files as $f) {
    $man[] = sprintf('%s  %s  %d B  sha256=%s', $f['created_at'], $f['src_path'], $f['bytes'], $f['sha256']);
}
$zip->add('manifest.txt', implode("\n", $man) . "\n");

// events.csv
$csv = "id,created_at,event_type,risk_level,ip,detail\n";
foreach ($events as $e) {
    $csv .= sprintf("%d,%s,%s,%s,%s,%s\n", $e['id'], $e['created_at'], $e['event_type'],
        $e['risk_level'], $e['ip'], '"' . str_replace('"', '""', (string)$e['detail']) . '"');
}
$zip->add('events.csv', $csv);

// tasks.csv
$csv = "task_id,type,status,attempts,created_at,delivered_at,result_at,result_code,result_message\n";
foreach ($tasks as $t) {
    $csv .= sprintf("%s,%s,%s,%d,%s,%s,%s,%d,%s\n", $t['task_id'], $t['type'], $t['status'],
        $t['attempts'], $t['created_at'], $t['delivered_at'], $t['result_at'], $t['result_code'],
        '"' . str_replace('"', '""', (string)$t['result_message']) . '"');
}
$zip->add('tasks.csv', $csv);

// timeline
$tl = [];
foreach ($events as $e) $tl[] = [$e['created_at'], '事件  ' . $e['event_type'] . ' [' . $e['risk_level'] . '] ' . $e['detail']];
foreach ($tasks as $t) $tl[] = [$t['created_at'], '任务  ' . $t['type'] . ' → ' . $t['status'] . ' ' . substr((string)$t['result_message'], 0, 120)];
foreach ($files as $f) $tl[] = [$f['created_at'], '取证  ' . $f['src_path'] . ' (' . $f['bytes'] . ' B)'];
usort($tl, fn($a, $b) => strcmp($a[0], $b[0]));
$out = '';
foreach ($tl as $row) $out .= $row[0] . '  ' . $row[1] . "\n";
$zip->add('timeline.txt', $out);

// 取证文件
$used = [];
foreach ($files as $f) {
    $base = basename(str_replace('\\', '/', (string)$f['src_path']));
    if ($base === '' || $base === '.' || $base === '..') $base = 'file_' . $f['id'];
    $name = $base;
    $i = 1;
    while (isset($used[$name])) { $name = $i . '_' . $base; $i++; }
    $used[$name] = true;
    $zip->addFile('files/' . $name, (string)$f['stored_path']);
}

$zip->close();

$download = isset($_GET['dl']);
header('Content-Type: application/zip');
header('Content-Disposition: ' . ($download ? 'attachment' : 'inline') . '; filename="onyx_' . $safe . '_' . $stamp . '.zip"');
header('Content-Length: ' . filesize($tmp));
header('Cache-Control: no-store');
readfile($tmp);
unlink($tmp);

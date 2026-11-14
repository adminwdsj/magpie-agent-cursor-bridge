// bridge.db 的统计与清理。数据库归引擎所有，这里只通过系统自带的 /usr/bin/sqlite3
// 读写：不加 npm 依赖，也不在插件进程里长期占着连接。清理和 VACUUM 前调用方
// 必须先停掉后台桥，避免和引擎的写入抢锁。
import fs from 'node:fs';
import {execFile} from 'node:child_process';

const SQLITE = process.env.CURSOR_BRIDGE_SQLITE || '/usr/bin/sqlite3';
const QUERY_TIMEOUT = 60000;
const CLEANUP_TIMEOUT = 300000;
const VACUUM_TIMEOUT = 900000;
export const SCOPES = ['traces', 'sessions', 'all', 'vacuum'];
const FK_ROLLBACK = '外键核对未通过，清理已整体回滚，数据未改动';

// 每张表的内容字节（UTF-8 字节，不是文件页）。新版本引擎多出来的表只计行数。
const TABLE_BYTES = {
  messages: 'SUM(length(CAST(payload_json AS BLOB)))',
  conversation_checkpoints: 'SUM(length(state_digest))',
  tool_rounds: 'SUM(length(CAST(assistant_json AS BLOB)))',
  tool_round_calls: "SUM(length(CAST(arguments_json AS BLOB)) + length(CAST(COALESCE(result_content, '') AS BLOB)))",
  llm_call_requests: 'SUM(byte_count)',
  llm_call_response_chunks: 'SUM(byte_count)',
  blobs: 'SUM(length(data))'
};

// 按依赖顺序排列：子表在前。配置表（model_configs、service_settings、_sqlx_migrations）不在其中，永不清理。
const TRACE_TABLES = ['llm_call_requests', 'llm_call_response_chunks', 'llm_calls', 'cursor_run_trace_artifacts', 'cursor_run_traces'];
const ALL_TABLES = [
  'tool_round_calls', 'tool_rounds', 'runs', 'checkpoint_messages', 'input_anchors',
  'conversation_checkpoints', 'messages', 'conversations', ...TRACE_TABLES, 'blob_edges', 'blobs'
];

const quoteIdent = name => `"${String(name).replaceAll('"', '""')}"`;
const quoteText = value => `'${String(value).replaceAll("'", "''")}'`;

// 不用 -readonly：停桥后引擎会删掉 -wal/-shm，只读打开 WAL 库就报 unable to open database file。
// 统计只发 SELECT，不会写数据。
function sqlite(dbPath, sql, {timeout = QUERY_TIMEOUT} = {}) {
  const args = ['-bail', '-json', '-cmd', '.timeout 15000', '-batch', dbPath, sql];
  return new Promise((resolve, reject) => {
    execFile(SQLITE, args, {encoding: 'utf8', timeout, maxBuffer: 16 * 1024 * 1024}, (error, stdout, stderr) => {
      if (error?.killed) return reject(new Error(`sqlite3 超过 ${timeout / 1000} 秒未完成，已中止；未提交的修改全部回滚`));
      const detail = String(stderr || '').trim();
      // RAISE(ROLLBACK) 之后 COMMIT 报「no transaction is active」，换成能看懂的原因。
      if (error) return reject(new Error(/no transaction is active/.test(detail) ? FK_ROLLBACK : detail || error.message));
      try { resolve(stdout.trim() ? JSON.parse(stdout) : []); } catch { resolve([]); }
    });
  });
}

const size = file => { try { return fs.statSync(file).size; } catch { return 0; } };
const fileSizes = dbPath => {
  const db = size(dbPath), wal = size(`${dbPath}-wal`), shm = size(`${dbPath}-shm`);
  return {db, wal, shm, total: db + wal + shm};
};

async function listTables(dbPath) {
  const rows = await sqlite(dbPath, "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name;");
  return rows.map(row => String(row.name));
}

export async function readDatabase(dbPath) {
  if (!fs.existsSync(dbPath)) return {exists: false, path: dbPath, file: fileSizes(dbPath), tables: []};
  const names = await listTables(dbPath);
  const parts = names.map(name =>
    `SELECT ${quoteText(name)} AS name, COUNT(*) AS rows, ${TABLE_BYTES[name] ? `COALESCE(${TABLE_BYTES[name]}, 0)` : 'NULL'} AS bytes FROM ${quoteIdent(name)}`);
  const tables = names.length ? await sqlite(dbPath, `${parts.join('\nUNION ALL\n')};`) : [];
  const [page] = await sqlite(dbPath, 'SELECT (SELECT page_size FROM pragma_page_size) AS pageSize, (SELECT freelist_count FROM pragma_freelist_count) AS freePages;');
  const [range] = names.includes('conversations')
    ? await sqlite(dbPath, 'SELECT MIN(updated_at_ms) AS oldest, MAX(updated_at_ms) AS newest FROM conversations;')
    : [{}];
  return {
    exists: true,
    path: dbPath,
    file: fileSizes(dbPath),
    freeBytes: (Number(page?.pageSize) || 0) * (Number(page?.freePages) || 0),
    conversations: {oldest: Number(range?.oldest) || null, newest: Number(range?.newest) || null},
    tables: tables.map(row => ({name: String(row.name), rows: Number(row.rows) || 0, bytes: row.bytes === null ? null : Number(row.bytes) || 0}))
  };
}

// 每个脚本先把要删的行数写进临时表，再按子表在前的顺序删除，最后一次性返回行数。
function script(present, tables, scopeFor, prelude = []) {
  const kept = tables.filter(name => present.has(name));
  // 删除时关掉外键检查：开着的话每删一个检查点都要全表扫描 checkpoint_messages 等没有索引的引用列，
  // 2 GB 的库 5 分钟都删不完。引用被删表的所有子表都在本次清理的表集合里，所以删完后逐表核对一次：
  // 引用断掉的行比删除前多，就由触发器 RAISE(ROLLBACK) 回滚整个事务。不能只靠出错退出：sqlite3 命令行
  // 在出错退出时会保留已执行的删除（3.54 实测）。
  const violations = kept.length
    ? `(SELECT COUNT(*) FROM (${kept.map(name => `SELECT 1 FROM pragma_foreign_key_check(${quoteText(name)})`).join(' UNION ALL ')}))`
    : '0';
  // 临时表建在事务之外：回滚后 _counts 还在但为空，cleanDatabase 据此判定整体回滚。
  return [
    'PRAGMA foreign_keys=OFF;',
    'CREATE TEMP TABLE _counts(t TEXT PRIMARY KEY, n INTEGER NOT NULL);',
    'CREATE TEMP TABLE _fk_guard(before INTEGER, after INTEGER);',
    `CREATE TEMP TRIGGER _fk_guard_check BEFORE INSERT ON _fk_guard WHEN NEW.after > NEW.before BEGIN SELECT RAISE(ROLLBACK, ${quoteText(FK_ROLLBACK)}); END;`,
    'BEGIN IMMEDIATE;',
    `CREATE TEMP TABLE _fk_before AS SELECT ${violations} AS n;`,
    ...prelude,
    ...kept.map(name => `INSERT OR REPLACE INTO _counts SELECT ${quoteText(name)}, COUNT(*) FROM ${quoteIdent(name)}${scopeFor(name)};`),
    ...kept.map(name => `DELETE FROM ${quoteIdent(name)}${scopeFor(name)};`),
    `INSERT INTO _fk_guard SELECT (SELECT n FROM _fk_before), ${violations};`,
    'COMMIT;', 'SELECT t, n FROM _counts ORDER BY t;'
  ].join('\n');
}

export function cleanupScript(scope, present, {olderThanDays = 30, now = Date.now()} = {}) {
  if (scope === 'traces') return script(present, TRACE_TABLES, () => '');
  if (scope === 'all') return script(present, ALL_TABLES, () => '');
  // sessions：删除超过 N 天没更新的会话及其消息、检查点、运行与工具调用；内容块（blobs）保留。
  const cutoff = now - olderThanDays * 86400000;
  const doomed = '(SELECT conversation_id FROM _doomed)';
  const runs = `(SELECT run_id FROM runs WHERE conversation_id IN ${doomed})`;
  const rounds = `(SELECT round_id FROM tool_rounds WHERE run_id IN ${runs})`;
  const scopes = {tool_round_calls: `round_id IN ${rounds}`, tool_rounds: `run_id IN ${runs}`};
  return script(present, ALL_TABLES.slice(0, 8), name => ` WHERE ${scopes[name] || `conversation_id IN ${doomed}`}`,
    [`CREATE TEMP TABLE _doomed AS SELECT conversation_id FROM conversations WHERE updated_at_ms < ${cutoff};`]);
}

export async function cleanDatabase(dbPath, {scope, olderThanDays = 30, vacuum = true} = {}) {
  if (!SCOPES.includes(scope)) throw new Error(`未知的清理范围：${scope || '空'}，可选 ${SCOPES.join(' / ')}`);
  if (scope === 'sessions' && (!Number.isInteger(olderThanDays) || olderThanDays < 1 || olderThanDays > 3650)) {
    throw new Error('保留天数需为 1–3650 的整数');
  }
  if (!fs.existsSync(dbPath)) throw new Error('数据库尚未创建，暂无可清理的数据');
  const started = Date.now();
  const before = fileSizes(dbPath);
  const counts = {};
  if (scope !== 'vacuum') {
    const present = new Set(await listTables(dbPath));
    if (scope === 'sessions' && !present.has('conversations')) throw new Error('数据库缺少会话表，无法按天清理');
    for (const row of await sqlite(dbPath, cleanupScript(scope, present, {olderThanDays}), {timeout: CLEANUP_TIMEOUT})) {
      counts[row.t] = Number(row.n) || 0;
    }
    // 脚本对每张涉及的表都记一行计数（哪怕是 0）；计数为空只能是事务被外键核对回滚了。
    const tables = scope === 'traces' ? TRACE_TABLES : scope === 'all' ? ALL_TABLES : ALL_TABLES.slice(0, 8);
    if (!Object.keys(counts).length && tables.some(name => present.has(name))) throw new Error(FK_ROLLBACK);
  }
  // VACUUM 重写整个文件，需要独占锁和约一倍数据库大小的临时空间。
  if (vacuum || scope === 'vacuum') {
    await sqlite(dbPath, 'PRAGMA wal_checkpoint(TRUNCATE);\nVACUUM;\nPRAGMA wal_checkpoint(TRUNCATE);', {timeout: VACUUM_TIMEOUT});
  }
  const after = fileSizes(dbPath);
  return {scope, counts, before, after, reclaimed: Math.max(0, before.total - after.total), durationMs: Date.now() - started};
}

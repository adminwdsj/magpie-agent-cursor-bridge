import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {execFileSync} from 'node:child_process';
import {readDatabase, cleanDatabase} from '../lib/database.js';

// 与引擎相同的外键关系，只保留清理会碰到的列。
const SCHEMA = `
PRAGMA journal_mode=WAL;
CREATE TABLE conversations (conversation_id TEXT PRIMARY KEY, updated_at_ms INTEGER NOT NULL);
CREATE TABLE messages (conversation_id TEXT NOT NULL, message_id TEXT NOT NULL, payload_json TEXT NOT NULL,
  PRIMARY KEY (conversation_id, message_id), FOREIGN KEY (conversation_id) REFERENCES conversations(conversation_id));
CREATE TABLE conversation_checkpoints (checkpoint_id TEXT PRIMARY KEY, conversation_id TEXT NOT NULL, parent_checkpoint_id TEXT, state_digest BLOB,
  FOREIGN KEY (conversation_id) REFERENCES conversations(conversation_id),
  FOREIGN KEY (parent_checkpoint_id) REFERENCES conversation_checkpoints(checkpoint_id));
CREATE TABLE checkpoint_messages (checkpoint_id TEXT NOT NULL, conversation_id TEXT NOT NULL, message_id TEXT NOT NULL,
  FOREIGN KEY (checkpoint_id) REFERENCES conversation_checkpoints(checkpoint_id),
  FOREIGN KEY (conversation_id, message_id) REFERENCES messages(conversation_id, message_id));
CREATE TABLE runs (run_id TEXT PRIMARY KEY, conversation_id TEXT NOT NULL, head_checkpoint_id TEXT,
  FOREIGN KEY (conversation_id) REFERENCES conversations(conversation_id),
  FOREIGN KEY (head_checkpoint_id) REFERENCES conversation_checkpoints(checkpoint_id));
CREATE TABLE tool_rounds (round_id TEXT PRIMARY KEY, run_id TEXT NOT NULL, assistant_json TEXT NOT NULL,
  FOREIGN KEY (run_id) REFERENCES runs(run_id));
CREATE TABLE tool_round_calls (round_id TEXT NOT NULL, call_id TEXT NOT NULL, arguments_json TEXT NOT NULL, result_content TEXT,
  FOREIGN KEY (round_id) REFERENCES tool_rounds(round_id));
CREATE TABLE llm_calls (call_id TEXT PRIMARY KEY);
CREATE TABLE llm_call_requests (call_id TEXT NOT NULL, byte_count INTEGER NOT NULL,
  FOREIGN KEY(call_id) REFERENCES llm_calls(call_id) ON DELETE CASCADE);
CREATE TABLE blobs (blob_id BLOB PRIMARY KEY, data BLOB NOT NULL);
CREATE TABLE model_configs (model_id TEXT PRIMARY KEY);
`;

function seed(dbPath, now) {
  const old = now - 40 * 86400000, recent = now - 86400000;
  let sql = SCHEMA + 'INSERT INTO model_configs VALUES (\'m\'); INSERT INTO blobs VALUES (x\'01\', x\'0203\'); INSERT INTO llm_calls VALUES (\'c\'); INSERT INTO llm_call_requests VALUES (\'c\', 10);';
  for (const [id, at] of [['old', old], ['new', recent]]) {
    sql += `INSERT INTO conversations VALUES ('${id}', ${at});
INSERT INTO messages VALUES ('${id}', 'm1', '{"text":"你好"}');
INSERT INTO conversation_checkpoints VALUES ('${id}-c1', '${id}', NULL, x'00');
INSERT INTO conversation_checkpoints VALUES ('${id}-c2', '${id}', '${id}-c1', x'00');
INSERT INTO checkpoint_messages VALUES ('${id}-c2', '${id}', 'm1');
INSERT INTO runs VALUES ('${id}-r', '${id}', '${id}-c2');
INSERT INTO tool_rounds VALUES ('${id}-t', '${id}-r', '{}');
INSERT INTO tool_round_calls VALUES ('${id}-t', 'call', '{}', 'ok');`;
  }
  execFileSync('/usr/bin/sqlite3', [dbPath, sql]);
}

const count = (dbPath, table) => Number(execFileSync('/usr/bin/sqlite3', [dbPath, `SELECT COUNT(*) FROM ${table}`], {encoding: 'utf8'}).trim());

test('sessions cleanup removes only stale conversations and keeps blobs and config', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'bridge-db-'));
  const dbPath = path.join(dir, 'bridge.db');
  seed(dbPath, Date.now());

  const result = await cleanDatabase(dbPath, {scope: 'sessions', olderThanDays: 30});

  assert.equal(result.counts.conversations, 1);
  assert.equal(result.counts.conversation_checkpoints, 2);
  assert.equal(result.counts.tool_round_calls, 1);
  for (const table of ['conversations', 'messages', 'runs', 'tool_rounds', 'tool_round_calls', 'checkpoint_messages']) {
    assert.equal(count(dbPath, table), 1, table);
  }
  assert.equal(count(dbPath, 'conversation_checkpoints'), 2);
  assert.equal(count(dbPath, 'blobs'), 1);
  assert.equal(count(dbPath, 'llm_calls'), 1);
  assert.equal(count(dbPath, 'model_configs'), 1);
  fs.rmSync(dir, {recursive: true, force: true});
});

test('traces and all scopes leave model configuration untouched', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'bridge-db-'));
  const dbPath = path.join(dir, 'bridge.db');
  seed(dbPath, Date.now());

  const traces = await cleanDatabase(dbPath, {scope: 'traces', vacuum: false});
  assert.deepEqual(traces.counts, {llm_call_requests: 1, llm_calls: 1});
  assert.equal(count(dbPath, 'conversations'), 2);

  await cleanDatabase(dbPath, {scope: 'all'});
  for (const table of ['conversations', 'messages', 'conversation_checkpoints', 'blobs']) assert.equal(count(dbPath, table), 0, table);
  assert.equal(count(dbPath, 'model_configs'), 1);

  const info = await readDatabase(dbPath);
  assert.equal(info.tables.find(t => t.name === 'model_configs').rows, 1);
  fs.rmSync(dir, {recursive: true, force: true});
});

test('a cleanup that would orphan a kept row is rolled back as a whole', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'bridge-db-'));
  const dbPath = path.join(dir, 'bridge.db');
  seed(dbPath, Date.now());
  // 新会话的运行指向旧会话的检查点：删旧会话会让这行引用断掉。
  execFileSync('/usr/bin/sqlite3', [dbPath, "UPDATE runs SET head_checkpoint_id = 'old-c2' WHERE run_id = 'new-r';"]);

  await assert.rejects(cleanDatabase(dbPath, {scope: 'sessions', olderThanDays: 30, vacuum: false}), /外键核对未通过/);
  assert.equal(count(dbPath, 'conversations'), 2);
  assert.equal(count(dbPath, 'conversation_checkpoints'), 4);
  fs.rmSync(dir, {recursive: true, force: true});
});

test('rejects unknown scopes and bad retention before touching the file', async () => {
  await assert.rejects(cleanDatabase('/nonexistent.db', {scope: 'everything'}), /未知的清理范围/);
  await assert.rejects(cleanDatabase('/nonexistent.db', {scope: 'sessions', olderThanDays: 0}), /保留天数/);
});

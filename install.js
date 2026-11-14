#!/usr/bin/env node
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {atomic,restoreProxy} from './lib/settings.js';
import {paths,run,verifyPackage,initializeCertificate,removeCertificate,stage,activate,stopJob,bootstrap,label} from './lib/install.js';
import {readDatabase,cleanDatabase,SCOPES} from './lib/database.js';
const source=path.dirname(fileURLToPath(import.meta.url));
const manifest=JSON.parse(fs.readFileSync(path.join(source,'package.json'),'utf8'));
const command=process.argv[2]||'setup';
// agent.js passes CURSOR_BRIDGE_ROOT when it falls back to setup(), and tests
// rely on it to install into a scratch tree. Ignoring it made "isolated" runs
// rewrite the real profile: installation.json, the release dir and the Startup
// entry all landed in the user's home, and a second supervisor came up.
const rooted=process.env.CURSOR_BRIDGE_ROOT;
const p=pathed(paths(os.homedir()));
function pathed(base) {
  if (!rooted) return base;
  // stage() puts releases under p.target, so a rooted install keeps them there.
  return {
    ...base,
    root: rooted,
    target: rooted,
    startupScript: process.platform === 'win32'
      ? path.join(rooted, 'Startup', `${label}.vbs`)
      : base.startupScript
  };
}

function findMagpie() {
  if (process.env.MAGPIE_PATH && fs.existsSync(process.env.MAGPIE_PATH)) return process.env.MAGPIE_PATH;
  if (process.platform === 'win32') {
    const candidates = [
      path.join(os.homedir(), 'Desktop/magpie-windows-amd64.exe'),
      path.join(process.env.LOCALAPPDATA || '', 'Programs/magpie/magpie.exe'),
      path.join(os.homedir(), 'Desktop/magpie.exe')
    ];
    for (const c of candidates) {
      if (fs.existsSync(c)) return c;
    }
    const where = run('where', ['magpie.exe'], {optional:true});
    if (where.status === 0 && where.stdout.trim()) return where.stdout.trim().split(/\r?\n/)[0];
    return null;
  }
  const macPath = '/Applications/magpie.app/Contents/MacOS/magpie';
  return fs.existsSync(macPath) ? macPath : null;
}

function bytes(value) {
  if (value === null || value === undefined) return '—';
  const units = ['B', 'KB', 'MB', 'GB'];
  let n = Number(value), i = 0;
  while (n >= 1024 && i < units.length - 1) { n /= 1024; i++; }
  return `${n.toFixed(i ? 1 : 0)} ${units[i]}`;
}

async function databaseCommand([action = 'stats', scope, ...flags]) {
  const dbPath = path.join(p.root, 'data/bridge.db');
  if (action === 'stats') {
    const info = await readDatabase(dbPath);
    if (!info.exists) return console.log(`数据库尚未创建：${dbPath}`);
    const day = ms => ms ? new Date(ms).toLocaleString() : '—';
    console.log(`${dbPath}\n文件 ${bytes(info.file.total)}（主文件 ${bytes(info.file.db)}，WAL ${bytes(info.file.wal)}，空闲页 ${bytes(info.freeBytes)}）`);
    console.log(`会话更新时间 ${day(info.conversations.oldest)} ～ ${day(info.conversations.newest)}`);
    for (const table of [...info.tables].sort((a, b) => (b.bytes ?? -1) - (a.bytes ?? -1))) {
      console.log(`  ${table.name.padEnd(28)} ${String(table.rows).padStart(9)} 行  ${bytes(table.bytes).padStart(9)}`);
    }
    return;
  }
  if (action !== 'clean') throw new Error('用法：node install.js db [stats | clean <范围>]，详见 --help');
  const daysFlag = flags.indexOf('--days');
  const olderThanDays = daysFlag >= 0 ? Number(flags[daysFlag + 1]) : 30;
  if (scope === 'all' && !flags.includes('--yes')) throw new Error('清空全部运行数据不可恢复，确认后加 --yes 重新执行');
  if (!SCOPES.includes(scope)) throw new Error(`清理范围需为 ${SCOPES.join(' / ')}`);
  if (process.platform !== 'darwin') throw new Error('数据库清理目前只支持 macOS');
  const domain = `gui/${process.getuid()}`;
  const lock = path.join(p.root, 'installer.lock');
  try { fs.mkdirSync(lock); } catch { throw new Error('另一个安装程序正在运行；若上次被强制终止，请检查后移除 installer.lock 再重试'); }
  // 引擎在 WAL 模式下一直开着数据库；先停后台，删除和 VACUUM 才不会和它抢写锁。
  const running = run('/bin/launchctl', ['print', `${domain}/${label}`], {optional: true}).status === 0;
  try {
    if (running) await stopJob(p, domain);
    const result = await cleanDatabase(dbPath, {scope, olderThanDays, vacuum: !flags.includes('--no-vacuum')});
    for (const [table, count] of Object.entries(result.counts)) console.log(`  删除 ${table} ${count} 行`);
    console.log(`完成：${bytes(result.before.total)} → ${bytes(result.after.total)}，释放 ${bytes(result.reclaimed)}，用时 ${(result.durationMs / 1000).toFixed(1)} 秒`);
  } finally {
    if (running && p.plist && fs.existsSync(p.plist)) await bootstrap(p, domain);
    try { fs.rmdirSync(lock); } catch {}
  }
}

try {
  if(command==='--help'||command==='-h') {
    console.log(`Usage: node install.js [setup|status|uninstall|db|--version]
setup: install/update the paired plugin and service.
db: show bridge.db usage.
db clean traces|sessions|vacuum|all [--days N] [--no-vacuum] [--yes]
  traces   调用日志与请求追踪
  sessions 超过 N 天（默认 30）没更新的会话、消息、检查点与工具调用；内容块保留
  vacuum   只压缩数据库文件
  all      清空全部运行数据（含内容块），保留模型配置与服务设置；需加 --yes
  清理前会停掉后台桥，结束后自动重新启动。`);
  } else if(command==='--version') {
    console.log(manifest.version);
  } else if(command==='status') {
    console.log(fs.existsSync(path.join(p.root,'status.json'))?fs.readFileSync(path.join(p.root,'status.json'),'utf8'):'未安装或尚未运行');
  } else if(command==='db') {
    await databaseCommand(process.argv.slice(3));
  } else {
    if(!['setup','uninstall'].includes(command)) throw new Error('未知命令；使用 --help 查看用法');
    const isSupported = (process.platform === 'darwin' && process.arch === 'arm64') || (process.platform === 'win32' && process.arch === 'x64');
    if(!isSupported) throw new Error('此版本支持 Apple Silicon macOS 与 Windows x64');
    const domain = process.platform === 'darwin' && process.getuid ? `gui/${process.getuid()}` : '';
    const magpie = findMagpie();
    fs.mkdirSync(p.root,{recursive:true,mode:0o700});
    if (process.platform !== 'win32') {
      try { fs.chmodSync(p.root,0o700); } catch {}
    }
    const lock=path.join(p.root,'installer.lock');
    try {fs.mkdirSync(lock);} catch {throw new Error('另一个安装程序正在运行；若上次被强制终止，请检查后移除 installer.lock 再重试');}
    try {
      if(command==='uninstall') {
        if(magpie) run(magpie,['cursor-bridge','default'],{optional:true});
        await stopJob(p,domain);
        restoreProxy(path.join(p.root,'settings-journal.json'));
        if(magpie) run(magpie,['plugin','rm',p.target],{optional:true});
        if(p.plist && fs.existsSync(p.plist)) fs.unlinkSync(p.plist);
        if(p.startupScript && fs.existsSync(p.startupScript)) fs.unlinkSync(p.startupScript);
        const ca=removeCertificate(p);
        console.log(ca.removed
          ? '后台已卸载，Cursor 代理已还原，本机根证书及其私钥已删除。若从 Magpie 插件页安装，请同时移除该插件。会话数据保留。'
          : `后台已卸载，Cursor 代理已还原，证书私钥已删除；钥匙串里的证书未能删除（SHA-1 ${ca.sha1}），请在「钥匙串访问」里手动删除 Cursor BYOK Local CA。`);
      } else {
        verifyPackage(source);
        const isWin = process.platform === 'win32';
        const enginePath = path.join(source, 'bin', isWin ? 'cursor-magpie-bridge.exe' : 'cursor-magpie-bridge');
        initializeCertificate(enginePath, p);
        const release=stage(source,p,manifest.version,process.execPath);
        const configHome=process.env.XDG_CONFIG_HOME||path.join(p.home,'.config');
        const packagePath=path.join(configHome,'magpie/plugins/node_modules',manifest.name);
        const desktopPlugins=path.join(p.home,'Desktop/data/plugins/node_modules',manifest.name);
        let registered=false;
        try {
          if (fs.existsSync(packagePath) && fs.realpathSync(packagePath) === fs.realpathSync(source)) registered = true;
          if (fs.existsSync(desktopPlugins) && fs.realpathSync(desktopPlugins) === fs.realpathSync(source)) registered = true;
        } catch {}
        if(registered) {
          if(magpie) run(magpie,['plugin','rm',p.target],{optional:true});
        } else {
          atomic(path.join(p.target,'agent.js'),fs.readFileSync(path.join(source,'agent.js'),'utf8'));
          atomic(path.join(p.target,'package.json'),JSON.stringify({name:manifest.name,version:manifest.version,type:'module',magpie:manifest.magpie},null,2));
        }
        await activate(p,release,domain);
        atomic(path.join(p.root,'installation.json'),JSON.stringify({version:manifest.version,release,source,installed:new Date().toISOString()},null,2));
        console.log(`已成功安装并启动 v${manifest.version}。在 Magpie → Agents 选择 Magpie-Cursor-Relay 和模型；首次连接后重启 Cursor 即可开始使用。`);
      }
    } finally {
      try { fs.rmdirSync(lock); } catch {}
    }
  }
} catch(error) {console.error(error.message);process.exitCode=1;}

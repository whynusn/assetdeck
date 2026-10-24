#!/usr/bin/env node
// ux-audit 进度状态机：原子更新 state.json、互斥锁、运行日志。
// 无第三方依赖。所有命令都在 ux-audit 工作树根目录执行。
import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(SCRIPT_DIR, '../../..');
const AUDIT_DIR = path.join(ROOT, 'automation', 'ux-audit');
const STATE_PATH = path.join(AUDIT_DIR, 'state.json');
const JOURNAL_PATH = path.join(AUDIT_DIR, 'journal.md');
const LOCK_PATH = path.join(ROOT, 'target', 'ux-audit', 'lock.json');
const LOCK_TTL_MS = 120 * 60 * 1000;

const COMPONENT_STATUSES = new Set(['pending', 'in_progress', 'done', 'blocked']);
const SEVERITIES = new Set(['P0', 'P1', 'P2', 'P3']);
const ROOT_CAUSES = new Set(['token', 'primitive', 'layout', 'viewmodel', 'platform', 'copy']);

function fail(message, code = 1) {
  console.error(`[ux-audit] ${message}`);
  process.exit(code);
}

function readState() {
  try {
    return JSON.parse(fs.readFileSync(STATE_PATH, 'utf8'));
  } catch (error) {
    fail(`读取 state.json 失败：${error.message}`);
  }
}

function atomicWrite(file, content) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const tmp = `${file}.tmp-${process.pid}`;
  fs.writeFileSync(tmp, content, 'utf8');
  try {
    fs.renameSync(tmp, file);
  } catch (error) {
    if (error.code === 'EEXIST' || error.code === 'EPERM') {
      fs.rmSync(file, { force: true });
      fs.renameSync(tmp, file);
    } else {
      throw error;
    }
  }
}

function writeState(state) {
  state.updatedAt = nowIso();
  atomicWrite(STATE_PATH, `${JSON.stringify(state, null, 2)}\n`);
}

function nowIso() {
  return new Date().toISOString();
}

function makeRunId(date = new Date()) {
  const pad = (n) => String(n).padStart(2, '0');
  return `run-${date.getFullYear()}${pad(date.getMonth() + 1)}${pad(date.getDate())}-${pad(date.getHours())}${pad(date.getMinutes())}${pad(date.getSeconds())}`;
}

function git(args) {
  const result = spawnSync('git', args, { cwd: ROOT, encoding: 'utf8' });
  return {
    code: result.status ?? 1,
    out: (result.stdout || '').trim(),
    err: (result.stderr || '').trim(),
  };
}

function currentBranch() {
  return git(['rev-parse', '--abbrev-ref', 'HEAD']).out;
}

function assertAuditBranch(state) {
  const branch = currentBranch();
  if (branch !== state.branch) {
    fail(`当前分支是 ${branch}，必须在 ${state.branch} 分支运行。`, 2);
  }
}

function dirtyFiles() {
  const result = git(['status', '--porcelain', '--untracked-files=normal']);
  if (result.code !== 0) {
    fail(`git status 失败：${result.err}`, 2);
  }
  return result.out ? result.out.split('\n').filter(Boolean) : [];
}

function ensureClean() {
  const dirty = dirtyFiles();
  if (dirty.length > 0) {
    return { ok: false, reason: 'dirty', files: dirty.slice(0, 30), total: dirty.length };
  }
  return null;
}

function readLock() {
  try {
    return JSON.parse(fs.readFileSync(LOCK_PATH, 'utf8'));
  } catch {
    return null;
  }
}

function lockAgeMs(lock) {
  const started = lock?.startedAt ? Date.parse(lock.startedAt) : NaN;
  if (!Number.isFinite(started)) {
    try {
      return Date.now() - fs.statSync(LOCK_PATH).mtimeMs;
    } catch {
      return Number.POSITIVE_INFINITY;
    }
  }
  return Date.now() - started;
}

function writeLock(state, run) {
  atomicWrite(
    LOCK_PATH,
    `${JSON.stringify(
      {
        runId: run.runId,
        runNumber: run.runNumber,
        componentId: run.componentId,
        pid: process.pid,
        startedAt: run.startedAt,
      },
      null,
      2,
    )}\n`,
  );
}

function clearLock() {
  fs.rmSync(LOCK_PATH, { force: true });
}

function allComponents(state) {
  return state.modules.flatMap((module) =>
    module.components.map((component) => ({ module, component })),
  );
}

function findEntry(state, componentId) {
  return allComponents(state).find(({ component }) => component.id === componentId) || null;
}

function depsDone(state, component) {
  return (component.dependsOn || []).every((dep) => findEntry(state, dep)?.component.status === 'done');
}

function selectNext(state) {
  const entries = allComponents(state)
    .filter(({ component }) => component.status === 'pending' && depsDone(state, component))
    .sort((a, b) => {
      if (a.component.priority !== b.component.priority) {
        return a.component.priority - b.component.priority;
      }
      const moduleIndex = state.modules.indexOf(a.module) - state.modules.indexOf(b.module);
      if (moduleIndex !== 0) return moduleIndex;
      return a.module.components.indexOf(a.component) - b.module.components.indexOf(b.component);
    });
  return entries[0] || null;
}

function openFindings(state, componentId) {
  return state.findings.filter(
    (finding) => finding.status === 'open' && (!componentId || finding.componentId === componentId),
  );
}

function workOrder(state, entry, run, extra = {}) {
  const { module, component } = entry;
  return {
    ok: true,
    ...extra,
    runId: run.runId,
    runNumber: run.runNumber,
    baseCommit: run.baseCommit,
    startedAt: run.startedAt,
    module: { id: module.id, name: module.name, intent: module.intent },
    component,
    openFindings: openFindings(state, component.id),
    paths: {
      root: ROOT,
      contract: path.relative(ROOT, path.join(AUDIT_DIR, 'design-contract.md')),
      runbook: path.relative(ROOT, path.join(AUDIT_DIR, 'runbook.md')),
      state: path.relative(ROOT, STATE_PATH),
      journal: path.relative(ROOT, JOURNAL_PATH),
      shots: path.relative(ROOT, path.join(ROOT, 'target', 'ux-audit', 'shots')),
    },
    commands: {
      captureBefore: `node automation/ux-audit/scripts/capture.mjs --component ${component.id} --phase before`,
      captureAfter: `node automation/ux-audit/scripts/capture.mjs --component ${component.id} --phase after`,
      addFinding:
        'node automation/ux-audit/scripts/state.mjs add-finding --component <ID> --severity P1 --root-cause token --title "..." --detail "..."',
      complete:
        `node automation/ux-audit/scripts/state.mjs complete --component ${component.id} --status done --summary "..." --gates "slint-check=pass;cargo-check=pass;deps-guard=pass;ui-hygiene=pass;visual=pass" --shots "target/ux-audit/shots/${component.id}/after"`,
    },
  };
}

function sync() {
  const state = readState();
  assertAuditBranch(state);
  const dirty = ensureClean();
  if (dirty) {
    console.log(JSON.stringify({ ...dirty, hint: '先提交或 stash 本次运行的改动，再 sync。' }, null, 2));
    process.exit(2);
  }
  const mainExists = git(['rev-parse', '--verify', 'main']);
  if (mainExists.code !== 0) {
    console.log(JSON.stringify({ ok: false, reason: 'missing-main', error: mainExists.err }, null, 2));
    process.exit(2);
  }
  const ancestor = git(['merge-base', '--is-ancestor', 'main', 'HEAD']);
  if (ancestor.code === 0) {
    console.log(JSON.stringify({ ok: true, merged: false, reason: 'up-to-date', head: git(['rev-parse', '--short', 'HEAD']).out }, null, 2));
    return;
  }
  const merge = git(['merge', '--no-edit', 'main']);
  if (merge.code !== 0) {
    git(['merge', '--abort']);
    console.log(
      JSON.stringify(
        {
          ok: false,
          reason: 'merge-conflict',
          hint: '已执行 git merge --abort。请结束本次运行，冲突留给用户处理。',
          detail: (merge.err || merge.out).split('\n').slice(-20),
        },
        null,
        2,
      ),
    );
    process.exit(2);
  }
  console.log(JSON.stringify({ ok: true, merged: true, head: git(['rev-parse', '--short', 'HEAD']).out }, null, 2));
}

function begin(options) {
  const state = readState();
  assertAuditBranch(state);

  const lock = readLock();
  if (lock && lockAgeMs(lock) < LOCK_TTL_MS) {
    console.log(JSON.stringify({ ok: false, reason: 'locked', lock, hint: '另一次运行仍在进行，本次直接结束。' }, null, 2));
    process.exit(2);
  }
  if (lock) {
    clearLock();
  }

  const dirty = ensureClean();
  if (dirty) {
    console.log(
      JSON.stringify(
        {
          ...dirty,
          hint: '上一次运行留下未提交改动。确认仅涉及 crates/app-ui、crates/ui-viewmodels、automation/ux-audit 后，用 git stash push -u -m "ux-audit-recovery-<ts>" 暂存并重试。',
        },
        null,
        2,
      ),
    );
    process.exit(2);
  }

  let entry = null;
  let resumed = false;
  if (state.current?.componentId) {
    const currentEntry = findEntry(state, state.current.componentId);
    if (currentEntry && currentEntry.component.status === 'in_progress') {
      entry = currentEntry;
      resumed = true;
    } else {
      state.current = null;
    }
  }

  if (!entry && options.component) {
    entry = findEntry(state, options.component);
    if (!entry) {
      console.log(JSON.stringify({ ok: false, reason: 'unknown-component', component: options.component }, null, 2));
      process.exit(2);
    }
  }

  if (!entry) {
    entry = selectNext(state);
  }

  if (!entry) {
    const regression = allComponents(state)
      .filter(({ component }) => component.status === 'done')
      .sort((a, b) => String(a.component.lastRun).localeCompare(String(b.component.lastRun)))[0];
    console.log(
      JSON.stringify(
        {
          ok: false,
          reason: 'no_pending',
          hint: '没有待审组件。按 runbook §7 做回归巡检：capture --all，必要时用 begin --component <ID> 重审。',
          suggestedRegression: regression
            ? { id: regression.component.id, name: regression.component.name, lastRun: regression.component.lastRun }
            : null,
        },
        null,
        2,
      ),
    );
    process.exit(2);
  }

  const { component } = entry;
  const run = {
    runId: makeRunId(),
    runNumber: state.runCount + 1,
    componentId: component.id,
    startedAt: nowIso(),
    baseCommit: git(['rev-parse', '--short', 'HEAD']).out || null,
  };
  state.runCount = run.runNumber;
  state.current = { ...run, resumed };
  component.status = 'in_progress';
  component.lastRun = run.runId;
  writeLock(state, run);
  writeState(state);
  console.log(JSON.stringify(workOrder(state, entry, run, { resumed }), null, 2));
}

function requireCurrent(state) {
  if (!state.current?.componentId) {
    fail('当前没有进行中的运行。先执行 begin。', 2);
  }
  return state.current;
}

function addFinding(options) {
  const state = readState();
  const current = requireCurrent(state);
  const componentId = options.component || current.componentId;
  if (componentId !== current.componentId) {
    fail(`finding 必须记录在当前组件 ${current.componentId} 上（收到 ${componentId}）。`, 2);
  }
  if (!options.title || !options.detail) fail('add-finding 需要 --title 与 --detail。', 2);
  const severity = options.severity || 'P2';
  const rootCause = options['root-cause'] || 'token';
  if (!SEVERITIES.has(severity)) fail(`severity 必须是 P0/P1/P2/P3。`, 2);
  if (!ROOT_CAUSES.has(rootCause)) fail(`root-cause 必须是 ${[...ROOT_CAUSES].join('/')}。`, 2);
  const max = state.findings.reduce((acc, finding) => {
    const n = Number(String(finding.id).replace(/^F-/, ''));
    return Number.isFinite(n) ? Math.max(acc, n) : acc;
  }, 0);
  const finding = {
    id: `F-${String(max + 1).padStart(3, '0')}`,
    componentId,
    severity,
    rootCause,
    title: options.title,
    detail: options.detail,
    evidence: options.evidence || '',
    status: 'open',
    createdRun: state.runCount,
    closedRun: null,
    fix: '',
    guard: '',
  };
  state.findings.push(finding);
  writeState(state);
  console.log(JSON.stringify({ ok: true, finding }, null, 2));
}

function closeFinding(options) {
  const state = readState();
  const current = requireCurrent(state);
  if (!options.id) fail('close-finding 需要 --id。', 2);
  const finding = state.findings.find((item) => item.id === options.id);
  if (!finding) fail(`找不到 finding ${options.id}。`, 2);
  if (finding.status !== 'open') fail(`finding ${options.id} 不是 open 状态（当前 ${finding.status}）。`, 2);
  finding.status = options.status === 'wontfix' ? 'wontfix' : 'fixed';
  finding.closedRun = state.runCount;
  finding.fix = options.fix || '';
  finding.guard = options.guard || '';
  writeState(state);
  console.log(JSON.stringify({ ok: true, finding, runId: current.runId }, null, 2));
}

function journalEntry(state, run, component, status, summary, gates, shots, extra) {
  // 本次运行新建或关闭的 finding 都要进日志——只按 createdRun 过滤会漏掉
  // “旧 finding 在本次修掉”这一最常见情况。
  const related = state.findings.filter(
    (finding) => finding.createdRun === run.runNumber || finding.closedRun === run.runNumber,
  );
  const lines = [];
  lines.push(`## ${run.runId} · ${component.id} · ${status}`);
  lines.push('');
  lines.push(`- 组件：${component.name}`);
  lines.push(`- 结论：${summary}`);
  if (related.length > 0) {
    lines.push(`- Findings：${related.map((finding) => `${finding.id}(${finding.status}) ${finding.title}`).join('；')}`);
  }
  if (typeof gates === 'string' && gates) lines.push(`- 门禁：${gates}`);
  if (typeof shots === 'string' && shots) lines.push(`- 截图：${shots}`);
  lines.push(`- 基线：${run.baseCommit || 'unknown'}`);
  if (extra) lines.push(`- 备注：${extra}`);
  lines.push('');
  fs.appendFileSync(JOURNAL_PATH, `${lines.join('\n')}\n`, 'utf8');
}

function complete(options) {
  const state = readState();
  const current = requireCurrent(state);
  const componentId = options.component || current.componentId;
  if (componentId !== current.componentId) {
    fail(`complete 的组件 ${componentId} 与当前运行 ${current.componentId} 不一致。`, 2);
  }
  const entry = findEntry(state, componentId);
  if (!entry) fail(`找不到组件 ${componentId}。`, 2);
  const status = options.status || 'done';
  if (!['done', 'blocked'].includes(status)) fail('status 只能是 done 或 blocked。', 2);
  if (!options.summary) fail('complete 需要 --summary。', 2);
  if (status === 'blocked' && !options['blocked-reason']) fail('blocked 需要 --blocked-reason。', 2);

  if (status === 'done') {
    const blocking = openFindings(state, componentId).filter(
      (finding) => finding.severity === 'P0' || finding.severity === 'P1',
    );
    if (blocking.length > 0) {
      fail(
        `组件仍有开放 P0/P1：${blocking.map((finding) => `${finding.id} ${finding.title}`).join('；')}。先修复或改 blocked。`,
        2,
      );
    }
  }

  const { component, module } = entry;
  const run = { ...current };
  component.status = status;
  component.lastRun = run.runId;
  component.auditedAtCommit = run.baseCommit || null;
  if (options.summary) {
    component.notes = [component.notes, `[${run.runId}] ${options.summary}`].filter(Boolean).join('\n');
  }
  if (status === 'blocked') {
    component.notes = [component.notes, `[${run.runId}] blocked: ${options['blocked-reason']}`]
      .filter(Boolean)
      .join('\n');
  }
  state.lastRuns = [
    {
      runId: run.runId,
      runNumber: run.runNumber,
      componentId,
      moduleId: module.id,
      status,
      summary: options.summary,
      gates: options.gates || '',
      shots: options.shots || '',
      baseCommit: run.baseCommit,
      finishedAt: nowIso(),
    },
    ...(state.lastRuns || []),
  ].slice(0, 20);
  state.current = null;
  journalEntry(
    state,
    run,
    component,
    status,
    options.summary,
    options.gates || '',
    options.shots || '',
    status === 'blocked' ? options['blocked-reason'] : options.notes,
  );
  writeState(state);
  clearLock();
  console.log(JSON.stringify({ ok: true, runId: run.runId, componentId, status, journal: path.relative(ROOT, JOURNAL_PATH) }, null, 2));
}

function abort(options) {
  const state = readState();
  const current = requireCurrent(state);
  const entry = findEntry(state, current.componentId);
  const reason = options.reason || '未提供原因';
  if (entry) {
    entry.component.status = options.blocked ? 'blocked' : 'pending';
    entry.component.notes = [entry.component.notes, `[${current.runId}] aborted: ${reason}`].filter(Boolean).join('\n');
    journalEntry(state, current, entry.component, 'aborted', reason, options.gates || '', options.shots || '', '');
  }
  state.current = null;
  writeState(state);
  clearLock();
  console.log(JSON.stringify({ ok: true, aborted: current.runId, componentId: current.componentId, reason }, null, 2));
}

function validateState() {
  const state = readState();
  const errors = [];
  if (state.schemaVersion !== 1) errors.push(`schemaVersion 必须是 1（当前 ${state.schemaVersion}）。`);
  if (!Array.isArray(state.modules)) errors.push('modules 必须是数组。');
  const ids = new Set();
  const entries = allComponents(state);
  for (const { module, component } of entries) {
    if (ids.has(component.id)) errors.push(`组件 ID 重复：${component.id}`);
    ids.add(component.id);
    if (!COMPONENT_STATUSES.has(component.status)) {
      errors.push(`${component.id} 状态非法：${component.status}`);
    }
    for (const dep of component.dependsOn || []) {
      if (!entries.some(({ component: candidate }) => candidate.id === dep)) {
        errors.push(`${component.id} 依赖不存在的组件 ${dep}`);
      }
    }
    if (!Array.isArray(component.source) || component.source.length === 0) {
      errors.push(`${component.id} 缺少 source。`);
    }
    if (!Array.isArray(component.states)) {
      errors.push(`${component.id} states 必须是数组。`);
    }
    if (!Number.isFinite(component.priority)) {
      errors.push(`${component.id} priority 必须是数字。`);
    }
    if (component.status === 'done') {
      const blocking = state.findings.filter(
        (finding) =>
          finding.componentId === component.id &&
          finding.status === 'open' &&
          (finding.severity === 'P0' || finding.severity === 'P1'),
      );
      if (blocking.length > 0) {
        errors.push(`${component.id} 标为 done 但仍有开放 P0/P1：${blocking.map((f) => f.id).join(',')}`);
      }
    }
  }
  for (const finding of state.findings || []) {
    if (!ids.has(finding.componentId)) errors.push(`${finding.id} 指向不存在的组件 ${finding.componentId}`);
    if (!SEVERITIES.has(finding.severity)) errors.push(`${finding.id} severity 非法`);
    if (!ROOT_CAUSES.has(finding.rootCause)) errors.push(`${finding.id} rootCause 非法`);
  }
  // 依赖环
  const visiting = new Set();
  const visited = new Set();
  function visit(id) {
    if (visited.has(id)) return;
    if (visiting.has(id)) {
      errors.push(`依赖成环：${id}`);
      return;
    }
    visiting.add(id);
    const component = entries.find((entry) => entry.component.id === id)?.component;
    for (const dep of component?.dependsOn || []) visit(dep);
    visiting.delete(id);
    visited.add(id);
  }
  for (const id of ids) visit(id);

  if (state.current) {
    const currentEntry = findEntry(state, state.current.componentId);
    if (!currentEntry) errors.push(`current 指向不存在的组件 ${state.current.componentId}`);
    else if (currentEntry.component.status !== 'in_progress') {
      errors.push(`current 组件 ${state.current.componentId} 状态是 ${currentEntry.component.status}，应为 in_progress`);
    }
  }

  if (errors.length > 0) {
    console.error(`[ux-audit] state 校验失败（${errors.length} 项）：`);
    for (const error of errors) console.error(` - ${error}`);
    process.exit(1);
  }
  console.log(`[ux-audit] state 校验通过：${entries.length} 个组件、${state.findings.length} 条 finding、${state.runCount} 次运行。`);
}

function status() {
  const state = readState();
  const entries = allComponents(state);
  const counts = { pending: 0, in_progress: 0, done: 0, blocked: 0 };
  for (const { component } of entries) counts[component.status] += 1;
  const open = state.findings.filter((finding) => finding.status === 'open');
  const bySeverity = { P0: 0, P1: 0, P2: 0, P3: 0 };
  for (const finding of open) bySeverity[finding.severity] += 1;
  const next = selectNext(state);
  console.log(`分支: ${currentBranch()} | 运行 ${state.runCount} 次 | 组件 ${entries.length}`);
  console.log(`进度: pending ${counts.pending} · in_progress ${counts.in_progress} · done ${counts.done} · blocked ${counts.blocked}`);
  console.log(`开放 finding: P0 ${bySeverity.P0} · P1 ${bySeverity.P1} · P2 ${bySeverity.P2} · P3 ${bySeverity.P3}`);
  if (state.current) console.log(`当前: ${state.current.componentId} (${state.current.runId})`);
  if (next) console.log(`下一条: ${next.component.id} · ${next.component.name} (priority ${next.component.priority})`);
  else console.log('下一条: 无（进入回归巡检）');
  if (state.lastRuns?.[0]) {
    const last = state.lastRuns[0];
    console.log(`上次: ${last.runId} · ${last.componentId} · ${last.status} · ${last.summary}`);
  }
}

function listFindings(options) {
  const state = readState();
  let findings = state.findings;
  if (options.component) findings = findings.filter((finding) => finding.componentId === options.component);
  if (options.open || options['open-only']) findings = findings.filter((finding) => finding.status === 'open');
  console.log(JSON.stringify(findings, null, 2));
}

function usage() {
  console.log(`ux-audit state machine

用法:
  node automation/ux-audit/scripts/state.mjs status
  node automation/ux-audit/scripts/state.mjs validate
  node automation/ux-audit/scripts/state.mjs sync
  node automation/ux-audit/scripts/state.mjs begin [--component M3-C3]
  node automation/ux-audit/scripts/state.mjs add-finding --component ID --severity P1 --root-cause token --title "..." --detail "..." [--evidence "..."]
  node automation/ux-audit/scripts/state.mjs close-finding --id F-001 --fix "..." [--guard "..."|--status wontfix]
  node automation/ux-audit/scripts/state.mjs complete --component ID --status done|blocked --summary "..." [--gates "..."] [--shots "..."] [--blocked-reason "..."]
  node automation/ux-audit/scripts/state.mjs abort --reason "..." [--blocked]
  node automation/ux-audit/scripts/state.mjs list-findings [--component ID] [--open]
`);
}

const [command, ...rest] = process.argv.slice(2);
const options = {};
for (let i = 0; i < rest.length; i += 1) {
  const arg = rest[i];
  if (!arg.startsWith('--')) continue;
  const eq = arg.indexOf('=');
  if (eq > 0) {
    options[arg.slice(2, eq)] = arg.slice(eq + 1);
  } else {
    const key = arg.slice(2);
    const next = rest[i + 1];
    if (next !== undefined && !next.startsWith('--')) {
      options[key] = next;
      i += 1;
    } else {
      options[key] = true;
    }
  }
}

switch (command) {
  case 'status':
    status();
    break;
  case 'validate':
    validateState();
    break;
  case 'sync':
    sync();
    break;
  case 'begin':
    begin(options);
    break;
  case 'add-finding':
  case 'addFinding':
    addFinding(options);
    break;
  case 'close-finding':
  case 'closeFinding':
    closeFinding(options);
    break;
  case 'complete':
    complete(options);
    break;
  case 'abort':
    abort(options);
    break;
  case 'list-findings':
  case 'listFindings':
    listFindings(options);
    break;
  case 'help':
  case undefined:
    usage();
    break;
  default:
    fail(`未知命令：${command}\n`);
}

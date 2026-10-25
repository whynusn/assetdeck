#!/usr/bin/env node
// theme-system 主题预览采集：把「某主题 × 某状态」渲染成确定性 PNG。
//
// 用法：
//   node automation/theme-system/scripts/preview.mjs --list
//   node automation/theme-system/scripts/preview.mjs --check
//   node automation/theme-system/scripts/preview.mjs --theme builtin-dark --state shell-grid
//   node automation/theme-system/scripts/preview.mjs --component T0-C2 --phase before
//   node automation/theme-system/scripts/preview.mjs --component T1-C2 --theme sakura-cat --phase after
//   node automation/theme-system/scripts/preview.mjs --all --theme builtin-light
//
// 主题来源（二选一，按优先级）：
//   1) --tokens <file.json>：已解析令牌快照（Rust 校验器可 dump；这是主题系统建成后的正路）。
//   2) --theme <name>：读 automation/theme-system/themes.json 的 themes[name] 段
//      （harness 自举期用：声明式令牌夹具，与 Rust 侧 Provider 同值）。
// 二者都缺省为 builtin-dark。
//
// 状态夹具复用 automation/ux-audit/states/fixtures.json（单一事实来源）：
// 同一套 UI 状态在两条自动化线上不会漂移；ux-audit 改名时本工具的 --check 会当场报错。
//
// 输出：target/theme-system/shots/<component|_all|_states>/<phase>/<theme>--<state>.png
// 成败判定：slint-viewer 非零退出 / 输出缺失或 0 字节 / 出现 Warning:|Error: → 失败。

import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(SCRIPT_DIR, '../../..');
const FIXTURES_PATH = path.join(ROOT, 'automation/ux-audit/states/fixtures.json');
const THEMES_JSON_PATH = path.join(SCRIPT_DIR, '../themes.json');
const STATE_PATH = path.join(SCRIPT_DIR, '../state.json');
const THEMES_DIR = path.join(ROOT, 'themes');
const SHOTS_DIR = path.join(ROOT, 'target/theme-system/shots');
const LOAD_DATA_DIR = path.join(ROOT, 'target/theme-system/load-data');
const GEN_DIR = path.join(ROOT, 'target/theme-system/gen');
const DEFAULT_FILE = 'crates/app-ui/ui/appwindow.slint';
const STYLE = 'fluent-dark';

function fail(msg) {
  console.error(`preview: ${msg}`);
  process.exit(1);
}

function readJson(file) {
  try {
    return JSON.parse(fs.readFileSync(file, 'utf8'));
  } catch (e) {
    fail(`读取 ${path.relative(ROOT, file)} 失败: ${e.message}`);
  }
}

const fixtures = readJson(FIXTURES_PATH);
const themeDecls = fs.existsSync(THEMES_JSON_PATH) ? readJson(THEMES_JSON_PATH) : { version: 1, themes: {} };
const state = readJson(STATE_PATH);

function parseArgs(argv) {
  const out = { components: [], states: [], phases: [], themes: [], tokens: null, all: false, list: false, check: false, dryRun: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const val = () => {
      const v = argv[++i];
      if (!v) fail(`${a} 缺少取值`);
      return v;
    };
    if (a === '--component') out.components.push(val());
    else if (a === '--state') out.states.push(val());
    else if (a === '--phase') out.phases.push(val());
    else if (a === '--theme') out.themes.push(val());
    else if (a === '--tokens') out.tokens = val();
    else if (a === '--all') out.all = true;
    else if (a === '--list') out.list = true;
    else if (a === '--check') out.check = true;
    else if (a === '--dry-run') out.dryRun = true;
    else if (a === '--help' || a === '-h') {
      console.log(fs.readFileSync(fileURLToPath(import.meta.url), 'utf8').split('\n').slice(1, 22).join('\n'));
      process.exit(0);
    } else fail(`未知参数 ${a}`);
  }
  return out;
}

const args = parseArgs(process.argv.slice(2));

// ---------- 主题解析 ----------
const declaredThemes = themeDecls.themes || {};

function resolveThemeGlobals(name) {
  if (args.tokens) {
    const snap = readJson(path.resolve(ROOT, args.tokens));
    // 快照形态：{ "globals": { "Theme.accent": "#..." , ... } } 或直接是键值对象
    return snap.globals && typeof snap.globals === 'object' ? snap.globals : snap;
  }
  const t = declaredThemes[name];
  if (!t) {
    if (name === 'builtin-dark') return { 'Palette.color-scheme': 'dark' };
    if (name === 'builtin-light') return { 'Palette.color-scheme': 'light', ...(declaredThemes['light']?.globals || {}) };
    const avail = ['builtin-dark', 'builtin-light', ...Object.keys(declaredThemes)].join(', ');
    fail(`未知主题 "${name}"（themes.json 里没有，且未提供 --tokens）。可用：${avail}`);
  }
  return t.globals || {};
}

// ---------- 状态解析 ----------
function resolveState(name, seen = new Set()) {
  if (seen.has(name)) fail(`fixture 继承环: ${[...seen, name].join(' -> ')}`);
  seen.add(name);
  const raw = fixtures.states[name];
  if (!raw) fail(`未知 fixture "${name}"`);
  const base = raw.extends ? resolveState(raw.extends, seen) : { props: {}, globals: {}, file: null };
  return {
    name,
    file: raw.file ?? base.file,
    window: raw.window ?? base.window ?? null,
    props: { ...base.props, ...(raw.props ?? {}) },
    globals: { ...base.globals, ...(raw.globals ?? {}) },
  };
}

const allNames = Object.keys(fixtures.states);

function themeNames() {
  return args.themes.length ? args.themes : ['builtin-dark'];
}

if (args.list) {
  console.log('# states');
  for (const n of allNames) {
    const s = resolveState(n);
    console.log(`  ${n.padEnd(26)}${s.file ? `(${s.file})` : ''}${s.window ? `  ${s.window.width}x${s.window.height}` : ''}`);
  }
  console.log('# themes (automation/theme-system/themes.json)');
  console.log(`  ${'builtin-dark'.padEnd(26)}(implicit: slint 默认暗色令牌)`);
  console.log(`  ${'builtin-light'.padEnd(26)}(Palette.color-scheme=light + themes.light 覆盖)`);
  for (const [n, t] of Object.entries(declaredThemes)) {
    const keys = Object.keys(t.globals || {}).length;
    console.log(`  ${n.padEnd(26)}${keys} 个全局令牌${t.base ? `  base=${t.base}` : ''}`);
  }
  process.exit(0);
}

const componentStates = new Map();
for (const mod of state.modules) {
  for (const c of mod.components) componentStates.set(c.id, c.states ?? []);
}

if (args.check) {
  const problems = [];
  // 方向一：theme-system 组件引用的状态必须真实存在。
  // 不反向要求「每个夹具都被引用」——那 42 个状态是所有 UI 模块的并集，本线只消费其中一部分。
  for (const [id, states] of componentStates) {
    for (const name of states) {
      if (!allNames.includes(name)) problems.push(`组件 ${id} 引用了不存在的 fixture ${name}`);
    }
  }
  // 方向二：被引用的状态必须可渲染（文件在、window 尺寸合法）。
  for (const name of allNames) {
    const s = resolveState(name);
    if (s.window && !(s.window.width > 0 && s.window.height > 0)) problems.push(`fixture ${name} 的 window 尺寸非法`);
    if (!s.window) {
      const file = path.join(ROOT, s.file ?? DEFAULT_FILE);
      if (!fs.existsSync(file)) problems.push(`fixture ${name} 的文件不存在: ${s.file}`);
    }
  }
  // 方向三：主题声明（themes.json）自检——色值形态合法、base 枚举合法。
  for (const [name, t] of Object.entries(declaredThemes)) {
    if (t.base && !['dark', 'light'].includes(t.base)) problems.push(`主题 ${name} 的 base=${t.base} 非法（只能 dark|light）`);
    for (const [key, value] of Object.entries(t.globals || {})) {
      if (key === 'Palette.color-scheme') {
        if (!['dark', 'light'].includes(value)) problems.push(`主题 ${name} 的 ${key}=${value} 非法`);
        continue;
      }
      if (!key.startsWith('Theme.')) problems.push(`主题 ${name} 的令牌键 ${key} 必须以 Theme. 开头`);
      if (!/^#[0-9a-fA-F]{3,8}$/.test(String(value))) problems.push(`主题 ${name} 的 ${key} 色值形态非法: ${value}`);
    }
  }
  // 方向四：主题包目录自检——有 assets 必须有 THIRD_PARTY.md（与 license-audit 同一口径）。
  if (fs.existsSync(THEMES_DIR)) {
    for (const dir of fs.readdirSync(THEMES_DIR).sort()) {
      if (dir.startsWith('_')) continue; // 骨架/草稿不是可加载主题包
      const packDir = path.join(THEMES_DIR, dir);
      if (!fs.statSync(packDir).isDirectory()) continue;
      const assets = path.join(packDir, 'assets');
      if (fs.existsSync(assets) && !fs.existsSync(path.join(packDir, 'THIRD_PARTY.md'))) {
        problems.push(`主题包 ${dir} 含 assets/ 但缺 THIRD_PARTY.md（许可门禁）`);
      }
    }
  }
  if (problems.length) {
    console.error(problems.map((p) => `  ✗ ${p}`).join('\n'));
    fail(`${problems.length} 个问题`);
  }
  console.log(`preview OK：${allNames.length} 个可用状态、${Object.keys(declaredThemes).length} 个声明主题，引用与令牌形态齐全`);
  process.exit(0);
}

function deepReplace(value, replacements) {
  if (typeof value === 'string') {
    let out = value;
    for (const [k, v] of Object.entries(replacements)) out = out.split(`{{${k}}}`).join(v);
    return out;
  }
  if (Array.isArray(value)) return value.map((v) => deepReplace(v, replacements));
  if (value && typeof value === 'object') {
    const o = {};
    for (const [k, v] of Object.entries(value)) o[k] = deepReplace(v, replacements);
    return o;
  }
  return value;
}

// 窗口尺寸经生成临时副本注入（同 ux-audit：Window 的 preferred-* 对解释器只读）。
function generateSizedWindow(baseFile, width, height) {
  const basePath = path.join(ROOT, baseFile);
  let src = fs.readFileSync(basePath, 'utf8');
  const abs = (rel) => path.resolve(path.dirname(basePath), rel).replace(/\\/g, '/');
  src = src
    .replace(/(from\s+)"(theme\.slint)"/g, (m, head, rel) => `${head}"${abs(rel)}"`)
    .replace(/@image-url\("([^"]+)"\)/g, (m, rel) => `@image-url("${abs(rel)}")`)
    .replace(/preferred-width:\s*[0-9.]+px;/, `preferred-width: ${width}px;`)
    .replace(/preferred-height:\s*[0-9.]+px;/, `preferred-height: ${height}px;`);
  if (!src.includes(`preferred-width: ${width}px;`) || !src.includes(`preferred-height: ${height}px;`)) {
    fail(`生成 ${width}x${height} 预览失败：${baseFile} 里找不到 preferred-width/height 声明`);
  }
  fs.mkdirSync(GEN_DIR, { recursive: true });
  const out = path.join(GEN_DIR, `appwindow-${width}x${height}.slint`);
  fs.writeFileSync(out, src);
  return out;
}

function sourceFileFor(s) {
  const base = s.file ?? DEFAULT_FILE;
  if (!s.window) return path.join(ROOT, base);
  return generateSizedWindow(base, s.window.width, s.window.height);
}

function buildLoadData(s, themeGlobals) {
  const merged = { ...(fixtures.defaults ?? {}), ...s.props, ...s.globals, ...themeGlobals };
  return deepReplace(merged, { ROOT: ROOT.replace(/\\/g, '/') });
}

function phaseFor(phases) {
  return phases.length ? phases : ['before'];
}

function previewOne(s, themeName, themeGlobals, outDir) {
  const png = path.join(outDir, `${themeName}--${s.name}.png`);
  const loadJsonPath = path.join(LOAD_DATA_DIR, `${path.basename(outDir)}--${themeName}--${s.name}.json`);
  const file = sourceFileFor(s);
  const loadData = buildLoadData(s, themeGlobals);

  fs.mkdirSync(outDir, { recursive: true });
  fs.mkdirSync(LOAD_DATA_DIR, { recursive: true });
  fs.writeFileSync(loadJsonPath, JSON.stringify(loadData, null, 2));
  if (fs.existsSync(png)) fs.rmSync(png);

  if (args.dryRun) {
    return { theme: themeName, state: s.name, png: path.relative(ROOT, png), skipped: true };
  }

  const res = spawnSync(
    'slint-viewer',
    ['--style', STYLE, '--load-data', loadJsonPath, '--screenshot', png, file],
    { cwd: ROOT, encoding: 'utf8', timeout: 180_000, windowsHide: true },
  );

  const output = `${res.stdout ?? ''}\n${res.stderr ?? ''}`.trim();
  const badLine = output.split(/\r?\n/).find((line) => /^(Warning|Error):/i.test(line.trim()));
  if (res.error) return { theme: themeName, state: s.name, png: path.relative(ROOT, png), error: `无法启动 slint-viewer: ${res.error.message}`, output };
  if (res.status !== 0) return { theme: themeName, state: s.name, png: path.relative(ROOT, png), error: `slint-viewer 退出码 ${res.status}`, output };
  if (badLine) return { theme: themeName, state: s.name, png: path.relative(ROOT, png), error: badLine.trim(), output };
  if (!fs.existsSync(png) || fs.statSync(png).size === 0) return { theme: themeName, state: s.name, png: path.relative(ROOT, png), error: '截图未生成或为空文件', output };
  return { theme: themeName, state: s.name, png: path.relative(ROOT, png), bytes: fs.statSync(png).size };
}

// ---------- 组装任务 ----------
const jobs = [];
const themes = themeNames();
if (args.all) {
  for (const phase of phaseFor(args.phases)) {
    jobs.push({ outDir: path.join(SHOTS_DIR, '_all', phase), states: allNames.map((n) => resolveState(n)) });
  }
}
for (const component of args.components) {
  const declared = componentStates.get(component);
  if (!declared) fail(`未知组件 ${component}`);
  const picked = args.states.length ? args.states.map((n) => resolveState(n)) : declared.map((n) => resolveState(n));
  if (picked.length === 0) fail(`组件 ${component} 的 states 为空，且未用 --state 指定`);
  const missing = args.states.filter((n) => !allNames.includes(n));
  if (missing.length) fail(`未知 fixture: ${missing.join(', ')}`);
  for (const phase of phaseFor(args.phases)) {
    jobs.push({ outDir: path.join(SHOTS_DIR, component, phase), states: picked });
  }
}
if (!args.all && args.states.length && args.components.length === 0) {
  for (const phase of phaseFor(args.phases)) {
    jobs.push({ outDir: path.join(SHOTS_DIR, '_states', phase), states: args.states.map((n) => resolveState(n)) });
  }
}
if (jobs.length === 0) fail('没有采集任务：用 --component / --state / --all 指定');

// 同一 theme 只解析一次全局令牌
const themeGlobalsCache = new Map();
for (const t of themes) themeGlobalsCache.set(t, resolveThemeGlobals(t));

const results = [];
for (const job of jobs) {
  for (const t of themes) {
    for (const s of job.states) results.push(previewOne(s, t, themeGlobalsCache.get(t), job.outDir));
  }
}

const failed = results.filter((r) => r.error);
for (const r of results) {
  if (r.error) console.error(`✗ ${r.theme} / ${r.state} -> ${r.png}\n  ${r.error}${r.output ? `\n  ${r.output.replace(/\n/g, '\n  ')}` : ''}`);
  else console.log(`✓ ${r.theme} / ${r.state} -> ${r.png}${r.skipped ? ' (dry-run)' : ` (${Math.round(r.bytes / 1024)} KB)`}`);
}
console.log(JSON.stringify({ ok: results.length - failed.length, failed: failed.length, shots: results.filter((r) => !r.error).map((r) => r.png) }, null, 2));
if (failed.length) process.exit(1);

#!/usr/bin/env node
// ux-audit 截图采集：把 fixtures.json 的状态渲染成确定性 PNG。
//
// 用法：
//   node automation/ux-audit/scripts/capture.mjs --list
//   node automation/ux-audit/scripts/capture.mjs --check
//   node automation/ux-audit/scripts/capture.mjs --component M3-C3 --phase before
//   node automation/ux-audit/scripts/capture.mjs --component M3-C3 --state shell-grid --state tile-kinds --phase after
//   node automation/ux-audit/scripts/capture.mjs --all --phase before
//
// 输出：target/ux-audit/shots/<component|_all|_states>/<phase>/<state>.png
// load-data 中间产物：target/ux-audit/load-data/<state>.json（便于复现/排查）
//
// 成败判定：slint-viewer 非零退出、输出文件缺失或为 0 字节、stdout/stderr
// 出现 Warning:/Error: —— 三者任一即失败（fixture 拼错属性名必须当场暴露，
// 不能带着 half-broken 截图继续审查）。

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(SCRIPT_DIR, "../../..");
const FIXTURES_PATH = path.join(SCRIPT_DIR, "../states/fixtures.json");
const STATE_PATH = path.join(SCRIPT_DIR, "../state.json");
const SHOTS_DIR = path.join(ROOT, "target/ux-audit/shots");
const LOAD_DATA_DIR = path.join(ROOT, "target/ux-audit/load-data");
const DEFAULT_FILE = "crates/app-ui/ui/appwindow.slint";
const STYLE = "fluent-dark";
const GEN_DIR = path.join(ROOT, "target/ux-audit/gen");

function fail(msg) {
  console.error(`capture: ${msg}`);
  process.exit(1);
}

function readJson(file) {
  try {
    return JSON.parse(fs.readFileSync(file, "utf8"));
  } catch (e) {
    fail(`读取 ${path.relative(ROOT, file)} 失败: ${e.message}`);
  }
}

const fixtures = readJson(FIXTURES_PATH);
const state = readJson(STATE_PATH);

function parseArgs(argv) {
  const out = { components: [], states: [], phases: [], all: false, list: false, check: false, dryRun: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const val = () => {
      const v = argv[++i];
      if (!v) fail(`${a} 缺少取值`);
      return v;
    };
    if (a === "--component") out.components.push(val());
    else if (a === "--state") out.states.push(val());
    else if (a === "--phase") out.phases.push(val());
    else if (a === "--all") out.all = true;
    else if (a === "--list") out.list = true;
    else if (a === "--check") out.check = true;
    else if (a === "--dry-run") out.dryRun = true;
    else if (a === "--help" || a === "-h") {
      console.log(fs.readFileSync(fileURLToPath(import.meta.url), "utf8").split("\n").slice(1, 16).join("\n"));
      process.exit(0);
    } else fail(`未知参数 ${a}`);
  }
  return out;
}

const args = parseArgs(process.argv.slice(2));

function resolveState(name, seen = new Set()) {
  if (seen.has(name)) fail(`fixture 继承环: ${[...seen, name].join(" -> ")}`);
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

if (args.list) {
  for (const n of allNames) {
    const s = resolveState(n);
    console.log(`${n.padEnd(26)}${s.file ? `  (${s.file})` : ""}${s.window ? `  ${s.window.width}x${s.window.height}` : ""}`);
  }
  process.exit(0);
}

const componentStates = new Map();
for (const mod of state.modules) {
  for (const c of mod.components) componentStates.set(c.id, c.states ?? []);
}

if (args.check) {
  const problems = [];
  const used = new Set();
  for (const [id, states] of componentStates) {
    for (const name of states) {
      if (!allNames.includes(name)) problems.push(`组件 ${id} 引用了不存在的 fixture ${name}`);
      else used.add(name);
    }
  }
  for (const n of allNames) {
    if (!used.has(n)) problems.push(`fixture ${n} 没有被任何组件的 states 引用（--all 之外的采集永远不会跑它）`);
    const s = resolveState(n);
    if (s.window && !(s.window.width > 0 && s.window.height > 0)) {
      problems.push(`fixture ${n} 的 window 尺寸非法`);
    }
    if (!s.window) {
      const file = path.join(ROOT, s.file ?? DEFAULT_FILE);
      if (!fs.existsSync(file)) problems.push(`fixture ${n} 的文件不存在: ${s.file}`);
    }
  }
  if (problems.length) {
    console.error(problems.map((p) => `  ✗ ${p}`).join("\n"));
    fail(`${problems.length} 个 fixture 问题`);
  }
  console.log(`fixtures OK：${allNames.length} 个状态，组件 states 引用齐全（单一事实来源 = state.json）`);
  process.exit(0);
}

function deepReplace(value, replacements) {
  if (typeof value === "string") {
    let out = value;
    for (const [k, v] of Object.entries(replacements)) out = out.split(`{{${k}}}`).join(v);
    return out;
  }
  if (Array.isArray(value)) return value.map((v) => deepReplace(v, replacements));
  if (value && typeof value === "object") {
    const o = {};
    for (const [k, v] of Object.entries(value)) o[k] = deepReplace(v, replacements);
    return o;
  }
  return value;
}

// 窗口尺寸不能经 load-data 注入（Window 的 preferred-* 对解释器只读），
// 而继承 AppWindow 的包装组件又拿不到继承属性的写入权限。正解：从真实
// appwindow.slint 生成一份仅改窗口尺寸的临时副本（每次采集重新生成，
// 不存第二份手写 UI 源码）；相对 import / image-url 改写成绝对路径。
function generateSizedWindow(baseFile, width, height) {
  const basePath = path.join(ROOT, baseFile);
  let src = fs.readFileSync(basePath, "utf8");
  const abs = (rel) => path.resolve(path.dirname(basePath), rel).replace(/\\/g, "/");
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

function buildLoadData(s) {
  const merged = { ...(fixtures.defaults ?? {}), ...s.props, ...s.globals };
  return deepReplace(merged, { ROOT: ROOT.replace(/\\/g, "/") });
}

function phaseFor(phases) {
  return phases.length ? phases : ["before"];
}

function captureOne(s, outDir) {
  const slug = path.basename(outDir);
  const png = path.join(outDir, `${s.name}.png`);
  const loadJsonPath = path.join(LOAD_DATA_DIR, `${slug}--${s.name}.json`);
  const file = sourceFileFor(s);
  const loadData = buildLoadData(s);

  fs.mkdirSync(outDir, { recursive: true });
  fs.mkdirSync(LOAD_DATA_DIR, { recursive: true });
  fs.writeFileSync(loadJsonPath, JSON.stringify(loadData, null, 2));
  if (fs.existsSync(png)) fs.rmSync(png);

  if (args.dryRun) {
    return { state: s.name, png: path.relative(ROOT, png), command: `slint-viewer --load-data ${loadJsonPath} --screenshot ${png} ${file}`, skipped: true };
  }

  const res = spawnSync(
    "slint-viewer",
    ["--style", STYLE, "--load-data", loadJsonPath, "--screenshot", png, file],
    { cwd: ROOT, encoding: "utf8", timeout: 180_000, windowsHide: true },
  );

  const output = `${res.stdout ?? ""}\n${res.stderr ?? ""}`.trim();
  const badLine = output.split(/\r?\n/).find((line) => /^(Warning|Error):/i.test(line.trim()));
  if (res.error) return { state: s.name, png: path.relative(ROOT, png), error: `无法启动 slint-viewer: ${res.error.message}`, output };
  if (res.status !== 0) return { state: s.name, png: path.relative(ROOT, png), error: `slint-viewer 退出码 ${res.status}`, output };
  if (badLine) return { state: s.name, png: path.relative(ROOT, png), error: badLine.trim(), output };
  if (!fs.existsSync(png) || fs.statSync(png).size === 0) return { state: s.name, png: path.relative(ROOT, png), error: "截图未生成或为空文件", output };
  return { state: s.name, png: path.relative(ROOT, png), bytes: fs.statSync(png).size, output };
}

// 组装采集任务：{ outDir, states[] }
const jobs = [];
if (args.all) {
  for (const phase of phaseFor(args.phases)) {
    jobs.push({ outDir: path.join(SHOTS_DIR, "_all", phase), states: allNames.map((n) => resolveState(n)) });
  }
}
for (const component of args.components) {
  const declared = componentStates.get(component);
  if (!declared) fail(`未知组件 ${component}`);
  const picked = args.states.length ? args.states.map((n) => resolveState(n)) : declared.map((n) => resolveState(n));
  if (picked.length === 0) fail(`组件 ${component} 的 states 为空，且未用 --state 指定`);
  const missing = args.states.filter((n) => !allNames.includes(n));
  if (missing.length) fail(`未知 fixture: ${missing.join(", ")}`);
  for (const phase of phaseFor(args.phases)) {
    jobs.push({ outDir: path.join(SHOTS_DIR, component, phase), states: picked });
  }
}
if (!args.all && args.states.length && args.components.length === 0) {
  for (const phase of phaseFor(args.phases)) {
    jobs.push({ outDir: path.join(SHOTS_DIR, "_states", phase), states: args.states.map((n) => resolveState(n)) });
  }
}
if (jobs.length === 0) fail("没有采集任务：用 --component / --state / --all 指定");

const results = [];
for (const job of jobs) for (const s of job.states) results.push(captureOne(s, job.outDir));

const failed = results.filter((r) => r.error);
for (const r of results) {
  if (r.error) console.error(`✗ ${r.state} -> ${r.png}\n  ${r.error}${r.output ? `\n  ${r.output.replace(/\n/g, "\n  ")}` : ""}`);
  else console.log(`✓ ${r.state} -> ${r.png}${r.skipped ? " (dry-run)" : ` (${Math.round(r.bytes / 1024)} KB)`}`);
}
console.log(JSON.stringify({ ok: results.length - failed.length, failed: failed.length, shots: results.filter((r) => !r.error).map((r) => r.png) }, null, 2));
if (failed.length) process.exit(1);

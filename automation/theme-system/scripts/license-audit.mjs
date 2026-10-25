#!/usr/bin/env node
// theme-system 素材许可与体积门禁（对外发布的前提，theme-spec §7）。
//
// 用法：
//   node automation/theme-system/scripts/license-audit.mjs            # 审计 themes/ 全部包
//   node automation/theme-system/scripts/license-audit.mjs --pack sakura-cat
//   node automation/theme-system/scripts/license-audit.mjs --json     # 机读输出
//
// 规则：
//   R1 含 assets/ 的包必须有 THIRD_PARTY.md。
//   R2 THIRD_PARTY.md 必须含许可表格，且每个素材文件名都在表里出现。
//   R3 许可证必须在白名单内（CC0-1.0 / Public Domain / OFL-1.1 / MIT / Apache-2.0 / CC-BY-4.0）。
//      CC-BY-4.0 允许但要计数（需署名）；CC-BY-NC* 一律拒绝（不可商用）。
//   R4 单图 ≤ 512KB、包内 assets 合计 ≤ 4MB。
//   R5 包名/目录/描述不得出现商业 IP 关键词（含常见动漫游戏角色与商标）。
//   R6 assets 下的每个文件都必须在 THIRD_PARTY.md 中出现（无来源不明素材）。
//
// 退出码：0 全过；1 有违规。

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(SCRIPT_DIR, '../../..');
const THEMES_DIR = path.join(ROOT, 'themes');

const ALLOWED = new Set(['CC0-1.0', 'PUBLIC DOMAIN', 'PUBLIC-DOMAIN', 'OFL-1.1', 'MIT', 'APACHE-2.0', 'CC-BY-4.0']);
const FORBIDDEN = ['CC-BY-NC', 'CC-BY-ND', 'NONCOMMERCIAL', 'NON-COMMERCIAL'];
// 商业 IP / 商标关键词：命中即拒绝。风格可以近似，形象与名称不得复刻。
const IP_DENYLIST = [
  'hello kitty', 'hellokitty', 'hello-kitty', 'sanrio', 'kitty white',
  'disney', 'mickey', 'minnie', 'winnie', 'elsa', 'pikachu', 'pokemon',
  'pokémon', 'nintendo', 'mario', 'kirby', 'totoro', 'ghibli', 'snoopy',
  'peanuts', 'line friends', 'brown bear', 'rira', 'mymelody', 'melody',
  'kuromi', 'cinnamoroll', 'pompompurin', 'gudetama', 'pochacco',
  '宝可梦', '皮卡丘', '迪士尼', '米奇', '三丽鸥', '凯蒂猫',
];
const IMG_EXT = new Set(['.png', '.jpg', '.jpeg', '.webp', '.avif', '.gif', '.svg', '.bmp']);
const MAX_FILE = 512 * 1024;
const MAX_TOTAL = 4 * 1024 * 1024;

function fail(msg) {
  console.error(`license-audit: ${msg}`);
  process.exit(1);
}

const args = process.argv.slice(2);
const jsonMode = args.includes('--json');
const packArg = args.indexOf('--pack') >= 0 ? args[args.indexOf('--pack') + 1] : null;

function listPacks() {
  if (!fs.existsSync(THEMES_DIR)) return [];
  return fs
    .readdirSync(THEMES_DIR)
    .sort()
    // `_` 前缀 = 骨架/草稿（如 _template），不是可加载的主题包，跳过。
    .filter((d) => !d.startsWith('_'))
    .filter((d) => fs.statSync(path.join(THEMES_DIR, d)).isDirectory())
    .filter((d) => !packArg || d === packArg);
}

function walk(dir) {
  const out = [];
  if (!fs.existsSync(dir)) return out;
  for (const name of fs.readdirSync(dir).sort()) {
    const p = path.join(dir, name);
    const st = fs.statSync(p);
    if (st.isDirectory()) out.push(...walk(p));
    else out.push(p);
  }
  return out;
}

const violations = [];
const warnings = [];
const packs = [];

for (const pack of listPacks()) {
  const packDir = path.join(THEMES_DIR, pack);
  const tomlPath = path.join(packDir, 'theme.toml');
  if (!fs.existsSync(tomlPath)) {
    violations.push(`[${pack}] 缺 theme.toml`);
    continue;
  }
  const toml = fs.readFileSync(tomlPath, 'utf8');
  const id = (toml.match(/^\s*id\s*=\s*"([^"]*)"/m) || [])[1] || '';
  const name = (toml.match(/^\s*name\s*=\s*"([^"]*)"/m) || [])[1] || '';
  const desc = (toml.match(/^\s*description\s*=\s*"([^"]*)"/m) || [])[1] || '';

  // R5：IP 关键词
  const haystack = `${pack} ${id} ${name} ${desc}`.toLowerCase();
  for (const kw of IP_DENYLIST) {
    if (haystack.includes(kw)) violations.push(`[${pack}] R5 包名/描述命中商业 IP 关键词「${kw}」——必须用原创形象与命名`);
  }

  const assetsDir = path.join(packDir, 'assets');
  const files = walk(assetsDir);
  const images = files.filter((f) => IMG_EXT.has(path.extname(f).toLowerCase()));

  if (files.length === 0) {
    packs.push({ pack, id, assets: 0, bytes: 0, licence: 'n/a' });
    continue;
  }

  // R1/R2：THIRD_PARTY.md
  const tpPath = path.join(packDir, 'THIRD_PARTY.md');
  if (!fs.existsSync(tpPath)) {
    violations.push(`[${pack}] R1 含 assets/ 但缺 THIRD_PARTY.md`);
    packs.push({ pack, id, assets: files.length, bytes: 0, licence: 'missing' });
    continue;
  }
  const tp = fs.readFileSync(tpPath, 'utf8');
  const tpLower = tp.toLowerCase();

  // R3：许可证白名单
  const licencesSeen = new Set();
  for (const raw of tp.matchAll(/\b(CC0-1\.0|OFL-1\.1|MIT|Apache-2\.0|CC-BY-4\.0|CC-BY-NC[^\s|]*|CC-BY-ND[^\s|]*|Public Domain)\b/gi)) {
    licencesSeen.add(raw[1]);
  }
  if (licencesSeen.size === 0) violations.push(`[${pack}] R3 THIRD_PARTY.md 里找不到可识别的许可证标识`);
  for (const l of licencesSeen) {
    const upper = l.toUpperCase();
    if (FORBIDDEN.some((f) => upper.includes(f))) violations.push(`[${pack}] R3 许可证「${l}」不可商用，禁止随包分发`);
    else if (!ALLOWED.has(upper)) violations.push(`[${pack}] R3 许可证「${l}」不在白名单内`);
    else if (upper === 'CC-BY-4.0') warnings.push(`[${pack}] CC-BY-4.0 素材需保留署名（确认 THIRD_PARTY.md 已列作者）`);
  }

  // R4：体积
  let total = 0;
  for (const f of files) {
    const size = fs.statSync(f).size;
    total += size;
    if (IMG_EXT.has(path.extname(f).toLowerCase()) && size > MAX_FILE) {
      violations.push(`[${pack}] R4 素材 ${path.relative(packDir, f)} 超单图上限 ${(MAX_FILE / 1024).toFixed(0)}KB（实际 ${(size / 1024).toFixed(0)}KB）`);
    }
  }
  if (total > MAX_TOTAL) {
    violations.push(`[${pack}] R4 包内素材合计 ${(total / 1024 / 1024).toFixed(2)}MB 超上限 ${(MAX_TOTAL / 1024 / 1024).toFixed(0)}MB`);
  }

  // R6：每个素材都要有来源记录
  for (const f of files) {
    const rel = path.relative(packDir, f).replace(/\\/g, '/');
    const base = path.basename(f);
    if (!tp.includes(rel) && !tp.includes(base)) {
      violations.push(`[${pack}] R6 素材 ${rel} 未出现在 THIRD_PARTY.md（来源不明不得随包分发）`);
    }
  }

  packs.push({
    pack,
    id,
    assets: files.length,
    images: images.length,
    bytes: total,
    licence: [...licencesSeen].join('|') || 'unparsed',
  });
}

if (jsonMode) {
  console.log(JSON.stringify({ ok: violations.length === 0, packs, violations, warnings }, null, 2));
} else {
  for (const p of packs) {
    const mb = (p.bytes / 1024 / 1024).toFixed(2);
    console.log(`· ${p.pack} (id=${p.id}) 素材 ${p.assets} 个 / ${mb}MB / 许可 ${p.licence}`);
  }
  for (const w of warnings) console.log(`! ${w}`);
  for (const v of violations) console.error(`✗ ${v}`);
  console.log(violations.length === 0 ? `license-audit OK：${packs.length} 个主题包` : `license-audit 失败：${violations.length} 项违规`);
}

if (violations.length) process.exit(1);

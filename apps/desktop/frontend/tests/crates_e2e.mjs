// End-to-end test of the crate boxes: every function laid out inside a box
// for its crate, the boxes in rows by call order (callers above callees,
// external code last), and a toolbar toggle that hides them.
//
// It runs against a multi-crate repository — this one:
//
//   lcw-dev ui /path/to/livewalk --no-open &
//   NODE_PATH=$(npm root -g) node tests/crates_e2e.mjs http://127.0.0.1:8765/ /tmp/shots
//
// Every step is an assertion; exits non-zero on the first failure or on any
// console error.
import { createRequire } from 'module';
const require = createRequire(import.meta.url);
const { chromium } = require('playwright');
const fs = require('fs');
const path = require('path');

const base = process.argv[2] || 'http://127.0.0.1:8765/';
const out = process.argv[3] || '.';
fs.mkdirSync(out, { recursive: true });

const log = (...a) => console.log(new Date().toISOString().slice(11, 23), ...a);
let passed = 0;
function check(cond, what, detail = '') {
  if (!cond) throw new Error(`FAIL: ${what}${detail ? ` — ${detail}` : ''}`);
  passed++;
  log('ok  ', what);
}

// The boxes and a digest of the positions, computed in the page so the (large)
// view never crosses into the test process whole.
const geometry = (page) => page.evaluate(async () => {
  const view = await (await fetch('fixture.json')).json();
  const groups = view.groups || [];
  const inside = (p, b) => p[0] >= b.min[0] && p[0] <= b.max[0] && p[1] >= b.min[1] && p[1] <= b.max[1];
  const homeless = view.positions.filter((p) => !groups.some((b) => inside(p, b))).length;
  const overlaps = [];
  groups.forEach((a, i) => groups.slice(i + 1).forEach((b) => {
    const apart = a.max[0] <= b.min[0] || b.max[0] <= a.min[0] || a.max[1] <= b.min[1] || b.max[1] <= a.min[1];
    if (!apart) overlaps.push(`${a.name}/${b.name}`);
  }));
  return { groups, nodes: view.positions.length, homeless, overlaps };
});

const canvasShot = (page) => page.locator('canvas.graph').screenshot();

(async () => {
  const browser = await chromium.launch({
    headless: true,
    args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist', '--disable-features=WebGPU'],
  });
  const page = await browser.newPage({ viewport: { width: 1500, height: 920 } });
  const errors = [];
  page.on('console', (m) => { if (m.type() === 'error') errors.push(m.text()); });
  page.on('pageerror', (e) => errors.push(`pageerror: ${e.message}`));
  let failure = null;

  try {
    await page.goto(base, { waitUntil: 'load' });
    await page.waitForSelector('.explorer .row.fn', { timeout: 120000 });
    check(true, 'the analysis loads by itself');

    // 1. The layout is grouped: one box per crate, every node in a box.
    const g = await geometry(page);
    const names = g.groups.map((b) => b.name);
    log(`${g.groups.length} boxes for ${g.nodes} nodes:`, names.join(', '));
    check(g.groups.length > 3, 'the view carries one box per crate', `${g.groups.length} boxes`);
    check(['lcw_cli', 'lcw_core', 'lcw_query'].every((n) => names.includes(n)), 'the workspace crates each have a box');
    check(g.homeless === 0, 'every node lies inside a box', `${g.homeless} outside`);
    check(g.overlaps.length === 0, 'boxes never overlap', g.overlaps.join(', '));
    const members = g.groups.reduce((s, b) => s + b.members, 0);
    check(members === g.nodes, 'box sizes add up to the node count', `${members} vs ${g.nodes}`);

    // 2. The rows follow the call order.
    const box = (n) => g.groups.find((b) => b.name === n);
    const below = (lower, upper) => box(lower).max[1] < box(upper).min[1];
    check(below('lcw_engine', 'lcw_cli'), 'lcw_engine sits below the CLI that calls it');
    check(below('lcw_query', 'lcw_cli'), 'lcw_query sits below the CLI that calls it');
    check(below('lcw_core', 'lcw_engine'), 'lcw_core sits below the engine that calls it');
    const crates = g.groups.filter((b) => !b.external);
    const lowest = Math.min(...crates.map((b) => b.max[1]));
    check(box('lcw_core').max[1] === lowest, 'lcw_core, which calls no other crate, is in the lowest row');
    const ext = g.groups[g.groups.length - 1];
    check(ext.external && ext.name === 'external', 'external code has its own box, listed last');
    check(g.groups.every((b) => b === ext || ext.max[1] < b.min[1]), 'the external box is below every crate');

    // 3. The boxes are drawn, and the toggle hides and restores them.
    await page.click('button:has-text("Fit")');
    await page.waitForTimeout(400);
    const toggle = page.locator('label.mode', { hasText: 'crates' }).locator('input');
    check(await toggle.isChecked(), 'the crates toggle starts on');
    const overlayNames = () => page.$$eval('.group-label', (l) => l.map((x) => x.textContent));
    const shown = await overlayNames();
    log('crate names on screen:', shown.length, shown.slice(0, 6).join(' | '));
    check(shown.some((t) => t.startsWith('external · ')), 'crate names are drawn over the boxes', shown.join(' | '));
    check(shown.some((t) => /^lcw_core · \d+ fn$/.test(t)), 'a name carries its function count', shown.join(' | '));
    await page.screenshot({ path: path.join(out, 'crates.png') });
    const on = await canvasShot(page);
    await toggle.uncheck();
    await page.waitForTimeout(400);
    const off = await canvasShot(page);
    await page.screenshot({ path: path.join(out, 'crates-off.png') });
    check(!on.equals(off), 'turning the toggle off changes the picture');
    check((await overlayNames()).length === 0, 'and removes the crate names');
    await toggle.check();
    await page.waitForTimeout(400);
    const again = await canvasShot(page);
    check(!again.equals(off), 'turning it back on draws the boxes again');
    check((await overlayNames()).length === shown.length, 'with their names');

    // 4. Picking and navigation are unchanged: a function picked in the
    //    Outline is selected and centered.
    await page.click('.tabs .tab:has-text("Outline")');
    await page.fill('.outline-tools .search', 'crate_groups');
    await page.waitForTimeout(300);
    await page.locator('.card.outline .tree .row.fn', { hasText: 'crate_groups' }).first().click();
    await page.waitForSelector('.detail .qname');
    const q = (await page.textContent('.detail .qname')).trim();
    check(q.endsWith('crate_groups'), 'an Outline click still selects the function', q);
    await page.screenshot({ path: path.join(out, 'crates-selected.png') });

    check(errors.length === 0, 'no console errors', errors.join(' | '));
  } catch (e) {
    failure = e;
    await page.screenshot({ path: path.join(out, 'crates-failure.png') }).catch(() => {});
  }
  await browser.close();
  if (failure) {
    console.error(`\n${failure.message}\n(${passed} assertions passed before it)`);
    process.exit(1);
  }
  console.log(`\nall ${passed} assertions passed`);
})();

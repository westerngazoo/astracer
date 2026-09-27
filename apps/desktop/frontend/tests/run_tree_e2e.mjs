// End-to-end test of the Explorer's run tree: a tree rooted at every place a
// thread of control begins — the program entry, exported entry points and
// spawned threads — that opens down what each one calls.
//
// It runs against the threaded fixture next to it, whose graph is known
// exactly:
//
//   lcw-dev ui apps/desktop/frontend/tests/fixtures/threads --no-open &
//   NODE_PATH=$(npm root -g) node tests/run_tree_e2e.mjs http://127.0.0.1:8765/ /tmp/shots
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
const eq = (a, b) => JSON.stringify(a) === JSON.stringify(b);

// Every visible run-tree row: depth from its indent, label, markers.
const rows = (page) => page.$$eval('.run-tree .row', (rs) => rs.map((r) => ({
  depth: Math.round((parseFloat(r.style.paddingLeft) - 6) / 14),
  label: r.querySelector('.label').textContent.trim(),
  badge: r.querySelector('.badge')?.textContent.trim() || null,
  spawned: !!r.querySelector('.via'),
  recursive: !!r.querySelector('.recur'),
  twisty: r.querySelector('.twisty').textContent.trim(),
})));
const clickRow = (page, label, nth = 0) =>
  page.locator('.run-tree .row', { has: page.locator('.label', { hasText: label }) }).nth(nth).click();

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
    await page.waitForSelector('.run-tree .row', { timeout: 120000 });
    check(true, 'the run tree loads by itself');
    check((await page.textContent('.tabs .tab.active')).trim() === 'Run tree', 'the run tree is the default view');

    // 1. Roots: the program entry first, then its threads, largest first.
    let r = await rows(page);
    const roots = r.filter((x) => x.depth === 0).map((x) => [x.label, x.badge]);
    log('roots:', JSON.stringify(roots));
    check(eq(roots, [
      ['threads::main', 'start'],
      ['threads::main::<spawned@L10>', 'thread'],
      ['threads::listener', 'thread'],
    ]), 'roots are main, then each spawned thread', JSON.stringify(roots));

    // 2. main is open one level, in source order, spawns marked.
    const underMain = r.slice(1, r.findIndex((x, i) => i > 0 && x.depth === 0));
    log('under main:', JSON.stringify(underMain.map((x) => [x.label, x.spawned])));
    check(eq(underMain.map((x) => [x.label, x.spawned]), [
      ['setup', false], ['<spawned@L10>', true], ['listener', true], ['report', false],
    ]), 'main opens to what it does, in source order, with spawns marked ⇉');
    check(underMain.every((x) => x.depth === 1), 'children sit one level down');

    // 3. Walk down the worker thread by clicking: each click opens the row.
    await clickRow(page, 'threads::main::<spawned@L10>');
    await clickRow(page, 'worker');
    await clickRow(page, 'crunch');
    r = await rows(page);
    const threadStart = r.findIndex((x) => x.label === 'threads::main::<spawned@L10>');
    const walk = r.slice(threadStart, threadStart + 4).map((x) => [x.depth, x.label, x.recursive]);
    log('worker thread:', JSON.stringify(walk));
    check(eq(walk, [[0, 'threads::main::<spawned@L10>', false], [1, 'worker', false], [2, 'crunch', false], [3, 'crunch', true]]),
      'clicking walks the thread: closure -> worker -> crunch -> crunch (recursive)');
    check(r[threadStart + 3].twisty === '', 'a recursive call is shown but cannot be opened');
    check((await page.textContent('.detail .qname')).trim() === 'threads::crunch', 'clicking a row also selects it');

    // 4. The twisty closes a row without selecting anything else.
    const before = (await rows(page)).length;
    await page.locator('.run-tree .row', { has: page.locator('.label', { hasText: 'threads::main::<spawned@L10>' }) })
      .first().locator('.twisty').click();
    const after = (await rows(page)).length;
    check(after === before - 3, 'the twisty collapses a subtree', `${before} -> ${after}`);

    // 5. A flow crosses a spawn: from main to accept, which only listener's
    //    thread calls.
    await page.click('button:has-text("Entry")');
    await page.waitForSelector('.detail .qname');
    await page.click('button:has-text("trace from here")');
    await clickRow(page, 'threads::listener');
    await clickRow(page, 'accept');
    await page.waitForSelector('.flow .chain');
    const chain = (await page.textContent('.flow .chain')).replace(/\s+/g, ' ');
    log('flow:', chain);
    check(/main → listener → accept$/.test(chain.trim()), 'a flow traces across a spawn edge', chain);
    await page.screenshot({ path: path.join(out, 'run-tree.png') });

    // 6. The other views are still one click away.
    await page.click('.tabs .tab:has-text("Outline")');
    check((await page.$$('.card.outline .tree .row')).length > 0, 'the Outline tab shows the outline');
    await page.click('.tabs .tab:has-text("Entries")');
    check((await page.$$('.card.entries .row.fn')).length > 0, 'the Entries tab shows entry points');

    check(errors.length === 0, 'no console errors', errors.join(' | '));
  } catch (e) {
    failure = e;
    await page.screenshot({ path: path.join(out, 'run-tree-failure.png') }).catch(() => {});
  }
  await browser.close();
  if (failure) {
    console.error(`\n${failure.message}\n(${passed} assertions passed before it)`);
    process.exit(1);
  }
  console.log(`\nall ${passed} assertions passed`);
})();

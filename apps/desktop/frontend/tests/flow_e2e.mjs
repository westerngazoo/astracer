// End-to-end test of flow tracing in the browser Explorer.
//
// Unlike `ui_smoke.mjs`, which walks the interface and reports what it sees,
// every step here is an assertion. It pins the behaviour behind a real bug
// report — "flow from here does not work" — in the real bundle, in a real
// browser, driven the way a person drives it:
//
//   * the analysis loads by itself (no typing `fixture.json`);
//   * source -> downstream target: a chain, in call order;
//   * source -> upstream target: still a chain, flagged as reaching the source
//     (this showed nothing before);
//   * source -> unrelated node: an explicit "no call path" message (this also
//     showed nothing before);
//   * a click with a trackpad's jitter between press and release selects the
//     node under it (this read as a drag before);
//   * a click on empty canvas keeps the flow source (this discarded it).
//
// It expects *this repository* as the analyzed target, because two steps
// name functions in it:
//
//   lcw-dev ui /path/to/livewalk --no-open &
//   NODE_PATH=$(npm root -g) node tests/flow_e2e.mjs http://127.0.0.1:8765/ /tmp/shots
//
// Exits non-zero on the first failed assertion, and on any console error.
// `.mjs` is an ES module, where `require` does not exist; `createRequire`
// restores it (and with it NODE_PATH, which a global Playwright install needs).
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
  if (!cond) {
    throw new Error(`FAIL: ${what}${detail ? ` — ${detail}` : ''}`);
  }
  passed++;
  log('ok  ', what);
}
const text = async (page, sel) => ((await page.textContent(sel)) || '').replace(/\s+/g, ' ').trim();
const lastSegment = (q) => q.split('::').pop();

(async () => {
  const browser = await chromium.launch({
    headless: true,
    args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist', '--disable-features=WebGPU'],
  });
  const page = await browser.newPage({ viewport: { width: 1500, height: 920 } });
  const consoleErrors = [];
  page.on('console', (m) => { if (m.type() === 'error') consoleErrors.push(m.text()); });
  page.on('pageerror', (e) => consoleErrors.push(`pageerror: ${e.message}`));
  let failure = null;

  try {
    await page.goto(base, { waitUntil: 'load' });

    // 1. Nothing typed, nothing clicked: the Explorer fills in by itself.
    await page.waitForSelector('.explorer .row.fn', { timeout: 120000 });
    check(true, 'analysis loads by itself in browser dev mode');
    await page.click('.tabs .tab:has-text("Outline")');
    check((await page.$$('.card.outline .tree .row')).length > 0, 'outline tree is populated');

    // 2. Downstream: source = the entry point, target = one of its callees.
    await page.click('button:has-text("Entry")');
    await page.waitForSelector('.detail .qname');
    const entry = await text(page, '.detail .qname');
    log('entry point:', entry);
    await page.click('button:has-text("trace from here")');
    check((await text(page, '.flow')).includes('now pick a target'), 'source set, asks for a target');

    const outputs = await page.$$('.detail ul.calls');
    const callee = await outputs[1].$('li.call:not(.ext)');
    check(callee !== null, 'entry point has an in-repo callee');
    const calleeName = await callee.$eval('.name', (n) => n.textContent.trim());
    await callee.click();
    await page.waitForSelector('.flow .chain');
    const down = await text(page, '.flow .chain');
    log('downstream chain:', down);
    check(/\d+ hop\(s\)/.test(down) && !down.includes('reaches the source'), 'downstream target traces a chain');
    check(down.includes(lastSegment(entry)) && down.endsWith(lastSegment(calleeName)),
      'downstream chain runs source -> target', down);

    // 3. Upstream: source = that callee, target = the entry point (its caller).
    const selected = await text(page, '.detail .qname');
    await page.click('button:has-text("trace from here")');
    const inputs = await page.$$('.detail ul.calls');
    let caller = null;
    for (const li of await inputs[0].$$('li.call')) {
      const n = await li.$eval('.name', (x) => x.textContent.trim());
      if (entry.endsWith(n) || n.endsWith(lastSegment(entry))) { caller = li; break; }
    }
    check(caller !== null, `the entry point is listed among ${lastSegment(selected)}'s callers`);
    await caller.click();
    await page.waitForSelector('.flow .chain');
    const up = await text(page, '.flow .chain');
    log('upstream chain:', up);
    check(up.includes('reaches the source'), 'upstream target still traces, flagged as reaching the source', up);
    check(up.includes(lastSegment(entry)) && up.endsWith(lastSegment(selected)),
      'upstream chain is in call order (caller first)', up);

    // 4. Unconnected: source = the CLI entry, target = a frontend function it
    //    can never reach and that never reaches it.
    await page.click('button:has-text("Entry")');
    await page.waitForSelector('.detail .qname');
    await page.click('button:has-text("trace from here")');
    await page.click('.tabs .tab:has-text("Outline")');
    await page.fill('.outline-tools .search', 'select_node');
    await page.waitForTimeout(300);
    await page.click('.card.outline .tree .row.fn');
    await page.waitForSelector('.flow .hint.warn');
    const none = await text(page, '.flow .hint.warn');
    log('unconnected:', none);
    check(none.includes('no call path between') && none.includes('either direction'),
      'an unconnected target says so instead of showing nothing', none);
    await page.fill('.outline-tools .search', '');
    await page.waitForTimeout(200);
    await page.screenshot({ path: path.join(out, 'flow-unconnected.png') });

    // 5. A click with trackpad jitter (2px, 1px between press and release) on
    //    a labelled node selects it. Labels sit at node centres, in canvas px.
    const box = await page.$eval('canvas.graph', (c) => { const r = c.getBoundingClientRect(); return { x: r.x, y: r.y }; });
    const label = await page.$$eval('.labels .node-label', (ls) => {
      const l = ls.find((x) => x.textContent.trim().length > 2);
      return l ? { name: l.textContent.trim(), x: parseFloat(l.style.left), y: parseFloat(l.style.top) } : null;
    });
    check(label !== null, 'a labelled node is on screen to click');
    const [px, py] = [box.x + label.x, box.y + label.y];
    await page.mouse.move(px, py);
    await page.mouse.down();
    await page.mouse.move(px + 2, py + 1);
    await page.mouse.up();
    await page.waitForTimeout(300);
    const picked = await text(page, '.detail .qname');
    log(`jitter click on "${label.name}" ->`, picked);
    check(picked.endsWith(label.name), 'a click with trackpad jitter selects the node under it', `${picked} vs ${label.name}`);

    // 6. With a source set, a click on empty canvas keeps it. Zoom far out so
    //    the corner is guaranteed empty, then click there.
    await page.click('button:has-text("trace from here")');
    const beforeMiss = await text(page, '.detail .qname');
    await page.mouse.move(box.x + 700, box.y + 400);
    for (let i = 0; i < 30; i++) await page.mouse.wheel(0, 200);
    await page.waitForTimeout(300);
    await page.mouse.click(box.x + 20, box.y + 20);
    await page.waitForTimeout(300);
    check((await page.$('.detail .qname')) !== null && (await text(page, '.detail .qname')) === beforeMiss,
      'a background click keeps the selection while a source is set');
    check((await text(page, '.flow')).includes('now pick a target'), 'a background click keeps the flow source');
    await page.screenshot({ path: path.join(out, 'flow-after-miss.png') });

    // 7. Without a source, a background click still clears the selection.
    await page.click('.flow button:has-text("clear")');
    await page.mouse.click(box.x + 20, box.y + 20);
    await page.waitForTimeout(300);
    check((await page.$('.detail .qname')) === null, 'without a source, a background click clears the selection');

    check(consoleErrors.length === 0, 'no console errors', consoleErrors.join(' | '));
  } catch (e) {
    failure = e;
    await page.screenshot({ path: path.join(out, 'flow-failure.png') }).catch(() => {});
  }
  await browser.close();
  if (failure) {
    console.error(`\n${failure.message}\n(${passed} assertions passed before it)`);
    process.exit(1);
  }
  console.log(`\nall ${passed} assertions passed`);
})();

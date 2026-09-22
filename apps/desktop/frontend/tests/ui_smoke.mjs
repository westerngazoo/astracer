// Browser smoke test for the Live Code Walk frontend.
//
// The UI is wasm + a GPU canvas, so unit tests cannot tell whether it actually
// *works*: this drives the real bundle in a real browser and reports what it
// finds. It runs against browser dev mode (`transport::FixtureTransport`), so
// no Tauri shell, engine or GPU hardware is needed.
//
//   cargo build -p lcw-cli --features viewer
//   ./target/debug/lcw analyze . --format view -o /tmp/fixture.json
//   cd apps/desktop/frontend && trunk build --release
//   cp /tmp/fixture.json dist/fixture.json
//   (cd dist && python3 -m http.server 8765) &
//   npm i -g playwright && node tests/ui_smoke.mjs http://127.0.0.1:8765/ /tmp/shots
//
// It is deliberately not wired into CI: that would add a Node/Playwright
// dependency to a pure-Rust workspace. The `wasm` CI job type-checks the same
// code; this is the manual "does it behave" pass.
//
// Exits non-zero when a step fails, and prints every console error, page error
// and UI problem it detected. Screenshots land in <outdir>.
const { chromium } = require('playwright');
const fs = require('fs');
const path = require('path');

const base = process.argv[2] || 'http://127.0.0.1:8765/';
const out = process.argv[3] || '.';
fs.mkdirSync(out, { recursive: true });

const log = (...a) => console.log(new Date().toISOString().slice(11, 23), ...a);
const shot = async (page, name) => {
  const file = path.join(out, `${name}.png`);
  await page.screenshot({ path: file, fullPage: false });
  log('screenshot', file);
};

(async () => {
  const browser = await chromium.launch({
    headless: true,
    args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist'].concat(process.env.WEBGPU ? ['--enable-unsafe-webgpu'] : ['--disable-features=WebGPU']),
  });
  const page = await browser.newPage({ viewport: { width: 1500, height: 920 } });
  const problems = [];
  page.on('console', (m) => {
    const type = m.type();
    const text = m.text();
    if (type === 'error' || type === 'warning') problems.push(`[console.${type}] ${text}`);
    if (type === 'error') log('console.error:', text.slice(0, 300));
  });
  page.on('pageerror', (e) => { problems.push(`[pageerror] ${e.message}`); log('pageerror:', e.message.slice(0, 500)); });

  try {
  const t0 = Date.now();
  await page.goto(base, { waitUntil: 'load' });
  await page.waitForSelector('.toolbar', { timeout: 60000 });
  log('page loaded in', Date.now() - t0, 'ms; placeholder =', await page.getAttribute('.toolbar .path', 'placeholder'));
  await shot(page, '01-idle');

  await page.fill('.toolbar .path', 'fixture.json');
  const t1 = Date.now();
  await page.click('button.primary');
  await page.waitForSelector('.explorer .row.fn', { timeout: 120000 });
  log('analysis presented in', Date.now() - t1, 'ms');
  await page.waitForTimeout(800);
  log('status:', await page.textContent('.status'));
  const err = await page.$('.side .error');
  if (err) log('ERROR BOX:', await err.textContent());
  log('first main row:', (await page.textContent('.explorer .card:first-child .row.fn')).replace(/\s+/g, ' '));
  log('entries rows:', await page.$$eval('.explorer .card:first-child .row.fn', (r) => r.length));
  log('outline rows visible:', await page.$$eval('.tree .row', (r) => r.length));
  log('overview:', (await page.textContent('.side')).replace(/\s+/g, ' ').slice(0, 200));
  await shot(page, '02-analyzed');

  // Jump to main.
  await page.click('button:has-text("Main")');
  await page.waitForSelector('.detail .qname', { timeout: 10000 });
  log('selected:', await page.textContent('.detail .qname'));
  log('meta:', await page.textContent('.detail .meta'));
  log('inputs/outputs:', await page.$$eval('.detail ul.calls', (u) => u.map((x) => x.children.length)));
  log('call tree rows:', await page.$$eval('.detail ul.calltree .tree-row', (r) => r.length));
  await shot(page, '03-main-selected');

  // Navigate to the first callee via the outputs list.
  const outputs = await page.$$('.detail ul.calls');
  const firstCallee = outputs.length > 1 ? await outputs[1].$('li.call') : null;
  if (firstCallee) {
    const name = await firstCallee.$eval('.name', (n) => n.textContent);
    await firstCallee.click();
    await page.waitForTimeout(300);
    log('after clicking callee', name, '-> selected:', await page.textContent('.detail .qname'));
    log('back enabled:', !(await page.$eval('.detail .actions button', (b) => b.disabled)));
  }
  await shot(page, '04-callee-selected');

  // Flow: trace from here, then pick a node from the call tree.
  await page.click('button:has-text("trace from here")');
  await page.waitForTimeout(200);
  log('flow hint:', (await page.textContent('.flow')).replace(/\s+/g, ' ').slice(0, 160));
  const treeRow = await page.$('.detail ul.calltree ul.calltree .tree-row');
  if (treeRow) {
    await treeRow.click();
    await page.waitForTimeout(300);
    log('flow after target:', (await page.textContent('.flow')).replace(/\s+/g, ' ').slice(0, 200));
  } else {
    log('no nested call tree row to pick as flow target');
  }
  await shot(page, '05-flow');

  // Back.
  await page.click('.detail .actions button:first-child');
  await page.waitForTimeout(200);
  log('after back:', await page.textContent('.detail .qname'));

  // Outline: toggle a scope, then filter.
  {
    const before = await page.$$eval('.tree .row', (r) => r.length);
    const label = await page.$eval('.tree .row.scope .label', (l) => l.textContent);
    await page.click('.tree .row.scope', { force: true });
    await page.waitForTimeout(250);
    const after = await page.$$eval('.tree .row', (r) => r.length);
    log(`toggling scope "${label}": rows ${before} -> ${after}`);
    if (before === after) problems.push(`[ui] toggling scope "${label}" did not change the row count (${before})`);
  }
  await page.fill('.outline-tools .search', 'resolve');
  await page.waitForTimeout(300);
  const filtered = await page.$$eval('.tree .row.fn .label', (l) => l.map((x) => x.textContent));
  log('filter "resolve" ->', filtered.length, 'functions:', filtered.slice(0, 8).join(', '));
  await shot(page, '06-outline-filter');
  const fnRow = await page.$('.tree .row.fn');
  if (fnRow) {
    await fnRow.click();
    await page.waitForTimeout(300);
    log('clicked outline row -> selected:', await page.textContent('.detail .qname'));
  }
  await page.fill('.outline-tools .search', '');
  await page.waitForTimeout(200);

  // Canvas: pixels actually drawn? (a WebGL/WebGPU context that never renders
  // leaves the canvas transparent, which the dark page background hides)
  const canvasInk = await page.evaluate(() => {
    const c = document.querySelector('canvas.graph');
    if (!c) return 'no canvas';
    const w = c.width, h = c.height;
    const tmp = document.createElement('canvas');
    tmp.width = w; tmp.height = h;
    const ctx = tmp.getContext('2d');
    ctx.drawImage(c, 0, 0);
    const d = ctx.getImageData(0, 0, w, h).data;
    let lit = 0, opaque = 0;
    for (let i = 0; i < d.length; i += 4) {
      if (d[i + 3] > 8) opaque++;
      if (d[i] + d[i + 1] + d[i + 2] > 120) lit++;
    }
    const px = w * h;
    return { size: `${w}x${h}`, opaquePct: +(100 * opaque / px).toFixed(1), litPct: +(100 * lit / px).toFixed(2) };
  });
  log('canvas ink:', JSON.stringify(canvasInk));
  if (canvasInk && canvasInk.opaquePct === 0) problems.push('[ui] graph canvas is completely transparent: nothing was rendered');

  // Canvas: click background to clear, then search highlight.
  await page.click('canvas.graph', { position: { x: 30, y: 30 } });
  await page.waitForTimeout(200);
  log('detail after background click:', (await page.$('.detail')) ? 'still shown' : 'cleared');
  await page.fill('.toolbar .search', 'parse');
  await page.waitForTimeout(300);
  await shot(page, '07-search');

  log('labels drawn:', await page.$$eval('.node-label', (l) => l.length));
  } catch (e) {
    console.error('SMOKE FAILED:', e.message.split('\n')[0]);
    process.exitCode = 1;
  } finally {
    log('PROBLEMS:', problems.length);
    for (const p of problems.slice(0, 30)) console.log('  ' + p.slice(0, 600));
    await browser.close();
  }
})();

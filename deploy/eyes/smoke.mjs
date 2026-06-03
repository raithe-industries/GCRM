// ─────────────────────────────────────────────────────────────────────────────
// GCRM dashboard "eyes" — liveness / invariant checks. NOT snapshots.
//
// Philosophy: a FLOOR, not a CAGE. We assert only things true of *any* good
// design — the page runs without crashing, the primary graph is actually
// rendered and legible (not collapsed/squished), the situational-awareness
// panels exist and are visible. We say NOTHING about colours, copy, layout
// shape, or "does it look like the old one". The self-improve agent stays free
// to redesign boldly; it just can't ship something broken.
//
// Exit 0 = dashboard is alive and legible enough to trust.
// Exit 1 = broken (with a precise reason). The wrapper rolls back on exit 1.
//
// Usage:  node smoke.mjs [url]
//   url defaults to $EYES_URL or http://127.0.0.1:8000/risk
//   $EYES_MIN_GRAPH_H overrides the legibility floor (px) for the timeline graph.
// ─────────────────────────────────────────────────────────────────────────────
import { chromium } from 'playwright';

const URL = process.argv[2] || process.env.EYES_URL || 'http://127.0.0.1:8000/risk';
const MIN_GRAPH_H = Number(process.env.EYES_MIN_GRAPH_H || 48); // legibility floor, px

const fail = [];
const ok = (m) => console.log('  ✓ ' + m);

const browser = await chromium.launch({ headless: true, args: ['--no-sandbox'] });
const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });

// Collect any uncaught JS exceptions and console.error output during load+render.
const jsErrors = [];
page.on('pageerror', (e) => jsErrors.push(String(e)));
page.on('console', (m) => { if (m.type() === 'error') jsErrors.push('console.error: ' + m.text()); });

try {
  await page.goto(URL, { waitUntil: 'networkidle', timeout: 20000 });
} catch (e) {
  console.error(`EYES FAIL: could not load ${URL} — ${e.message}`);
  await browser.close();
  process.exit(1);
}
// Let the dashboard JS fetch data and draw the charts.
await page.waitForTimeout(2500);

const box = async (sel) => { const el = await page.$(sel); return el ? el.boundingBox() : null; };

// 1) No uncaught JS errors — a crashed dashboard is the worst kind of broken.
if (jsErrors.length) fail.push('JS errors on load:\n      ' + jsErrors.slice(0, 6).join('\n      '));
else ok('no JS errors on load/render');

// 2) Primary P(WWIII) timeline graph: present and NOT collapsed/squished.
//    (This is the exact regression the theater-ladder strip caused.)
const tl = await box('#timeline-chart');
if (!tl) fail.push('#timeline-chart canvas missing or not visible');
else if (tl.height < MIN_GRAPH_H) fail.push(`#timeline-chart squished: ${Math.round(tl.height)}px tall (legibility floor ${MIN_GRAPH_H}px)`);
else ok(`timeline graph rendered ${Math.round(tl.width)}×${Math.round(tl.height)}px`);

// 3) Secondary domain-breakdown chart: present and not collapsed.
const dm = await box('#domain-chart');
if (!dm) fail.push('#domain-chart canvas missing or not visible');
else if (dm.height < 24) fail.push(`#domain-chart collapsed: ${Math.round(dm.height)}px tall`);
else ok(`domain chart rendered ${Math.round(dm.width)}×${Math.round(dm.height)}px`);

// 4) Core situational-awareness panels exist and occupy real space (no layout
//    opinion — just "is it there and visible").
for (const sel of ['#theater-ladder', '#gauge-canvas', '.charts-area']) {
  const b = await box(sel);
  if (!b || b.width < 2 || b.height < 2) fail.push(`${sel} missing or collapsed`);
  else ok(`${sel} present ${Math.round(b.width)}×${Math.round(b.height)}px`);
}

// 5) SEMANTIC liveness — the headline must not be saturated at its engineering ceiling.
//    A P(WWIII) pinned at the 0.90 cap has lost all resolution and meaning (the June-2026
//    flatline). This is the honesty/legibility floor in DATA form: it says nothing about
//    what the number should be, only that a credible read must not be pegged at the cap.
const CEILING = 0.90, MARGIN = 0.01;
try {
  const apiLatest = URL.replace(/\/+$/, '') + '/api/latest';
  const j = await (await fetch(apiLatest)).json();
  const pa = j?.probabilities?.annual;
  if (typeof pa !== 'number') fail.push('api/latest has no probabilities.annual');
  else if (pa >= CEILING - MARGIN) fail.push(`annual P(WWIII) saturated at ceiling: ${pa} — non-credible / no resolution`);
  else ok(`annual P(WWIII) = ${(pa * 100).toFixed(1)}% (not pegged at ceiling)`);
} catch (e) {
  fail.push(`semantic check could not reach api/latest: ${e.message}`);
}

await browser.close();

if (fail.length) {
  console.error(`\nEYES FAIL (${fail.length}) @ ${URL}:`);
  for (const f of fail) console.error('  ✗ ' + f);
  process.exit(1);
}
console.log(`\nEYES OK @ ${URL} — dashboard alive, graph legible, panels intact.`);
process.exit(0);

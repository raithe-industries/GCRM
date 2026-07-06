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
page.on('pageerror', (e) => jsErrors.push(String(e))); // uncaught JS exceptions — always fatal
page.on('console', (m) => {
  if (m.type() !== 'error') return;
  const t = m.text();
  // A transient sub-resource / feed fetch 4xx-5xx (a map layer briefly down, an aborted
  // request, a missing favicon) logs console.error but is NOT a code regression — it must
  // not block the deploy and trigger a rollback. Filter that network/resource noise; only a
  // real script-level console.error survives. Genuine crashes still come through 'pageerror'
  // above and remain fatal. (audit ops-1)
  if (/Failed to load resource|net::ERR|ERR_[A-Z_]+|status of (4|5)\d\d|favicon/i.test(t)) return;
  jsErrors.push('console.error: ' + t);
});

try {
  // Wait for `domcontentloaded`, NOT `networkidle`. The dashboard holds a live WebSocket
  // open and polls feeds, so the network NEVER goes idle — `networkidle` only happened to
  // settle within 20s on a WARM start, and timed out on every COLD restart (the cold
  // /api/map fan-out keeps the network busy past the threshold). That false timeout rolled
  // back this deploy and the 2026-06-16 one alike, before any real check ran.
  // The cache-warm (OSINT map fan-out + the growing feed roster) can keep a freshly-restarted
  // gcrm busy enough that the FIRST load exceeds the budget, even though the server is healthy.
  // That is a slow cold start, not a broken deploy — so give it ONE retry with a longer ceiling
  // before rolling back. (This intermittent 30s page.goto timeout is what flapped the deploy.)
  try {
    await page.goto(URL, { waitUntil: 'domcontentloaded', timeout: 45000 });
  } catch (e1) {
    console.error(`EYES: first load slow (${e1.message}) — retrying once with a longer ceiling…`);
    await page.goto(URL, { waitUntil: 'domcontentloaded', timeout: 60000 });
  }
} catch (e) {
  console.error(`EYES FAIL: could not load ${URL} after retry — ${e.message}`);
  await browser.close();
  process.exit(1);
}
// Wait for the charts/panels to actually RENDER, bounded per element — robust to a heavy
// cold-start render (prod's full dataset draws slower than an empty warmup), where a fixed
// sleep checks too early. waitForSelector returns as soon as the element is visible and
// never hangs (the per-element timeout is the ceiling); a genuinely missing panel still
// fails, precisely, in the assertions below.
await Promise.all(
  ['#timeline-chart', '#domain-chart', '#theater-ladder', '#gauge-canvas'].map((sel) =>
    page.waitForSelector(sel, { state: 'visible', timeout: 12000 }).catch(() => {})),
);
await page.waitForTimeout(1200); // brief settle for canvas draw after the elements attach

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
for (const sel of ['#theater-ladder', '#gauge-canvas', '.compute', '.charts-area']) {
  const b = await box(sel);
  if (!b || b.width < 2 || b.height < 2) fail.push(`${sel} missing or collapsed`);
  else ok(`${sel} present ${Math.round(b.width)}×${Math.round(b.height)}px`);
}

// 5) SEMANTIC liveness — the headline must not be saturated at its engineering ceiling.
//    A P(WWIII) pinned at the 0.90 cap has lost all resolution and meaning (the June-2026
//    flatline). This is the honesty/legibility floor in DATA form: it says nothing about
//    what the number should be, only that a credible read must not be pegged at the cap.
const CEILING = 0.90, MARGIN = 0.01;
let latest = null;
const apiLatest = URL.replace(/\/+$/, '') + '/api/latest';
// Readiness poll: right after a restart the new process needs one compute cycle
// before api/latest carries a snapshot (until then it's {} or the socket is briefly
// refused while the old process finishes draining). Wait for a real snapshot, THEN
// assert — a warmup window can't false-fail the gate, but a truly unreachable/empty
// endpoint still does (latest stays null → fail below).
for (let i = 0; i < 20; i++) {            // ≈ 20 × 800ms ≈ 16s readiness budget
  try {
    // Bounded fetch: without a timeout a half-open connection during the restart window
    // would hang this await indefinitely, jamming the eyes gate (and, with no timeout on the
    // node run itself, the whole flock-held deploy). (audit ops-2)
    const r = await fetch(apiLatest, { signal: AbortSignal.timeout(4000) });
    if (r.ok) {
      const j = await r.json();
      if (typeof j?.probabilities?.annual === 'number') { latest = j; break; }
    }
  } catch { /* connection refused during the restart window — retry */ }
  await new Promise((res) => setTimeout(res, 800));
}
if (!latest) {
  fail.push('api/latest never returned a snapshot with probabilities.annual within readiness budget');
} else {
  const pa = latest.probabilities.annual;
  // Deliberate fail-safe: an honestly-capped read ≥ 0.89 (a genuine ceiling-grade world crisis) also
  // hard-fails here and blocks ALL deploys until a human looks — intended behavior, not a bug.
  if (pa >= CEILING - MARGIN) fail.push(`annual P(WWIII) saturated at ceiling: ${pa} — non-credible / no resolution`);
  else ok(`annual P(WWIII) = ${(pa * 100).toFixed(1)}% (not pegged at ceiling)`);
}

// 6) 6h-TREND CONTRACT — the trailing-6h delta is computed server-side (durable)
//    and shipped in the payload as `trend_6h`; the cockpit's top-right readout
//    renders it. This guards the recurring "6h Trend = —" regression that used to
//    appear whenever a client refactor dropped the session-buffer seed. We assert
//    the payload carries the field (numeric delta when available) AND the readout
//    element actually renders something — without dictating the value.
if (latest) {
  const tr = latest.trend_6h;
  if (!tr || typeof tr !== 'object') fail.push('api/latest missing trend_6h object — 6h-trend contract broken (server side)');
  else if (tr.available && !Number.isFinite(tr.delta)) fail.push(`trend_6h.available but delta not finite: ${tr.delta}`);
  else ok(`trend_6h present (available=${!!tr.available}${tr.available ? `, Δ=${(tr.delta * 100).toFixed(3)}%` : ''})`);
}
const trendTxt = await page.$eval('#cmd-trend', el => el.textContent).catch(() => null);
if (trendTxt === null) fail.push('#cmd-trend readout element missing (6h Trend not rendered)');
else if (trendTxt.trim() === '') fail.push('#cmd-trend readout is empty');
else ok(`#cmd-trend renders "${trendTxt.trim()}"`);

// 7) INDICATIONS & WARNING board — the "why" panel. The engine ships a fixed 12-condition
//    board in `data.indicators`; the client renders one cell per indicator (plus neutral
//    aria-hidden filler cells to square off the 3-column grid). This is the densest
//    awareness surface and the one core panel the gate never looked at: a client refactor
//    that dropped cells, crashed renderIndicators, or left the "awaiting indicator data…"
//    placeholder up would ship an EMPTY "why" board undetected. Assert the board renders
//    exactly the indicators the server sent (fillers excluded), each with a legible
//    (non-empty) label, and occupies real space. No opinion on which lights are lit.
if (latest) {
  const inds = Array.isArray(latest.indicators) ? latest.indicators : null;
  if (!inds || inds.length === 0) {
    fail.push('api/latest carries no indicators array — the I&W "why" board has nothing to render');
  } else {
    // The board renders from the WS snapshot, which can land a beat after api/latest is
    // ready. Poll the DOM briefly for the board to populate to the server's count before
    // asserting, so a warm-up race can't false-fail (mirrors the api/latest readiness poll).
    // Fillers carry aria-hidden; exclude them so the count is future-proof if the board size
    // ever changes to a non-multiple of 3.
    const realLabels = () => page.$$eval(
      '#iw-board .iw-cell:not([aria-hidden="true"])',
      (els) => els.map((e) => (e.querySelector('.iw-label')?.textContent || '').trim()),
    );
    let labels = [];
    for (let i = 0; i < 15; i++) {                 // ≈ 15 × 500ms ≈ 7.5s
      labels = await realLabels().catch(() => []);
      // Populated = one real cell per indicator, and not the single "awaiting…" placeholder.
      if (labels.length === inds.length && !labels.some((t) => /awaiting/i.test(t))) break;
      await new Promise((res) => setTimeout(res, 500));
    }
    const bd = await box('#iw-board');
    if (!bd || bd.height < 8) {
      fail.push(`#iw-board missing or collapsed (${bd ? Math.round(bd.height) + 'px' : 'no box'})`);
    } else if (labels.length !== inds.length) {
      fail.push(`I&W board rendered ${labels.length} cells, server sent ${inds.length} indicators — the "why" panel is broken/partial`);
    } else if (labels.some((t) => t === '')) {
      fail.push(`I&W board has ${labels.filter((t) => t === '').length} blank warning light(s) — unreadable`);
    } else {
      ok(`I&W board rendered all ${labels.length} indicator cells, labels legible`);
    }
  }
}

// 8) NEWEST AWARENESS SURFACES — the durable recent-range readout (context strip) and the
//    load-bearing-modality footer both render from the live snapshot and both initialize to the
//    "—" placeholder in static HTML. A client refactor that dropped or crashed renderReadRange or
//    the model-state footer would leave those placeholders up, shipping a BLIND surface the gate
//    never looked at — the same regression class as the "6h Trend = —" and empty-I&W-board bugs.
//    Assert each populates to a real, non-placeholder value once the snapshot lands. We assert
//    only "not stuck on the placeholder" — no opinion on the value, and any honest-null copy
//    (a session fallback %, "diffuse …", "held by …") counts as rendered, so a healthy prod in
//    any state passes; only a genuinely un-rendered surface fails.
if (latest) {
  const settled = async (sel) => {
    let txt = null;
    for (let i = 0; i < 15; i++) {                 // ≈ 15 × 500ms ≈ 7.5s, mirrors the I&W poll
      txt = await page.$eval(sel, (el) => el.textContent).catch(() => null);
      txt = txt == null ? null : txt.trim();
      if (txt && txt !== '—') break;
      await new Promise((res) => setTimeout(res, 500));
    }
    return txt;
  };
  const clip = (t) => (t.length > 56 ? t.slice(0, 56) + '…' : t);

  const peak = await settled('#ca-peak');
  if (peak == null) fail.push('#ca-peak (recent-range readout) missing — context-strip range surface not rendered');
  else if (peak === '' || peak === '—') fail.push('#ca-peak stuck on the "—" placeholder — renderReadRange did not populate the recent-range band');
  else ok(`recent-range readout populated (#ca-peak = "${peak}")`);

  const lb = await settled('#f-loadbearing');
  if (lb == null) fail.push('#f-loadbearing (load-bearing modality) missing — model-state footer not rendered');
  else if (lb === '' || lb === '—') fail.push('#f-loadbearing stuck on the "—" placeholder — the load-bearing modality footer did not render');
  else ok(`load-bearing modality footer populated (#f-loadbearing = "${clip(lb)}")`);

  const lt = await settled('#f-loadtheater');
  if (lt == null) fail.push('#f-loadtheater (load-bearing theater) missing — model-state footer not rendered');
  else if (lt === '' || lt === '—') fail.push('#f-loadtheater stuck on the "—" placeholder — the load-bearing theater footer did not render');
  else ok(`load-bearing theater footer populated (#f-loadtheater = "${clip(lt)}")`);
}

await browser.close();

if (fail.length) {
  console.error(`\nEYES FAIL (${fail.length}) @ ${URL}:`);
  for (const f of fail) console.error('  ✗ ' + f);
  process.exit(1);
}
console.log(`\nEYES OK @ ${URL} — dashboard alive, graph legible, panels intact.`);
process.exit(0);

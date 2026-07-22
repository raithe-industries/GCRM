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

// 1b) HTML STRUCTURAL INTEGRITY — the markup's <div> tree must balance. A dropped closing
//     </div> is INVISIBLE to every other check: malformed HTML throws no JS error, the browser
//     silently REPARENTS the following elements. On 2026-07-08 a routine dropped the </div>
//     closing .formula; the parser nested .right-panel inside .center-panel, which collapsed
//     .charts-area {flex:1} to ~0 — a 9h deploy freeze the gate caught only as the downstream
//     symptom (#timeline-chart squished to 2px), never the cause. We parse the RAW served HTML
//     (a fresh fetch — NOT page.content(), which returns the already auto-corrected DOM that
//     hides the imbalance), strip <script>/<style>/comments so JS-string and CSS text can't skew
//     the tag count, then walk <div>/</div> on a stack. <div> is never a void element, so a
//     balanced tree is an invariant of ANY valid design — this constrains correctness, not looks.
try {
  const raw = await fetch(URL, { signal: AbortSignal.timeout(8000) }).then((r) => r.text());
  const markup = raw
    .replace(/<script\b[\s\S]*?<\/script>/gi, '')
    .replace(/<style\b[\s\S]*?<\/style>/gi, '')
    .replace(/<!--[\s\S]*?-->/g, '');
  const stack = [];
  let stray = 0;
  const re = /<div\b([^>]*)>|<\/div\s*>/gi;
  let m;
  while ((m = re.exec(markup)) !== null) {
    if (m[1] === undefined) { if (stack.length) stack.pop(); else stray++; }       // a </div>
    else if (!/\/\s*$/.test(m[1])) {                                               // a <div …> (not self-closed <div/>)
      const id = /\bid="([^"]*)"/.exec(m[1])?.[1];
      const cls = /\bclass="([^"]*)"/.exec(m[1])?.[1];
      stack.push(id ? `#${id}` : cls ? `.${cls.trim().split(/\s+/)[0]}` : '<div>');
    }
  }
  const opens = (markup.match(/<div\b/gi) || []).length;
  if (stray) fail.push(`unbalanced <div> tree: ${stray} stray </div> with no opening <div> — the markup structure is broken`);
  if (stack.length) fail.push(`unbalanced <div> tree: ${stack.length} unclosed <div> (still open at EOF: ${stack.slice(-3).join(' › ')}) — a dropped </div> silently reparents following siblings and collapses the layout, with NO JS error to flag it`);
  if (!stray && !stack.length) ok(`HTML <div> tree balanced (${opens} divs, well-formed)`);

  // 1c) RENDER-COMPLETENESS — the startup substitution fills `{{TOKEN}}` placeholders
  //     (`{{ELEVATION_THRESHOLD}}`, `{{BASELINE_ANNUAL_PCT}}`, …) with live model values. A
  //     placeholder added to the HTML whose `.replace()` was never wired ships raw template
  //     syntax to the operator — a broken honesty claim with NO JS error to flag it. Scan the
  //     RAW served HTML (the pre-substitution `raw` above, already fetched) for any surviving
  //     `{{[A-Z0-9_]+}}`. (Mirrors the server-side self-check + Rust test, but at the wire.)
  const rawLeft = raw.match(/\{\{[A-Z0-9_]+\}\}/g) || [];
  if (rawLeft.length) fail.push(`/risk ships ${rawLeft.length} unsubstituted template placeholder(s) (${[...new Set(rawLeft)].slice(0, 4).join(', ')}) — raw template syntax reached the operator`);
  else ok('no unsubstituted template placeholders on /risk');
} catch (e) {
  fail.push(`structural-integrity check could not fetch raw HTML: ${e.message}`);
}

// 1d) METHODOLOGY HONESTY PAGE — the served whitepaper (calibration evidence, anchor table,
//     alert bands — the last two made load-bearing by the 1.y/1.33 work) is the system's honesty
//     surface, yet every check above only ever loaded /risk, so a broken methodology render
//     shipped UNSEEN. Fetch /methodology and assert it (a) serves real HTML, (b) carries NO
//     unsubstituted `{{TOKEN}}` placeholder, and (c) actually rendered its live figures — the
//     calibration-evidence block and a real alert-band %. No opinion on the prose, only "is the
//     honesty page whole".
try {
  const methURL = URL.replace(/\/+$/, '') + '/methodology';
  const mres = await fetch(methURL, { signal: AbortSignal.timeout(8000) });
  if (!mres.ok) {
    fail.push(`/methodology honesty page returned HTTP ${mres.status} — the whitepaper is not being served`);
  } else {
    const mraw = await mres.text();
    const mLeft = mraw.match(/\{\{[A-Z0-9_]+\}\}/g) || [];
    if (mraw.length < 500) fail.push(`/methodology served only ${mraw.length} bytes — the honesty whitepaper did not render`);
    else if (mLeft.length) fail.push(`/methodology ships ${mLeft.length} unsubstituted template placeholder(s) (${[...new Set(mLeft)].slice(0, 4).join(', ')}) — raw template syntax on an honesty surface`);
    else if (!/Brier/.test(mraw)) fail.push('/methodology is missing the rendered calibration-evidence block (no "Brier") — the {{CALIBRATION_EVIDENCE}} fragment did not render');
    else if (!/\d+\.\d+%/.test(mraw)) fail.push('/methodology is missing a rendered alert-band % — the {{ALERT_*}} bands did not render');
    else ok('/methodology honesty page whole (no raw placeholder, calibration + bands rendered)');
  }
} catch (e) {
  fail.push(`/methodology honesty page did not load: ${e.message}`);
}

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
  // Support-breadth clause: when a load-bearing modality is named, the footer appends the effective
  // number of modalities the headline leans on ("· leans on ≈N.N modality|modalities[, qualifier]").
  // It is optional (hidden when diffuse/held, or when an older backend omits support_breadth), so we
  // don't demand it; but WHEN present it must be well-formed — a broken render that appended a stray
  // "leans on …" token should fail rather than ship an unreadable breadth read.
  else if (/leans on/.test(lb) && !/leans on ≈\d+\.\d+ modalit(y|ies)(, (single-sourced|broad-based))?/.test(lb))
    fail.push(`#f-loadbearing carries a malformed support-breadth clause ("${clip(lb)}") — expected "· leans on ≈N.N modality|modalities[, single-sourced|broad-based]"`);
  else ok(`load-bearing modality footer populated (#f-loadbearing = "${clip(lb)}")`);

  const lt = await settled('#f-loadtheater');
  if (lt == null) fail.push('#f-loadtheater (load-bearing theater) missing — model-state footer not rendered');
  else if (lt === '' || lt === '—') fail.push('#f-loadtheater stuck on the "—" placeholder — the load-bearing theater footer did not render');
  else ok(`load-bearing theater footer populated (#f-loadtheater = "${clip(lt)}")`);

  // Memory-load footer (#f-memory): the pp of headline carried by remembered war-state vs. fresh
  // evidence — the quantitative form of the ⏸ held flag. Honest-null (no floor-held theater) HIDES
  // its row, so we cannot demand non-"—". Instead: the node must EXIST (a dropped/renamed element
  // fails), and WHEN its row is visible it must carry a well-formed lift string
  // ("+X.XXpp from N memory-held theater(s) …"), never a stuck "—" or a stray token.
  const mEl = await page.$('#f-memory');
  if (!mEl) fail.push('#f-memory (memory load) missing — the "carried by memory" footer was dropped from the DOM');
  else {
    const mVisible = await page.$eval('#f-memory-row', (el) => {
      const s = window.getComputedStyle(el);
      return s.display !== 'none' && s.visibility !== 'hidden';
    }).catch(() => false);
    const mv = await page.$eval('#f-memory', (el) => el.textContent.trim()).catch(() => null);
    if (!mVisible) ok('memory-load footer honest-null (hidden: no floor-held theater)');
    else if (!mv || mv === '—') fail.push('#f-memory row is visible but stuck on the "—" placeholder — the memory-load footer did not render');
    else if (!/^\+\d+\.\d+pp from \d+ memory-held theater/.test(mv)) fail.push(`#f-memory rendered a malformed memory-load line ("${clip(mv)}") — expected "+X.XXpp from N memory-held theater(s) …"`);
    else ok(`memory-load footer populated (#f-memory = "${clip(mv)}")`);
  }

  // Escalation-coherence footer (#f-coherence): is the number heating WHERE it rests (coherent) or
  // on a DIFFERENT emerging front (divergent) — the relation between the load-bearing theater and
  // per-theater escalation momentum. Honest-null (no load-bearing theater, or nothing decisively
  // escalating) HIDES its row, so we cannot demand non-"—". Instead: the node must EXIST (a
  // dropped/renamed element fails), and WHEN its row is visible it must carry a well-formed
  // coherent/divergent line, never a stuck "—" or a stray token.
  const ecEl = await page.$('#f-coherence');
  if (!ecEl) fail.push('#f-coherence (escalation coherence) missing — the "where it rests vs where it heats" footer was dropped from the DOM');
  else {
    const ecVisible = await page.$eval('#f-coherence-row', (el) => {
      const s = window.getComputedStyle(el);
      return s.display !== 'none' && s.visibility !== 'hidden';
    }).catch(() => false);
    const ec = await page.$eval('#f-coherence', (el) => el.textContent.trim()).catch(() => null);
    if (!ecVisible) ok('escalation-coherence footer honest-null (hidden: no load-bearing theater / nothing decisively escalating)');
    else if (!ec || ec === '—') fail.push('#f-coherence row is visible but stuck on the "—" placeholder — the escalation-coherence footer did not render');
    else if (!/^coherent — escalating where the number rests \([^)]+\)$/.test(ec) &&
             !/^divergent — escalation building in .+ \([^)]+\), not where the number rests$/.test(ec))
      fail.push(`#f-coherence rendered a malformed coherence line ("${clip(ec)}") — expected "coherent — …" or "divergent — escalation building in X (…)"`);
    else ok(`escalation-coherence footer populated (#f-coherence = "${clip(ec)}")`);
  }

  // Escalation-breadth footer (#f-breadth): how many fronts are decisively escalating AT ONCE —
  // isolated single-front vs. a synchronized multi-front escalation. Honest-null (nothing decisively
  // escalating) HIDES its row, so we cannot demand non-"—". Instead: the node must EXIST (a
  // dropped/renamed element fails), and WHEN its row is visible it must carry a well-formed
  // single/multi-front line, never a stuck "—" or a stray token.
  const ebEl = await page.$('#f-breadth');
  if (!ebEl) fail.push('#f-breadth (escalation breadth) missing — the "how many fronts escalating at once" footer was dropped from the DOM');
  else {
    const ebVisible = await page.$eval('#f-breadth-row', (el) => {
      const s = window.getComputedStyle(el);
      return s.display !== 'none' && s.visibility !== 'hidden';
    }).catch(() => false);
    const eb = await page.$eval('#f-breadth', (el) => el.textContent.trim()).catch(() => null);
    if (!ebVisible) ok('escalation-breadth footer honest-null (hidden: nothing decisively escalating)');
    else if (!eb || eb === '—') fail.push('#f-breadth row is visible but stuck on the "—" placeholder — the escalation-breadth footer did not render');
    else if (!/^single front escalating — .+$/.test(eb) &&
             !/^\d+ fronts escalating at once — .+ \(synchronized\)$/.test(eb))
      fail.push(`#f-breadth rendered a malformed breadth line ("${clip(eb)}") — expected "single front escalating — X" or "N fronts escalating at once — … (synchronized)"`);
    else ok(`escalation-breadth footer populated (#f-breadth = "${clip(eb)}")`);
  }

  // Band self-validation caption (#gauge-band-cov): the honest coverage read on the uncertainty band.
  // Unlike the surfaces above it has NO "—" sentinel — an honest-null (thin ring) renders EMPTY — so we
  // cannot demand non-empty without false-rolling a cold ring. Instead: the element must EXIST (a
  // dropped/renamed node fails), and WHEN it carries text it must be the well-formed coverage line
  // ("band held N% of reads · <verdict> (n=P)"), never a stray token from a broken render.
  const bcPresent = await page.$('#gauge-band-cov');
  if (!bcPresent) fail.push('#gauge-band-cov (band self-validation) missing — the uncertainty-band coverage caption was dropped from the DOM');
  else {
    // Two optional clauses: "· N⇧ M⇩" is the breach-direction read (shown only on an overconfident
    // verdict) and "· at floor M%" is the sharpness read (backends carrying floor_bound_pct) — accept
    // the line with either, both, or neither so an older backend and every verdict class all pass.
    const bc = await page.$eval('#gauge-band-cov', (el) => el.textContent.trim()).catch(() => null);
    if (bc && !/^band held \d+% of reads · \w+( · \d+⇧ \d+⇩)?( · at floor \d+%)? \(n=\d+\)$/.test(bc))
      fail.push(`#gauge-band-cov rendered a malformed coverage line ("${clip(bc)}") — expected "band held N% of reads · <verdict>[ · N⇧ M⇩][ · at floor M%] (n=P)"`);
    else ok(`band self-validation caption intact (#gauge-band-cov = "${bc ? clip(bc) : '(honest-null: thin ring)'}")`);
  }

  // Alert-band dwell readout (#ca-dwell): the TIME the read has held at/above its current band.
  // Honest-null (cold ring / older backend) HIDES its box, so we cannot demand non-"—". Instead:
  // the node must EXIST (a dropped/renamed element fails), and WHEN its box is visible it must
  // carry a well-formed dwell string ("[≥]Xd Yh @ Level"), never a stuck "—" or a stray token.
  const dwEl = await page.$('#ca-dwell');
  if (!dwEl) fail.push('#ca-dwell (alert-band dwell) missing — the "at level" duration readout was dropped from the DOM');
  else {
    const visible = await page.$eval('#ca-dwell-box', (el) => {
      const s = window.getComputedStyle(el);
      return s.display !== 'none' && s.visibility !== 'hidden';
    }).catch(() => false);
    const dw = await page.$eval('#ca-dwell', (el) => el.textContent.trim()).catch(() => null);
    if (!visible) ok('alert-band dwell honest-null (hidden: cold ring / no durable field)');
    else if (!dw || dw === '—') fail.push('#ca-dwell box is visible but stuck on the "—" placeholder — renderDwell did not populate the dwell');
    else if (!/^≥?\d+[dhms]( \d+[hm])? @ \w+$/.test(dw)) fail.push(`#ca-dwell rendered a malformed dwell ("${clip(dw)}") — expected "[≥]Xd Yh @ Level"`);
    else ok(`alert-band dwell populated (#ca-dwell = "${clip(dw)}")`);
  }

  // Locus-concentration readout (#ca-locus): how concentrated the WHERE has been over 24h.
  // Honest-null (cold ring / quiet world with no lead / older backend) HIDES its box, so we cannot
  // demand non-"—". Instead: the node must EXIST (a dropped/renamed element fails), and WHEN its
  // box is visible it must carry a well-formed locus string — "<theater> — N% of 24h …", or
  // "<theater> — N% of last Nh …" while a post-restart ring spans under the full day (the
  // span-honest label shipped 2026-07-17; a deploy landing 6–23h after a restart sees that
  // form) — never a stuck "—" or a stray token.
  const lcEl = await page.$('#ca-locus');
  if (!lcEl) fail.push('#ca-locus (locus concentration) missing — the "where over time" readout was dropped from the DOM');
  else {
    const lcVisible = await page.$eval('#ca-locus-box', (el) => {
      const s = window.getComputedStyle(el);
      return s.display !== 'none' && s.visibility !== 'hidden';
    }).catch(() => false);
    const lc = await page.$eval('#ca-locus', (el) => el.textContent.trim()).catch(() => null);
    if (!lcVisible) ok('locus concentration honest-null (hidden: cold ring / quiet world / no durable field)');
    else if (!lc || lc === '—') fail.push('#ca-locus box is visible but stuck on the "—" placeholder — renderLocus did not populate the locus');
    else if (!/ — \d+% of (24h|last \d+h)/.test(lc)) fail.push(`#ca-locus rendered a malformed locus ("${clip(lc)}") — expected "<theater> — N% of 24h …" (or "… N% of last Nh …" on a partial post-restart ring)`);
    else ok(`locus concentration populated (#ca-locus = "${clip(lc)}")`);
  }
}

// 8b) COMMAND STRIP — the top-of-cockpit "grasp the state at a glance" row (the six .cmd-cell
//    readouts: Threat Level, WWIII Risk, Primary Driver, Confidence, 6h Trend, Momentum). Every
//    cell initializes to the "—" placeholder in static HTML and is populated from the live
//    snapshot by the render pass. The gate already guards #cmd-trend (§6), but the other five —
//    the FIRST thing an operator reads — were never looked at: a client refactor that crashed the
//    strip render or dropped a cell would ship a top row stuck on "—" (a correct number rendered
//    BLIND — pillar-2 legibility failure) undetected. Assert the four cells that always resolve to
//    a real value once the snapshot lands do so, and the two with strong stable formats (Risk %,
//    Momentum signed decimal) are well-formed rather than a stray token. Primary Driver is
//    honest-null-capable (no dominant domain named → "—", its sub-line still carries the fallback),
//    so we require its node to EXIST but tolerate the placeholder. No opinion on any value.
if (latest) {
  const settled = async (sel) => {
    let txt = null;
    for (let i = 0; i < 15; i++) {                 // ≈ 15 × 500ms ≈ 7.5s, mirrors the §8 poll
      txt = await page.$eval(sel, (el) => el.textContent).catch(() => null);
      txt = txt == null ? null : txt.trim();
      if (txt && txt !== '—') break;
      await new Promise((res) => setTimeout(res, 500));
    }
    return txt;
  };
  const clip = (t) => (t.length > 56 ? t.slice(0, 56) + '…' : t);

  const threat = await settled('#cmd-threat');
  if (threat == null) fail.push('#cmd-threat (Threat Level) missing — the command strip did not render');
  else if (threat === '' || threat === '—') fail.push('#cmd-threat stuck on the "—" placeholder — the command strip did not populate the threat level');
  else ok(`command strip: threat level populated (#cmd-threat = "${clip(threat)}")`);

  const risk = await settled('#cmd-risk');
  if (risk == null) fail.push('#cmd-risk (WWIII Risk) missing — the command strip did not render');
  else if (risk === '' || risk === '—') fail.push('#cmd-risk stuck on the "—" placeholder — the command strip did not populate the annual risk');
  else if (!/^≥?\d+(\.\d+)?%$/.test(risk)) fail.push(`#cmd-risk rendered a malformed risk readout ("${clip(risk)}") — expected "[≥]N.NN%"`);
  else ok(`command strip: annual risk populated (#cmd-risk = "${risk}")`);

  const conf = await settled('#cmd-conf');
  if (conf == null) fail.push('#cmd-conf (Confidence) missing — the command strip did not render');
  else if (conf === '' || conf === '—') fail.push('#cmd-conf stuck on the "—" placeholder — the command strip did not populate the confidence label');
  else ok(`command strip: confidence populated (#cmd-conf = "${clip(conf)}")`);

  // Momentum (6th cell, operator 2026-07-18): board-wide news-flow direction. Always resolves to a
  // signed 2-decimal value once the snapshot lands (neutral shows the bare "+0.04"; decisive states
  // carry the ⇧/⇩ arrow). A malformed momentum (missing the value, stray token) is a real defect.
  const mom = await settled('#cmd-mom');
  if (mom == null) fail.push('#cmd-mom (Momentum) missing — the six-cell command strip did not render');
  else if (mom === '' || mom === '—') fail.push('#cmd-mom stuck on the "—" placeholder — the momentum cell did not populate');
  else if (!/^([⇧⇩] )?[+-]?\d+\.\d{2}$/.test(mom)) fail.push(`#cmd-mom rendered a malformed momentum ("${clip(mom)}") — expected "[⇧|⇩ ][+|-]N.NN"`);
  else ok(`command strip: momentum populated (#cmd-mom = "${mom}")`);

  // Primary Driver: honest-null-capable — when no dominant domain is named the value stays "—" and
  // the sub-line ("hottest: X" / "no theater elevated") carries the fallback. Require the node to
  // exist (a dropped/renamed cell fails) but do not demand a non-placeholder value.
  const drvEl = await page.$('#cmd-driver');
  if (!drvEl) fail.push('#cmd-driver (Primary Driver) missing — the command strip cell was dropped from the DOM');
  else {
    const drv = await page.$eval('#cmd-driver', (el) => el.textContent.trim()).catch(() => null);
    if (drv && drv !== '—') ok(`command strip: primary driver populated (#cmd-driver = "${clip(drv)}")`);
    else ok('command strip: primary driver honest-null (no dominant domain named; sub-line carries the fallback)');
  }
}

// 9) RESPONSIVE / SMALL-VIEWPORT LEGIBILITY — every check above ran at ONE size (1440×900),
//    but the dashboard ships deliberate layout rules for phones (`@media(max-width:680px)`:
//    stack the columns, wrap the 5-stat strip to 2 cols) and short displays
//    (`@media(max-height:640px)`: let the page scroll, pin the charts to fixed heights). Those
//    rules exist to fix DOCUMENTED regressions — the 5th stat clipped off the right edge, and the
//    Chart.js no-bounded-height resize→render loop squishing the timeline toward 2px. Nothing
//    re-checked them, so a CSS refactor that broke either breakpoint would ship a horizontally-
//    clipped or squished phone cockpit undetected. We re-drive the SAME loaded page at two sizes
//    and assert two invariants of any good responsive design (no layout opinion): (a) the page body
//    does not overflow horizontally — `body.scrollWidth` catches content spilling past the edge even
//    under the `overflow-x:hidden` that hides the scrollbar but not the clip (the exact clipped-stat
//    bug); off-screen fixed drawers (`translateX(100%)`) do NOT inflate it, so no false positive —
//    and (b) the primary timeline graph is still rendered above the legibility floor.
const OVF_TOL = 2; // px — sub-pixel rounding slack
const hOverflow = () => page.evaluate(() => {
  const b = document.body;
  // Best-effort culprit for the diagnostic (fail message only): the in-flow element reaching
  // furthest past the viewport's right edge, skipping fixed/sticky (a parked drawer is not a clip).
  let worst = null, worstR = 0;
  const vw = document.documentElement.clientWidth;
  for (const el of document.body.querySelectorAll('*')) {
    const pos = getComputedStyle(el).position;
    if (pos === 'fixed' || pos === 'sticky') continue;
    const r = el.getBoundingClientRect().right;
    if (r > worstR) { worstR = r; worst = el.id ? '#' + el.id : '.' + (el.className && el.className.toString().trim().split(/\s+/)[0] || el.tagName.toLowerCase()); }
  }
  return { over: b.scrollWidth - b.clientWidth, sw: b.scrollWidth, cw: b.clientWidth, worst, worstR: Math.round(worstR), vw };
});
for (const [w, h, name] of [[390, 844, 'phone-portrait'], [1280, 560, 'short-landscape']]) {
  await page.setViewportSize({ width: w, height: h });
  await page.waitForTimeout(1000); // let the media query re-flow and Chart.js re-render at the new size
  const ov = await hOverflow();
  if (ov.over > OVF_TOL) {
    fail.push(`${name} (${w}×${h}): page overflows horizontally by ${ov.over}px (body ${ov.sw}px > viewport ${ov.cw}px)` +
      (ov.worst ? ` — widest in-flow element ${ov.worst} reaches ${ov.worstR}px past a ${ov.vw}px viewport` : '') +
      ' — content is clipped off the right edge');
  } else {
    ok(`${name} (${w}×${h}): no horizontal overflow (body ${ov.sw}px ≤ viewport ${ov.cw}px)`);
  }
  const tlv = await box('#timeline-chart');
  if (!tlv) fail.push(`${name}: #timeline-chart missing or not visible after resize`);
  else if (tlv.height < MIN_GRAPH_H) fail.push(`${name}: #timeline-chart squished to ${Math.round(tlv.height)}px (legibility floor ${MIN_GRAPH_H}px) — the responsive height rule collapsed`);
  else ok(`${name}: timeline graph legible ${Math.round(tlv.width)}×${Math.round(tlv.height)}px`);
}

await browser.close();

if (fail.length) {
  console.error(`\nEYES FAIL (${fail.length}) @ ${URL}:`);
  for (const f of fail) console.error('  ✗ ' + f);
  process.exit(1);
}
console.log(`\nEYES OK @ ${URL} — dashboard alive, graph legible, panels intact.`);
process.exit(0);

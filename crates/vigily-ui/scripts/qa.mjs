// Browser QA for the vigily web app: screenshots of every core screen in light and dark, at
// desktop and phone widths, plus computed checks (console errors, horizontal overflow, undersized
// targets, headings over two lines).
//
//   vigily up                        # with the demo repository (examples/demo.py)
//   npm run qa -- [out-dir]          # default: target/qa (not committed; see assets/src/screenshots.py)
//
// VIGILY_URL overrides the server (default http://127.0.0.1:7600). PLAYWRIGHT_CHROMIUM is a
// Chromium executable to use instead of Playwright's own download.

import { mkdirSync, readdirSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";
import { chromium } from "playwright";

const BASE = process.env.VIGILY_URL ?? "http://127.0.0.1:7600";
const OUT = resolve(process.argv[2] ?? join(import.meta.dirname, "../../../target/qa"));
mkdirSync(OUT, { recursive: true });

function chromiumPath() {
  if (process.env.PLAYWRIGHT_CHROMIUM) return process.env.PLAYWRIGHT_CHROMIUM;
  const cache = join(homedir(), ".cache/ms-playwright");
  try {
    const dirs = readdirSync(cache).filter((d) => /^chromium-\d+$/.test(d)).sort((a, b) => +b.split("-")[1] - +a.split("-")[1]);
    for (const d of dirs) for (const sub of ["chrome-linux64/chrome", "chrome-linux/chrome", "chrome-mac/Chromium.app/Contents/MacOS/Chromium"]) {
      const p = join(cache, d, sub);
      try {
        readdirSync(join(p, ".."));
        return p;
      } catch {}
    }
  } catch {}
  return undefined;
}

const api = async (path) => (await fetch(BASE + path)).json();
const runs = (await api("/api/v1/runs")).rows;
const longRun = runs.find((r) => r.name.endsWith("-long")) ?? runs[0];
const failed = runs.find((r) => r.status === "failed") ?? runs[0];
const cifar = runs.filter((r) => r.experiment === "cifar10-resnet18").map((r) => r.id);

const suites = await api("/api/v1/suites").catch(() => []);
const v2 = suites.find((x) => x.label === "intent-clf-v2") ?? suites.find((x) => x.scores.length) ?? suites[0];
const suiteDetail = v2 ? await api(`/api/v1/suites/${v2.key}`) : null;
const evalRun = suiteDetail?.selected;
const evalRuns = suiteDetail?.runs ?? [];
const before = evalRuns.length > 9 ? evalRuns[evalRuns.length - 9].run_id : null;
const evalItems = evalRun ? ((await api(`/api/v1/runs/${evalRun}/items`).catch(() => ({ items: [] }))).items ?? []).length : 0;
const schedules = (await api("/api/v1/schedules").catch(() => ({ schedules: [] }))).schedules;
const datasets = await api("/api/v1/datasets").catch(() => []);
const traces = await api("/api/v1/traces?limit=200").catch(() => ({ rows: [] }));
const failedTrace = traces.rows.find((t) => t.status === "error") ?? traces.rows[0];
const tracedRun = traces.rows.find((t) => t.run_id)?.run_id;

const evalPages = [
  ["datasets", "/datasets"],
  datasets[0] && ["dataset", `/datasets/${datasets[0].name}`],
  evalRun && ["eval-items", `/runs/${evalRun}?tab=items`],
  evalRun && ["eval-audit", `/runs/${evalRun}?tab=audit`],
  evalRun && before && ["compare", `/compare?a=${before}&b=${evalRun}`],
  ["schedules", "/schedules"],
  schedules[0] && ["schedule", `/schedules/${schedules[0].name}`],
  ["drift", "/drift"],
  v2 && ["suite", `/drift/${v2.key}`],
  ["alerts", "/alerts?show=all"],
].filter(Boolean);

const pages = [
  ...evalPages,
  ["runs", "/?sort=metric:min:loss%5Bval%5D"],
  ["metrics", `/metrics?q=${encodeURIComponent('exp="cifar10-resnet18"')}&m=${encodeURIComponent("loss[val]")}&m=${encodeURIComponent("loss[train]")}&m=${encodeURIComponent("accuracy[val]")}&m=lr&group=param:optimizer.name&smooth=0.6`],
  ["metrics-aggregate", `/metrics?q=${encodeURIComponent('exp="cifar10-resnet18"')}&m=${encodeURIComponent("loss[val]")}&m=${encodeURIComponent("accuracy[val]")}&group=param:hparams.lr&agg=1&log=1`],
  ["run", `/runs/${longRun.id}`],
  ["run-failed", `/runs/${failed.id}?tab=metrics`],
  ["experiments", "/experiments"],
  ["traces", "/traces"],
  failedTrace && ["trace", `/traces/${failedTrace.id}`],
  tracedRun && ["traces-run", `/traces?run=${tracedRun}&range=7d`],
  cifar.length && ["compare-runs", `/metrics/compare?runs=${cifar.slice(0, 4).join(",")}`],
  ["settings", "/settings"],
].filter(Boolean);
const viewports = [
  ["desktop", { width: 1440, height: 900 }],
  ["phone", { width: 400, height: 860 }],
];

const browser = await chromium.launch({ executablePath: chromiumPath() });
const report = [];
for (const theme of ["light", "dark"]) {
  for (const [vp, size] of viewports) {
    const ctx = await browser.newContext({ viewport: size, colorScheme: theme, deviceScaleFactor: vp === "desktop" ? 1 : 2 });
    await ctx.addInitScript((t) => localStorage.setItem("vigily.theme", t), theme);
    if (cifar.length) await ctx.addInitScript((ids) => sessionStorage.setItem("vigily.selection", JSON.stringify(ids)), cifar.slice(0, 4));
    const page = await ctx.newPage();
    const errors = [];
    page.on("console", (m) => m.type() === "error" && errors.push(m.text()));
    page.on("pageerror", (e) => errors.push(String(e)));
    for (const [name, path] of pages) {
      if (vp === "phone" && name === "metrics-aggregate") continue;
      await page.goto(BASE + path, { waitUntil: "networkidle" });
      await page.waitForTimeout(600);
      if (name === "metrics" && vp === "desktop") {
        // Hover the first chart so the crosshair and readout show values.
        const c = page.locator(".u-over").first();
        const box = await c.boundingBox();
        if (box) await page.mouse.move(box.x + box.width * 0.62, box.y + box.height * 0.55);
        await page.waitForTimeout(200);
      }
      const checks = await page.evaluate(() => {
        const overflow = document.documentElement.scrollWidth > document.documentElement.clientWidth;
        const small = [...document.querySelectorAll("a,button,[role=button],input,select,textarea,[role=tab],[role=radio]")]
          .filter((e) => {
            const r = e.getBoundingClientRect();
            if (r.width === 0 || r.height === 0) return false;
            // A visually hidden skip link is a keyboard target only; it is full size when focused.
            if (e.matches(".sr-only:not(:focus)")) return false;
            if (e.matches("input[type=checkbox],input[type=radio]")) {
              const l = e.closest("label") ?? e.parentElement;
              const lr = l.getBoundingClientRect();
              return lr.height < 24 || lr.width < 24;
            }
            return r.width < 24 || r.height < 24;
          })
          .map((e) => e.outerHTML.slice(0, 90));
        const longHeads = [...document.querySelectorAll("h1,h2")].filter((h) => h.getBoundingClientRect().height > parseFloat(getComputedStyle(h).lineHeight) * 2.2).map((h) => h.textContent);
        const h1 = document.querySelectorAll("h1").length;
        return { overflow, small: [...new Set(small)].slice(0, 6), longHeads, h1 };
      });
      const file = `${name}-${vp}-${theme}.png`;
      await page.screenshot({ path: join(OUT, file), fullPage: false });
      report.push({ file, ...checks });
    }
    if (vp === "desktop") {
      await page.goto(BASE + "/", { waitUntil: "networkidle" });
      await page.keyboard.press("Control+k");
      await page.keyboard.type("gpt");
      await page.waitForTimeout(300);
      await page.screenshot({ path: join(OUT, `palette-desktop-${theme}.png`) });
      await page.keyboard.press("Escape");
    }
    report.push({ file: `console ${vp} ${theme}`, errors });
    await ctx.close();
  }
}
// Keyboard only: every core task without the pointer.
{
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const page = await ctx.newPage();
  const keys = [];
  const check = async (name, fn) => {
    try {
      keys.push({ check: name, ok: !!(await fn()) });
    } catch (e) {
      keys.push({ check: name, ok: false, error: String(e).slice(0, 120) });
    }
  };
  const active = () => page.evaluate(() => {
    const a = document.activeElement;
    const cs = a ? getComputedStyle(a) : null;
    return { tag: a?.tagName, role: a?.getAttribute("role"), label: a?.getAttribute("aria-label") ?? a?.textContent?.trim().slice(0, 40), outline: cs?.outlineStyle, outlineColor: cs?.outlineColor };
  });
  await page.goto(BASE + "/", { waitUntil: "networkidle" });
  await page.keyboard.press("Tab");
  await check("Tab reaches the skip link first", async () => (await active()).label === "Skip to content");
  await page.keyboard.press("Tab");
  await check("Focus ring is visible", async () => (await active()).outline === "solid");
  await page.keyboard.press("Escape");
  await page.evaluate(() => document.activeElement?.blur());
  await page.keyboard.press("/");
  await check("/ focuses the run filter", async () => (await active()).role === "combobox");
  await page.keyboard.type("status:finished lr<0.005");
  await page.keyboard.press("Enter");
  await page.waitForTimeout(800);
  await check("Filter narrows the runs", async () => /of [0-9,]+ runs match/.test(await page.locator("[data-summary]").innerText()));
  await page.keyboard.press("Escape");
  await page.keyboard.press("j");
  await page.keyboard.press("j");
  await page.keyboard.press("x");
  await check("j and x select a run", async () => /1 run selected/.test(await page.locator("[aria-label='Selected runs']").innerText()));
  await page.keyboard.press("c");
  await page.waitForURL(/\/metrics\/compare\?runs=/);
  await check("c opens the comparison", () => true);
  await page.waitForTimeout(800);
  let found = false;
  for (let i = 0; i < 80 && !found; i++) {
    await page.keyboard.press("Tab");
    found = (await active()).role === "img";
  }
  await check("Tab reaches a chart", () => found);
  await page.keyboard.press("ArrowLeft");
  await page.waitForTimeout(150);
  await check("Arrow keys read values", async () => /^Step /.test(await page.locator("[data-readout]").innerText()));
  await page.keyboard.press("Control+k");
  await check("Ctrl K opens the palette", () => page.getByRole("dialog").isVisible());
  await page.keyboard.type("Experiments");
  await page.keyboard.press("Enter");
  await page.waitForURL(/\/experiments/);
  await check("The palette navigates", () => true);
  await page.keyboard.press("?");
  await check("? shows the shortcuts", () => page.getByRole("dialog", { name: "Keyboard shortcuts" }).isVisible());
  await page.keyboard.press("Escape");
  await check("Escape closes dialogs", async () => !(await page.getByRole("dialog").count()));
  await page.keyboard.press("g");
  await page.keyboard.press("r");
  await page.waitForTimeout(300);
  await check("g r goes to runs", () => new URL(page.url()).pathname === "/");
  if (failedTrace) {
    await page.keyboard.press("g");
    await page.keyboard.press("t");
    await page.waitForURL(/\/traces$/);
    await page.waitForTimeout(500);
    await page.keyboard.press("j");
    await page.keyboard.press("Enter");
    await page.waitForURL(/\/traces\/[0-9a-f]+/);
    await check("g t, j and Enter open a trace", () => true);
    await page.goto(`${BASE}/traces/${failedTrace.id}`, { waitUntil: "networkidle" });
    const first = await page.locator("[role=option][aria-selected=true]").getAttribute("id");
    await page.keyboard.press("j");
    await page.waitForTimeout(200);
    await check("j selects the next span", async () => (await page.locator("[role=option][aria-selected=true]").getAttribute("id")) !== first);
    await check("The span panel follows the selection", async () => (await page.locator("#span-head").innerText()).length > 0);
  }
  {
    // A malformed and very large synthetic trace: a self-parented span, a parent cycle, and more
    // spans than the waterfall draws. Dated 1970 so it sorts last in the list.
    const id = "qa0malformed0trace1";
    const spans = [
      { trace_id: id, span_id: "root", name: "qa_root", start_time: 1_000_000, end_time: 9_000_000 },
      { trace_id: id, span_id: "self", parent_id: "self", name: "qa_self", start_time: 500_100, end_time: 500_200 },
      { trace_id: id, span_id: "cx", parent_id: "cy", name: "qa_cycle_x", start_time: 500_300, end_time: 500_400 },
      { trace_id: id, span_id: "cy", parent_id: "cx", name: "qa_cycle_y", start_time: 500_500, end_time: 500_600 },
    ];
    for (let i = 0; i < 2500; i++) spans.push({ trace_id: id, span_id: `s${i}`, parent_id: "root", name: `qa_step_${i}`, start_time: 1_001_000 + i * 3000, end_time: 1_002_000 + i * 3000 });
    const sent = await fetch(`${BASE}/api/v1/traces/ingest`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ spans }) });
    await check("Ingest takes a synthetic trace", () => sent.ok);
    const csrf = await fetch(`${BASE}/api/v1/traces/ingest`, { method: "POST", headers: { "content-type": "text/plain" }, body: JSON.stringify({ spans }) });
    await check("Ingest refuses text/plain", () => csrf.status === 415);
    await page.goto(`${BASE}/traces/${id}`, { waitUntil: "networkidle" });
    const names = await page.locator("[role=option]").allInnerTexts();
    await check("Spans in a parent cycle are drawn as roots", () => ["qa_self", "qa_cycle_x", "qa_cycle_y"].every((n) => names.some((t) => t.includes(n))));
    await check("A large trace draws a bounded number of rows", async () => names.length <= 2000 && (await page.getByText(/Showing the first 2,000 of 2,504 spans/).count()) === 1);
  }
  if (evalRun && v2) {
    if (evalItems) {
      await page.goto(`${BASE}/runs/${evalRun}?tab=items`, { waitUntil: "networkidle" });
      let onItem = false;
      for (let i = 0; i < 80 && !onItem; i++) {
        await page.keyboard.press("Tab");
        onItem = await page.evaluate(() => document.activeElement?.closest("tbody") !== null && document.activeElement?.tagName === "BUTTON");
      }
      await check("Tab reaches an eval item", () => onItem);
      await page.keyboard.press("Enter");
      await check("Enter opens the item", () => page.getByRole("dialog").isVisible());
      await page.keyboard.press("Escape");
      await check("Escape closes the item", async () => !(await page.getByRole("dialog").count()));
    } else keys.push({ check: "Tab reaches an eval item", ok: true, skipped: "the eval run has no items" });
    await page.goto(`${BASE}/drift/${v2.key}`, { waitUntil: "networkidle" });
    // A suite without scores draws no chart (it says why); one with scores must draw it.
    if (!suiteDetail.scores.length) {
      keys.push({ check: "Arrow keys select another drift run", ok: true, skipped: "the suite has no scores" });
      await check("A suite without scores says why", async () => (await page.getByText(/recorded no scores/).count()) > 0);
    } else if (await page.locator("[role=img]").count()) {
      await page.locator("[role=img]").first().focus();
      const before = new URL(page.url()).searchParams.get("run");
      await page.keyboard.press("ArrowLeft");
      await page.waitForTimeout(400);
      await check("Arrow keys select another drift run", () => new URL(page.url()).searchParams.get("run") !== before);
    } else await check("A suite with scores draws its chart", () => false);
  }
  report.push({ file: "keyboard", checks: keys });
  await ctx.close();
}

await browser.close();
console.log(JSON.stringify(report, null, 1));

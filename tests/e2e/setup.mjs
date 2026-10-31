// Drives the lemonade-engines page like a person: press Set up on one engine, wait for each step,
// read the test chat the page prints, then press Stop. Exit code 0 only when the test chat answered.
//
//   node setup.mjs --url http://127.0.0.1:13500 --engine burn [--channel chrome|msedge] [--exe /usr/bin/google-chrome]
//                  [--accept] [--plan] [--shots DIR] [--timeout-min 30] [--no-stop]
// --expect-fail TEXT: pass when a step fails with TEXT in its message (e.g. "No stick found" on a machine without the stick).
// --accept: press "Accept and set up" (licence-gated engines). --plan: after the plan is shown, press "Download and start".
import { chromium } from "playwright-core";
import fs from "node:fs";
import path from "node:path";

const a = process.argv.slice(2);
const opt = (k, d) => { const i = a.indexOf("--" + k); return i < 0 ? d : (a[i + 1]?.startsWith("--") || i + 1 >= a.length ? true : a[i + 1]); };
const url = opt("url", "http://127.0.0.1:13500"), id = opt("engine"), shots = opt("shots", "shots");
const timeoutMs = Number(opt("timeout-min", 30)) * 60_000;
if (!id) { console.error("--engine <id> is required"); process.exit(2); }
fs.mkdirSync(shots, { recursive: true });

const launch = { headless: true };
if (opt("exe")) launch.executablePath = opt("exe"); else if (opt("channel")) launch.channel = opt("channel");
if (opt("swiftshader")) launch.args = ["--use-gl=angle", "--use-angle=swiftshader"];
const browser = await chromium.launch(launch);
const page = await browser.newPage({ viewport: { width: 1100, height: 1400 } });
const sec = page.locator(`section.eng:has(h2#h-${id})`);
const shot = async (n) => page.screenshot({ path: path.join(shots, `${id}-${n}.png`), fullPage: true });
const steps = async () => (await sec.locator("ol.steps li").allInnerTexts()).map(t => t.replace(/\s+/g, " ").trim());
let ok = false;
try {
  await page.goto(url);
  await sec.waitFor();
  const setupBtn = async (planStep) => {
    // "Set up", or "Accept and set up", or (engine already running and in Lemonade) "Test a chat".
    const sel = planStep ? 'button[data-act="setup"][data-plan]' : 'button.primary.big[data-act]';
    await sec.locator(sel).first().click();
  };
  await setupBtn(false);
  const done = sec.locator(".out.test, .out.err, .out:not(.err):has(+ .actions button[data-plan]), ol.steps li.fail");
  await done.first().waitFor({ timeout: timeoutMs });
  const planShown = await sec.locator('button[data-plan]').count();
  if (planShown && !opt("plan")) {
    console.log("STEPS:\n  " + (await steps()).join("\n  "));
    console.log("PLAN:", (await sec.locator(".out").first().innerText()).slice(0, 1500));
    await shot("plan");
    await browser.close();
    console.log("RESULT PLAN-ONLY (the engine asks the operator to accept its download; re-run with --plan to accept)");
    process.exit(3);
  }
  if (opt("plan") && planShown) {
    console.log("PLAN:", (await sec.locator(".out").first().innerText()).slice(0, 600));
    await shot("plan");
    await setupBtn(true);
    await sec.locator(".out.test, ol.steps li.fail, .out.err").first().waitFor({ timeout: timeoutMs });
  }
  console.log("STEPS:\n  " + (await steps()).join("\n  "));
  const want = opt("expect-fail");
  if (want && want !== true && (await steps()).some(t => t.startsWith("failed") && t.includes(want))) {
    console.log(`EXPECTED FAILURE seen: ${want}`);
    await shot("expected-fail"); await browser.close(); console.log("RESULT PASS (expected failure)"); process.exit(0);
  }
  const text = await sec.locator(".out.test").first().innerText().catch(() => "");
  const answer = (await sec.locator(".answer").first().innerText().catch(() => "")).trim();
  if (text && answer.startsWith("The model returned no visible text")) console.log("EMPTY ANSWER: the engine answered but returned no text, so this is not a pass");
  else if (text) { ok = true; console.log("TEST CHAT:", text.replace(/\n+/g, " | ")); }
  else console.log("NO TEST CHAT. page says:", (await sec.innerText()).replace(/\n+/g, " | ").slice(0, 800));
  await shot(ok ? "pass" : "fail");
  if (ok && !opt("no-stop")) {
    const stop = sec.locator('button[data-act="stop"]');
    // The page redraws the section on every state change, which closes "Individual steps": open it again each time.
    for (let i = 0; i < 10 && !(await stop.count()); i++) { await sec.locator("details.more").evaluate(d => { d.open = true; }); await page.waitForTimeout(1000); }
    await sec.locator("details.more").evaluate(d => { d.open = true; });
    if (await stop.count()) {
      for (let i = 0; i < 4; i++) {
        try { await sec.locator("details.more").evaluate(d => { d.open = true; }); await stop.click({ timeout: 4000 }); break; } catch (_) { if (i === 3) console.log("STOP: click failed 4 times"); }
      }
      await sec.locator(".chips:has-text('server not running')").waitFor({ timeout: 20000 }).catch(() => console.log("STOP: chip still says the server answers after 20 s")); console.log("STOP: pressed; chips:", (await sec.locator(".chips").innerText()).replace(/\n+/g, " | ")); }
    else console.log("STOP: no Stop button (engine not started by this page, e.g. attach mode)");
    await shot("stopped");
  }
} catch (e) {
  console.log("ERROR:", String(e).split("\n")[0]);
  await shot("error").catch(() => {});
}
await browser.close();
console.log("RESULT", ok ? "PASS" : "FAIL");
process.exit(ok ? 0 : 1);

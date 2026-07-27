import { chromium } from "@playwright/test";
import path from "node:path";
import fs from "node:fs";

const HTML = "file://" + path.resolve("motion-stage.html");
const FPS = Number(process.env.FPS ?? 60);
const MODE = process.env.MODE ?? "probe";
const OUTDIR = process.env.OUTDIR ?? "frames";

(async () => {
  const browser = await chromium.launch();
  const DSF = Number(process.env.DSF ?? 1);
  const page = await (await browser.newContext({ viewport: { width: 1920, height: 1080 }, deviceScaleFactor: DSF })).newPage();
  await page.goto(HTML);
  await page.waitForTimeout(400); // let images decode
  await page.evaluate(async () => { await Promise.all(Array.from(document.images).map(im => im.decode().catch(()=>{}))); });
  const TOTAL = await page.evaluate(() => window.TOTAL);

  if (MODE === "probe") {
    const times = (process.env.TIMES ?? "0.8,2.5,4.0,5.4,6.8,8.3,10.4,12.4,15.7,18.5,22,25,28,31").split(",").map(Number);
    fs.mkdirSync("probes", { recursive: true });
    for (const t of times) {
      await page.evaluate((tt) => window.SEEK(tt), t);
      await page.waitForTimeout(30);
      const name = `probes/p_${String(t).replace('.', '_')}.png`;
      await page.screenshot({ path: name });
      console.log("probe", t, "->", name);
    }
  } else {
    fs.mkdirSync(OUTDIR, { recursive: true });
    const N = Math.round(TOTAL * FPS);
    console.log("rendering", N, "frames @", FPS, "fps, total", TOTAL, "s");
    for (let f = 0; f < N; f++) {
      await page.evaluate((tt) => window.SEEK(tt), f / FPS);
      await page.screenshot({ path: `${OUTDIR}/f_${String(f).padStart(5, "0")}.jpg`, type: "jpeg", quality: 95 });
      if (f % 120 === 0) console.log("frame", f, "/", N);
    }
    console.log("DONE frames");
  }
  await browser.close();
})().catch((e) => { console.error(e); process.exit(1); });

// Captures the README screenshots from `llca demo`, in light and dark.
//
// Usage (from the repository root, after `cargo build --release`):
//   npm install --no-save playwright && npx playwright install chromium
//   node scripts/screenshots.mjs
//
// Writes assets/screenshots/{overview,activity,alert,map,worth}-{light,dark}.png
// at twice the screen resolution, on the demo data over the last 30 days.
// With Python and Pillow installed (pip install pillow), the images are then
// reduced to 256 colours, which divides their size by about three.

import { spawn, spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const out = path.join(root, 'assets', 'screenshots');
const binary = process.env.LLCA_BIN || path.join(root, 'target', 'release', 'llca');
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');

// Start the demo dashboard and read its address (with the access token).
const demo = spawn(binary, ['demo', '--no-browser', '--port', '0'], { stdio: ['ignore', 'pipe', 'inherit'] });
const url = await new Promise((resolve, reject) => {
  let text = '';
  demo.stdout.on('data', (chunk) => {
    text += chunk;
    const match = text.match(/http:\/\/127\.0\.0\.1:\d+\/#token=\S+/);
    if (match) resolve(match[0]);
  });
  demo.on('exit', (code) => reject(new Error(`llca demo stopped (code ${code})`)));
});

const files = [];
const browser = await chromium.launch();
try {
  for (const scheme of ['light', 'dark']) {
    const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2, colorScheme: scheme });
    const page = await context.newPage();
    const shot = async (name, target) => {
      const file = path.join(out, `${name}-${scheme}.png`);
      await (target ? target.screenshot({ path: file }) : page.screenshot({ path: file }));
      files.push(file);
    };
    // The demo banner and the "Filtered by" chips are left out of the pictures.
    const tidy = () =>
      page.evaluate(() => {
        for (const id of ['demo-banner', 'active-filters']) {
          const el = document.getElementById(id);
          if (el) {
            el.hidden = true;
            el.style.display = 'none';
          }
        }
      });

    await page.goto(url);
    await page.waitForTimeout(2000);
    await page.selectOption('#range', '30d');
    await page.waitForTimeout(2000);
    await tidy();
    await shot('overview');
    await shot('map', page.locator('.card:has(h2:text("Where the agents went"))'));
    await shot('worth', page.locator('.card:has(h2:text("Worth a look"))'));
    await shot('alert', page.locator('.secret-alert'));

    await page.click('.page-link[data-page="activity"]');
    await page.waitForTimeout(1500);
    await page.locator('text=list zones').first().click();
    await page.waitForTimeout(800);
    await tidy();
    await shot('activity');
    await context.close();
  }
} finally {
  await browser.close();
  demo.kill();
}

const python = spawnSync(
  'python3',
  [
    '-c',
    `import sys
from PIL import Image
for f in sys.argv[1:]:
    Image.open(f).convert("RGB").quantize(colors=256, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.NONE).save(f, optimize=True)`,
    ...files,
  ],
  { stdio: 'inherit' },
);
console.log(`${files.length} screenshots in ${path.relative(root, out)}${python.status === 0 ? ', reduced to 256 colours' : ' (not reduced: Python with Pillow not found)'}`);

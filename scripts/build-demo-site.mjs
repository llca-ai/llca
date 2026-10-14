#!/usr/bin/env node
// Builds the static demo for GitHub Pages: the real dashboard (static/) plus
// the answers of `llca demo` saved as JSON (see scripts/demo-site/demo-shim.js).
//
//   node scripts/build-demo-site.mjs [OUT_DIR]      (default: demo-site/)
//
// Needs a built binary (cargo build --release) and Node 18 or later.
import {spawn} from 'node:child_process';
import {cp, mkdir, readFile, rm, writeFile} from 'node:fs/promises';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const out = process.argv[2] || join(root, 'demo-site');
const port = 8000 + Math.floor(Math.random() * 1000);
const DAY = 86400000;

// Same wording as the dashboard's own demo, kept off any real path.
const DEMO_DIR = '~/Library/Application Support/llca';

const server = spawn(join(root, 'target/release/llca'), ['demo', '--no-browser', '--port', String(port)], {
  env: {...process.env, LLCA_HOME: join(root, 'target/demo-site-home')},
});
const token = await new Promise((resolve, reject) => {
  let text = '';
  server.stdout.on('data', chunk => {
    text += chunk;
    const m = text.match(/#token=(\S+)/);
    if (m) resolve(m[1]);
  });
  server.on('exit', code => reject(new Error('llca demo stopped early (' + code + ')')));
  setTimeout(() => reject(new Error('llca demo did not start')), 15000);
});

const get = async (path, params = {}) => {
  const query = new URLSearchParams(params);
  const r = await fetch(`http://127.0.0.1:${port}${path}${String(query) ? '?' + query : ''}`, {headers: {Authorization: 'Bearer ' + token}});
  if (!r.ok) throw new Error(path + ' answered ' + r.status);
  return r.json();
};

try {
  await rm(out, {recursive: true, force: true});
  await mkdir(join(out, 'data'), {recursive: true});
  const save = (name, value) => writeFile(join(out, 'data', name + '.json'), JSON.stringify(value));

  const status = await get('/api/status');
  const generated = status.stats.last_event;
  await save('meta', {generated_at: generated});

  // The demo's own folder and the machine's agent folders stay out of the page.
  status.data_dir = DEMO_DIR;
  status.os = 'macos';
  status.installation.codex.config_path = '~/.codex/hooks.json';
  status.installation['claude-code'].config_path = '~/.claude/settings.json';
  await save('status', status);

  // Days start at midnight UTC: the same for every visitor.
  const midnight = new Date(generated);
  midnight.setUTCHours(0, 0, 0, 0);
  const stamp = ms => new Date(ms).toISOString().replace('Z', '+00:00');
  for (const [name, back] of [['today', 0], ['7d', 6], ['30d', 29]]) {
    for (const agent of ['', 'codex', 'claude-code']) {
      const scope = {from: stamp(midnight - back * DAY), to: stamp(+midnight + DAY), tz: '0'};
      if (agent) scope.agent = agent;
      for (const commands of ['1', '0']) {
        const ov = await get('/api/overview', {...scope, commands});
        if (ov.watch) ov.watch.path = DEMO_DIR + '/watch.rules';
        await save(`overview-${name}-${agent || 'all'}${commands === '0' ? '-nocmd' : ''}`, ov);
      }
    }
  }

  // Every action of the 30 days: the page filters them by period and agent.
  const events = [];
  const scope = {from: stamp(midnight - 29 * DAY), to: stamp(+midnight + DAY)};
  for (let page = await get('/api/events', scope); ; page = await get('/api/events', {...scope, before: page.next})) {
    events.push(...page.events);
    if (page.next === null) break;
  }
  await save('events-30d-all', {events});

  // The dashboard itself, unchanged, with relative addresses for a project site.
  for (const file of ['style.css', 'app.js', 'brands.js']) await cp(join(root, 'static', file), join(out, file));
  await cp(join(root, 'assets/llca-logo.svg'), join(out, 'favicon.svg'));
  await cp(join(root, 'scripts/demo-site/theme.js'), join(out, 'theme.js'));
  await cp(join(root, 'scripts/demo-site/demo-shim.js'), join(out, 'demo-shim.js'));
  const csp = "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' blob: data:; base-uri 'none'; form-action 'none'";
  const page = (await readFile(join(root, 'static/index.html'), 'utf8'))
    .replace('<script src="/theme.js"></script>', `<meta http-equiv="Content-Security-Policy" content="${csp}"><script src="theme.js"></script><script src="demo-shim.js"></script>`)
    .replace('href="/style.css"', 'href="style.css"><link rel="icon" href="favicon.svg" type="image/svg+xml"')
    .replace('src="/brands.js"', 'src="brands.js"')
    .replace('src="/app.js"', 'src="app.js"')
    .replace('<a class="brand" href="/">', '<a class="brand" href="./">');
  for (const needle of ['demo-shim.js', 'href="style.css"', 'src="brands.js"', 'src="app.js"']) {
    if (!page.includes(needle)) throw new Error('index.html changed: could not rewrite ' + needle);
  }
  await writeFile(join(out, 'index.html'), page);
  await writeFile(join(out, '.nojekyll'), '');
  console.log(`Demo site written to ${out} (${events.length} actions, newest ${generated})`);
} finally {
  server.kill();
}

// Builds every framework app into dist/<fw>/{index.html,main.js,main.css}. Usage: node build.mjs [react|vue|angular|svelte ...]
import { build } from 'esbuild';
import { mkdirSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';

const want = process.argv.slice(2);
const apps = {
  react: { entry: 'apps/react/main.jsx', title: 'React SPA' },
  vue: { entry: 'apps/vue/main.js', title: 'Vue SPA', alias: { vue: 'vue/dist/vue.esm-bundler.js' }, define: { __VUE_OPTIONS_API__: 'true', __VUE_PROD_DEVTOOLS__: 'false', __VUE_PROD_HYDRATION_MISMATCH_DETAILS__: 'false' } },
  angular: { entry: 'apps/angular/main.ts', title: 'Angular SPA', target: 'es2017', tsconfig: 'apps/angular/tsconfig.json' },
  svelte: { entry: 'apps/svelte/main.js', title: 'Svelte SPA', svelte: true },
};
for (const [fw, a] of Object.entries(apps)) {
  if (want.length && !want.includes(fw)) continue;
  const plugins = [];
  if (a.svelte) { const sv = (await import('esbuild-svelte')).default; plugins.push(sv({ compilerOptions: { css: 'external' } })); }
  mkdirSync(`dist/${fw}`, { recursive: true });
  const r = await build({
    entryPoints: [a.entry], bundle: true, minify: true, sourcemap: false, format: 'esm', target: a.target || 'es2022',
    outfile: `dist/${fw}/main.js`, alias: a.alias, define: { 'process.env.NODE_ENV': '"production"', ...(a.define || {}) },
    tsconfig: a.tsconfig, plugins, loader: { '.css': 'css' }, logLevel: 'warning', metafile: true,
  });
  writeFileSync(`dist/${fw}/index.html`, `<!doctype html>\n<html lang="en"><head><meta charset="utf-8"><title>${a.title}</title><meta name="viewport" content="width=device-width,initial-scale=1">\n<link rel="stylesheet" href="/${fw}/main.css"></head>\n<body>${fw === "angular" ? "<app-root></app-root>" : "<div id=\"root\"></div>"}<script type="module" src="/${fw}/main.js"></script></body></html>\n`);
  const bytes = Object.values(r.metafile.outputs).reduce((s, o) => s + o.bytes, 0);
  console.log(`${fw}: ${(bytes / 1024).toFixed(0)} KiB`);
}

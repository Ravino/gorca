// Bundle the three virtual-scroll demos into self-contained dist/<name>.html files.
// usage: npm install && node build.mjs
import { build } from 'esbuild';
import { mkdirSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const dist = join(here, 'dist');
mkdirSync(dist, { recursive: true });

const common = { bundle: true, write: false, format: 'iife', minify: false, logLevel: 'warning', define: { 'process.env.NODE_ENV': '"production"' } };

const targets = [
  { name: 'tanstack', entry: 'src/tanstack.jsx', opts: { jsx: 'automatic', loader: { '.jsx': 'jsx' } } },
  { name: 'vue-recycle', entry: 'src/vue-recycle.js', opts: { loader: { '.css': 'text' }, define: { ...common.define, __VUE_OPTIONS_API__: 'true', __VUE_PROD_DEVTOOLS__: 'false', __VUE_PROD_HYDRATION_MISMATCH_DETAILS__: 'false' } } },
  {
    name: 'cdk',
    entry: 'src/cdk.ts',
    opts: { target: 'es2017', tsconfigRaw: { compilerOptions: { experimentalDecorators: true, useDefineForClassFields: false, target: 'es2017' } } },
  },
];

for (const t of targets) {
  const r = await build({ ...common, ...t.opts, entryPoints: [join(here, t.entry)], absWorkingDir: here });
  const js = r.outputFiles[0].text.replaceAll('</script', '<\\/script');
  const cssPrebuilt = ''; // CDK scrolling styles ship inside the component (ViewEncapsulation.None)
  const html = `<!doctype html><html lang="en"><head><meta charset="utf-8"><title>b6 virt ${t.name}</title><style>body{font:14px sans-serif;margin:8px}${cssPrebuilt}</style></head><body><script>${js}</script></body></html>`;
  writeFileSync(join(dist, `${t.name}.html`), html);
  console.log(`dist/${t.name}.html ${(html.length / 1024).toFixed(0)} KiB`);
}

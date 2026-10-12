// One static server for all built apps. SPA fallback: /<fw>/anything -> dist/<fw>/index.html unless a file exists.
// usage: node serve.mjs [port]   (default 8089; prints "listening <port>")
import http from 'node:http';
import { readFileSync, existsSync, statSync } from 'node:fs';
import { join, extname, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(fileURLToPath(new URL('.', import.meta.url)), 'dist');
const types = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.css': 'text/css', '.json': 'application/json', '.map': 'application/json' };
const port = +(process.argv[2] || process.env.PORT || 8089);
http.createServer((req, res) => {
  const u = new URL(req.url, 'http://x'); let p = normalize(decodeURIComponent(u.pathname)).replace(/^(\.\.[/\\])+/, '');
  let f = join(root, p);
  if (!f.startsWith(root)) { res.writeHead(403).end(); return; }
  if (!(existsSync(f) && statSync(f).isFile())) {
    const fw = p.split('/')[1];
    f = existsSync(join(root, fw, 'index.html')) ? join(root, fw, 'index.html') : join(root, 'index.html');
    if (!existsSync(f)) { res.writeHead(404).end('not found'); return; }
  }
  res.writeHead(200, { 'content-type': types[extname(f)] || 'application/octet-stream', 'cache-control': 'no-store' });
  res.end(readFileSync(f));
}).listen(port, '127.0.0.1', () => console.log(`listening ${port}`));

// B-6 page driver, injected by the harness's page server before </body>.
// The harness cannot call into the page (no Marionette/CDP on purpose), so the page long-polls the
// harness for commands and acknowledges each one with its own wall-clock timestamps
// (performance.timeOrigin + performance.now(), epoch milliseconds, same clock as the harness).
(function () {
  const now = () => performance.timeOrigin + performance.now();
  const post = (body) =>
    fetch('/b6/ack', { method: 'POST', body, keepalive: true }).catch(() => {});
  addEventListener('load', () => post('id=0&op=loaded&t0=' + now() + '&t1=' + now()));
  function run(c) {
    if (c.op === 'scrollY') { window.scrollTo(0, c.n || 0); return 'scrollY ' + window.scrollY; }
    if (c.op === 'goto') { setTimeout(() => { location.href = c.arg; }, 60); return 'goto ' + c.arg; }
    if (c.op === 'click') { document.querySelector(c.sel).click(); return 'clicked'; }
    if (window.b6ops && window.b6ops[c.op]) return window.b6ops[c.op](c.n || 0, c.arg || '') || 'ok';
    if (window.mut && window.mut[c.op]) { window.mut[c.op](c.n || 0); return 'mut'; }
    throw new Error('unknown op ' + c.op);
  }
  async function loop() {
    for (;;) {
      let c;
      try { c = await (await fetch('/b6/poll')).json(); }
      catch (e) { await new Promise((r) => setTimeout(r, 200)); continue; }
      if (!c || c.op === 'noop') continue;
      const t0 = now();
      let info = '';
      try { info = String(run(c)); } catch (e) { info = 'ERR ' + e; }
      const t1 = now();
      // Ack after the next frame so the harness also learns when layout/a11y could have run.
      requestAnimationFrame(() => post('id=' + c.id + '&op=' + c.op + '&t0=' + t0 + '&t1=' + t1 + '&t2=' + now() + '&info=' + encodeURIComponent(info)));
    }
  }
  loop();
})();

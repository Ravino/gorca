// JSWindowActorChild running with system principal in the content process.
// Walks the in-process LocalAccessible tree (no IPC mirror) and reports timings.
export class GorcaProbeChild extends JSWindowActorChild {
  receiveMessage(msg) {
    if (msg.name !== "walk") return null;
    const svc = Cc["@mozilla.org/accessibilityService;1"].getService(Ci.nsIAccessibilityService);
    const t0 = ChromeUtils.now();
    const doc = svc.getAccessibleFor(this.document);
    const tGet = ChromeUtils.now() - t0;
    if (!doc) return { error: "no doc accessible", tGet };
    let n = 0, named = 0, withState = 0;
    const out = [];   // compact serialisation: [uniqueID, parentIdx, role, state, nameIdx]
    const names = [];
    const t1 = ChromeUtils.now();
    const stack = [[doc, -1]];
    while (stack.length) {
      const [a, p] = stack.pop();
      const idx = n++;
      const nm = a.name;
      let ni = -1;
      if (nm) { named++; ni = names.length; names.push(nm); }
      const st = {}, ex = {}; a.getState(st, ex);
      if (st.value) withState++;
      if (msg.data.serialize) out.push(idx, p, a.role, st.value, ni);
      const kids = [];
      for (let c = a.firstChild; c; c = c.nextSibling) kids.push(c);
      for (let i = kids.length - 1; i >= 0; i--) stack.push([kids[i], idx]);
    }
    const t2 = ChromeUtils.now();
    let bytes = 0;
    if (msg.data.serialize) bytes = JSON.stringify(out).length + JSON.stringify(names).length;
    const t3 = ChromeUtils.now();
    if (msg.data.ship) return { nodes: n, out, names };
    return { tGetMs: Math.round(tGet), nodes: n, named, withState, walkMs: Math.round(t2 - t1), serializeMs: Math.round(t3 - t2), jsonBytes: bytes };
  }
}

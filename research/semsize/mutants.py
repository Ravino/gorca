"""Mutation check: each seeded bug in the A3 oracle must be caught by the F-checks."""
import sys
sys.path.insert(0, "<repo>/tools/research")
import semantic_fusion as sf

orig_status = sf.World2.status
orig_cap = sf.hedge_cap
orig_inexp = dict(sf.INEXPRESSIBLE)

def run(name):
    st = sf.exp_sources(trials=30, n=20, n_events=300, perms=2, verbose=False)
    keys = {k: v for k, v in st.items() if v and not k.endswith(("runs", "checked", "drops", "transitions"))
            and k.split("-")[0] in ("F1", "F11", "F12", "F13", "F14", "F15", "F16", "F18", "F19", "FS5", "predicate", "trail", "bound", "hedge", "LB5")}
    print(f"{name:28s} caught={bool(keys)} {keys}")

run("baseline")
def m_rebound(self, R, x, item):
    (src, anchor, g), seq, rec, b = item
    rec = rec[:3] + (b[2],) + rec[4:]
    return orig_status(self, R, x, ((src, anchor, g), seq, rec, b))
sf.World2.status = m_rebound; run("ignore BindGen"); sf.World2.status = orig_status
def m_epoch(self, R, x, item):
    ep = R.epoch; R.epoch = {}
    try: return orig_status(self, R, x, item)
    finally: R.epoch = ep
sf.World2.status = m_epoch; run("ignore epochs"); sf.World2.status = orig_status
def m_vis(self, R, x, item):
    v = R.vis; R.vis = {}
    try: return orig_status(self, R, x, item)
    finally: R.vis = v
sf.World2.status = m_vis; run("ignore vis_rev"); sf.World2.status = orig_status
def m_floor(self, R, x, item):
    st, h = orig_status(self, R, x, item)
    return ("ok", h if h is not None else 3) if st == "low-confidence" else (st, h)
sf.World2.status = m_floor; run("ignore floor"); sf.World2.status = orig_status
def m_bind(self, R, x, item):
    (k, seq, rec, (bx, q, gen)) = item
    return orig_status(self, R, x, (k, seq, rec, (bx, sf.BQ["Exact"], gen)))
sf.World2.status = m_bind; run("ignore binding minimum"); sf.World2.status = orig_status
sf.hedge_cap = lambda h, tier: h; run("no T3 hedge cap"); sf.hedge_cap = orig_cap
orig_base2 = sf.World2.base2
def m_base2(self, R, x, facts):
    sf.INEXPRESSIBLE.update({"firefox": set(sf.MODELLED), "gtk-newton": set(sf.MODELLED)})
    try: return orig_base2(self, R, x, facts)
    finally: sf.INEXPRESSIBLE.clear(); sf.INEXPRESSIBLE.update(orig_inexp)
sf.World2.base2 = m_base2; run("secondary on all groups (H-3)"); sf.World2.base2 = orig_base2
orig_fuse = sf.World2.fuse_node2
def m_fuse(self, *a):
    self.content_rule = "all"
    return orig_fuse(self, *a)
sf.World2.fuse_node2 = m_fuse; run("content rule 'all'"); sf.World2.fuse_node2 = orig_fuse
orig_admit = sf.admit
def m_scope(ev, scope, layout):
    return orig_admit(ev[:2] + (scope,) + ev[3:], scope, layout)
sf.admit = m_scope; run("no scope pinning"); sf.admit = orig_admit

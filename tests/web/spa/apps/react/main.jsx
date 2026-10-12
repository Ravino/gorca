import React, { useState, useEffect, useRef, useCallback, useSyncExternalStore, useMemo } from 'react';
import { createRoot } from 'react-dom/client';
import { FixedSizeList } from 'react-window';
import { tasks, FEED_N, ROW_H, FRUITS, PRODUCTS, GRID_COLS, gridRows, fmFlag, announce } from '../../shared/data.js';
import { createRouter } from '../../shared/router.js';
import '../../shared/app.css';

const router = createRouter('/react');
const FM = fmFlag();
const usePath = () => useSyncExternalStore(router.subscribe, router.path);
function Link({ to, children, ...p }) {
  return <a href={router.href(to)} onClick={(e) => { e.preventDefault(); router.navigate(to); }} {...p}>{children}</a>;
}

/* ---------- route change ---------- */
let booted = false;   // module-level: Page remounts on every route change
function Page({ title, children }) {
  const h1 = useRef(null);
  const [msg, setMsg] = useState('');
  useEffect(() => {
    document.title = `${title} - React SPA`;
    if (!booted) { booted = true; return; }
    if (FM) { h1.current && h1.current.focus(); setMsg(announce(title)); }
  }, [title]);
  return (<>
    <h1 ref={h1} tabIndex={-1} id="page-title">{title}</h1>
    {FM && <div id="route-announcer" role="status" aria-live="polite" className="sr-only">{msg}</div>}
    {children}
  </>);
}
function Home() {
  const links = ['list/keyed', 'list/unkeyed', 'list/virtual', 'infinite', 'modal', 'live', 'form', 'combobox', 'tabs', 'grid'];
  return <Page title="Home"><p>Scenario index.</p><ul className="plain">{links.map((l) => <li key={l}><Link to={'/' + l}>{l}</Link></li>)}</ul></Page>;
}
function Products() {
  return <Page title="Products"><ul className="cards">{PRODUCTS.map((p) => (
    <li key={p.id} className="card"><h2><Link to={`/route/products/${p.id}`}>{p.name}</Link></h2><p>${p.price}</p></li>))}</ul></Page>;
}
function Product({ id }) { return <Page title={`Product ${id}`}><p>Details for product {id}.</p><Link to="/route/products">Back to products</Link></Page>; }
function About() { return <Page title="About"><p>This is the about page. It has two paragraphs.</p><p>Second paragraph.</p></Page>; }

/* ---------- lists ---------- */
function KeyedList({ keyed }) {
  const [items, setItems] = useState(() => tasks(50));
  const next = useRef(51);
  return (<Page title={keyed ? 'Keyed list' : 'Unkeyed list'}>
    <div role="toolbar" aria-label="List actions">
      <button id="btn-prepend" onClick={() => setItems((a) => [{ id: next.current, text: `Task ${next.current++} (new)` }, ...a])}>Prepend</button>
      <button id="btn-reverse" onClick={() => setItems((a) => [...a].reverse())}>Reverse</button>
      <button id="btn-remove-mid" onClick={() => setItems((a) => a.filter((_, i) => i !== Math.floor(a.length / 2)))}>Remove middle</button>
      <button id="btn-update" onClick={() => setItems((a) => a.map((x) => (x.id === 10 ? { ...x, text: 'Task 10 (edited)' } : x)))}>Edit item 10</button>
    </div>
    <ul className="plain" id="items" aria-label="Tasks">
      {items.map((it, i) => (<li key={keyed ? it.id : i} id={`item-${it.id}`}><span className="t">{it.text}</span> <button>Done</button></li>))}
    </ul>
  </Page>);
}
function VirtualRow({ index, style }) {
  return (<div className="vrow" style={style} role="listitem" id={`row-${index}`} aria-posinset={index + 1} aria-setsize={FEED_N}>
    <span className="title">Feed item {index}</span><button>Like</button></div>);
}
const outer = React.forwardRef((p, ref) => <div ref={ref} {...p} role="list" aria-label="Feed" />);
function Virtual() {
  return <Page title="Virtual feed"><div id="vlist-wrap"><FixedSizeList height={360} width={420} itemCount={FEED_N} itemSize={ROW_H} overscanCount={5} outerElementType={outer} outerRef={(el) => { if (el) { el.id = 'vlist'; el.tabIndex = 0; } }}>
    {VirtualRow}</FixedSizeList></div></Page>;
}

/* ---------- infinite scroll ---------- */
function Infinite() {
  const [n, setN] = useState(20);
  const [loading, setLoading] = useState(false);
  const sent = useRef(null); const box = useRef(null);
  const more = useCallback(() => { setLoading(true); setTimeout(() => { setN((x) => x + 20); setLoading(false); }, 250); }, []);
  useEffect(() => {
    const io = new IntersectionObserver((es) => { if (es[0].isIntersecting && !loading) more(); }, { root: box.current });
    io.observe(sent.current); return () => io.disconnect();
  }, [loading, more, n]);
  return (<Page title="Infinite scroll">
    <div id="inf-scroll" ref={box} aria-busy={loading}>
      <ul className="plain" id="inf-list" aria-label="Articles">{Array.from({ length: n }, (_, i) => <li key={i} id={`art-${i}`}><a href="#x">Article {i}</a></li>)}</ul>
      <div ref={sent} id="sentinel" role="status">{loading ? 'Loading more...' : ''}</div>
    </div></Page>);
}

/* ---------- modal ---------- */
function Modal({ onClose }) {
  const ref = useRef(null);
  useEffect(() => {
    const prev = document.activeElement; const first = ref.current.querySelector('input'); first.focus();
    return () => prev && prev.focus();
  }, []);
  const onKey = (e) => {
    if (e.key === 'Escape') { onClose(); return; }
    if (e.key !== 'Tab') return;
    const f = [...ref.current.querySelectorAll('button, input')];
    const i = f.indexOf(document.activeElement);
    if (e.shiftKey && i <= 0) { e.preventDefault(); f[f.length - 1].focus(); }
    else if (!e.shiftKey && i === f.length - 1) { e.preventDefault(); f[0].focus(); }
  };
  return (<div className="modal-backdrop"><div className="modal" role="dialog" aria-modal="true" aria-labelledby="dlg-title" ref={ref} onKeyDown={onKey} id="dlg">
    <h2 id="dlg-title">Rename item</h2><label>Name <input id="dlg-name" defaultValue="Draft" /></label>
    <div><button id="dlg-ok" onClick={onClose}>OK</button><button id="dlg-cancel" onClick={onClose}>Cancel</button></div></div></div>);
}
function ModalPage() {
  const [open, setOpen] = useState(false);
  return (<Page title="Modal dialog">
    <div id="behind" inert={open ? '' : undefined}><button id="open-modal" onClick={() => setOpen(true)}>Open dialog</button><p>Background content.</p><a href="#x">A link</a></div>
    {open && <Modal onClose={() => setOpen(false)} />}
  </Page>);
}

/* ---------- live floods ---------- */
function Live() {
  const [msgs, setMsgs] = useState([]);
  const [tick, setTick] = useState('AAPL 100.00');
  const run = (fn, n, ms) => { let i = 0; const t = setInterval(() => { fn(i); if (++i >= n) clearInterval(t); }, ms); };
  return (<Page title="Live regions">
    <button id="start-chat" onClick={() => run((i) => setMsgs((m) => [...m, `Message ${i}`]), 100, 20)}>Start chat flood</button>
    <button id="start-ticker" onClick={() => run((i) => setTick(`AAPL ${(100 + i * 0.37).toFixed(2)}`), 30, 100)}>Start ticker</button>
    <div id="chat" role="log" aria-live="polite" aria-label="Chat">{msgs.map((m, i) => <p key={i}>{m}</p>)}</div>
    <div id="ticker" role="status" aria-live="polite" aria-atomic="true">{tick}</div>
  </Page>);
}

/* ---------- form validation ---------- */
function Form() {
  const [v, setV] = useState({ name: '', email: '', age: '' });
  const [touched, setTouched] = useState({});
  const [done, setDone] = useState(false); const [summary, setSummary] = useState(false);
  const refs = { name: useRef(), email: useRef(), age: useRef() };
  const err = (k) => (k === 'name' ? (v.name ? '' : 'Name is required') : k === 'email' ? (/^\S+@\S+\.\S+$/.test(v.email) ? '' : 'Enter a valid email address') : (+v.age >= 18 ? '' : 'Age must be at least 18'));
  const show = (k) => (touched[k] || summary) && err(k);
  const submit = (e) => {
    e.preventDefault(); setSummary(true);
    const bad = ['name', 'email', 'age'].find((k) => err(k));
    if (bad) refs[bad].current.focus(); else setDone(true);
  };
  const field = (k, label, type) => (<div><label htmlFor={`f-${k}`}>{label}</label>
    <input id={`f-${k}`} ref={refs[k]} type={type} value={v[k]} required aria-invalid={show(k) ? 'true' : 'false'} aria-describedby={show(k) ? `${k}-err` : undefined}
      onChange={(e) => { setV({ ...v, [k]: e.target.value }); setDone(false); }} onBlur={() => setTouched({ ...touched, [k]: true })} />
    {show(k) && <span id={`${k}-err`} className="err">{err(k)}</span>}</div>);
  const nErr = ['name', 'email', 'age'].filter(err).length;
  return (<Page title="Sign up form"><form onSubmit={submit} noValidate id="signup">
    {summary && nErr > 0 && <div role="alert" id="err-summary">There are {nErr} errors in the form</div>}
    {field('name', 'Name', 'text')}{field('email', 'Email', 'email')}{field('age', 'Age', 'number')}
    <button type="submit" id="submit">Create account</button>{done && <p role="status" id="ok">Account created</p>}</form></Page>);
}

/* ---------- combobox ---------- */
function Combobox() {
  const [q, setQ] = useState(''); const [open, setOpen] = useState(false); const [act, setAct] = useState(-1);
  const list = useMemo(() => FRUITS.filter((f) => f.toLowerCase().includes(q.toLowerCase())), [q]);
  const key = (e) => {
    if (e.key === 'ArrowDown') { e.preventDefault(); setOpen(true); setAct((a) => Math.min(a + 1, list.length - 1)); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); setAct((a) => Math.max(a - 1, 0)); }
    else if (e.key === 'Enter' && act >= 0) { setQ(list[act]); setOpen(false); setAct(-1); }
    else if (e.key === 'Escape') { setOpen(false); setAct(-1); }
  };
  return (<Page title="Combobox"><label htmlFor="combo">Fruit</label>
    <input id="combo" role="combobox" aria-autocomplete="list" aria-expanded={open} aria-controls="combo-list" aria-activedescendant={act >= 0 ? `opt-${act}` : undefined}
      value={q} onChange={(e) => { setQ(e.target.value); setOpen(true); setAct(-1); }} onKeyDown={key} />
    <ul id="combo-list" role="listbox" aria-label="Fruit suggestions" hidden={!open}>
      {open && list.map((f, i) => <li key={f} id={`opt-${i}`} role="option" aria-selected={i === act} onMouseDown={() => { setQ(f); setOpen(false); }}>{f}</li>)}</ul>
    <div role="status" aria-live="polite" id="combo-count">{open ? `${list.length} suggestions available` : ''}</div></Page>);
}

/* ---------- tabs ---------- */
function Tabs() {
  const [a, setA] = useState(0); const refs = useRef([]);
  const names = ['Overview', 'Specs', 'Reviews', 'Support', 'History'];
  const key = (e) => {
    let n = a; if (e.key === 'ArrowRight') n = (a + 1) % 5; else if (e.key === 'ArrowLeft') n = (a + 4) % 5; else if (e.key === 'Home') n = 0; else if (e.key === 'End') n = 4; else return;
    e.preventDefault(); setA(n); setTimeout(() => refs.current[n] && refs.current[n].focus(), 0);
  };
  return (<Page title="Tabs"><div role="tablist" aria-label="Product info" onKeyDown={key}>
    {names.map((n, i) => <button key={n} role="tab" id={`tab-${i}`} aria-selected={i === a} aria-controls={`panel-${i}`} tabIndex={i === a ? 0 : -1} ref={(el) => (refs.current[i] = el)} onClick={() => setA(i)}>{n}</button>)}</div>
    <div role="tabpanel" id={`panel-${a}`} aria-labelledby={`tab-${a}`} tabIndex={0}>
      <h2>{names[a]}</h2>{Array.from({ length: 5 }, (_, i) => <p key={i}>{names[a]} paragraph {i}.</p>)}</div></Page>);
}

/* ---------- data grid ---------- */
function Grid() {
  const [rows, setRows] = useState(() => gridRows(100)); const [sort, setSort] = useState(null); const [sel, setSel] = useState(new Set());
  const [pos, setPos] = useState([0, 0]); const root = useRef(null);
  const sorted = useMemo(() => { if (!sort) return rows; const [k, d] = sort; return [...rows].sort((x, y) => (x[k] > y[k] ? 1 : x[k] < y[k] ? -1 : 0) * d); }, [rows, sort]);
  const move = (e) => {
    const [r, c] = pos; let nr = r, nc = c;
    if (e.key === 'ArrowDown') nr++; else if (e.key === 'ArrowUp') nr--; else if (e.key === 'ArrowRight') nc++; else if (e.key === 'ArrowLeft') nc--; else return;
    e.preventDefault(); nr = Math.max(0, Math.min(sorted.length - 1, nr)); nc = Math.max(0, Math.min(GRID_COLS.length - 1, nc)); setPos([nr, nc]);
    requestAnimationFrame(() => root.current.querySelector(`[data-r="${nr}"][data-c="${nc}"]`)?.focus());
  };
  return (<Page title="Data grid"><table className="grid" role="grid" id="grid" aria-label="People" aria-rowcount={sorted.length + 1} aria-colcount={GRID_COLS.length} ref={root} onKeyDown={move}>
    <thead><tr role="row">{GRID_COLS.map((c, i) => <th key={c} role="columnheader" aria-sort={sort && sort[0] === c ? (sort[1] > 0 ? 'ascending' : 'descending') : 'none'}>
      <button id={`sort-${c}`} onClick={() => setSort(sort && sort[0] === c ? [c, -sort[1]] : [c, 1])}>{c}</button></th>)}</tr></thead>
    <tbody>{sorted.map((r, ri) => (<tr key={r.id} id={`grow-${r.id}`} role="row" aria-rowindex={ri + 2} aria-selected={sel.has(r.id)}>
      {GRID_COLS.map((c, ci) => <td key={c} role="gridcell" data-r={ri} data-c={ci} tabIndex={pos[0] === ri && pos[1] === ci ? 0 : -1}
        onClick={() => { setPos([ri, ci]); if (c === 'id') setSel((s) => { const n = new Set(s); n.has(r.id) ? n.delete(r.id) : n.add(r.id); return n; }); }}>{r[c]}</td>)}</tr>))}</tbody></table></Page>);
}

/* ---------- shell ---------- */
function App() {
  const p = usePath();
  let v;
  const m = p.match(/^\/route\/products\/(\d+)$/);
  if (p === '/') v = <Home />; else if (p === '/route/products') v = <Products />; else if (m) v = <Product id={m[1]} />;
  else if (p === '/route/about') v = <About />; else if (p === '/list/keyed') v = <KeyedList keyed />; else if (p === '/list/unkeyed') v = <KeyedList />;
  else if (p === '/list/virtual') v = <Virtual />; else if (p === '/infinite') v = <Infinite />; else if (p === '/modal') v = <ModalPage />;
  else if (p === '/live') v = <Live />; else if (p === '/form') v = <Form />; else if (p === '/combobox') v = <Combobox />;
  else if (p === '/tabs') v = <Tabs />; else if (p === '/grid') v = <Grid />; else v = <Page title="Not found"><p>No such page.</p></Page>;
  return (<><header><nav aria-label="Main"><Link to="/" id="nav-home">Home</Link><Link to="/route/products" id="nav-products">Products</Link><Link to="/route/about" id="nav-about">About</Link></nav></header>
    <main id="main">{v}</main><footer><p>Footer text</p></footer></>);
}
createRoot(document.getElementById('root')).render(<App />);

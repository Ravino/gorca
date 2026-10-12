<script>
  import { tick } from 'svelte';
  import { tasks, FEED_N, ROW_H, PRODUCTS, fmFlag, announce } from '../../shared/data.js';
  import { createRouter } from '../../shared/router.js';
  import '../../shared/app.css';

  const router = createRouter('/svelte');
  const FM = fmFlag();
  let path = $state(router.path());
  let msg = $state('');
  let first = true;
  const titleOf = (p) => ({ '/': 'Home', '/route/products': 'Products', '/route/about': 'About', '/list/keyed': 'Keyed list', '/list/unkeyed': 'Unkeyed list', '/list/virtual': 'Virtual feed' }[p] ?? (p.startsWith('/route/products/') ? 'Product ' + p.split('/').pop() : 'Not found'));
  router.subscribe(async (p) => {
    path = p; document.title = titleOf(p) + ' - Svelte SPA';
    if (first) { first = false; }
    if (FM) { await tick(); document.getElementById('page-title')?.focus(); msg = announce(titleOf(p)); }
  });
  first = false;
  const go = (e, to) => { e.preventDefault(); router.navigate(to); };

  let items = $state(tasks(50)); let next = 51;
  const prepend = () => { items = [{ id: next, text: `Task ${next++} (new)` }, ...items]; };
  const reverse = () => { items = [...items].reverse(); };
  const removeMid = () => { items = items.filter((_, i) => i !== Math.floor(items.length / 2)); };
  const update = () => { items = items.map((x) => (x.id === 10 ? { ...x, text: 'Task 10 (edited)' } : x)); };

  let top = $state(0);
  const range = $derived.by(() => { const s = Math.max(0, Math.floor(top / ROW_H) - 5), e = Math.min(FEED_N, Math.ceil((top + 360) / ROW_H) + 5); return Array.from({ length: e - s }, (_, i) => s + i); });
  const links = ['list/keyed', 'list/unkeyed', 'list/virtual'];
</script>

<header><nav aria-label="Main">
  <a href={router.href('/')} id="nav-home" onclick={(e) => go(e, '/')}>Home</a>
  <a href={router.href('/route/products')} id="nav-products" onclick={(e) => go(e, '/route/products')}>Products</a>
  <a href={router.href('/route/about')} id="nav-about" onclick={(e) => go(e, '/route/about')}>About</a>
</nav></header>
<main id="main">
  {#if FM}<div id="route-announcer" role="status" aria-live="polite" class="sr-only">{msg}</div>{/if}
  {#if path === '/'}
    <h1 id="page-title" tabindex="-1">Home</h1><p>Scenario index.</p>
    <ul class="plain">{#each links as l (l)}<li><a href={router.href('/' + l)} onclick={(e) => go(e, '/' + l)}>{l}</a></li>{/each}</ul>
  {:else if path === '/route/products'}
    <h1 id="page-title" tabindex="-1">Products</h1>
    <ul class="cards">{#each PRODUCTS as p (p.id)}<li class="card"><h2><a href={router.href('/route/products/' + p.id)} onclick={(e) => go(e, '/route/products/' + p.id)}>{p.name}</a></h2><p>${p.price}</p></li>{/each}</ul>
  {:else if path.startsWith('/route/products/')}
    <h1 id="page-title" tabindex="-1">Product {path.split('/').pop()}</h1><p>Details for product {path.split('/').pop()}.</p>
    <a href={router.href('/route/products')} onclick={(e) => go(e, '/route/products')}>Back to products</a>
  {:else if path === '/route/about'}
    <h1 id="page-title" tabindex="-1">About</h1><p>This is the about page. It has two paragraphs.</p><p>Second paragraph.</p>
  {:else if path === '/list/keyed' || path === '/list/unkeyed'}
    <h1 id="page-title" tabindex="-1">{path === '/list/keyed' ? 'Keyed list' : 'Unkeyed list'}</h1>
    <div role="toolbar" aria-label="List actions">
      <button id="btn-prepend" onclick={prepend}>Prepend</button><button id="btn-reverse" onclick={reverse}>Reverse</button>
      <button id="btn-remove-mid" onclick={removeMid}>Remove middle</button><button id="btn-update" onclick={update}>Edit item 10</button>
    </div>
    <ul class="plain" id="items" aria-label="Tasks">
      {#if path === '/list/keyed'}
        {#each items as it (it.id)}<li id={'item-' + it.id}><span class="t">{it.text}</span> <button>Done</button></li>{/each}
      {:else}
        {#each items as it}<li id={'item-' + it.id}><span class="t">{it.text}</span> <button>Done</button></li>{/each}
      {/if}
    </ul>
  {:else if path === '/list/virtual'}
    <h1 id="page-title" tabindex="-1">Virtual feed</h1>
    <div id="vlist-wrap"><div id="vlist" role="list" aria-label="Feed" tabindex="0" onscroll={(e) => (top = e.currentTarget.scrollTop)} style="height:360px;width:420px;overflow:auto;position:relative">
      <div style="position:relative;width:100%;height:{FEED_N * ROW_H}px">
        {#each range as i (i)}<div class="vrow" role="listitem" id={'row-' + i} aria-posinset={i + 1} aria-setsize={FEED_N} style="position:absolute;top:{i * ROW_H}px;left:0;width:100%"><span class="title">Feed item {i}</span><button>Like</button></div>{/each}
      </div></div></div>
  {:else}
    <h1 id="page-title" tabindex="-1">Not found</h1>
  {/if}
</main>
<footer><p>Footer text</p></footer>

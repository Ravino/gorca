import { createApp, ref, reactive, computed, nextTick } from 'vue';
import { createRouter, createWebHistory, RouterLink, RouterView } from 'vue-router';
import { tasks, FEED_N, ROW_H, PRODUCTS, fmFlag, announce } from '../../shared/data.js';
import '../../shared/app.css';

const FM = fmFlag();
const msg = ref('');
const H1 = '<h1 id="page-title" tabindex="-1">{{ title }}</h1>';

const Home = { template: `${H1.replace('{{ title }}', 'Home')}<p>Scenario index.</p><ul class="plain"><li v-for="l in links" :key="l"><router-link :to="'/' + l">{{ l }}</router-link></li></ul>`,
  data: () => ({ links: ['list/keyed', 'list/unkeyed', 'list/virtual'] }) };
const Products = { template: `${H1.replace('{{ title }}', 'Products')}<ul class="cards"><li v-for="p in products" :key="p.id" class="card"><h2><router-link :to="'/route/products/' + p.id">{{ p.name }}</router-link></h2><p>\${{ p.price }}</p></li></ul>`,
  data: () => ({ products: PRODUCTS }) };
const Product = { template: `<h1 id="page-title" tabindex="-1">Product {{ $route.params.id }}</h1><p>Details for product {{ $route.params.id }}.</p><router-link to="/route/products">Back to products</router-link>` };
const About = { template: `${H1.replace('{{ title }}', 'About')}<p>This is the about page. It has two paragraphs.</p><p>Second paragraph.</p>` };

const makeList = (keyed) => ({
  setup() {
    const items = ref(tasks(50)); let next = 51;
    return { items, keyed,
      prepend: () => { items.value = [{ id: next, text: `Task ${next++} (new)` }, ...items.value]; },
      reverse: () => { items.value = [...items.value].reverse(); },
      removeMid: () => { items.value = items.value.filter((_, i) => i !== Math.floor(items.value.length / 2)); },
      update: () => { items.value = items.value.map((x) => (x.id === 10 ? { ...x, text: 'Task 10 (edited)' } : x)); } };
  },
  template: `<h1 id="page-title" tabindex="-1">${keyed ? 'Keyed list' : 'Unkeyed list'}</h1>
  <div role="toolbar" aria-label="List actions"><button id="btn-prepend" @click="prepend">Prepend</button><button id="btn-reverse" @click="reverse">Reverse</button><button id="btn-remove-mid" @click="removeMid">Remove middle</button><button id="btn-update" @click="update">Edit item 10</button></div>
  <ul class="plain" id="items" aria-label="Tasks">${keyed
    ? '<li v-for="it in items" :key="it.id" :id="\'item-\' + it.id"><span class="t">{{ it.text }}</span> <button>Done</button></li>'
    : '<li v-for="it in items" :id="\'item-\' + it.id"><span class="t">{{ it.text }}</span> <button>Done</button></li>'}</ul>`,
});

const Virtual = {
  setup() {
    const top = ref(0);
    const range = computed(() => { const s = Math.max(0, Math.floor(top.value / ROW_H) - 5), e = Math.min(FEED_N, Math.ceil((top.value + 360) / ROW_H) + 5); return Array.from({ length: e - s }, (_, i) => s + i); });
    return { top, range, N: FEED_N, H: ROW_H, onScroll: (e) => { top.value = e.target.scrollTop; } };
  },
  template: `<h1 id="page-title" tabindex="-1">Virtual feed</h1><div id="vlist-wrap"><div id="vlist" role="list" aria-label="Feed" tabindex="0" @scroll="onScroll" style="height:360px;width:420px;overflow:auto;position:relative">
    <div :style="{height: N*H + 'px', width: '100%', position: 'relative'}"><div v-for="i in range" :key="i" class="vrow" role="listitem" :id="'row-' + i" :aria-posinset="i + 1" :aria-setsize="N" :style="{position:'absolute', top: i*H + 'px', left: 0, width: '100%'}"><span class="title">Feed item {{ i }}</span><button>Like</button></div></div></div></div>`,
};

const routes = [
  { path: '/', component: Home, meta: { title: 'Home' } }, { path: '/route/products', component: Products, meta: { title: 'Products' } },
  { path: '/route/products/:id', component: Product, meta: { title: 'Product' } }, { path: '/route/about', component: About, meta: { title: 'About' } },
  { path: '/list/keyed', component: makeList(true), meta: { title: 'Keyed list' } }, { path: '/list/unkeyed', component: makeList(false), meta: { title: 'Unkeyed list' } },
  { path: '/list/virtual', component: Virtual, meta: { title: 'Virtual feed' } },
];
const router = createRouter({ history: createWebHistory('/vue/'), routes });
let firstNav = true;
router.afterEach(async (to) => {
  document.title = `${to.meta.title} - Vue SPA`;
  if (firstNav) { firstNav = false; return; }
  if (FM) { await nextTick(); document.getElementById('page-title')?.focus(); msg.value = announce(to.meta.title); }
});

const App = {
  components: { RouterLink, RouterView },
  setup: () => ({ msg, FM }),
  template: `<header><nav aria-label="Main"><router-link to="/" id="nav-home">Home</router-link><router-link to="/route/products" id="nav-products">Products</router-link><router-link to="/route/about" id="nav-about">About</router-link></nav></header>
  <main id="main"><div v-if="FM" id="route-announcer" role="status" aria-live="polite" class="sr-only">{{ msg }}</div><router-view /></main><footer><p>Footer text</p></footer>`,
};
createApp(App).use(router).mount('#root');

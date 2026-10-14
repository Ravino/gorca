// TBD-WEB-3: vue-virtual-scroller RecycleScroller (Vue 3): a pool of item views that are reused
// (and repositioned with transform) for other items while scrolling.
import { createApp, h } from 'vue/dist/vue.esm-bundler.js';
import { RecycleScroller } from 'vue-virtual-scroller';
import css from 'vue-virtual-scroller/dist/vue-virtual-scroller.css';

const q = new URLSearchParams(location.search);
const N = Number(q.get('n') || 10000);
const items = Array.from({ length: N }, (_, i) => ({ id: i }));

const style = document.createElement('style');
style.textContent = css + '\n.scroller { height: 400px; width: 520px; }';
document.head.append(style);
document.body.insertAdjacentHTML('afterbegin', '<h1>vue-virtual-scroller RecycleScroller</h1><div id="root"></div>');

let scrollerEl = null;
const app = createApp({
  components: { RecycleScroller },
  data: () => ({ items, N }),
  mounted() { scrollerEl = this.$el; },
  template: `
    <RecycleScroller class="scroller" :items="items" :item-size="32" key-field="id" role="list" aria-label="Items" v-slot="{ item, index }">
      <div role="listitem" :id="'row-' + item.id" :aria-label="'Row ' + item.id" :aria-posinset="index + 1" :aria-setsize="N">Row {{ item.id }}</div>
    </RecycleScroller>`,
});
app.mount('#root');
window.b6ops = {
  scrollTo(n) { scrollerEl.scrollTop = n * 32; return 'scrollTo ' + n; },
  scrollBy(px) { scrollerEl.scrollTop += px; return 'scrollBy ' + px; },
};

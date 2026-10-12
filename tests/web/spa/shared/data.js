// Deterministic data shared by all framework apps.
export const tasks = (n, start = 1) => Array.from({ length: n }, (_, i) => ({ id: start + i, text: `Task ${start + i}` }));
export const FEED_N = 10000;
export const ROW_H = 40;
export const FRUITS = ['Apple','Apricot','Avocado','Banana','Blackberry','Blueberry','Cherry','Coconut','Cranberry','Date','Dragonfruit','Elderberry','Fig','Grape','Grapefruit','Guava','Kiwi','Lemon','Lime','Lychee','Mango','Melon','Nectarine','Orange','Papaya','Peach','Pear','Persimmon','Pineapple','Plum','Pomegranate','Quince','Raspberry','Strawberry','Tangerine','Watermelon'];
export const PRODUCTS = Array.from({ length: 12 }, (_, i) => ({ id: i + 1, name: `Product ${i + 1}`, price: 10 + i * 3 }));
export const GRID_COLS = ['id', 'name', 'city', 'score', 'status', 'date'];
const CITIES = ['Berlin','Lisbon','Oslo','Paris','Rome','Vienna','Madrid','Prague'];
export const gridRows = (n = 100) => Array.from({ length: n }, (_, i) => ({
  id: i + 1, name: `Person ${((i * 37) % n) + 1}`, city: CITIES[(i * 5) % CITIES.length], score: (i * 7919) % 1000,
  status: i % 3 ? 'active' : 'idle', date: `2026-${String((i % 12) + 1).padStart(2, '0')}-${String((i % 27) + 1).padStart(2, '0')}`,
}));
export const fmFlag = () => new URLSearchParams(location.search).get('fm') === '1';
export const announce = (title) => `Navigated to ${title}`;

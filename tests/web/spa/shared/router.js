// Tiny pushState router used by the React and Svelte apps (Vue uses vue-router, Angular @angular/router).
export function createRouter(base) {
  const subs = new Set();
  const path = () => { let p = location.pathname; if (p.startsWith(base)) p = p.slice(base.length) || '/'; return p; };
  const notify = () => subs.forEach((f) => f(path()));
  addEventListener('popstate', notify);
  return {
    path,
    navigate(to) { history.pushState({}, '', base + (to === '/' ? '/' : to) + location.search); notify(); },
    subscribe(f) { subs.add(f); return () => subs.delete(f); },
    href: (to) => base + (to === '/' ? '/' : to) + location.search,
  };
}

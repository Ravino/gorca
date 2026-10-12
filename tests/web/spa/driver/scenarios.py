"""Scenario table. Each scenario = url template + steps. Steps are dicts: op in click|key|type|wheel|eval; setup=True steps are
executed (and settled) but not reported. The DOM contract (ids, roles) is identical across the four framework apps, so the same
steps drive all of them. See README.md for the contract."""

def st(label, op, *args, setup=False, quiet=0.7, maxwait=6.0):
    return dict(label=label, op=op, args=list(args), setup=setup, quiet=quiet, maxwait=maxwait)

ROUTE = [st('click Products', 'click', '#nav-products'), st('click first product', 'click', '.cards a'),
         st('click Back-to-products', 'click', 'main a'), st('click About', 'click', '#nav-about'),
         st('history.back()', 'eval', 'history.back()')]
LIST = [st('prepend', 'click', '#btn-prepend'), st('reverse', 'click', '#btn-reverse'),
        st('remove middle', 'click', '#btn-remove-mid'), st('edit item 10', 'click', '#btn-update')]
VIRT = [st('focus list', 'eval', "document.getElementById('vlist').focus()", setup=True),
        st('wheel +120', 'wheel', '#vlist', 120), st('wheel +600', 'wheel', '#vlist', 600), st('key PageDown', 'key', 'PageDown'),
        st('jump scrollTop=200000', 'eval', "document.getElementById('vlist').scrollTop=200000"),
        st('key End', 'key', 'End')]

REACT_ONLY = {
    'infinite': ('/react/infinite', [st('wheel +3000 (load 1)', 'wheel', '#inf-scroll', 3000, maxwait=4), st('wheel +3000 (load 2)', 'wheel', '#inf-scroll', 3000, maxwait=4),
                                      st('wheel +3000 (load 3)', 'wheel', '#inf-scroll', 3000, maxwait=4)]),
    'modal': ('/react/modal', [st('open dialog', 'click', '#open-modal'), st('Tab', 'key', 'Tab'), st('Tab', 'key', 'Tab'), st('Tab (wrap)', 'key', 'Tab'),
                               st('Escape', 'key', 'Escape')]),
    'live': ('/react/live', [st('chat flood 100 msgs/2s', 'click', '#start-chat', quiet=1.2, maxwait=10), st('ticker 30 updates/3s', 'click', '#start-ticker', quiet=1.2, maxwait=10)]),
    'form': ('/react/form', [st('focus email', 'click', '#f-email', setup=True), st('type "bad"', 'type', 'bad'), st('Tab (blur: error appears)', 'key', 'Tab'),
                             st('Enter (submit: summary + focus first invalid)', 'key', 'Enter'), st('type name "Al" (error clears?)', 'type', 'Al')]),
    'combobox': ('/react/combobox', [st('focus', 'click', '#combo', setup=True), st('type "a"', 'type', 'a'), st('type "p"', 'type', 'p'), st('ArrowDown', 'key', 'ArrowDown'),
                                     st('ArrowDown', 'key', 'ArrowDown'), st('Enter (select)', 'key', 'Enter')]),
    'tabs': ('/react/tabs', [st('focus tab 0', 'click', '#tab-0', setup=True), st('ArrowRight', 'key', 'ArrowRight'), st('ArrowRight', 'key', 'ArrowRight'), st('Home', 'key', 'Home')]),
    'grid': ('/react/grid', [st('focus cell', 'click', '[data-r="0"][data-c="1"]', setup=True), st('ArrowDown', 'key', 'ArrowDown'), st('ArrowRight', 'key', 'ArrowRight'),
                             st('sort score asc', 'click', '#sort-score'), st('sort score desc', 'click', '#sort-score')]),
}
# scenarios whose initial tree is dumped to research/web-graph/real-trees (kept at <= ~50 dumps in total)
DUMP = {'react/route-focus', 'react/keyed', 'react/virtual', 'react/infinite', 'react/modal', 'react/live', 'react/form', 'react/combobox', 'react/tabs', 'react/grid',
        'vue/keyed', 'vue/virtual', 'angular/keyed', 'angular/virtual', 'svelte/keyed', 'svelte/virtual'}
# final-state dumps taken after the last reported step
DUMP_AFTER = {'react/virtual', 'react/grid', 'react/modal', 'react/combobox'}

def all_scenarios():
    out = {}
    for fw in ('react', 'vue', 'angular', 'svelte'):
        out[f'{fw}/route-nofocus'] = (f'/{fw}/', ROUTE)
        out[f'{fw}/route-focus'] = (f'/{fw}/?fm=1', ROUTE)
        out[f'{fw}/keyed'] = (f'/{fw}/list/keyed', LIST)
        out[f'{fw}/unkeyed'] = (f'/{fw}/list/unkeyed', LIST)
        out[f'{fw}/virtual'] = (f'/{fw}/list/virtual', VIRT)
    for k, v in REACT_ONLY.items():
        out[f'react/{k}'] = v
    return out

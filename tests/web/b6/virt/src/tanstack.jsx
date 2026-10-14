// TBD-WEB-3: TanStack Virtual under React 19. Two DOM disciplines of the same library:
//   ?keys=index (default): React keys rows by item index  -> rows are mounted/unmounted on scroll
//   ?keys=slot          : React keys rows by index % POOL -> the same DOM nodes are reused for other
//                         items (what a recycling virtualiser such as RecycleScroller does)
import React, { useRef } from 'react';
import { createRoot } from 'react-dom/client';
import { useVirtualizer } from '@tanstack/react-virtual';

const q = new URLSearchParams(location.search);
const N = Number(q.get('n') || 10000);
const KEYS = q.get('keys') || 'index';
const POOL = 24;

function App() {
  const parentRef = useRef(null);
  const v = useVirtualizer({ count: N, getScrollElement: () => parentRef.current, estimateSize: () => 32, overscan: 3 });
  window.b6ops = {
    scrollTo(n) { parentRef.current.scrollTop = n * 32; return 'scrollTo ' + n; },
    scrollBy(px) { parentRef.current.scrollTop += px; return 'scrollBy ' + px; },
  };
  return (
    <div ref={parentRef} id="vp" role="list" aria-label="Items" tabIndex={0} style={{ height: 400, width: 520, overflow: 'auto' }}>
      <div style={{ height: v.getTotalSize(), position: 'relative' }}>
        {v.getVirtualItems().map((r) => (
          <div
            key={KEYS === 'slot' ? r.index % POOL : r.key}
            role="listitem"
            id={'row-' + r.index}
            aria-label={'Row ' + r.index}
            aria-posinset={r.index + 1}
            aria-setsize={N}
            style={{ position: 'absolute', top: 0, left: 0, width: '100%', height: 32, transform: `translateY(${r.start}px)` }}
          >
            Row {r.index}
          </div>
        ))}
      </div>
    </div>
  );
}

document.body.insertAdjacentHTML('afterbegin', '<h1>TanStack Virtual (' + KEYS + ')</h1><div id="root"></div>');
createRoot(document.getElementById('root')).render(<App />);

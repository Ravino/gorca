wit_bindgen::generate!({ path: "../wit", world: "plugin" });
use spike::plugin::{
    hostapi,
};
use std::cell::RefCell;

thread_local! { static STATE: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) }; }

struct G;
impl Guest for G {
    fn noop() -> u32 {
        1
    }
    fn ten(r: Rec10) -> u32 {
        (r.a.len()
            + r.b.len()
            + r.c.len()
            + r.d.len()
            + r.e.len()
            + r.f.len()
            + r.g.len()
            + r.h.len()
            + r.i.len()
            + r.j.len()) as u32
    }
    fn list20() -> Vec<Item> {
        (0..20)
            .map(|i| Item { id: i, kind: (i % 7) as u8, name: format!("item-{i}") })
            .collect()
    }
    fn call_host(n: u32) -> u32 {
        let mut s = 0u32;
        for _ in 0..n {
            s = s.wrapping_add(hostapi::ping());
        }
        s
    }
    fn spin(n: u64) -> u64 {
        let mut x = 1u64;
        for i in 0..n {
            x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(i));
        }
        x
    }
    fn forever() {
        let mut x = 0u64;
        loop {
            x = std::hint::black_box(x.wrapping_add(1));
        }
    }
    fn trap() {
        std::arch::wasm32::unreachable();
    }
    fn do_panic() {
        panic!("guest panic");
    }
    fn grow(bytes: u32) -> u32 {
        STATE.with(|s| {
            let mut v = vec![0u8; bytes as usize];
            for i in (0..v.len()).step_by(4096) {
                v[i] = 1;
            }
            *s.borrow_mut() = v;
        });
        bytes
    }
}
export!(G);

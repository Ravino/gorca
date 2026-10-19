use std::sync::Arc;
#[repr(align(8))] #[allow(dead_code)] struct Sem64([u8; 64]);
#[repr(align(8))] #[allow(dead_code)] struct Sem80([u8; 80]);
#[allow(dead_code)] struct R2 { sem: Sem64, name: Option<Arc<str>>, prov: u64, hedge: u32, sem_rev: u32, known: u16, stale: u16, fallback: u16 }
#[allow(dead_code)] struct X32 { sem: Sem64, name: Option<Arc<str>>, prov: [u64; 2], hedge: u64, taint: u64, sem_rev: u32, known: u32, stale: u32, fallback: u32 }
#[allow(dead_code)] struct X32u128 { sem: Sem64, name: Option<Arc<str>>, prov: u128, hedge: u64, taint: u64, sem_rev: u32, known: u32, stale: u32, fallback: u32 }
#[allow(dead_code)] struct X32s80 { sem: Sem80, name: Option<Arc<str>>, prov: [u64; 2], hedge: u64, taint: u64, sem_rev: u32, known: u32, stale: u32, fallback: u32 }
#[allow(dead_code)] struct X64 { sem: Sem80, name: Option<Arc<str>>, prov: [u64; 4], hedge: u128, taint: u128, sem_rev: u32, known: u64, stale: u64, fallback: u64 }
fn main() {
    println!("r2-16 {} | x32 {} | x32 with u128 ProvTag {} | x32 + Semantics 80 {} | x64 {}",
        std::mem::size_of::<R2>(), std::mem::size_of::<X32>(), std::mem::size_of::<X32u128>(),
        std::mem::size_of::<X32s80>(), std::mem::size_of::<X64>());
}

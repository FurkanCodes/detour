//! Repeatable CPU/allocator benchmark; does not capture or transmit traffic.
use detour_core::DomainList;
use detour_engine::{packet, Strategy};
use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

struct CountingAllocator;
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        System.realloc(ptr, layout, size)
    }
}
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn measure(name: &str, mut work: impl FnMut()) {
    const RUNS: usize = 200_000;
    for _ in 0..1000 {
        work();
    }
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    let start = Instant::now();
    for _ in 0..RUNS {
        work();
    }
    let elapsed = start.elapsed();
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    println!(
        "{name}: {:.0} ns/op, {:.2} allocations/op, {:.2} M ops/s",
        elapsed.as_nanos() as f64 / RUNS as f64,
        allocations as f64 / RUNS as f64,
        RUNS as f64 / elapsed.as_secs_f64() / 1_000_000.0
    );
}

fn main() {
    let hosts = DomainList::parse("discord.com\nroblox.com");
    let strategy = Strategy::default();
    let packet = hello_packet("gateway.discord.com");
    assert!(strategy.plan(&packet, Some(&hosts)).is_some());
    measure("domain match", || {
        black_box(hosts.matches(black_box("gateway.discord.com")));
    });
    measure("owned TLS plan", || {
        black_box(strategy.plan(black_box(&packet), Some(&hosts)));
    });
    let mut planner = detour_engine::strategy::PacketPlanner::default();
    measure("reused TLS plan", || {
        black_box(planner.plan(&strategy, black_box(&packet), Some(&hosts)));
    });
}

fn hello_packet(host: &str) -> Vec<u8> {
    let mut extension = vec![0, 0];
    extension.extend_from_slice(&((host.len() + 5) as u16).to_be_bytes());
    extension.extend_from_slice(&((host.len() + 3) as u16).to_be_bytes());
    extension.push(0);
    extension.extend_from_slice(&(host.len() as u16).to_be_bytes());
    extension.extend_from_slice(host.as_bytes());
    let mut body = vec![3, 3];
    body.extend_from_slice(&[7; 32]);
    body.extend_from_slice(&[0, 0, 2, 0x13, 1, 1, 0]);
    body.extend_from_slice(&(extension.len() as u16).to_be_bytes());
    body.extend(extension);
    let mut hello = vec![0x16, 3, 1];
    hello.extend_from_slice(&((body.len() + 4) as u16).to_be_bytes());
    hello.push(1);
    hello.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
    hello.extend(body);
    let mut packet = vec![0u8; 40];
    packet[0] = 0x45;
    packet[6] = 0x40;
    packet[8] = 64;
    packet[9] = packet::TCP;
    packet[12..16].copy_from_slice(&[10, 0, 0, 1]);
    packet[16..20].copy_from_slice(&[1, 1, 1, 1]);
    packet::put16(&mut packet, 20, 50000);
    packet::put16(&mut packet, 22, 443);
    packet[32] = 0x50;
    packet[33] = 0x18;
    packet.extend(hello);
    let len = packet.len() as u16;
    packet::put16(&mut packet, 2, len);
    packet
}

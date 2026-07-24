//! Subject of subprocess invocation in `tests/coordination_debug_cli.rs`.
//! Two globally-serial dynamic tests: run with real thread concurrency,
//! whichever starts second is guaranteed to hit a genuine coordination
//! wait on its first attempt — no scheduler-timing bet involved, since
//! global-serial blocks against anything already running by definition.

fn main() {
    let mut runner = skuld::TestRunner::new();
    runner.add_serial("hold_a", &[], false, || {
        std::thread::sleep(std::time::Duration::from_millis(300));
    });
    runner.add_serial("hold_b", &[], false, || {
        std::thread::sleep(std::time::Duration::from_millis(300));
    });
    runner.run();
}

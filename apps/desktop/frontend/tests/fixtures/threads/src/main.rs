// Fixture for `tests/run_tree_e2e.mjs`: a small threaded program whose run
// tree is known exactly. Not built — only analyzed. Line numbers matter: the
// spawned closure below is named `<spawned@L10>` after its line.
use std::thread;

fn main() {
    let rx = setup();
    let h = thread::spawn(
        // the closure starts on line 10
        move || worker(rx),
    );
    thread::spawn(listener);
    report();
    h.join().unwrap();
}

fn setup() -> u32 {
    1
}

fn worker(n: u32) -> u32 {
    crunch(n)
}

fn crunch(n: u32) -> u32 {
    if n > 10 {
        crunch(n - 1)
    } else {
        n
    }
}

fn listener() {
    accept();
}

fn accept() {}

fn report() {}

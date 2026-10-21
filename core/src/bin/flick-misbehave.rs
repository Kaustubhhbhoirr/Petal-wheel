//! A deliberately misbehaving worker used only by the supervisor tests.
//!
//! Usage: `flick-misbehave <mode> [arg]`
//!   echo           read the request and send it back as `{"ok":true,"echo":...}`
//!   memory-bomb    allocate and touch memory until something stops it
//!   spin           burn CPU forever
//!   spawn-spin F   start a child that spins, write its PID to file F, then spin
//!   crash          exit with code 3 after writing to stderr
//!   garbage        write something that is not JSON

use std::io::{Read, Write};

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_default();
    match mode.as_str() {
        "echo" => {
            let mut input = String::new();
            std::io::stdin().read_to_string(&mut input).unwrap();
            let request: serde_json::Value = serde_json::from_str(&input).unwrap();
            println!("{}", serde_json::json!({ "ok": true, "echo": request }));
        }
        "memory-bomb" => {
            let mut hoard: Vec<Vec<u8>> = Vec::new();
            loop {
                // Touch every page so the memory is really committed.
                hoard.push(vec![1u8; 16 << 20]);
            }
        }
        "spin" => spin(),
        "spawn-spin" => {
            let pid_file = args.next().expect("pid file");
            // Never waited on on purpose: the test checks that the job kills it.
            #[allow(clippy::zombie_processes)]
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("spin")
                .spawn()
                .unwrap();
            std::fs::write(pid_file, child.id().to_string()).unwrap();
            spin();
        }
        "crash" => {
            eprintln!("simulated crash");
            std::process::exit(3);
        }
        "garbage" => {
            std::io::stdout()
                .write_all(b"<html>not json</html>")
                .unwrap();
        }
        other => panic!("unknown mode {other:?}"),
    }
}

fn spin() -> ! {
    let mut x: u64 = 0;
    loop {
        x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
    }
}

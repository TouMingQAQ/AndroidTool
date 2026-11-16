use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    thread,
    time::Duration,
};

fn main() {
    let root = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let args: Vec<String> = std::env::args().skip(1).collect();
    writeln!(
        OpenOptions::new()
            .append(true)
            .create(true)
            .open(root.join("commands.log"))
            .unwrap(),
        "{args:?}"
    )
    .unwrap();
    if args.first().map(String::as_str) == Some("devices") {
        println!("List of devices attached");
        if !root.join("empty").exists() {
            println!("device-one device model:Mock_One");
            println!("device-two device model:Mock_Two");
            println!("locked unauthorized");
            println!("gone offline");
        }
        return;
    }
    assert_eq!(args[0], "-s", "all device commands require -s");
    assert!(args[1] == "device-one" || args[1] == "device-two");
    if root.join("fail").exists() {
        eprintln!("mock device disconnected");
        std::process::exit(1);
    }
    if root.join("slow").exists() {
        thread::sleep(Duration::from_secs(60));
    }
    match args[2].as_str() {
        "exec-out" => io::stdout()
            .write_all(&fs::read(root.join("screen.png")).unwrap())
            .unwrap(),
        "install" => println!("Success"),
        "pull" => {
            fs::write(&args[4], b"downloaded").unwrap();
            println!("pulled");
        }
        "push" => println!("pushed"),
        "logcat" => {
            println!("11-12 00:00:00.000 1 2 I Mock: needle");
            println!("11-12 00:00:01.000 1 2 I Mock: other");
        }
        "shell" => {
            if args[3] == "cat" {
                println!("<?xml version=\"1.0\"?><hierarchy/>");
            } else if args[3] == "pm" {
                println!("package:com.example.mock");
            } else if args[3] == "wm" {
                println!("Physical size: 1080x2400");
            } else if args[3] == "ls" && args.iter().any(|arg| arg.contains("huge")) {
                print!("{}", "x".repeat(300_000));
            } else {
                println!("OK");
            }
        }
        _ => panic!("unexpected command: {:?}", args),
    }
}

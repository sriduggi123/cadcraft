//! Ask the assistant from a terminal, against a drawing in memory:
//! `cargo run -p cadcraft-ai --example ask -- [--server wss://host] [--open file.dxf] [--yes] "request"`.
//! Prints the chat as it goes and the entity counts at the end. `--yes` allows what the assistant
//! asks to do; its questions are answered from the keyboard; picks in the drawing are cancelled.

use cadcraft_ai::{Answer, Assistant, Entry, Need};
use cadcraft_engine::Session;
use serde_json::json;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut take = |flag: &str| {
        let i = args.iter().position(|a| a == flag)?;
        let v = args.get(i + 1).cloned();
        args.drain(i..(i + 2).min(args.len()));
        v
    };
    let server = take("--server").unwrap_or_else(|| cadcraft_engine::cmd::collab::DEFAULT_SERVER.to_string());
    let open = take("--open");
    let yes = args.iter().position(|a| a == "--yes").map(|i| args.remove(i)).is_some();
    let prompt = args.join(" ");
    if prompt.trim().is_empty() {
        eprintln!("usage: ask [--server wss://host] [--open file.dxf] \"request\"");
        std::process::exit(2);
    }
    cadcraft_engine::cmd::file::set_io(cadcraft_engine::cmd::file::IoHooks {
        read: |b, name| cadcraft_io::read(b, name).map_err(|e| e.to_string()),
        write: |d, name| cadcraft_io::write(d, name).map_err(|e| e.to_string()),
        plot: None,
    });
    let mut s = Session::new();
    let opened = match &open {
        Some(path) => s.execute("open", &json!({ "path": path })),
        None => s.execute("new", &json!({})),
    };
    if let Err(e) = opened {
        eprintln!("can't open the drawing: {e}");
        std::process::exit(1);
    }
    let mut a = Assistant::new(&server);
    a.ask(&mut s, &prompt);
    let mut shown = 0;
    while a.busy() {
        a.tick(&mut s);
        for e in a.log.iter().skip(shown) {
            match e {
                Entry::User(t) => println!("> {t}"),
                Entry::Assistant(t) => println!("{t}"),
                Entry::Tool { line, ok } => println!("  {} {line}", if *ok { "✓" } else { "✗" }),
                Entry::Error(e) => println!("! {e}"),
            }
        }
        shown = a.log.len();
        match a.need().cloned() {
            Some(Need::Confirm { what, .. }) => {
                println!("? allow: {what} -> {}", if yes { "yes" } else { "no" });
                a.answer(&mut s, if yes { Answer::Allow } else { Answer::Deny });
            }
            Some(Need::Ask { question, options }) => {
                println!("? {question} {options:?}");
                let mut line = String::new();
                let _ = std::io::stdin().read_line(&mut line);
                a.answer(&mut s, Answer::Text(line.trim().to_string()));
            }
            Some(Need::Points { .. } | Need::Objects { .. }) => {
                println!("? (a pick in the drawing; cancelled here)");
                a.answer(&mut s, Answer::Cancel);
            }
            None => {}
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    for e in a.log.iter().skip(shown) {
        match e {
            Entry::Assistant(t) => println!("{t}"),
            Entry::Tool { line, ok } => println!("  {} {line}", if *ok { "✓" } else { "✗" }),
            Entry::Error(e) => println!("! {e}"),
            Entry::User(_) => {}
        }
    }
    let summary = s.execute("drawing.inspect", &json!({"entities": false})).unwrap_or_default();
    println!("counts: {}", summary["counts"]);
    if let Ok(st) = s.state() {
        println!("undo: {:?}", st.undo.iter().map(|u| u.label.as_str()).collect::<Vec<_>>());
    }
}

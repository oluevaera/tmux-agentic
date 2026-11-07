//! A stand-in coding agent for the live tests: copied to a file named claude/codex/opencode, it shows the screen in
//! $FAKE_DIR/<folder name>.screen, and logs its arguments to <folder name>.args and each line typed into it to .typed.
//! Named claude, /cd moves the process as the real one does, unless <folder name>.decline exists: you said no.
use std::io::{BufRead, Write};
use std::{env, fs, thread, time::Duration};

fn main() {
    let dir = env::var("FAKE_DIR").expect("FAKE_DIR");
    let here = env::current_dir().unwrap();
    let name = here.file_name().unwrap().to_string_lossy().into_owned();
    let screen = format!("{dir}/{name}.screen");
    let typed = format!("{dir}/{name}.typed");
    let args: Vec<String> = env::args().skip(1).collect();
    let claude = env::args().next().is_some_and(|me| me.ends_with("claude"));
    let decline = format!("{dir}/{name}.decline");
    fs::write(format!("{dir}/{name}.args"), args.join(" ")).unwrap();
    let path = screen.clone();
    print!("\x1b[?1049h"); // the alternate screen: a redraw leaves no old text in the history state detection reads
    thread::spawn(move || {
        let mut shown = None;
        loop {
            let text = fs::read_to_string(&path).unwrap_or_else(|_| "❯ ".into());
            if shown.as_ref() != Some(&text) {
                print!("\x1b[2J\x1b[H{text}");
                std::io::stdout().flush().unwrap();
                shown = Some(text);
            }
            thread::sleep(Duration::from_millis(100));
        }
    });
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let mut log = fs::OpenOptions::new().create(true).append(true).open(&typed).unwrap();
        writeln!(log, "{line}").unwrap();
        if let Some(folder) = line.strip_prefix("/cd ").filter(|_| claude && !fs::exists(&decline).unwrap_or(false)) {
            let _ = env::set_current_dir(folder);
        }
    }
}

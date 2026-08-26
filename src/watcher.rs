//! File watcher for config.toml auto-reload
//! Uses `notify` crate to watch parent directory and signal main thread via atomic flag.
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub static CONFIG_DIRTY: AtomicBool = AtomicBool::new(false);
static mut WATCHER: Option<notify::RecommendedWatcher> = None;

/// Spawn watcher for config file. Watches parent directory, debounces 500ms.
pub fn spawn_watcher(path: PathBuf) {
    let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    let file_name = path.file_name().map(|n| n.to_owned());

    std::thread::spawn(move || {
        use notify::{Watcher, RecursiveMode, EventKind, Config};

        let (tx, rx) = std::sync::mpsc::channel();
        let mut watcher: notify::RecommendedWatcher = match notify::RecommendedWatcher::new(tx, Config::default()) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("[watcher] failed to create watcher for {:?}: {:?}", dir, e);
                return;
            }
        };
        if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
            eprintln!("[watcher] watch failed {:?}: {:?}", dir, e);
            return;
        }
        // keep watcher alive
        unsafe { WATCHER = Some(watcher); }

        println!("[watcher] watching {:?} for {:?}", dir, file_name);
        let mut last_emit = Instant::now() - Duration::from_secs(1);

        for res in rx {
            match res {
                Ok(event) => {
                    // filter to our file
                    let is_our_file = if let Some(ref name) = file_name {
                        event.paths.iter().any(|p| p.file_name() == Some(name))
                    } else { true };
                    if !is_our_file { continue; }
                    match event.kind {
                        EventKind::Modify(_) | EventKind::Create(_) => {
                            // debounce 500ms
                            if last_emit.elapsed() < Duration::from_millis(500) { continue; }
                            last_emit = Instant::now();
                            println!("[watcher] {} changed -> reload pending", path.display());
                            CONFIG_DIRTY.store(true, Ordering::SeqCst);
                            crate::RETILE_PENDING.store(true, Ordering::SeqCst);
                            // also wake up message loop via PostMessage to host window?
                            // host timer will pick up CONFIG_DIRTY and do full reload
                        }
                        _ => {}
                    }
                }
                Err(e) => eprintln!("[watcher] error: {:?}", e),
            }
        }
    });
}

pub fn should_reload() -> bool {
    CONFIG_DIRTY.swap(false, Ordering::SeqCst)
}

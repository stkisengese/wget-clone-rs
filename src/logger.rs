use chrono::Local;
use std::io::Write;
use std::sync::Mutex;
use std::fs::OpenOptions;

// This is a simple global logger for background downloads
lazy_static::lazy_static! {
    static ref LOG_FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);
}

/// Initialize the logger for background downloads
pub fn _init_background_logger() -> std::io::Result<()> {
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("wget-log")?;
    
    *LOG_FILE.lock().unwrap() = Some(file);
    println!("Output will be written to \"wget-log\".");
    Ok(())
}

/// Log a message to the appropriate output (stdout or log file)
pub fn log_message(message: &str, background: bool) {
    if background {
        if let Some(file) = &mut *LOG_FILE.lock().unwrap() {
            let _ = writeln!(file, "{}", message);
            let _ = file.flush();
        }
    } else {
        println!("{}", message);
    }
}

/// Log a blank line to the appropriate output
pub fn log_blank_line(background: bool) {
    if background {
        if let Some(file) = &mut *LOG_FILE.lock().unwrap() {
            let _ = writeln!(file);
            let _ = file.flush();
        }
    } else {
        println!();
    }
}

/// Log the start time
pub fn log_start_time() {
    let now = Local::now();
    let formatted = now.format("%Y-%m-%d %H:%M:%S").to_string();
    log_message(&format!("start at {}", formatted), false);
}

/// Log the finish time
pub fn log_finish_time() {
    let now = Local::now();
    let formatted = now.format("%Y-%m-%d %H:%M:%S").to_string();
    log_message(&format!("finished at {}", formatted), false);
}

/// Format file size for display
pub fn format_file_size(size: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if size >= GB {
        format!("{:.2} GB", size as f64 / GB as f64)
    } else if size >= MB {
        format!("{:.2} MB", size as f64 / MB as f64)
    } else if size >= KB {
        format!("{:.2} KB", size as f64 / KB as f64)
    } else {
        format!("{} bytes", size)
    }
}
use indicatif::{ProgressBar, ProgressStyle, HumanBytes};
use std::time::Duration;

/// Create a progress bar for file downloads
pub fn create_progress_bar(total_size: u64) -> ProgressBar {
    let pb = ProgressBar::new(total_size);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{bytes}/{total_bytes} [{bar:80}] {percent:>3}% {binary_bytes_per_sec} {eta}")
            .unwrap()
            .progress_chars("=> "),
    );
    pb.enable_steady_tick(Duration::from_millis(100));
    pb
}

/// Create a progress bar for tasks with unknown size
pub fn create_spinner() -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.green} {msg}")
            .unwrap(),
    );
    pb.enable_steady_tick(Duration::from_millis(100));
    pb
}

/// Format the progress bar message for printing to log file
pub fn format_progress(current: u64, total: u64, speed: f64) -> String {
    let percent = if total > 0 { current as f64 / total as f64 * 100.0 } else { 0.0 };
    let current_human = HumanBytes(current);
    let total_human = HumanBytes(total);
    let speed_human = HumanBytes(speed as u64);
    let eta = if speed > 0.0 { Duration::from_secs_f64((total - current) as f64 / speed) } else { Duration::from_secs(0) };
    let eta_secs = eta.as_secs();
    
    let bar_width = 50;
    let filled_width = ((current as f64 / total as f64) * bar_width as f64) as usize;
    let bar: String = [
        "[".to_string(),
        "=".repeat(filled_width),
        if filled_width < bar_width { ">" } else { "" }.to_string(),
        " ".repeat(bar_width.saturating_sub(filled_width + if filled_width < bar_width { 1 } else { 0 })),
        "]".to_string(),
    ].concat();
    
    format!(
        "{} / {} {} {:.2}% {} {}/s {}s",
        current_human, total_human, bar, percent, if percent >= 100.0 { "✓" } else { "" }, speed_human,
        if eta_secs > 0 { eta_secs.to_string() } else { "0".to_string() }
    )
}
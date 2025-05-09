use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
pub struct WgetArgs {
    /// URL to download
    pub url: Option<String>,
    
    /// Download file in background, output to wget-log
    #[arg(short = 'B', long)]
    pub background: bool,
    
    /// Save file with a different name
    #[arg(short = 'O', long)]
    pub output_file: Option<String>,
    
    /// Directory to save the downloaded file
    #[arg(short = 'P', long)]
    pub directory: Option<String>,
    
    /// Limit download speed (e.g., 200k, 2M)
    #[arg(long = "rate-limit")]
    pub rate_limit: Option<String>,
    
    /// File containing URLs to download
    #[arg(short = 'i', long = "input-file")]
    pub input_file: Option<String>,
    
    /// Mirror a website (download entire website)
    #[arg(long)]
    pub mirror: bool,
    
    /// List of file suffixes to reject when mirroring (e.g., jpg,gif)
    #[arg(short = 'R', long)]
    pub reject: Option<String>,
    
    /// List of directories to exclude when mirroring (e.g., /assets,/css)
    #[arg(short = 'X', long)]
    pub exclude: Option<String>,
    
    /// Convert links for offline viewing when mirroring
    #[arg(long = "convert-links")]
    pub convert_links: bool,
}

pub fn parse_args() -> WgetArgs {
    let mut args = WgetArgs::parse();
    
    // If we're parsing args from the command line, the last argument might be the URL
    // if it's not associated with a flag
    if args.url.is_none() {
        let cmd_args: Vec<String> = std::env::args().collect();
        if let Some(last_arg) = cmd_args.last() {
            if !last_arg.starts_with('-') && !last_arg.contains('=') && 
               !cmd_args[cmd_args.len() - 2].starts_with('-') {
                args.url = Some(last_arg.clone());
            }
        }
    }
    
    args
}

/// Parses a rate limit string (like "200k" or "2M") into bytes per second
pub fn parse_rate_limit(rate_limit: &str) -> Option<u64> {
    let rate_limit = rate_limit.trim().to_lowercase();
    
    if rate_limit.ends_with('k') {
        let num: Result<u64, _> = rate_limit[..rate_limit.len() - 1].parse();
        num.ok().map(|n| n * 1024)
    } else if rate_limit.ends_with('m') {
        let num: Result<u64, _> = rate_limit[..rate_limit.len() - 1].parse();
        num.ok().map(|n| n * 1024 * 1024)
    } else {
        rate_limit.parse().ok()
    }
}

/// Splits a comma-separated string into a vector of strings
pub fn split_comma_separated(s: &str) -> Vec<String> {
    s.split(',')
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect()
}
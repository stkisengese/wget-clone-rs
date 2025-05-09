mod cli;
mod downloader;
mod mirror;
mod logger;
mod progress;

use std::error::Error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Parse command-line arguments
    let args = cli::parse_args();
    
    // Display start time
    logger::log_start_time();
    
    // Process the command based on the flags
    match &args {
        // Mirror a website
        args if args.mirror => {
            mirror::mirror_website(args).await?;
        },
        
        // Download multiple files from a list
        args if args.input_file.is_some() => {
            let file_path = args.input_file.as_ref().unwrap();
            downloader::download_multiple_files(file_path, args).await?;
        },
        
        // Download a single file (default behavior)
        _ => {
            if let Some(url) = &args.url {
                downloader::download_file(url, &args).await?;
            } else {
                eprintln!("Error: URL not provided");
                std::process::exit(1);
            }
        }
    }
    
    // Display finish time for non-background downloads
    if !args.background {
        logger::log_finish_time();
    }
    
    Ok(())
}
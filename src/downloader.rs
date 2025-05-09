use crate::cli::{WgetArgs, parse_rate_limit};
use crate::logger::{log_message, format_file_size};
use crate::progress::{create_progress_bar, format_progress};
use futures::StreamExt;
use reqwest::Client;
use std::error::Error;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use tokio::time::{sleep, Duration, Instant};
use url::Url;

/// Download a single file
pub async fn download_file(url: &str, args: &WgetArgs) -> Result<(), Box<dyn Error>> {
    // Create a client
    let client = Client::new();
    
    // Send the request
    log_message(&format!("sending request, awaiting response..."), args.background);
    let response = client.get(url).send().await?;
    
    // Check status
    let status = response.status();
    log_message(&format!("status {}", status), args.background);
    
    if !status.is_success() {
        return Err(format!("Failed to download: {}", status).into());
    }
    
    // Get content length
    let content_length = response.content_length().unwrap_or(0);
    log_message(
        &format!(
            "content size: {} [~{}]",
            content_length,
            format_file_size(content_length)
        ),
        args.background,
    );
    
    // Determine the file name and path
    let parsed_url = Url::parse(url)?;
    let file_name = if let Some(output_file) = &args.output_file {
        output_file.clone()
    } else {
        parsed_url
            .path_segments()
            .and_then(|segments| segments.last())
            .unwrap_or("index.html")
            .to_string()
    };
    
    let file_path = if let Some(directory) = &args.directory {
        let mut path = PathBuf::from(directory);
        path.push(&file_name);
        path
    } else {
        PathBuf::from(&file_name)
    };
    
    log_message(&format!("saving file to: {}", file_path.display()), args.background);
    
    // Create the directory if it doesn't exist
    if let Some(parent) = file_path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)?;
        }
    }
    
    // Create the file
    let mut file = File::create(&file_path)?;
    
    // Set up rate limiting if needed
    let rate_limit = args.rate_limit.as_ref().and_then(|r| parse_rate_limit(r));
    
    // Download with progress bar
    let mut stream = response.bytes_stream();
    let mut downloaded: u64 = 0;
    let start_time = Instant::now();
    let progress_bar = create_progress_bar(content_length);
    
    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result?;
        
        // Apply rate limiting if needed
        if let Some(limit) = rate_limit {
            let elapsed = start_time.elapsed().as_secs_f64();
            let expected_bytes = (elapsed * limit as f64) as u64;
            
            if downloaded > expected_bytes {
                let sleep_duration = Duration::from_secs_f64(
                    (downloaded - expected_bytes) as f64 / limit as f64
                );
                sleep(sleep_duration).await;
            }
        }
        
        file.write_all(&chunk)?;
        
        downloaded += chunk.len() as u64;
        progress_bar.set_position(downloaded);
        
        // For background downloads, periodically update the log file
        if args.background && downloaded % (1024 * 1024) == 0 {
            // Write progress to log file
            let elapsed = start_time.elapsed().as_secs_f64();
            let speed = if elapsed > 0.0 { downloaded as f64 / elapsed } else { 0.0 };
            let progress_str = format_progress(downloaded, content_length, speed);
            log_message(&progress_str, args.background);
        }
    }
    
    progress_bar.finish();
    
    log_message(&format!("Downloaded [{}]", url), args.background);
    
    Ok(())
}

/// Download multiple files from a file containing URLs
pub async fn download_multiple_files(file_path: &str, args: &WgetArgs) -> Result<(), Box<dyn Error>> {
    let content = std::fs::read_to_string(file_path)?;
    let urls: Vec<String> = content
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();
    
    log_message(&format!("content size: {:?}", urls.len()), args.background);
    
    let mut tasks = Vec::new();
    
    for url in &urls {
        let url_clone = url.clone();
        let args_clone = args.clone();
        
        let task = tokio::spawn(async move {
            match download_file(&url_clone, &args_clone).await {
                Ok(_) => log_message(&format!("finished {}", url_clone), args_clone.background),
                Err(e) => log_message(&format!("failed to download {}: {}", url_clone, e), args_clone.background),
            }
        });
        
        tasks.push(task);
    }
    
    // Wait for all downloads to complete
    for task in tasks {
        let _ = task.await;
    }
    
    log_message(&format!("Download finished: {:?}", urls), args.background);
    
    Ok(())
}
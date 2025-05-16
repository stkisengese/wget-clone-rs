use crate::cli::{WgetArgs, split_comma_separated};
use crate::logger::log_message;
use crate::progress::create_spinner;

use reqwest::Client;
use scraper::{Html, Selector};
use std::collections::{HashSet, VecDeque};
use std::error::Error;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use url::Url;
use regex::Regex;
use tokio::sync::Semaphore;
use async_recursion::async_recursion;
use std::sync::{Arc, Mutex};
// use futures::future::join_all;
use tokio::time::timeout;
use std::time::Duration;

// Maximum concurrent connections
const MAX_CONCURRENT_REQUESTS: usize = 10;
// Connection timeout in seconds
const CONNECTION_TIMEOUT: u64 = 30;

/// Main function to mirror a website
pub async fn mirror_website(args: &WgetArgs) -> Result<(), Box<dyn Error>> {
    let url = match &args.url {
        Some(url) => url,
        None => return Err("URL is required for mirroring".into()),
    };
    
    // Parse the URL
    let base_url = Url::parse(url)?;
    let domain = base_url.host_str().ok_or("Invalid URL: no host found")?;
    
    // Create a directory for the mirrored website
    let output_dir = PathBuf::from(domain);
    if !output_dir.exists() {
        fs::create_dir_all(&output_dir)?;
    }
    
    log_message(&format!("Mirroring website: {}", url), args.background);
    log_message(&format!("Output directory: {}", domain), args.background);
    
    // Parse reject patterns
    let reject_patterns = if let Some(reject) = &args.reject {
        split_comma_separated(reject)
    } else {
        Vec::new()
    };
    
    // Parse exclude paths
    let exclude_paths = if let Some(exclude) = &args.exclude {
        split_comma_separated(exclude)
    } else {
        Vec::new()
    };
    
    log_message(&format!("Reject patterns: {:?}", reject_patterns), args.background);
    log_message(&format!("Exclude paths: {:?}", exclude_paths), args.background);
    
    // Create a HTTP client with timeout
    let client = Client::builder()
        .timeout(Duration::from_secs(CONNECTION_TIMEOUT))
        .build()?;
    
    // Keep track of visited URLs
    let visited = Arc::new(Mutex::new(HashSet::new()));
    
    // Limit concurrent requests using a semaphore
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_REQUESTS));

    // Queue for URLs to visit
    let queue = Arc::new(Mutex::new(VecDeque::new()));
    queue.lock().unwrap().push_back((base_url.clone(), output_dir.clone()));

    // Process the queue
    let spinner = create_spinner();
    spinner.set_message("Mirroring website...");

    let downloaded_files = Arc::new(Mutex::new(Vec::new()));
    let active_tasks = Arc::new(Mutex::new(Vec::new()));
    let mut completed_urls = 0;

    // Process queue until empty and all tasks are completed
    while {
        let queue_empty = queue.lock().unwrap().is_empty();
        let tasks_empty = active_tasks.lock().unwrap().is_empty();
        // Continue loop if either queue has items or tasks are running
        !queue_empty || !tasks_empty
    } {
        // Get next URL from queue if available
        let url_info = {
            let mut queue_lock = queue.lock().unwrap();
            queue_lock.pop_front()
        };
        
        if let Some((url, dir_path)) = url_info {
            // Skip if we've already visited this URL
            let url_string = url.to_string();
            let skip = {
                let mut visited_set = visited.lock().unwrap();
                if visited_set.contains(&url_string) {
                    println!("[Debug] URL {} already visited, skipping", url_string);
                    true
                } else {
                    visited_set.insert(url_string.clone());
                    false
                }
            };
            
            if skip {
                continue;
            }
            
            // Update spinner
            spinner.set_message(format!("Processing: {}", url));
            
            // Check if we should exclude this path
            let path = url.path();
            let should_exclude = exclude_paths.iter().any(|exclude| path.starts_with(exclude));
            if should_exclude {
                log_message(&format!("Skipping excluded path: {}", path), args.background);
                continue;
            }
            
            // Get a permit from the semaphore
            match semaphore.clone().acquire_owned().await {
                Ok(permit) => {
                    // Clone necessary data for the task
                    let client_clone = client.clone();
                    let base_url_clone = base_url.clone();
                    let reject_patterns_clone = reject_patterns.clone();
                    let exclude_paths_clone = exclude_paths.clone();
                    let visited_clone = visited.clone();
                    let downloaded_files_clone = downloaded_files.clone();
                    let queue_clone = queue.clone();
                    let background = args.background;
                    let convert_links = args.convert_links;
                    
                    // Spawn a task to process this URL
                    let task = tokio::spawn(async move {
                        println!("[Debug] Starting task for URL: {}", url);
                        // Use timeout for the request
                        let result = timeout(
                            Duration::from_secs(CONNECTION_TIMEOUT),
                            process_url(
                                &client_clone,
                                &url,
                                &dir_path,
                                &base_url_clone,
                                &reject_patterns_clone,
                                &exclude_paths_clone,
                                visited_clone.clone(),
                                queue_clone,
                                convert_links,
                                downloaded_files_clone,
                            )
                        ).await;
                        
                        match result {
                            Ok(Ok(_)) => {
                                println!("[Debug] Successfully processed URL: {}", url);
                            },
                            Ok(Err(e)) => {
                                log_message(&format!("Error processing {}: {}", url, e), background);
                            },
                            Err(_) => {
                                log_message(&format!("Timeout processing {}", url), background);
                            }
                        }
                        
                        // Explicitly drop the permit when we're done
                        drop(permit);
                    });
                    
                    // Store the task handle
                    active_tasks.lock().unwrap().push(task);
                },
                Err(e) => {
                    log_message(&format!("Failed to acquire semaphore: {}", e), args.background);
                }
            }
        }
        
        // Check for completed tasks
        let mut completed_tasks = Vec::new();
        {
            let mut tasks = active_tasks.lock().unwrap();
            let mut i = 0;
            while i < tasks.len() {
                if tasks[i].is_finished() {
                    completed_tasks.push(tasks.remove(i));
                } else {
                    i += 1;
                }
            }
        }
        
        // Process completed tasks
        for task in completed_tasks {
            match task.await {
                Ok(_) => {
                    completed_urls += 1;
                    spinner.set_message(format!("Processed {} URLs, {} in queue, {} active", 
                        completed_urls, 
                        queue.lock().unwrap().len(),
                        active_tasks.lock().unwrap().len()));
                },
                Err(e) => {
                    log_message(&format!("Task error: {}", e), args.background);
                }
            }
        }
        
        // If queue is empty but tasks are still running, wait a bit
        if queue.lock().unwrap().is_empty() && !active_tasks.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    
    spinner.finish_with_message("Mirroring completed!");
    
    // If convert_links is enabled, update all the links in the downloaded files
    if args.convert_links {
        let files = downloaded_files.lock().unwrap();
        log_message(&format!("Converting links in {} files for offline viewing...", files.len()), args.background);
        
        for file_path in files.iter() {
            if let Err(e) = convert_links_in_file(file_path, &base_url, &output_dir) {
                log_message(&format!("Error converting links in {}: {}", file_path.display(), e), args.background);
            }
        }
    }
    
    log_message(&format!("Website mirroring completed: {}. Downloaded {} URLs.", url, completed_urls), args.background);
    
    Ok(())
}
/// Process a single URL in the mirroring process
#[async_recursion]
async fn process_url(
    client: &Client,
    url: &Url,
    dir_path: &Path,
    base_url: &Url,
    reject_patterns: &[String],
    exclude_paths: &[String],
    visited: Arc<Mutex<HashSet<String>>>,
    queue: Arc<Mutex<VecDeque<(Url, PathBuf)>>>,
    _convert_links: bool,
    downloaded_files: Arc<Mutex<Vec<PathBuf>>>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    // Determine the file name and path
    let (file_name, file_path) = get_file_path(url, dir_path)?;
    
    // Check if the file matches any of the reject patterns
    if reject_patterns.iter().any(|pattern| file_name.ends_with(pattern)) {
        return Ok(());
    }
    
    // Create parent directories if they don't exist
    if let Some(parent) = file_path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)?;
        }
    }
    
    // Download the file
    let response = client.get(url.clone()).send().await?;
    
    if !response.status().is_success() {
        return Err(format!("Failed to download {}: {}", url, response.status()).into());
    }
    
    let content_type = response.headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("text/html");
    
    let is_html = content_type.contains("text/html");
    let is_css = content_type.contains("text/css");
    
    // Get the content
    let bytes = response.bytes().await?;
    
    // Write the content to file
    let mut file = File::create(&file_path)?;
    file.write_all(&bytes)?;
    
    // Add to downloaded files list
    {
        let mut files = downloaded_files.lock().unwrap();
        files.push(file_path.clone());
    }
    
    // Process HTML content to find more links
    if is_html {
        let html_content = String::from_utf8_lossy(&bytes);
        let document = Html::parse_document(&html_content);
        
        // Extract links from a, link, and img tags
        let mut new_urls = Vec::new();
        
        // Process <a> tags
        extract_links_from_tag(&document, "a", "href", &mut new_urls, url)?;
        
        // Process <link> tags
        extract_links_from_tag(&document, "link", "href", &mut new_urls, url)?;
        
        // Process <img> tags
        extract_links_from_tag(&document, "img", "src", &mut new_urls, url)?;
        
        // Process <script> tags
        extract_links_from_tag(&document, "script", "src", &mut new_urls, url)?;
        
        // Add unique and valid URLs to the queue
        for new_url in new_urls {
            // Check if it's from the same domain
            if is_same_domain(&new_url, base_url) {
                // Check if we've already seen this URL
                let url_string = new_url.to_string();
                let is_visited = {
                    let visited_set = visited.lock().unwrap();
                    visited_set.contains(&url_string)
                };
                
                if !is_visited {
                    // Calculate the directory path for this URL
                    let new_dir_path = calculate_directory_path(&new_url, base_url, dir_path)?;
                    
                    // Check exclude paths
                    let path = new_url.path();
                    let should_exclude = exclude_paths.iter().any(|exclude| path.starts_with(exclude));
                    
                    if !should_exclude {
                        queue.lock().unwrap().push_back((new_url, new_dir_path));
                    }
                }
            }
        }
    } else if is_css {
        // Process CSS for @import and url() references
        let css_content = String::from_utf8_lossy(&bytes);
        let urls = extract_urls_from_css(&css_content, url)?;
        
        for new_url in urls {
            if is_same_domain(&new_url, base_url) {
                let url_string = new_url.to_string();
                let is_visited = {
                    let visited_set = visited.lock().unwrap();
                    visited_set.contains(&url_string)
                };
                
                if !is_visited {
                    let new_dir_path = calculate_directory_path(&new_url, base_url, dir_path)?;
                    
                    // Check exclude paths
                    let path = new_url.path();
                    let should_exclude = exclude_paths.iter().any(|exclude| path.starts_with(exclude));
                    
                    if !should_exclude {
                        queue.lock().unwrap().push_back((new_url, new_dir_path));
                    }
                }
            }
        }
    }
    
    Ok(())
}

/// Extract links from specific HTML tags
fn extract_links_from_tag(
    document: &Html,
    tag_name: &str,
    attr_name: &str,
    urls: &mut Vec<Url>,
    base_url: &Url,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let selector = Selector::parse(tag_name).map_err(|e| format!("Invalid selector: {}", e))?;
    
    for element in document.select(&selector) {
        if let Some(href) = element.value().attr(attr_name) {
            if let Ok(new_url) = base_url.join(href) {
                urls.push(new_url);
            }
        }
    }
    
    Ok(())
}

/// Extract URLs from CSS content (url() and @import)
fn extract_urls_from_css(css: &str, base_url: &Url) -> Result<Vec<Url>, Box<dyn Error + Send + Sync>> {
    let mut urls = Vec::new();
    
    // Match url() patterns
    let url_regex = Regex::new(r#"url\s*\(\s*['"]?([^'"]+)['"]?\s*\)"#).unwrap();
    for cap in url_regex.captures_iter(css) {
        let url_str = &cap[1];
        if let Ok(new_url) = base_url.join(url_str) {
            urls.push(new_url);
        }
    }
    
    // Match @import patterns
    let import_regex = Regex::new(r#"@import\s+['"]([^'"]+)['"]"#).unwrap();
    for cap in import_regex.captures_iter(css) {
        let url_str = &cap[1];
        if let Ok(new_url) = base_url.join(url_str) {
            urls.push(new_url);
        }
    }
    
    Ok(urls)
}

/// Determine if two URLs belong to the same domain
fn is_same_domain(url1: &Url, url2: &Url) -> bool {
    url1.host() == url2.host()
}

/// Calculate the directory path for a URL
fn calculate_directory_path(url: &Url, base_url: &Url, base_dir: &Path) -> Result<PathBuf, Box<dyn Error + Send + Sync>> {
    // For the base URL, just return the base directory
    if url.as_str() == base_url.as_str() {
        return Ok(base_dir.to_path_buf());
    }
    
    // Always start with the base directory
    let mut full_path = base_dir.to_path_buf();
    
    // For other URLs, determine the relative path
    let url_path = url.path();
    
    // If the URL path is just "/" or empty, return the base directory
    if url_path == "/" || url_path.is_empty() {
        return Ok(full_path);
    }
    
    // Otherwise, handle the path components
    let path_without_leading_slash = url_path.trim_start_matches('/');
    
    // If the path has filename part (doesn't end with slash), we need the directory part only
    let dir_part = if url_path.ends_with('/') {
        path_without_leading_slash.trim_end_matches('/')
    } else {
        // Get the directory part by removing the filename
        match path_without_leading_slash.rfind('/') {
            Some(index) => &path_without_leading_slash[..index],
            None => "", // No directory part
        }
    };
    
    // If there's a directory part, add it to the path
    if !dir_part.is_empty() {
        for component in dir_part.split('/') {
            if !component.is_empty() {
                full_path.push(component);
            }
        }
    }
    
    Ok(full_path)
}
/// Determine file name and path from a URL
fn get_file_path(url: &Url, dir_path: &Path) -> Result<(String, PathBuf), Box<dyn Error + Send + Sync>> {
    let path = url.path();
    
    // First, create the base directory path
    let mut file_path = dir_path.to_path_buf();
    
    // Handle paths ending with slash differently
    if path == "/" || path.is_empty() {
        // Root path - just use index.html in the root directory
        file_path.push("index.html");
        return Ok(("index.html".to_string(), file_path));
    } else if path.ends_with('/') {
        // Path ends with slash - treat as directory
        // Create all directories in the path
        let trimmed_path = path.trim_start_matches('/').trim_end_matches('/');
        if !trimmed_path.is_empty() {
            for component in trimmed_path.split('/') {
                if !component.is_empty() {
                    file_path.push(component);
                }
            }
        }
        // Add index.html at the end
        file_path.push("index.html");
        return Ok(("index.html".to_string(), file_path));
    } else {
        // Regular file path
        // Split into directory components and filename
        let components: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        let file_name = components.last().unwrap_or(&"index.html");
        
        // Add all directories except the last component (which is the filename)
        if components.len() > 1 {
            for component in &components[0..components.len() - 1] {
                if !component.is_empty() {
                    file_path.push(component);
                }
            }
        }
        
        // Add the filename
        if !file_name.is_empty() {
            file_path.push(file_name);
            return Ok((file_name.to_string(), file_path));
        } else {
            file_path.push("index.html");
            return Ok(("index.html".to_string(), file_path));
        }
    }
}

/// Convert links in an HTML or CSS file for offline viewing
fn convert_links_in_file(file_path: &Path, base_url: &Url, output_dir: &Path) -> Result<(), Box<dyn Error>> {
    // Skip if file doesn't exist
    if !file_path.exists() {
        return Ok(());
    }
    
    // Read the file content
    let content = fs::read_to_string(file_path)?;
    
    // Determine the file type based on extension
    let is_html = file_path.extension().map_or(false, |ext| ext == "html" || ext == "htm");
    let is_css = file_path.extension().map_or(false, |ext| ext == "css");
    
    if is_html {
        // Convert HTML links
        let converted = convert_html_links(&content, base_url, file_path, output_dir)?;
        fs::write(file_path, converted)?;
    } else if is_css {
        // Convert CSS links
        let converted = convert_css_links(&content, base_url, file_path, output_dir)?;
        fs::write(file_path, converted)?;
    }
    
    Ok(())
}

/// Convert links in HTML content for offline viewing
fn convert_html_links(content: &str, base_url: &Url, file_path: &Path, output_dir: &Path) -> Result<String, Box<dyn Error>> {
    let document = Html::parse_document(content);
    let mut html_content = content.to_string();
    
    // Define selectors for elements with links
    let selectors = [
        ("a", "href"),
        ("link", "href"),
        ("img", "src"),
        ("script", "src"),
    ];
    
    for (tag_name, attr_name) in selectors.iter() {
        let selector = Selector::parse(tag_name).map_err(|e| format!("Invalid selector: {}", e))?;
        
        for element in document.select(&selector) {
            if let Some(href) = element.value().attr(attr_name) {
                // Skip if it's an anchor link or absolute URL to a different domain
                if href.starts_with('#') || href.starts_with("http") && !href.starts_with(base_url.as_str()) {
                    continue;
                }
                
                // Convert absolute URL to relative path
                if let Ok(absolute_url) = base_url.join(href) {
                    if let Some(relative_path) = url_to_relative_path(&absolute_url, base_url, file_path, output_dir) {
                        // Replace the link in the HTML content
                        let old_attr = format!("{}=\"{}\"", attr_name, href);
                        let new_attr = format!("{}=\"{}\"", attr_name, relative_path);
                        html_content = html_content.replace(&old_attr, &new_attr);
                    }
                }
            }
        }
    }
    
    Ok(html_content)
}

/// Convert links in CSS content for offline viewing
fn convert_css_links(content: &str, base_url: &Url, file_path: &Path, output_dir: &Path) -> Result<String, Box<dyn Error>> {
    let mut css_content = content.to_string();
    
    // Convert url() references
    let url_regex = Regex::new(r#"url\s*\(\s*['"]?([^'"]+)['"]?\s*\)"#).unwrap();
    let mut offset = 0;
    
    for cap in url_regex.captures_iter(content) {
        let url_str = &cap[1];
        let full_match = &cap[0];
        
        // Skip data URLs and absolute URLs to different domains
        if url_str.starts_with("data:") || url_str.starts_with("http") && !url_str.starts_with(base_url.as_str()) {
            continue;
        }
        
        // Convert absolute URL to relative path
        if let Ok(absolute_url) = base_url.join(url_str) {
            if let Some(relative_path) = url_to_relative_path(&absolute_url, base_url, file_path, output_dir) {
                // Replace the URL in the CSS content
                let new_url = format!("url(\"{}\")", relative_path);
                
                // Find the position of the match in the modified content
                if let Some(pos) = css_content[offset..].find(full_match) {
                    let real_pos = pos + offset;
                    css_content.replace_range(real_pos..real_pos + full_match.len(), &new_url);
                    offset = real_pos + new_url.len();
                }
            }
        }
    }
    
    // Convert @import references
    let import_regex = Regex::new(r#"@import\s+['"]([^'"]+)['"]"#).unwrap();
    offset = 0;
    
    for cap in import_regex.captures_iter(content) {
        let url_str = &cap[1];
        let full_match = &cap[0];
        
        // Skip absolute URLs to different domains
        if url_str.starts_with("http") && !url_str.starts_with(base_url.as_str()) {
            continue;
        }
        
        // Convert absolute URL to relative path
        if let Ok(absolute_url) = base_url.join(url_str) {
            if let Some(relative_path) = url_to_relative_path(&absolute_url, base_url, file_path, output_dir) {
                // Replace the URL in the CSS content
                let new_import = format!("@import \"{}\"", relative_path);
                
                // Find the position of the match in the modified content
                if let Some(pos) = css_content[offset..].find(full_match) {
                    let real_pos = pos + offset;
                    css_content.replace_range(real_pos..real_pos + full_match.len(), &new_import);
                    offset = real_pos + new_import.len();
                }
            }
        }
    }
    
    Ok(css_content)
}

/// Convert an absolute URL to a relative path for offline viewing
fn url_to_relative_path(url: &Url, base_url: &Url, file_path: &Path, output_dir: &Path) -> Option<String> {
    // Skip URLs from different domains
    if url.host() != base_url.host() {
        return None;
    }
    
    // Calculate the relative path from the current file to the target file
    let target_path = output_dir.join(url.path().trim_start_matches('/'));
    
    // Get the parent directory of the current file
    let current_dir = file_path.parent()?;
    
    // Calculate relative path
    let relative_path = pathdiff::diff_paths(target_path, current_dir)?;
    
    // Convert to string
    let path_str = relative_path.to_string_lossy().to_string();
    
    Some(path_str)
}

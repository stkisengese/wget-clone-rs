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
use futures::future::join_all;
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
    let tasks = Arc::new(Mutex::new(Vec::new()));

    // Process queue until empty
    while !queue.lock().unwrap().is_empty() {
        // Get next URL from queue
        let (url, dir_path) = {
            let mut queue_lock = queue.lock().unwrap();
            match queue_lock.pop_front() {
                Some(item) => item,
                None => break,
            }
        };
        
        // Skip if we've already visited this URL
        let url_string = url.to_string();
        {
            let mut visited_set = visited.lock().unwrap();
            if visited_set.contains(&url_string) {
                continue;
            }
            visited_set.insert(url_string.clone());
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
        let permit = semaphore.clone().acquire_owned().await?;
        
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
                Ok(Ok(_)) => {},
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
        tasks.lock().unwrap().push(task);
        
        // If we have too many tasks, wait for some to complete
        if tasks.lock().unwrap().len() >= MAX_CONCURRENT_REQUESTS * 2 {
            let tasks_to_await = {
                let mut tasks_lock = tasks.lock().unwrap();
                let drain_count = tasks_lock.len() / 2;
                tasks_lock.drain(0..drain_count).collect::<Vec<_>>()
            };
            
            // Wait for the tasks to complete
            for task_result in join_all(tasks_to_await).await {
                if let Err(e) = task_result {
                    log_message(&format!("Task error: {}", e), args.background);
                }
            }
        }
    }
    
    // Wait for all remaining tasks to complete
    let remaining_tasks = {
        let mut tasks_lock = tasks.lock().unwrap();
        std::mem::take(&mut *tasks_lock)
    };
    
    for task_result in join_all(remaining_tasks).await {
        if let Err(e) = task_result {
            log_message(&format!("Task error: {}", e), args.background);
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
    
    log_message(&format!("Website mirroring completed: {}", url), args.background);
    
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
    let path = url.path();
    
    // Remove the common prefix between base_url and url
    let base_path = base_url.path();
    let relative_path = if path.starts_with(base_path) && base_path != "/" {
        &path[base_path.len()..]
    } else {
        path
    };
    
    // Combine with the base directory
    let mut full_path = base_dir.to_path_buf();
    
    // Handle the path components
    if relative_path.starts_with('/') {
        // For absolute paths, append to the base directory
        let path_components: Vec<&str> = relative_path[1..].split('/').collect();
        if path_components.len() > 1 {
            for component in &path_components[0..path_components.len() - 1] {
                full_path.push(component);
            }
        }
    } else {
        // For relative paths
        let path_components: Vec<&str> = relative_path.split('/').collect();
        if path_components.len() > 1 {
            for component in &path_components[0..path_components.len() - 1] {
                full_path.push(component);
            }
        }
    }
    
    Ok(full_path)
}

/// Determine file name and path from a URL
fn get_file_path(url: &Url, dir_path: &Path) -> Result<(String, PathBuf), Box<dyn Error + Send + Sync>> {
    let path = url.path();
    
    // Extract the file name from the path
    let file_name = path.split('/').last().unwrap_or("index.html");
    let file_name = if file_name.is_empty() { "index.html" } else { file_name };
    
    // Create the full file path
    let mut file_path = dir_path.to_path_buf();
    
    // Split the path into components
    let path_components: Vec<&str> = path.split('/').collect();
    
    // If we have a path with multiple components, create subdirectories
    if path_components.len() > 1 {
        for component in &path_components[0..path_components.len() - 1] {
            if !component.is_empty() {
                file_path.push(component);
            }
        }
    }
    
    // Add the file name
    file_path.push(file_name);
    
    // If the path ends with a slash, assume it's a directory and add index.html
    if path.ends_with('/') || file_name.is_empty() {
        file_path.push("index.html");
        Ok(("index.html".to_string(), file_path))
    } else {
        Ok((file_name.to_string(), file_path))
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

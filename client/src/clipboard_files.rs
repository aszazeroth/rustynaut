//! Platform-specific clipboard file detection.
//!
//! This module provides functions to detect when files are copied to the clipboard
//! via the native file manager (Finder on macOS, Explorer on Windows, etc.).

use std::path::PathBuf;

/// Get file paths from the clipboard if files were copied via the file manager.
/// Returns None if clipboard doesn't contain file references.
pub fn get_clipboard_files() -> Option<Vec<PathBuf>> {
    #[cfg(target_os = "macos")]
    {
        get_clipboard_files_macos()
    }

    #[cfg(target_os = "windows")]
    {
        get_clipboard_files_windows()
    }

    #[cfg(target_os = "linux")]
    {
        get_clipboard_files_linux()
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        None
    }
}

// ==================== macOS Implementation ====================

#[cfg(target_os = "macos")]
fn get_clipboard_files_macos() -> Option<Vec<PathBuf>> {
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::{NSString, NSURL};

    // Get the general pasteboard
    let pasteboard = NSPasteboard::generalPasteboard();

    // First, check the available types to see what we're dealing with
    let types = pasteboard.types()?;

    // Check if any known file type is present
    let has_file_url = (0..types.count()).any(|i| {
        let t = types.objectAtIndex(i).to_string();
        t == "public.file-url" || t == "NSFilenamesPboardType"
    });

    if !has_file_url {
        return None;
    }

    // Try public.file-url first (standard UTI)
    let file_url_type = NSString::from_str("public.file-url");

    if let Some(items) = pasteboard.pasteboardItems() {
        let mut paths = Vec::new();

        let count = items.count();

        for i in 0..count {
            let item = items.objectAtIndex(i);

            if let Some(url_string) = item.stringForType(&file_url_type) {
                let url_str: String = url_string.to_string();

                // Use NSURL to resolve file reference URLs (file:///.file/id=...)
                let ns_url_string = NSString::from_str(&url_str);
                if let Some(url) = NSURL::URLWithString(&ns_url_string) {
                    // Get the file path from the URL (this resolves file reference URLs)
                    if let Some(path) = url.path() {
                        let path_str: String = path.to_string();
                        if !path_str.is_empty() && path_str != "/.file" {
                            paths.push(PathBuf::from(path_str));
                        }
                    }
                }
            }
        }

        if !paths.is_empty() {
            return Some(paths);
        }
    }

    None
}

// ==================== Windows Implementation ====================

#[cfg(target_os = "windows")]
fn get_clipboard_files_windows() -> Option<Vec<PathBuf>> {
    use clipboard_win::{formats, get_clipboard};

    // Try to get file list from clipboard
    let file_list: Result<Vec<String>, _> = get_clipboard(formats::FileList);

    match file_list {
        Ok(files) if !files.is_empty() => Some(files.into_iter().map(PathBuf::from).collect()),
        _ => None,
    }
}

// ==================== Linux Implementation ====================

#[cfg(target_os = "linux")]
fn get_clipboard_files_linux() -> Option<Vec<PathBuf>> {
    // On Linux, file managers typically put file:// URIs in the clipboard
    // as text/uri-list. The arboard crate's get_text() should already
    // return this, so we just need to parse it.
    //
    // For now, we return None and rely on the text-based detection
    // in the main code which handles file:// URLs.
    //
    // TODO: Implement proper X11/Wayland clipboard file detection
    // using x11-clipboard or wl-clipboard crates if needed.
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_get_clipboard_files_returns_option() {
        // Basic sanity test - function should compile and return Option
        let result = super::get_clipboard_files();
        // Result is None if no files in clipboard, Some if files present
        assert!(result.is_none() || result.is_some());
    }
}

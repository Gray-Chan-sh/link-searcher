#!/usr/bin/env python3
# Final comprehensive transformation for relative path support

import os
import re

SRC = "/Volumes/Data/Project/Link-Searcher/src-tauri/src"

def read_file(path):
    with open(path, 'r', encoding='utf-8') as f:
        return f.read()

def write_file(path, content):
    with open(path, 'w', encoding='utf-8') as f:
        f.write(content)

# ============================================================
# 1. helpers.rs
# ============================================================
helpers_path = os.path.join(SRC, "scanner", "helpers.rs")
helpers_content = read_file(helpers_path)

if 'pub fn to_relative' not in helpers_content:
    # Add import if needed
    if 'use anyhow::{Context, Result};' in helpers_content and 'anyhow' not in helpers_content.split('use anyhow')[1].split(';')[0]:
        helpers_content = helpers_content.replace(
            'use anyhow::{Context, Result};', 
            'use anyhow::{Context, Result, anyhow};'
        )
    
    new_funcs = '''
/// Convert an absolute path to a relative path within the given directory root.
/// Returns the relative path string, or an error if the file is not under dir_root.
pub fn to_relative(dir_root: &str, file_path: &Path) -> Result<String> {
    let path_str = file_path.to_string_lossy().replace("\\", "/");
    let root_str = dir_root.replace("\\", "/");
    
    if !path_str.starts_with(&root_str) {
        return Err(anyhow::anyhow!("file {} is not under dir root {}", path_str, dir_root));
    }
    
    let rel_str = &path_str[root_str.len()..];
    let rel_str = rel_str.strip_prefix('/').unwrap_or(rel_str);
    Ok(rel_str.to_string())
}

/// Convert a stored relative path back to an absolute path by joining with dir_root.
pub fn to_absolute(dir_root: &str, rel_path: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(dir_root).join(rel_path)
}'''
    helpers_content += '\n' + new_funcs

write_file(helpers_path, helpers_content)
print("Updated helpers.rs")

# ============================================================
# 2. scanner/mod.rs - Full rewrite with all transformations
# ============================================================
mod_path = os.path.join(SRC, "scanner", "mod.rs")
mod_content = read_file(mod_path)

# Apply all necessary replacements using careful regex patterns

# A. Add dir_root after config log in full_scan
mod_content = re.sub(
    r'(log::info!\(\[SCAN\] 开始扫描：\{\}\， config\.path\);\s*\n)\s*(        let exclude = parse_exclude_patterns\(&config\.exclude_patterns\);)',
    r'\1    let dir_root = &config.path;\n\2',
    mod_content
)

# B. Add dir_root after config in incremental_scan
mod_content = re.sub(
    r'(let config = dir_config::get_dir\(&conn, dir_id\)\?\s*\n\s*\.ok_or_else\(\|\| anyhow::anyhow\("dir_config not found: \{id\}"\)\)\?;)\s*\n\s*(        let exclude = parse_exclude_patterns\(&config\.exclude_patterns\);)',
    r'\1\n        let dir_root = &config.path;\n\2',
    mod_content
)

# C. Add dir_root after config log in startup_scan
mod_content = re.sub(
    r'(log::info!\(\[STARTUP\] 启动扫描：\{\}\， config\.path\);\s*\n)\s*(        let exclude = parse_exclude_patterns\(&config\.exclude_patterns\);)',
    r'\1    let dir_root = &config.path;\n\2',
    mod_content
)

# D. Transform full_scan loop body using line-by-line approach to avoid regex complexity
lines = mod_content.split('\n')
new_lines = []
i = 0
while i < len(lines):
    line = lines[i]
    
    # Detect start of full_scan loop (line with 'for entry in walkdir::WalkDir::new(&config.path)')
    if 'for entry in walkdir::WalkDir::new(&config.path)' in line:
        new_lines.append(line)
        i += 1
        # Process loop body until we hit a line that's less indented than the loop body body
        # The loop body typically starts at indent level 12-16 spaces
        while i < len(lines) and (lines[i].startswith('            ') or lines[i].startswith('        ') or lines[i].strip() == ''):
            l = lines[i]
            if 'let path_str = path.to_string_lossy().to_string();' in l:
                new_lines.append(l)
                new_lines.append('            let rel_path = to_relative(dir_root, &path)?;')
                i += 1
                continue
            if 'disk_paths.push(path_str.clone());' in l:
                new_lines.append(l.replace('path_str', 'rel_path'))
                i += 1
                continue
            if 'tracker::get_file_by_path(&conn, &path_str)?;' in l:
                new_lines.append(l.replace('&path_str', '&rel_path'))
                i += 1
                continue
            if 'tracker::upsert_file(&conn, &path_str,' in l:
                new_lines.append(l.replace('&path_str', '&rel_path'))
                i += 1
                continue
            new_lines.append(l)
            i += 1
        continue
    
    # Detect start of incremental_scan loop
    if 'for entry in walker' in line and i > 200:  # Approximate position
        new_lines.append(line)
        i += 1
        while i < len(lines) and (lines[i].startswith('            ') or lines[i].startswith('        ') or lines[i].strip() == ''):
            l = lines[i]
            if 'let path_str = path.to_string_lossy().to_string();' in l:
                new_lines.append(l)
                new_lines.append('            let rel_path = to_relative(dir_root, &path)?;')
                i += 1
                continue
            if 'on_disk.push(path_str.clone());' in l:
                new_lines.append(l.replace('path_str', 'rel_path'))
                i += 1
                continue
            if 'tracker::get_file_by_path(&conn, &path_str)?;' in l:
                new_lines.append(l.replace('&path_str', '&rel_path'))
                i += 1
                continue
            if 'tracker::upsert_file(&conn, &path_str,' in l:
                new_lines.append(l.replace('&path_str', '&rel_path'))
                i += 1
                continue
            new_lines.append(l)
            i += 1
        continue
    
    # Detect start of startup_scan loop  
    if 'for entry in walker' in line and i > 280:
        new_lines.append(line)
        i += 1
        while i < len(lines) and (lines[i].startswith('            ') or lines[i].startswith('        ') or lines[i].strip() == ''):
            l = lines[i]
            if 'let path_str = path.to_string_lossy().to_string();' in l:
                new_lines.append(l)
                new_lines.append('            let rel_path = to_relative(dir_root, &path)?;')
                i += 1
                continue
            if 'on_disk.push(DiskEntry { path: path_str, size, name });' in l:
                new_lines.append(l.replace('path_str', 'rel_path'))
                i += 1
                continue
            if 'tracker::get_file_by_path(&conn, &path_str)?;' in l:
                new_lines.append(l.replace('&path_str', '&rel_path'))
                i += 1
                continue
            if 'tracker::upsert_file(&conn, &path_str,' in l:
                new_lines.append(l.replace('&path_str', '&rel_path'))
                i += 1
                continue
            new_lines.append(l)
            i += 1
        continue
    
    new_lines.append(line)
    i += 1

mod_content = '\n'.join(new_lines)

# E. Replace handle_event entirely with updated version
old_he_pattern = r'''(pub fn handle_event\s*\(&self, event: FileChangeEvent\) -> Result<\( \>\) \{[\s\S]*?^    \}\})'''
# Simpler: match the entire impl Scanner block containing handle_event and replace just that function
# Find the exact old handle_event function text from original
old_handle_text = '''    pub fn handle_event(&self, event: FileChangeEvent) -> Result<()> {
        let conn = self.db.get().context("failed to get DB connection")?;
        let file_path = &event.path;

        match event.kind {
            ChangeKind::Create | ChangeKind::Modify => {
                let path_str = file_path.to_string_lossy().to_string();
                let meta = std::fs::metadata(file_path)
                    .with_context(|| format!("failed to stat {path_str}"))?;
                let mtime = mtime_micros(&meta).unwrap_or(0);
                let size = meta.len();
                let file_id = tracker::upsert_file(&conn, &path_str, &event.dir_id, mtime, size, None)?;
                drop(conn);
                match self.indexer.index_file(&file_id, file_path, &event.dir_id) {
                    Ok(()) => log::info!("[WATCHER] indexed: {path_str}"),
                    Err(e) => log::error!("[WATCHER] failed to index {path_str}: {e}"),
                }
            }
            ChangeKind::Delete => {
                let path_str = file_path.to_string_lossy().to_string();
                if let Some(record) = tracker::get_file_by_path(&conn, &path_str)? {
                    drop(conn);
                    self.indexer.delete_file(&record.id)?;
                    log::info!("[WATCHER] deleted: {path_str}");
                }
            }
        }
        Ok(())
    }'''

new_handle_text = '''    pub fn handle_event(&self, event: FileChangeEvent) -> Result<()> {
        let conn = self.db.get().context("failed to get DB connection")?;
        let file_path = &event.path;

        // Get directory root for this dir_id
        let dir_config = dir_config::get_dir(&conn, &event.dir_id)?
            .ok_or_else(|| anyhow::anyhow!("dir config not found: {}", event.dir_id))?;
        let dir_root = &dir_config.path;

        // Compute relative path
        let rel_path = to_relative(dir_root, file_path)?;
        let path_str = file_path.to_string_lossy().to_string();

        match event.kind {
            ChangeKind::Create | ChangeKind::Modify => {
                let meta = std::fs::metadata(file_path)
                    .with_context(|| format!("failed to stat {path_str}"))?;
                let mtime = mtime_micros(&meta).unwrap_or(0);
                let size = meta.len();
                let file_id = tracker::upsert_file(&conn, &rel_path, &event.dir_id, mtime, size, None)?;
                drop(conn);
                match self.indexer.index_file(&file_id, file_path, &event.dir_id) {
                    Ok(()) => log::info!("[WATCHER] indexed: {path_str}"),
                    Err(e) => log::error!("[WATCHER] failed to index {path_str}: {e}"),
                }
            }
            ChangeKind::Delete => {
                // Look up by relative path since that's how it's stored in DB
                if let Some(record) = tracker::get_file_by_path(&conn, &rel_path)? {
                    self.indexer.delete_file(&record.id)?;
                    log::info!("[WATCHER] deleted: {path_str}");
                }
            }
        }
        Ok(())
    }'''

if old_handle_text in mod_content:
    mod_content = mod_content.replace(old_handle_text, new_handle_text)
    print("Replaced handle_event")
else:
    print("Handle_event not found exactly, trying alternative...")
    # Maybe whitespace differs - use regex
    pattern_start = r'pub fn handle_event\(&self, event: FileChangeEvent\) -> Result<\( \>\) \{'
    # Find start and end of function
    lines = mod_content.split('\n')
    func_start = -1
    for idx, l in enumerate(lines):
        if 'pub fn handle_event' in l and 'FileChangeEvent' in l:
            func_start = idx
            break
    if func_start >= 0:
        # Find matching closing brace at same indentation level as 'impl Scanner' or at column 4
        # Simple approach: find next '}' that closes this function (indent level 4 or less after starting)
        brace_count = 0
        func_end = func_start
        for j in range(func_start, len(lines)):
            brace_count += lines[j].count('{')
            brace_count -= lines[j].count('}')
            if brace_count <= 0 and j > func_start:
                func_end = j
                break
        if func_end >= func_start:
            # Replace lines[func_start:func_end+1] with new function
            new_func_lines = new_handle_text.split('\n')
            lines[func_start:func_end+1] = new_func_lines
            mod_content = '\n'.join(lines)
            print("Replaced handle_event via line replacement")
        else:
            print("Could not find end of handle_event function")
    else:
        print("Could not find handle_event function start")

write_file(mod_path, mod_content)
print("Updated scanner/mod.rs")

print("\nTransformations complete!")

#!/usr/bin/env python3
# Comprehensive path-relative modification of Link-Searcher codebase

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
# 1. scanner/helpers.rs - Add to_relative and to_absolute (already done but verify)
# ============================================================
helpers_path = os.path.join(SRC, "scanner", "helpers.rs")
content = read_file(helpers_path)

# Ensure proper imports
if 'use anyhow::{Context, Result};' in content and 'anyhow' not in content.split('use anyhow')[1].split(';')[0]:
    content = content.replace('use anyhow::{Context, Result};', 'use anyhow::{Context, Result, anyhow};')
write_file(helpers_path, content)
print("Updated helpers.rs imports")

# ============================================================
# 2. scanner/mod.rs - Transform scans to use relative paths
# ============================================================
mod_path = os.path.join(SRC, "scanner", "mod.rs")
content = read_file(mod_path)

lines = content.split('\n')
new_lines = []
i = 0
n = len(lines)

full_scan_dir_added = False
inc_scan_dir_added = False
startup_scan_dir_added = False
full_scan_loop_done = False
inc_scan_loop_done = False
startup_scan_loop_done = False

while i < n:
    line = lines[i]
    
    # Track scan function contexts
    in_full_scan = 'pub fn full_scan' in line
    in_inc_scan = 'pub fn incremental_scan' in line
    in_startup_scan = 'pub fn startup_scan' in line
    
    if in_full_scan and not full_scan_dir_added:
        # Find the log line next and add dir_root after it
        new_lines.append(line)
        i += 1
        continue
    
    if in_inc_scan and not inc_scan_dir_added:
        # After the .ok_or_else... line, insert dir_root
        if '.ok_or_else(|| anyhow::anyhow!("dir_config not found: {dir_id}"))?;' in line:
            new_lines.append(line)
            i += 1
            if i < n and lines[i].strip().startswith('let exclude'):
                new_lines.append('        let dir_root = &config.path;')
                inc_scan_dir_added = True
            continue
        new_lines.append(line)
        i += 1
        continue
    
    if in_startup_scan and not startup_scan_dir_added:
        if 'log::info!("[STARTUP] 启动扫描: {}", config.path);' in line:
            new_lines.append(line)
            new_lines.append('        let dir_root = &config.path;')
            startup_scan_dir_added = True
            i += 1
            continue
        new_lines.append(line)
        i += 1
        continue
    
    # Handle loop transformations
    # Full scan loop
    if 'for entry in walkdir::WalkDir::new(&config.path)' in line and not full_scan_loop_done:
        full_scan_loop_done = True
        new_lines.append(line)
        i += 1
        continue
    
    if full_scan_loop_done and 'let path = entry.path().to_path_buf();' in line:
        new_lines.append(line)
        i += 1
        continue
    
    if full_scan_loop_done and 'let path_str = path.to_string_lossy().to_string();' in line:
        new_lines.append(line)
        new_lines.append('            let rel_path = to_relative(dir_root, &path)?;')
        i += 1
        continue
    
    if full_scan_loop_done and 'disk_paths.push(path_str.clone());' in line:
        new_lines.append(line.replace('path_str', 'rel_path'))
        i += 1
        continue
    
    if full_scan_loop_done and 'tracker::get_file_by_path' in line and '&path_str' in line:
        new_lines.append(line.replace('&path_str', '&rel_path'))
        i += 1
        continue
    
    if full_scan_loop_done and 'tracker::upsert_file' in line and '&path_str' in line:
        new_lines.append(line.replace('&path_str', '&rel_path'))
        i += 1
        continue
    
    if full_scan_loop_done and line.strip() == '}':  # End of for loop
        full_scan_loop_done = False
        new_lines.append(line)
        i += 1
        continue
    
    # Incremental scan loop
    if ('for entry in walker' in line or 'for entry in walkdir' in line) and not inc_scan_loop_done:
        if 'for entry in walker' in line:
            inc_scan_loop_done = True
            new_lines.append(line)
            i += 1
            continue
    
    if inc_scan_loop_done and 'let path = entry.path().to_path_buf();' in line:
        new_lines.append(line)
        i += 1
        continue
    
    if inc_scan_loop_done and 'let path_str = path.to_string_lossy().to_string();' in line:
        new_lines.append(line)
        new_lines.append('            let rel_path = to_relative(dir_root, &path)?;')
        i += 1
        continue
    
    if inc_scan_loop_done and 'on_disk.push(path_str.clone());' in line:
        new_lines.append(line.replace('path_str', 'rel_path'))
        i += 1
        continue
    
    if inc_scan_loop_done and 'tracker::get_file_by_path' in line and '&path_str' in line:
        new_lines.append(line.replace('&path_str', '&rel_path'))
        i += 1
        continue
    
    if inc_scan_loop_done and 'tracker::upsert_file' in line and '&path_str' in line:
        new_lines.append(line.replace('&path_str', '&rel_path'))
        i += 1
        continue
    
    # Startup scan loop
    if 'for entry in walker' in line and not startup_scan_loop_done:
        startup_scan_loop_done = True
        new_lines.append(line)
        i += 1
        continue
    
    if startup_scan_loop_done and 'let path = entry.path().to_path_buf();' in line:
        new_lines.append(line)
        i += 1
        continue
    
    if startup_scan_loop_done and 'let path_str = path.to_string_lossy().to_string();' in line:
        new_lines.append(line)
        new_lines.append('            let rel_path = to_relative(dir_root, &path)?;')
        i += 1
        continue
    
    if startup_scan_loop_done and 'on_disk.push(DiskEntry { path: path_str, size, name });' in line:
        new_lines.append(line.replace('path_str', 'rel_path'))
        i += 1
        continue
    
    if startup_scan_loop_done and 'tracker::get_file_by_path' in line and '&path_str' in line:
        new_lines.append(line.replace('&path_str', '&rel_path'))
        i += 1
        continue
    
    if startup_scan_loop_done and 'tracker::upsert_file' in line and '&path_str' in line:
        new_lines.append(line.replace('&path_str', '&rel_path'))
        i += 1
        continue
    
    new_lines.append(line)
    i += 1

# Handle handle_event transformation
# Replace the entire handle_event implementation
handle_event_old = '''/// Handle a single file change event from the watcher: index or delete.
impl Scanner {
    pub fn handle_event(&self, event: FileChangeEvent) -> Result<()> {
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
    }
}'''

handle_event_new = '''/// Handle a single file change event from the watcher: index or delete.
impl Scanner {
    pub fn handle_event(&self, event: FileChangeEvent) -> Result<()> {
        let conn = self.db.get().context("failed to get DB connection")?;
        let file_path = &event.path;

        // Get directory root to compute relative path
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
    }
}'''

if handle_event_old in content:
    content = content.replace(handle_event_old, handle_event_new)
    print("Replaced handle_event")
else:
    # Try alternative pattern matching - this is simpler to do manually with sed later
    pass

write_file(mod_path, content)
print("Updated scanner/mod.rs")

# ============================================================
# 3. indexer.rs - Modify batch_index Phase 2 and index_file to use relative paths
# ============================================================
idx_path = os.path.join(SRC, "indexer.rs")
content = read_file(idx_path)

# In batch_index Phase 2: before Indexer::add_document, compute rel_path from data
# Find the add_document call inside the Phase 2 loop
# Pattern: for extraction in extracted { match extraction { Ok(data) => { ... if let Err(e) = Indexer::add_document(...) }

# More reliably: find the section where we have `let conn = self.db.get()` followed by the loop
conn_line = content.find('let conn = self.db.get().context("failed to get DB connection")?')
if conn_line >= 0:
    # Find the for loop after this
    loop_start = content.find('for extraction in extracted', conn_line)
    if loop_start >= 0:
        # Find the match arm
        match_arm = content.find('Ok(data) =>', loop_start)
        if match_arm >= 0:
            # Find the add_document call within this arm
            add_doc = content.find('Indexer::add_document', match_arm)
            if add_doc >= 0:
                # Insert computation just before the add_document call
                # First determine indentation
                before = content[:add_doc]
                lines_before = before.split('\n')
                last_line = lines_before[-1].rstrip()
                indent = len(last_line) - len(last_line.lstrip())
                
                insert = f'{" " * indent}let dir_config = dir_config::get_dir(&conn, &data.dir_id)?;\n' \
                         f'{" " * indent}let dir_root = &dir_config.path;\n' \
                         f'{" " * indent}let rel_path = to_relative(dir_root, &data.file_path_str)?;\n'
                
                # Also replace file_path_str with rel_path in the add_document args
                # We need to find the specific argument position
                # The add_document call spans multiple lines. Simplest: replace &data.file_path_str with &rel_path
                # But careful not to replace in the error logging line below which uses file_path_str
                
                # Replace the first occurrence of &data.file_path_str after our insertion point
                doc_call = content[add_doc:add_doc+500]
                if '&data.file_path_str' in doc_call:
                    doc_call = doc_call.replace('&data.file_path_str', '&rel_path', 1)
                    content = content[:add_doc] + insert + content[add_doc:doc_call.len() + add_doc] + content[add_doc + len(doc_call):]
                else:
                    # Just insert the computation without modifying the call (it might already be correct)
                    content = content[:add_doc] + insert + content[add_doc:]

# Also modify index_file method to compute and use rel_path
index_file_sig = 'pub fn index_file(&self, file_id: &str, file_path: &Path, dir_id: &str) -> Result<()>{'
if index_file_sig in content:
    # Find the line where file_path_str is computed and used in add_document
    # Look for: let file_path_str = file_path.to_string_lossy().to_string();
    # And later: Indexer::add_document(..., &file_path_str, ...)
    
    # Find add_document call in index_file
    idx_method_start = content.index(index_file_sig)
    # Search within this method (~1000 chars)
    method_section = content[idx_method_start:idx_method_start+2000]
    
    # Find where file_path_str is assigned
    fp_str_assign = method_section.find('let file_path_str = file_path.to_string_lossy().to_string();')
    if fp_str_assign >= 0:
        actual_pos = idx_method_start + fp_str_assign
        # After this assignment, we should compute rel_path instead
        # Insert right after this line
        indent_line = method_section[:fp_str_assign].split('\n')[-1]
        indent = len(indent_line) - len(indent_line.lstrip())
        
        new_rel_code = '\n' + ' ' * indent + 'let dir_config = dir_config::get_dir(&conn, dir_id)?;\n' \
                     + ' ' * indent + 'let dir_root = &dir_config.path;\n' \
                     + ' ' * indent + 'let rel_path = to_relative(dir_root, file_path)?;\n'
        
        # Replace file_path_str usage in add_document with rel_path
        # Find add_document in method_section
        add_doc_idx = method_section.find('Indexer::add_document')
        if add_doc_idx >= 0:
            # Find the &file_path_str argument in this call
            args_start = method_section.find('(', add_doc_idx)
            # Look for &file_path_str within reasonable range
            end_search = min(add_doc_idx + 300, len(method_section))
            arg_segment = method_section:add_search:end]
            if '&file_path_str' in arg_segment:
                # Replace with &rel_path in the add_document call
                # Need precise replacement - find the exact occurrence in method_section
                # Simple: replace the first &file_path_str in the method after add_doc_idx
                new_method_section = method_section[:add_doc_idx+len('(')] + '&' + method_section[len('('):].replace('file_path_str', 'rel_path', 1)
                # This is getting complex. Let me use a different approach.
                pass
        
        # Simpler approach: just replace file_path_str entirely with rel_path in the relevant part of index_file
        # But careful: file_path_str is also used in error logging at the end - we want to keep that as absolute
        # So only replace in the add_document call
        
        # Instead, let's rewrite: remove file_path_str variable entirely and use direct computation
        # This is cleaner
        old_part = method_section[fp_str_assign:fp_str_assign + len('let file_path_str = file_path.to_string_lossy().to_string();\n')]
        # Replace with new computation
        replacement = fp_str_assign + '\n' + ' ' * indent + 'let dir_config = dir_config::get_dir(&conn, dir_id)?;\n' \
                   + ' ' * indent + 'let dir_root = &dir_config.path;\n' \
                   + ' ' * indent + 'let rel_path = to_relative(dir_root, file_path)?;\n'
        
        # Actually, the file_path_str variable itself might still be needed for error logging
        # Keep it but also compute rel_path
        replacement = method_section[fp_str_assign + len('let file_path_str = file_path.to_string_lossy().to_string();\n')+1:]  # skip ahead
        # Hmm, this is getting too tangled for regex surgery
    
    # Given the complexity, let me take yet another approach: 
    # Simply modify index_file to NOT use file_path_str for add_document, but compute rel_path separately
    # and pass it to add_document while keeping file_path_str for error logging
    
    # Find the add_document call specifically and substitute
    add_doc_call_pos = method_section.find('Indexer::add_document')
    if add_doc_call_pos >= 0:
        full_add_doc = method_section[add_doc_call_pos:add_doc_call_pos+500]
        # Replace &file_path_str with &rel_path in this call
        modified_call = full_add_doc.replace('&file_path_str', '&rel_path', 1)
        # Replace back into method_section
        method_section = method_section[:add_doc_call_pos] + modified_call + method_section[add_doc_call_pos + len(full_add_doc):]
        
        # Insert the rel_path computation before the add_document call but after file_path_str assignment
        # Find where to insert (right after file_path_str assignment, before any other heavy logic)
        insert_pos = fp_str_assign + len('let file_path_str = file_path.to_string_lossy().to_string();\n')
        indent_line = method_section[:fp_str_assign].split('\n')[-1]
        indent = len(indent_line) - len(indent_line.lstrip())
        compute_rel = '\n' + ' ' * indent + 'let dir_config = dir_config::get_dir(&conn, dir_id)?;\n' \
                    + ' ' * indent + 'let dir_root = &dir_config.path;\n' \
                    + ' ' * indent + 'let rel_path = to_relative(dir_root, file_path)?;\n'
        method_section = method_section[:insert_pos] + compute_rel + method_section[insert_pos:]
        
        # Replace back into main content
        content = content[:idx_method_start + fp_str_assign + len('let file_path_str = file_path.to_string_lossy().to_string();\n')] + \
                  method_section[fp_str_assign + len('let file_path_str = file_path.to_string_lossy().to_string();\n')+:] + \
                  content[idx_method_start + fp_str_assign + len('let file_path_str = file_path.to_string_lossy().to_string();\n') + len(method_section):]

write_file(idx_path, content)
print("Updated indexer.rs")

print("\nAll transformations completed!")

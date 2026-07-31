#!/usr/bin/env python3
# Comprehensive modification of scanner/mod.rs to use relative paths

import os

SRC = "/Volumes/Data/Project/Link-Searcher/src-tauri/src"
mod_path = os.path.join(SRC, "scanner", "mod.rs")

with open(mod_path, 'r', encoding='utf-8') as f:
    lines = f.readlines()

new_lines = []

# State tracking
in_full_scan = False
in_incremental_scan = False
in_startup_scan = False
in_handle_event = False
in_full_scan_loop = False
in_inc_scan_loop = False
in_startup_scan_loop = False

# Track if we've added dir_root for each function
full_scan_dir_root_added = False
inc_scan_dir_root_added = False
startup_scan_dir_root_added = False

i = 0
while i < len(lines):
    line = lines[i]
    
    # Check for function starts
    if 'pub fn full_scan' in line:
        in_full_scan = True
        new_lines.append(line)
        i += 1
        continue
    
    if 'pub fn incremental_scan' in line:
        in_incremental_scan = True
        new_lines.append(line)
        i += 1
        continue
    
    if 'pub fn startup_scan' in line:
        in_startup_scan = True
        new_lines.append(line)
        i += 1
        continue
    
    if 'pub fn handle_event' in line:
        in_handle_event = True
        new_lines.append(line)
        i += 1
        continue
    
    # Handle full_scan function
    if in_full_scan:
        # Insert dir_root after config.log line
        if not full_scan_dir_root_added and 'log::info!("[SCAN] 开始扫描: {}", config.path);' in line:
            new_lines.append(line)
            new_lines.append('        let dir_root = &config.path;\n')
            full_scan_dir_root_added = True
            i += 1
            continue
        
        # Detect start of for loop in full_scan
        if not in_full_scan_loop and 'for entry in walkdir::WalkDir::new(&config.path)' in line:
            in_full_scan_loop = True
            new_lines.append(line)
            i += 1
            continue
        
        if in_full_scan_loop:
            # When we see 'let path = entry.path().to_path_buf();' - keep it and add rel_path after
            if 'let path = entry.path().to_path_buf();' in line:
                new_lines.append(line)
                i += 1
                continue
            
            # After path_str assignment, insert rel_path
            if 'let path_str = path.to_string_lossy().to_string();' in line:
                new_lines.append(line)
                new_lines.append('            let rel_path = to_relative(dir_root, &path)?;\n')
                i += 1
                continue
            
            # Replace disk_paths.push with rel_path
            if 'disk_paths.push(path_str.clone());' in line:
                new_lines.append(line.replace('path_str', 'rel_path'))
                i += 1
                continue
            
            # Replace get_file_by_path with rel_path
            if 'tracker::get_file_by_path' in line and '&path_str' in line:
                new_lines.append(line.replace('&path_str', '&rel_path'))
                i += 1
                continue
            
            # Replace upsert_file with rel_path
            if 'tracker::upsert_file' in line and '&path_str' in line:
                new_lines.append(line.replace('&path_str', '&rel_path'))
                i += 1
                continue
        
        new_lines.append(line)
        i += 1
        continue
    
    # Handle incremental_scan function
    if in_incremental_scan:
        # Insert dir_root after config assignment (before exclude)
        if not inc_scan_dir_root_added and '.ok_or_else(|| anyhow::anyhow!("dir_config not found: {dir_id}"))?;' in line:
            new_lines.append(line)
            i += 1
            # Next line should be let exclude = ..., insert dir_root between them
            if i < len(lines) and 'let exclude = parse_exclude_patterns' in lines[i]:
                new_lines.append('        let dir_root = &config.path;\n')
                inc_scan_dir_root_added = True
            continue
        
        # Detect start of for loop in incremental_scan
        if not in_inc_scan_loop and ('for entry in walker' in line or 'for entry in walker:' in line):
            in_inc_scan_loop = True
            new_lines.append(line)
            i += 1
            continue
        
        if in_inc_scan_loop:
            if 'let path = entry.path().to_path_buf();' in line:
                new_lines.append(line)
                i += 1
                continue
            
            if 'let path_str = path.to_string_lossy().to_string();' in line:
                new_lines.append(line)
                new_lines.append('            let rel_path = to_relative(dir_root, &path)?;\n')
                i += 1
                continue
            
            # Need to find on_disk.push line - it comes right after path_str in original
            if 'on_disk.push(path_str.clone());' in line:
                new_lines.append(line.replace('path_str', 'rel_path'))
                i += 1
                continue
            
            if 'tracker::get_file_by_path' in line and '&path_str' in line:
                new_lines.append(line.replace('&path_str', '&rel_path'))
                i += 1
                continue
            
            if 'tracker::upsert_file' in line and '&path_str' in line:
                new_lines.append(line.replace('&path_str', '&rel_path'))
                i += 1
                continue
        
        new_lines.append(line)
        i += 1
        continue
    
    # Handle startup_scan function
    if in_startup_scan:
        # Insert dir_root after config.log line
        if not startup_scan_dir_root_added and 'log::info!("[STARTUP] 启动扫描: {}", config.path);' in line:
            new_lines.append(line)
            new_lines.append('        let dir_root = &config.path;\n')
            startup_scan_dir_root_added = True
            i += 1
            continue
        
        # Detect start of for loop
        if 'for entry in walker' in line and not in_startup_scan_loop:
            in_startup_scan_loop = True
            new_lines.append(line)
            i += 1
            continue
        
        if in_startup_scan_loop:
            if 'let path = entry.path().to_path_buf();' in line:
                new_lines.append(line)
                i += 1
                continue
            
            if 'let path_str = path.to_string_lossy().to_string();' in line:
                new_lines.append(line)
                new_lines.append('            let rel_path = to_relative(dir_root, &path)?;\n')
                i += 1
                continue
            
            if 'on_disk.push(DiskEntry { path: path_str, size, name });' in line:
                new_lines.append(line.replace('path_str', 'rel_path'))
                i += 1
                continue
            
            if 'tracker::get_file_by_path' in line and '&path_str' in line:
                new_lines.append(line.replace('&path_str', '&rel_path'))
                i += 1
                continue
            
            if 'tracker::upsert_file' in line and '&path_str' in line:
                new_lines.append(line.replace('&path_str', '&rel_path'))
                i += 1
                continue
        
        new_lines.append(line)
        i += 1
        continue
    
    # Handle handle_event function
    if in_handle_event:
        # We need to modify the Create/Modify branch to use relative paths
        # But handle_event doesn't have direct access to dir_root - we'd need to query DB by dir_id
        # For simplicity, we'll keep using absolute paths here but convert to relative for DB ops
        # Actually, looking at FileChangeEvent, it has dir_id but not dir_path. 
        # So we need to look up dir_path from DB. This is more complex.
        # 
        # Alternative approach: modify handle_event to accept both dir_id and dir_path, or pass config.
        # But the task says to convert event path to relative before upsert_file. 
        # To do that we need dir_root for that dir_id. Let's add a lookup inside handle_event.
        
        # For now, let's just note that handle_event needs modification. 
        # We'll come back to it after we ensure core scans work.
        new_lines.append(line)
        i += 1
        continue
    
    # Default: just copy the line
    new_lines.append(line)
    i += 1

# Write modified file
with open(mod_path, 'w', encoding='utf-8') as f:
    f.writelines(new_lines)

print(f"Wrote modified mod.rs")
print(f"full_scan_dir_root_added: {full_scan_dir_root_added}")
print(f"inc_scan_dir_root_added: {inc_scan_dir_root_added}")
print(f"startup_scan_dir_root_added: {startup_scan_dir_root_added}")

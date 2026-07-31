#!/usr/bin/env python3
"""Modify scanner/mod.rs to use relative paths."""

import os
import sys

SRC = "/Volumes/Data/Project/Link-Searcher/src-tauri/src"
mod_path = os.path.join(SRC, "scanner", "mod.rs")

with open(mod_path, 'r', encoding='utf-8') as f:
    lines = f.readlines()

# First pass: find line numbers for key sections
def find_line(pattern, start=0):
    for i in range(start, len(lines)):
        if pattern in lines[i]:
            return i
    return -1

# 1. Add dir_root after log line in full_scan (around line 68-69)
for i, line in enumerate(lines):
    if 'log::info!("[SCAN] 开始扫描: {}", config.path);' in line:
        # Insert after this line
        lines.insert(i+1, '        let dir_root = &config.path;\n')
        print(f'Added dir_root at line {i+2}')
        break

# Now work with updated lines
# 2. Modify full_scan loop
for i, line in enumerate(lines):
    if line.strip().startswith('for entry in walkdir::WalkDir::new(&config.path)'):
        # Find the next few lines to inject rel_path computation
        # Look for 'let path = entry.path().to_path_buf();' after this
        j = i + 1
        while j < len(lines) and not lines[j].strip().startswith('let path ='):
            j += 1
        if j < len(lines):
            # Insert rel_path after path_str assignment
            k = j + 1
            while k < len(lines) and not ('path_str = path.to_string_lossy()' in lines[k]):
                k += 1
            if k < len(lines):
                # Insert after path_str line
                lines.insert(k+1, '            let rel_path = to_relative(dir_root, &path)?;\n')
                print(f'Inserted rel_path computation in full_scan at line {k+2}')
                
                # Find and replace disk_paths.push(path_str.clone())
                for m in range(k+2, len(lines)):
                    if 'disk_paths.push(path_str.clone());' in lines[m]:
                        lines[m] = lines[m].replace('path_str', 'rel_path')
                        print(f'Replaced disk_paths with rel_path at line {m+1}')
                        break
                
                # Replace get_file_by_path with rel_path
                for m in range(k+2, len(lines)):
                    if 'tracker::get_file_by_path' in lines[m] and '&path_str' in lines[m]:
                        lines[m] = lines[m].replace('&path_str', '&rel_path')
                        print(f'Replaced get_file_by_path param at line {m+1}')
                        break
                
                # Replace upsert_file with rel_path
                for m in range(k+2, len(lines)):
                    if 'tracker::upsert_file' in lines[m] and '&path_str' in lines[m]:
                        lines[m] = lines[m].replace('&path_str', '&rel_path')
                        print(f'Replaced upsert_file param at line {m+1}')
                        break
        break

# 3. Modify incremental_scan loop
for i, line in enumerate(lines):
    if 'pub fn incremental_scan' in line and 'Scanner' in ''.join(lines[max(0,i-5):i+1]):
        # Find the for loop after this
        for j in range(i, len(lines)):
            if 'for entry in walker:' in lines[j] or 'for entry in walker' in lines[j]:
                # Actually looking for the for entry line without colon
                pass
        # Alternative: find 'for entry in walker' 
        for j in range(i, len(lines)):
            if 'for entry' in lines[j] and 'walker' in lines[j]:
                # Find path_str assignment
                for k in range(j, len(lines)):
                    if 'let path_str = path.to_string_lossy()' in lines[k]:
                        lines.insert(k+1, '            let rel_path = to_relative(dir_root, &path)?;\n')
                        print(f'Inserted rel_path in incremental_scan at line {k+2}')
                        
                        # Fix on_disk.push to use rel_path
                        for m in range(k+2, len(lines)):
                            if 'on_disk.push(path_str.clone());' in lines[m]:
                                lines[m] = lines[m].replace('path_str', 'rel_path')
                                print(f'Replaced on_disk.push with rel_path at line {m+1}')
                                break
                        
                        # Replace get_file_by_path
                        for m in range(k+2, len(lines)):
                            if 'tracker::get_file_by_path' in lines[m] and '&path_str' in lines[m]:
                                lines[m] = lines[m].replace('&path_str', '&rel_path')
                                break
                        
                        # Replace upsert_file
                        for m in range(k+2, len(lines)):
                            if 'tracker::upsert_file' in lines[m] and '&path_str' in lines[m]:
                                lines[m] = lines[m].replace('&path_str', '&rel_path')
                                break
                        break
                break
        break

# 4. Modify startup_scan loop  
for i, line in enumerate(lines):
    if 'pub fn startup_scan' in line:
        for j in range(i, len(lines)):
            if 'for entry in walker' in lines[j]:
                for k in range(j, len(lines)):
                    if 'let path_str = path.to_string_lossy()' in lines[k]:
                        lines.insert(k+1, '            let rel_path = to_relative(dir_root, &path)?;\n')
                        print(f'Inserted rel_path in startup_scan at line {k+2}')
                        
                        # Fix on_disk.push(DiskEntry { path: path_str ... })
                        for m in range(k+2, len(lines)):
                            if 'on_disk.push(DiskEntry { path: path_str,' in lines[m]:
                                lines[m] = lines[m].replace('path_str', 'rel_path')
                                print(f'Replaced DiskEntry path with rel_path at line {m+1}')
                                break
                        
                        # Replace get_file_by_path
                        for m in range(k+2, len(lines)):
                            if 'tracker::get_file_by_path' in lines[m] and '&path_str' in lines[m]:
                                lines[m] = lines[m].replace('&path_str', '&rel_path')
                                break
                        
                        # Replace upsert_file
                        for m in range(k+2, len(lines)):
                            if 'tracker::upsert_file' in lines[m] and '&path_str' in lines[m]:
                                lines[m] = lines[m].replace('&path_str', '&rel_path')
                                break
                        break
                break
        break

# Write back
with open(mod_path, 'w', encoding='utf-8') as f:
    f.writelines(lines)

print('\nModified file written.')
# Verify changes
with open(mod_path, 'r', encoding='utf-8') as f:
    content = f.read()
    if 'let dir_root = &config.path;' in content:
        print('✓ dir_root found')
    if 'let rel_path = to_relative(dir_root, &path)' in content:
        count = content.count('let rel_path = to_relative(dir_root, &path)')
        print(f'✓ rel_path added {count} times')

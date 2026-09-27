# 🗜️ Squishy

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)


A convenient wrapper around the [backhand](https://github.com/wcampbell0x2a/backhand) library for reading and extracting files from SquashFS and DwarFS filesystems. Provides both a library and CLI tool.

## Features

- 📚 **Library Features**
  - Read and extract files from SquashFS and DwarFS filesystems
  - Extract whole images to a directory, preserving permissions and symlink targets
  - Traverse filesystem entries
  - Handle symlinks with cycle detection
  - Search for files using custom predicates

- 🛠️ **CLI Features**
  - Extract AppImage resources:
    - Icon files (PNG/SVG)
    - Desktop entries
    - AppStream metadata
  - Flexible output options

## Installation

### From crates.io

```bash
cargo install squishy-cli
```

### From source

```bash
git clone https://github.com/pkgforge/squishy-rs
cd squishy-rs
cargo install --path squishy-cli
```

## Library Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
squishy = "0.5.3"
```

### Example

```rust
use squishy::{SquashFS, EntryKind};
use std::path::Path;

// Open a SquashFS file
let squashfs = SquashFS::from_path(&Path::new("example.squashfs"))?;

// List all entries
for entry in squashfs.entries() {
    println!("{}", entry.path.display());
}

// Optionally, parallel read with rayon
use rayon::iter::ParallelIterator;
for entry in squashfs.par_entries() {
    println!("{}", entry.path.display());
}

// Write file entries to disk
for entry in squashfs.entries() {
    if let EntryKind::File(file) = entry.kind {
        squashfs.write_file(file, "/path/to/output/file")?;
    }
}

// Read a specific file
// Note: the whole file content will be loaded into memory
let contents = squashfs.read_file("path/to/file.txt")?;

// Extract the whole image to a directory
// Files keep their stored permissions, symlink targets are preserved
// verbatim, and directory modes are applied after their contents
squashfs.extract("/path/to/output/dir")?;
```

## CLI Usage

The CLI tool provides convenient commands for working with AppImage files.

### Basic Commands

```bash
# Extract icon from an AppImage
squishy appimage path/to/app.AppImage --icon

# Extract desktop file
squishy appimage path/to/app.AppImage --desktop

# Extract AppStream metadata
squishy appimage path/to/app.AppImage --appstream

# Extract and save files to a specific directory
squishy appimage path/to/app.AppImage --icon --write /output/path

# Extract multiple resources at once
squishy appimage path/to/app.AppImage --icon --desktop --appstream --write

# Filter path by query
squishy appimage path/to/app.AppImage --filter "squishy" --icon --desktop --appstream --write

# Provide custom offset (it'd be calculated automatically if not provided)
# Appimage offset can be read using `path/to/app.AppImage --appimage-offset`
squishy appimage path/to/app.AppImage --offset 128128 --icon --desktop --appstream --write

# Extract contents of squashfs to a specific directory
squishy unsquashfs path/to/app.AppImage -w /output/path
```

### Command Options

- `--offset`: Custom offset (i.e. the size of ELF)
- `--filter`: Filter the files using provided query
- `--icon`: Extract application icon
- `--desktop`: Extract desktop entry file
- `--appstream`: Extract AppStream metadata
- `--write`: Write files to disk (optional path argument)

## License

This project is licensed under the [MIT] License - see the [LICENSE](LICENSE) file for details.

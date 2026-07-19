use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use goblin::elf::Elf;

use crate::{error::SquishyError, EntryKind, SquashFS};

#[cfg(feature = "dwarfs")]
use crate::dwarfs::{DwarFS, DwarFSEntryKind, DWARFS_MAGIC};

pub type Result<T> = std::result::Result<T, SquishyError>;

/// Magic bytes for SquashFS filesystem
const SQUASHFS_MAGIC: &[u8] = b"hsqs";

/// Upper bound on the size of a candidate desktop file, used to skip large
/// `.desktop`-suffixed binaries without reading them.
const MAX_DESKTOP_SIZE: u64 = 1 << 20;

/// Ranks a desktop file candidate for [`AppImage::find_desktop`].
///
/// Lower sorts first: the image root (the AppImage-spec location) beats
/// `/usr/share/applications/`, which beats any other path. Within a location,
/// a file stem exactly matching the filter is preferred, then paths compare
/// lexicographically for a deterministic order.
fn desktop_rank(path: &Path, filter: Option<&str>) -> (u8, u8, PathBuf) {
    let lossy = path.to_string_lossy().to_lowercase();
    let trimmed = lossy.trim_start_matches('/');
    let location = if !trimmed.contains('/') {
        0
    } else if trimmed.starts_with("usr/share/applications/")
        || lossy.contains("/usr/share/applications/")
    {
        1
    } else {
        2
    };
    let stem_mismatch = match (filter, path.file_stem()) {
        (Some(filter), Some(stem)) if stem.eq_ignore_ascii_case(filter) => 0,
        _ => 1,
    };
    (location, stem_mismatch, path.to_path_buf())
}

/// Returns whether the given bytes are a desktop entry file.
fn is_desktop_file(bytes: &[u8]) -> bool {
    if bytes.starts_with(goblin::elf::header::ELFMAG) {
        return false;
    }
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .any(|line| line == "[Desktop Entry]")
}

/// Detected filesystem type in an AppImage
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesystemType {
    SquashFS,
    #[cfg(feature = "dwarfs")]
    DwarFS,
}

/// Unified entry type that works with both SquashFS and DwarFS
#[derive(Debug)]
pub struct AppImageEntry {
    pub path: PathBuf,
    pub size: u64,
    pub kind: AppImageEntryKind,
}

/// Unified entry kind for both filesystem types
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppImageEntryKind {
    File,
    Directory,
    Symlink(PathBuf),
    Unknown,
}

/// Get offset for AppImage. This is used by default if no offset is provided.
///
/// # Arguments
/// * `path` - Path to the appimage file.
///
/// # Returns
/// Offset of the appimage, or an error if it fails to parse Elf
pub fn get_offset<P: AsRef<Path>>(path: P) -> std::io::Result<u64> {
    let mut file = File::open(path)?;

    let mut elf_header_raw = [0; 64];
    file.read_exact(&mut elf_header_raw)?;

    let section_table_offset = u64::from_le_bytes(elf_header_raw[40..48].try_into().unwrap());
    let section_count = u16::from_le_bytes(elf_header_raw[60..62].try_into().unwrap());

    let section_table_size = section_count as u64 * 64;
    let required_bytes = section_table_offset + section_table_size;

    let mut header_data = vec![0; required_bytes as usize];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut header_data)?;

    let elf = Elf::parse(&header_data)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let section_table_end =
        elf.header.e_shoff + (elf.header.e_shentsize as u64 * elf.header.e_shnum as u64);

    let last_section_end = elf
        .section_headers
        .last()
        .map(|section| section.sh_offset + section.sh_size)
        .unwrap_or(0);

    Ok(section_table_end.max(last_section_end))
}

/// Detect the filesystem type at the given offset in the file.
///
/// # Arguments
/// * `path` - Path to the file.
/// * `offset` - Offset at which to check for filesystem magic.
///
/// # Returns
/// The detected filesystem type, or an error if no supported filesystem is found.
pub fn detect_filesystem_type<P: AsRef<Path>>(path: P, offset: u64) -> Result<FilesystemType> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;

    let mut magic = [0u8; 6];
    file.read_exact(&mut magic)?;

    // Check SquashFS magic (4 bytes)
    if &magic[..4] == SQUASHFS_MAGIC {
        return Ok(FilesystemType::SquashFS);
    }

    // Check DwarFS magic (6 bytes)
    #[cfg(feature = "dwarfs")]
    if &magic == DWARFS_MAGIC {
        return Ok(FilesystemType::DwarFS);
    }

    Err(SquishyError::NoFilesystemFound)
}

/// The internal filesystem representation for AppImage
pub enum AppImageFS<'a> {
    SquashFS(SquashFS<'a>),
    #[cfg(feature = "dwarfs")]
    DwarFS(DwarFS),
}

pub struct AppImage<'a> {
    filter: Option<&'a str>,
    pub fs: AppImageFS<'a>,
}

impl<'a> AppImage<'a> {
    /// Creates a new AppImage instance
    ///
    /// # Arguments
    ///
    /// * `filter` - Filter to apply
    /// * `path` - Path to AppImage
    /// * `offset` - Offset to seek to
    pub fn new<P: AsRef<Path>>(
        filter: Option<&'a str>,
        path: &'a P,
        offset: Option<u64>,
    ) -> Result<Self> {
        let offset = offset.unwrap_or(get_offset(path)?);
        let fs_type = detect_filesystem_type(path, offset)?;

        let fs = match fs_type {
            FilesystemType::SquashFS => {
                let squashfs = SquashFS::from_path_with_offset(path, offset).map_err(|_| {
                    SquishyError::InvalidSquashFS(
                        "Couldn't find squashfs. Try providing valid offset.".to_owned(),
                    )
                })?;
                AppImageFS::SquashFS(squashfs)
            }
            #[cfg(feature = "dwarfs")]
            FilesystemType::DwarFS => {
                let dwarfs = DwarFS::from_path_with_offset(path, offset)?;
                AppImageFS::DwarFS(dwarfs)
            }
        };

        Ok(AppImage { filter, fs })
    }

    /// Creates a new AppImage instance with SquashFS explicitly
    ///
    /// # Arguments
    ///
    /// * `filter` - Filter to apply
    /// * `path` - Path to AppImage
    /// * `offset` - Offset to seek to
    pub fn new_squashfs<P: AsRef<Path>>(
        filter: Option<&'a str>,
        path: &'a P,
        offset: Option<u64>,
    ) -> Result<Self> {
        let offset = offset.unwrap_or(get_offset(path)?);
        let squashfs = SquashFS::from_path_with_offset(path, offset).map_err(|_| {
            SquishyError::InvalidSquashFS(
                "Couldn't find squashfs. Try providing valid offset.".to_owned(),
            )
        })?;
        Ok(AppImage {
            filter,
            fs: AppImageFS::SquashFS(squashfs),
        })
    }

    /// Creates a new AppImage instance with DwarFS explicitly
    ///
    /// # Arguments
    ///
    /// * `filter` - Filter to apply
    /// * `path` - Path to AppImage
    /// * `offset` - Offset to seek to
    #[cfg(feature = "dwarfs")]
    pub fn new_dwarfs<P: AsRef<Path>>(
        filter: Option<&'a str>,
        path: P,
        offset: Option<u64>,
    ) -> Result<Self> {
        let offset = offset.unwrap_or(get_offset(&path)?);
        let dwarfs = DwarFS::from_path_with_offset(path, offset)?;
        Ok(AppImage {
            filter,
            fs: AppImageFS::DwarFS(dwarfs),
        })
    }

    /// Returns the filesystem type of this AppImage
    pub fn filesystem_type(&self) -> FilesystemType {
        match &self.fs {
            AppImageFS::SquashFS(_) => FilesystemType::SquashFS,
            #[cfg(feature = "dwarfs")]
            AppImageFS::DwarFS(_) => FilesystemType::DwarFS,
        }
    }

    /// Get a reference to the SquashFS filesystem, if this AppImage uses SquashFS
    pub fn as_squashfs(&self) -> Option<&SquashFS<'a>> {
        match &self.fs {
            AppImageFS::SquashFS(fs) => Some(fs),
            #[cfg(feature = "dwarfs")]
            _ => None,
        }
    }

    /// Get a mutable reference to the DwarFS filesystem, if this AppImage uses DwarFS
    #[cfg(feature = "dwarfs")]
    pub fn as_dwarfs_mut(&mut self) -> Option<&mut DwarFS> {
        match &mut self.fs {
            AppImageFS::DwarFS(fs) => Some(fs),
            _ => None,
        }
    }

    /// Returns an iterator over unified AppImageEntry items
    pub fn entries(&self) -> Box<dyn Iterator<Item = AppImageEntry> + '_> {
        match &self.fs {
            AppImageFS::SquashFS(squashfs) => {
                Box::new(squashfs.entries().map(|entry| AppImageEntry {
                    path: entry.path.clone(),
                    size: entry.size as u64,
                    kind: match &entry.kind {
                        EntryKind::File(_) => AppImageEntryKind::File,
                        EntryKind::Directory => AppImageEntryKind::Directory,
                        EntryKind::Symlink(target) => AppImageEntryKind::Symlink(target.clone()),
                        EntryKind::Unknown => AppImageEntryKind::Unknown,
                    },
                }))
            }
            #[cfg(feature = "dwarfs")]
            AppImageFS::DwarFS(dwarfs) => Box::new(dwarfs.entries().map(|entry| AppImageEntry {
                path: entry.path.clone(),
                size: entry.size,
                kind: match &entry.kind {
                    DwarFSEntryKind::File => AppImageEntryKind::File,
                    DwarFSEntryKind::Directory => AppImageEntryKind::Directory,
                    DwarFSEntryKind::Symlink(target) => AppImageEntryKind::Symlink(target.clone()),
                    _ => AppImageEntryKind::Unknown,
                },
            })),
        }
    }

    /// Find icon in AppImage, filtered
    ///
    /// It looks for icon in order:
    /// - DirIcon at AppImage root
    /// - Largest png icon in /usr/share/icons
    /// - Largest svg icon in /usr/share/icons
    /// - Largest png icon in any path
    /// - Largest svg icon in any path
    ///
    /// # Returns
    /// A unified entry to the icon, if found
    pub fn find_icon(&self) -> Option<AppImageEntry> {
        self.search_diricon()
            .or_else(|| self.find_largest_icon_path())
            .or_else(|| self.find_png_icon())
            .or_else(|| self.find_svg_icon())
            .and_then(|entry| self.resolve_symlink(entry))
    }

    /// Find desktop file in AppImage, filtered
    ///
    /// The `.desktop` suffix alone is not enough, since a binary can share it.
    /// Oversized candidates are skipped and the contents are verified to be a
    /// desktop entry. Candidates are ranked by location: the AppImage-spec
    /// desktop file at the image root wins, then `/usr/share/applications/`,
    /// then anything else. Within a location, a file stem exactly matching the
    /// filter is preferred, so bundled rootfs entries don't shadow the app's
    /// own desktop file.
    ///
    /// # Returns
    /// A unified entry to the desktop file, if found
    pub fn find_desktop(&mut self) -> Option<AppImageEntry> {
        let mut candidates: Vec<AppImageEntry> = self
            .entries()
            .filter(|entry| {
                let path = entry.path.to_string_lossy().to_lowercase();
                self.filter_path(&path) && path.ends_with(".desktop")
            })
            .filter_map(|entry| self.resolve_symlink(entry))
            .filter(|entry| entry.kind == AppImageEntryKind::File && entry.size <= MAX_DESKTOP_SIZE)
            .collect();

        let filter = self.filter;
        candidates.sort_by_cached_key(|entry| desktop_rank(&entry.path, filter));

        candidates.into_iter().find(|entry| {
            self.read_entry(entry)
                .map(|bytes| is_desktop_file(&bytes))
                .unwrap_or(false)
        })
    }

    /// Find appstream file in AppImage (appdata.xml | metainfo.xml)
    ///
    /// # Returns
    /// A unified entry to the appstream, if found
    pub fn find_appstream(&self) -> Option<AppImageEntry> {
        let appstream = self.entries().find(|entry| {
            let path = entry.path.to_string_lossy().to_lowercase();
            self.filter_path(&path)
                && (path.ends_with("appdata.xml") || path.ends_with("metainfo.xml"))
        });

        appstream.and_then(|entry| self.resolve_symlink(entry))
    }

    /// Resolve symlink to final target entry
    fn resolve_symlink(&self, entry: AppImageEntry) -> Option<AppImageEntry> {
        match &entry.kind {
            AppImageEntryKind::Symlink(target) => {
                let target = target.clone();
                self.entries()
                    .find(|e| e.path == target)
                    .and_then(|e| self.resolve_symlink(e))
            }
            _ => Some(entry),
        }
    }

    /// Find DirIcon at AppImage root
    fn search_diricon(&self) -> Option<AppImageEntry> {
        self.entries()
            .find(|entry| entry.path.to_string_lossy() == "/.DirIcon")
    }

    /// Helper method to filter paths
    fn filter_path(&self, path: &str) -> bool {
        self.filter
            .as_ref()
            .map_or(true, |filter| path.contains(filter))
    }

    /// Find largest png (preferred) or svg icon in /usr/share/icons, filtered
    fn find_largest_icon_path(&self) -> Option<AppImageEntry> {
        let png_entry = self
            .entries()
            .filter(|entry| {
                let path = entry.path.to_string_lossy().to_lowercase();
                path.starts_with("/usr/share/icons/")
                    && self.filter_path(&path)
                    && path.ends_with(".png")
            })
            .max_by_key(|entry| entry.size);

        if png_entry.is_some() {
            return png_entry;
        }

        self.entries().find(|entry| {
            let path = entry.path.to_string_lossy().to_lowercase();
            path.starts_with("/usr/share/icons")
                && self.filter_path(&path)
                && path.ends_with(".svg")
        })
    }

    /// Find largest png icon in AppImage, filtered
    fn find_png_icon(&self) -> Option<AppImageEntry> {
        self.entries()
            .filter(|entry| {
                let p = entry.path.to_string_lossy().to_lowercase();
                self.filter_path(&p) && p.ends_with(".png")
            })
            .max_by_key(|entry| entry.size)
    }

    /// Find largest svg icon in AppImage, filtered
    fn find_svg_icon(&self) -> Option<AppImageEntry> {
        self.entries().find(|entry| {
            let path = entry.path.to_string_lossy().to_lowercase();
            self.filter_path(&path) && path.ends_with(".svg")
        })
    }

    /// Read file contents from the AppImage
    ///
    /// # Arguments
    /// * `path` - Path to the file within the AppImage
    ///
    /// # Returns
    /// The contents of the file as a Vec<u8>
    pub fn read_file<P: AsRef<Path>>(&mut self, path: P) -> Result<Vec<u8>> {
        match &mut self.fs {
            AppImageFS::SquashFS(squashfs) => squashfs.read_file(path),
            #[cfg(feature = "dwarfs")]
            AppImageFS::DwarFS(dwarfs) => dwarfs.read_file(path),
        }
    }

    /// Read file contents from a unified AppImageEntry
    ///
    /// # Arguments
    /// * `entry` - The entry to read
    ///
    /// # Returns
    /// The contents of the file as a Vec<u8>
    pub fn read_entry(&mut self, entry: &AppImageEntry) -> Result<Vec<u8>> {
        if entry.kind != AppImageEntryKind::File {
            return Err(SquishyError::InvalidSquashFS("Entry is not a file".into()));
        }
        self.read_file(&entry.path)
    }

    /// Write file from a unified AppImageEntry to the specified destination
    ///
    /// # Arguments
    /// * `entry` - The entry to extract
    /// * `dest` - The destination path to write the file to
    ///
    /// # Returns
    /// An empty result, or an error if the file cannot be read or written
    pub fn write_entry<P: AsRef<Path>>(&mut self, entry: &AppImageEntry, dest: P) -> Result<()> {
        use std::io::Write;

        if entry.kind != AppImageEntryKind::File {
            return Err(SquishyError::InvalidSquashFS("Entry is not a file".into()));
        }

        let contents = self.read_file(&entry.path)?;
        let output_file = std::fs::File::create(&dest)?;
        let mut writer = std::io::BufWriter::new(output_file);
        writer.write_all(&contents)?;

        Ok(())
    }

    /// Write file from a unified AppImageEntry to the specified destination with permissions
    ///
    /// # Arguments
    /// * `entry` - The entry to extract
    /// * `dest` - The destination path to write the file to
    /// * `mode` - The file mode/permissions to set
    ///
    /// # Returns
    /// An empty result, or an error if the file cannot be read or written
    pub fn write_entry_with_permissions<P: AsRef<Path>>(
        &mut self,
        entry: &AppImageEntry,
        dest: P,
        mode: u32,
    ) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        self.write_entry(entry, &dest)?;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(mode))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{desktop_rank, is_desktop_file};

    fn ranked(paths: &[&str], filter: Option<&str>) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
        paths.sort_by_cached_key(|p| desktop_rank(p, filter));
        paths
    }

    #[test]
    fn prefers_root_desktop_over_bundled_rootfs() {
        let order = ranked(
            &[
                "/usr/share/applications/avahi-discover.desktop",
                "/usr/share/applications/steam.desktop",
                "/steam.desktop",
            ],
            None,
        );
        assert_eq!(order[0], Path::new("/steam.desktop"));
    }

    #[test]
    fn prefers_stem_matching_filter_within_location() {
        let order = ranked(
            &[
                "/usr/share/applications/steam-screensaver-fix.desktop",
                "/usr/share/applications/steam.desktop",
            ],
            Some("steam"),
        );
        assert_eq!(
            order[0],
            Path::new("/usr/share/applications/steam.desktop")
        );
    }

    #[test]
    fn prefers_applications_dir_over_other_paths() {
        let order = ranked(
            &[
                "/usr/lib/steam/steam.desktop",
                "/usr/share/applications/steam.desktop",
            ],
            None,
        );
        assert_eq!(
            order[0],
            Path::new("/usr/share/applications/steam.desktop")
        );
    }

    #[test]
    fn detects_desktop_entry() {
        assert!(is_desktop_file(b"[Desktop Entry]\nName=Foo\n"));
        assert!(is_desktop_file(b"\n  [Desktop Entry]  \nName=Foo\n"));
    }

    #[test]
    fn rejects_elf_binary() {
        assert!(!is_desktop_file(b"\x7fELF\x02\x01\x01\x00"));
    }

    #[test]
    fn rejects_embedded_header_substring() {
        assert!(!is_desktop_file(b"binary junk [Desktop Entry] more junk"));
        assert!(!is_desktop_file(b"\x7fELF\x00[Desktop Entry]\x00"));
    }
}

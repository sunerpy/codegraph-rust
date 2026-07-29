//! Private cross-platform filesystem-object identity helpers.

#[cfg(windows)]
use std::fs::OpenOptions;
use std::fs::{File, Metadata};
use std::io;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileIdentity {
    #[cfg(unix)]
    Unix { device: u64, inode: u64 },
    #[cfg(windows)]
    Windows {
        volume_serial_number: u64,
        file_id: [u8; 16],
    },
    #[cfg(not(any(unix, windows)))]
    Portable {
        len: u64,
        modified: Option<std::time::SystemTime>,
        created: Option<std::time::SystemTime>,
    },
}

pub(crate) fn is_alias(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    false
}

pub(crate) fn is_regular(metadata: &Metadata) -> bool {
    metadata.file_type().is_file() && !is_alias(metadata)
}

pub(crate) fn metadata_observation_matches(left: &Metadata, right: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        left.dev() == right.dev() && left.ino() == right.ino()
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        left.file_attributes() == right.file_attributes()
            && left.creation_time() == right.creation_time()
            && left.last_write_time() == right.last_write_time()
            && left.file_size() == right.file_size()
    }
    #[cfg(not(any(unix, windows)))]
    {
        left.len() == right.len()
            && left.modified().ok() == right.modified().ok()
            && left.created().ok() == right.created().ok()
    }
}

pub(crate) fn identity_for_file(file: &File) -> io::Result<FileIdentity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = file.metadata()?;
        Ok(FileIdentity::Unix {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle as _;

        const FILE_ID_INFO_CLASS: i32 = 18;
        #[repr(C)]
        struct FileIdInfo {
            volume_serial_number: u64,
            file_id: [u8; 16],
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetFileInformationByHandleEx(
                h_file: isize,
                file_information_class: i32,
                lp_file_information: *mut core::ffi::c_void,
                dw_buffer_size: u32,
            ) -> i32;
        }

        let mut info = FileIdInfo {
            volume_serial_number: 0,
            file_id: [0; 16],
        };
        // SAFETY: `file` owns a live Windows handle and `info` is a correctly
        // sized writable FILE_ID_INFO buffer for FileIdInfo.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle() as isize,
                FILE_ID_INFO_CLASS,
                (&mut info as *mut FileIdInfo).cast(),
                core::mem::size_of::<FileIdInfo>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(FileIdentity::Windows {
            volume_serial_number: info.volume_serial_number,
            file_id: info.file_id,
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let metadata = file.metadata()?;
        Ok(FileIdentity::Portable {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            created: metadata.created().ok(),
        })
    }
}

pub(crate) fn identity_for_validated_path(
    path: &Path,
    metadata: &Metadata,
) -> io::Result<FileIdentity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let _ = path;
        Ok(FileIdentity::Unix {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(windows)]
    {
        let file = open_no_follow(path)?;
        let opened = file.metadata()?;
        if !is_regular(&opened) || !metadata_observation_matches(metadata, &opened) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "path changed while capturing exact Windows file identity",
            ));
        }
        identity_for_file(&file)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Ok(FileIdentity::Portable {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            created: metadata.created().ok(),
        })
    }
}

pub(crate) fn path_still_names_file(path: &Path, file: &File) -> io::Result<bool> {
    path_still_names_identity(path, identity_for_file(file)?)
}

fn path_still_names_identity(path: &Path, expected: FileIdentity) -> io::Result<bool> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !is_regular(&metadata) {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        Ok(identity_for_validated_path(path, &metadata)? == expected)
    }
    #[cfg(windows)]
    {
        let file = open_no_follow(path)?;
        let opened = file.metadata()?;
        if !is_regular(&opened) || !metadata_observation_matches(&metadata, &opened) {
            return Ok(false);
        }
        Ok(identity_for_file(&file)? == expected)
    }
    #[cfg(not(any(unix, windows)))]
    {
        Ok(identity_for_validated_path(path, &metadata)? == expected)
    }
}

#[cfg(windows)]
fn open_no_follow(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

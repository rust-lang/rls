use std::io;
use std::path::Path;
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct DirectoryListing {
    pub path: Vec<String>,
    pub files: Vec<Listing>,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct Listing {
    pub kind: ListingKind,
    pub name: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub enum ListingKind {
    Directory,
    // Time last modified.
    File(SystemTime),
}

impl DirectoryListing {
    pub fn from_path(path: &Path) -> io::Result<DirectoryListing> {
        let mut files = vec![];
        let dir = path.read_dir()?;

        for entry in dir {
            if let Ok(entry) = entry {
                let name = entry.file_name().to_str().unwrap().to_owned();
                if let Ok(file_type) = entry.file_type() {
                    if file_type.is_dir() {
                        files.push(Listing { kind: ListingKind::Directory, name });
                    } else if file_type.is_file() {
                        files.push(Listing {
                            kind: ListingKind::File(entry.metadata()?.modified()?),
                            name,
                        });
                    }
                }
            }
        }

        files.sort();

        Ok(DirectoryListing {
            path: path.components().map(|c| c.as_os_str().to_str().unwrap().to_owned()).collect(),
            files,
        })
    }
}

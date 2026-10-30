//! The 9000 series: five sister apps, which files each opens, and handing
//! a file to the sister that opens it. Each app keeps the files it handles
//! itself and passes the rest on: a video dropped on text9000 opens in
//! splice9000, a comic dropped on grace9000 opens in leaf9000.

use std::path::{Path, PathBuf};

pub struct Sister {
    pub name: &'static str,
    /// What it is, for messages ("the video editor").
    pub kind: &'static str,
    /// How macOS finds it.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub bundle_id: &'static str,
    /// Its own documents or projects.
    pub owns: &'static [&'static str],
    /// The other files it opens, as the sister best for them.
    pub opens: &'static [&'static str],
    pub repo: &'static str,
    /// Its signal color, as RGB.
    pub accent: [u8; 3],
}

impl Sister {
    pub fn handles(&self, path: &Path) -> bool {
        let ext = extension(path);
        self.owns.contains(&ext.as_str()) || self.opens.contains(&ext.as_str())
    }

    /// Where it's installed, if it is. `configured` (a path from the app's
    /// settings) wins.
    pub fn find(&self, configured: Option<&Path>) -> Option<PathBuf> {
        configured
            .filter(|p| p.exists())
            .map(Path::to_path_buf)
            .or_else(|| os::find(self))
    }

    pub fn installed(&self) -> bool {
        self.find(None).is_some()
    }

    /// Opens it, with `files` if any. Says why it couldn't.
    pub fn open(&self, files: &[&Path], configured: Option<&Path>) -> Result<(), String> {
        let Some(app) = self.find(configured) else {
            return Err(format!(
                "{} isn't installed. It's the {} of the 9000 series: {}/releases/latest",
                self.name, self.kind, self.repo
            ));
        };
        let files: Vec<PathBuf> = files
            .iter()
            .map(|f| std::path::absolute(f).unwrap_or_else(|_| f.to_path_buf()))
            .collect();
        os::launch(&app, &files).map_err(|e| format!("Couldn't open {}: {e}", self.name))
    }
}

pub const VIDEO: &[&str] = &[
    "mp4", "mov", "m4v", "mkv", "webm", "avi", "mpg", "mpeg", "mts", "m2ts", "wmv", "flv", "3gp",
    "mxf", "ts",
];
pub const SOUND: &[&str] = &[
    "wav", "mp3", "flac", "aiff", "aif", "m4a", "aac", "ogg", "oga", "opus", "wma", "caf",
];
pub const PHOTO: &[&str] = &[
    "jpg", "jpeg", "png", "tif", "tiff", "webp", "heic", "heif", "avif", "bmp", "gif", "jxl",
    "dng", "cr2", "cr3", "nef", "nrw", "arw", "srf", "sr2", "raf", "orf", "rw2", "pef", "srw",
    "3fr", "iiq", "x3f",
];
pub const BOOK: &[&str] = &["cbz", "cbr", "cb7", "cbt", "epub"];

pub const ALL: [Sister; 5] = [
    Sister {
        name: "text9000",
        kind: "writing app",
        bundle_id: "app.text9000.editor",
        owns: &["t9k", "folio"],
        // Its own documents only: a plain text file isn't one.
        opens: &[],
        repo: "https://github.com/solacecasino/text9000",
        accent: [0x3f, 0xd6, 0x8f],
    },
    Sister {
        name: "splice9000",
        kind: "video editor",
        bundle_id: "app.splice9000.editor",
        owns: &["splice"],
        opens: VIDEO,
        repo: "https://github.com/solacecasino/splice9000",
        accent: [0xff, 0x5c, 0x39],
    },
    Sister {
        name: "wave9000",
        kind: "audio workstation",
        bundle_id: "app.wave9000.workstation",
        owns: &["wave"],
        opens: SOUND,
        repo: "https://github.com/solacecasino/wave9000",
        accent: [0x4a, 0x9d, 0xff],
    },
    Sister {
        name: "grace9000",
        kind: "photo editor",
        bundle_id: "app.grace9000.editor",
        owns: &["grace"],
        opens: PHOTO,
        repo: "https://github.com/solacecasino/grace9000",
        accent: [0xa9, 0x8c, 0xff],
    },
    Sister {
        name: "leaf9000",
        kind: "comic reader",
        bundle_id: "app.leaf9000.reader",
        owns: &[],
        opens: BOOK,
        repo: "https://github.com/solacecasino/leaf9000",
        accent: [0xff, 0xc2, 0x3d],
    },
];

pub fn get(name: &str) -> Option<&'static Sister> {
    ALL.iter().find(|s| s.name.eq_ignore_ascii_case(name))
}

/// Every sister but this app.
pub fn others() -> impl Iterator<Item = &'static Sister> {
    let me = crate::app().name;
    ALL.iter().filter(move |s| s.name != me)
}

/// The sister a file belongs with, other than this app: the one owning it,
/// else the one opening it.
pub fn for_file(path: &Path) -> Option<&'static Sister> {
    let ext = extension(path);
    others()
        .find(|s| s.owns.contains(&ext.as_str()))
        .or_else(|| others().find(|s| s.opens.contains(&ext.as_str())))
}

/// Hands each file the app doesn't `keep` to its sister. Returns the files
/// kept (or that no sister takes), and what went wrong opening the rest.
pub fn hand_off(files: Vec<PathBuf>, keep: impl Fn(&Path) -> bool) -> (Vec<PathBuf>, Vec<String>) {
    let (mut kept, mut problems) = (Vec::new(), Vec::new());
    for f in files {
        match for_file(&f).filter(|_| !keep(&f)) {
            Some(s) => match s.open(&[&f], None) {
                Ok(()) => crate::log::info(format!("Opened {} in {}", f.display(), s.name)),
                Err(e) => problems.push(e),
            },
            None => kept.push(f),
        }
    }
    (kept, problems)
}

/// Whether `path`'s extension is one of `list`, in any case.
pub fn is(path: &Path, list: &[&str]) -> bool {
    list.contains(&extension(path).as_str())
}

fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

#[cfg(target_os = "macos")]
mod os {
    use std::path::{Path, PathBuf};

    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;

    use super::Sister;

    pub fn find(s: &Sister) -> Option<PathBuf> {
        // SAFETY: NSWorkspace's documented class and instance methods, with
        // live objects; nil comes back as a null pointer.
        unsafe {
            let class = AnyClass::get(c"NSWorkspace")?;
            let workspace: *mut AnyObject = msg_send![class, sharedWorkspace];
            let id = NSString::from_str(s.bundle_id);
            let url: *mut AnyObject =
                msg_send![workspace, URLForApplicationWithBundleIdentifier: &*id];
            if url.is_null() {
                return None;
            }
            let path: *mut NSString = msg_send![url, path];
            path.as_ref().map(|p| PathBuf::from(p.to_string()))
        }
    }

    pub fn launch(app: &Path, files: &[PathBuf]) -> std::io::Result<()> {
        let status = std::process::Command::new("/usr/bin/open")
            .arg("-a")
            .arg(app)
            .args(files)
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!("open exited with {status}")))
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod os {
    use std::path::{Path, PathBuf};

    use super::Sister;

    fn exe_name(s: &Sister) -> String {
        if cfg!(windows) {
            format!("{}.exe", s.name)
        } else {
            s.name.to_owned()
        }
    }

    pub fn find(s: &Sister) -> Option<PathBuf> {
        let exe = exe_name(s);
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(path) = std::env::var_os("PATH") {
            candidates.extend(std::env::split_paths(&path).map(|dir| dir.join(&exe)));
        }
        // The downloads are folders you unpack anywhere: look beside this
        // app's own folder (Downloads/splice9000-0.0.8-windows-x64/...).
        if let Some(parent) = std::env::current_exe()
            .ok()
            .and_then(|e| Some(e.parent()?.parent()?.to_path_buf()))
            && let Ok(entries) = std::fs::read_dir(&parent)
        {
            let mut beside: Vec<PathBuf> = entries
                .flatten()
                .filter(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .to_ascii_lowercase()
                        .starts_with(s.name)
                })
                .map(|e| e.path().join(&exe))
                .collect();
            // The newest version sorts last.
            beside.sort();
            candidates.extend(beside.into_iter().rev());
        }
        if cfg!(windows) {
            if let Some(local) = std::env::var_os("LOCALAPPDATA") {
                candidates.push(
                    PathBuf::from(local)
                        .join("Programs")
                        .join(s.name)
                        .join(&exe),
                );
            }
        } else {
            candidates.extend(desktop_entry(s));
        }
        candidates.into_iter().find(|p| p.is_file())
    }

    /// The program a sister's menu entry starts (its install.sh writes one
    /// with the full path).
    fn desktop_entry(s: &Sister) -> Vec<PathBuf> {
        let home = std::env::home_dir().unwrap_or_default();
        let data = std::env::var_os("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map_or_else(|| home.join(".local/share"), PathBuf::from);
        [
            data,
            PathBuf::from("/usr/local/share"),
            PathBuf::from("/usr/share"),
        ]
        .iter()
        .filter_map(|dir| {
            std::fs::read_to_string(dir.join("applications").join(format!("{}.desktop", s.name)))
                .ok()
        })
        .filter_map(|entry| {
            Some(PathBuf::from(super::exec_program(
                entry.lines().find_map(|l| l.strip_prefix("Exec="))?,
            )))
        })
        .collect()
    }

    pub fn launch(app: &Path, files: &[PathBuf]) -> std::io::Result<()> {
        let mut cmd = std::process::Command::new(app);
        cmd.args(files);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        cmd.spawn().map(drop)
    }
}

/// The program in a desktop entry's Exec= line: the first word, or the
/// quoted path. Desktop files escape twice (the file's strings, then the
/// quoted argument), so `\\$` in the file is `$`.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn exec_program(exec: &str) -> String {
    let unescaped = exec.trim().replace("\\\\", "\\");
    let exec = unescaped.as_str();
    if let Some(quoted) = exec.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = quoted.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => break,
                '\\' => out.extend(chars.next()),
                c => out.push(c),
            }
        }
        out
    } else {
        exec.split_whitespace()
            .next()
            .unwrap_or_default()
            .to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_go_to_their_sister() {
        let to = |p: &str| for_file(Path::new(p)).map(|s| s.name);
        assert_eq!(to("a.splice"), Some("splice9000"));
        assert_eq!(to("/x/Song.FLAC"), Some("wave9000"));
        assert_eq!(to("p.cr3"), Some("grace9000"));
        assert_eq!(to("vol 1.cbz"), Some("leaf9000"));
        assert_eq!(to("notes.t9k"), Some("text9000"));
        assert_eq!(to("data.xyz"), None);
        // Every extension belongs to one sister only.
        let mut seen = std::collections::HashSet::new();
        for s in &ALL {
            for e in s.owns.iter().chain(s.opens) {
                assert!(seen.insert(*e), "{e} is listed twice");
            }
        }
        assert_eq!(
            exec_program(r#""/opt/my apps/wave9000" %F"#),
            "/opt/my apps/wave9000"
        );
    }
}

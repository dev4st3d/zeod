//! Saved window geometry, multi-display visibility validation, and launch directory resolution.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub(crate) const MIN_WINDOW_WIDTH: f32 = 800.0;
pub(crate) const MIN_WINDOW_HEIGHT: f32 = 540.0;
pub(crate) const DEFAULT_WINDOW_WIDTH: f32 = 1440.0;
pub(crate) const DEFAULT_WINDOW_HEIGHT: f32 = 900.0;

const TITLEBAR_BAND_HEIGHT: f32 = 40.0;
const MIN_VISIBLE_TITLEBAR_WIDTH: f32 = 120.0;
const MIN_VISIBLE_TITLEBAR_HEIGHT: f32 = 24.0;
const WINDOW_STATE_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rect {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
}

impl Rect {
    pub(crate) fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    fn overlap_area(self, other: Self) -> (f32, f32) {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = (self.x + self.width).min(other.x + other.width);
        let y1 = (self.y + self.height).min(other.y + other.height);
        ((x1 - x0).max(0.0), (y1 - y0).max(0.0))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum WindowMode {
    #[default]
    Windowed,
    Maximized,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct SavedWindow {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
    #[serde(default)]
    pub(crate) mode: WindowMode,
}

impl SavedWindow {
    fn is_valid(self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
            && self.width >= MIN_WINDOW_WIDTH
            && self.height >= MIN_WINDOW_HEIGHT
    }

    fn rect(self) -> Rect {
        Rect::new(self.x, self.y, self.width, self.height)
    }
}

/// Compute the initial window geometry, falling back to centered defaults if no
/// saved state exists or if the saved titlebar would sit off every connected display.
pub(crate) fn resolve_window_bounds(
    saved: Option<SavedWindow>,
    displays: &[Rect],
    primary: Rect,
) -> (Rect, WindowMode) {
    if let Some(saved) = saved.filter(|saved| saved.is_valid()) {
        let titlebar = Rect::new(saved.x, saved.y, saved.width, TITLEBAR_BAND_HEIGHT);
        let visible = displays.iter().any(|display| {
            let (w, h) = titlebar.overlap_area(*display);
            w >= MIN_VISIBLE_TITLEBAR_WIDTH && h >= MIN_VISIBLE_TITLEBAR_HEIGHT
        });
        if visible {
            return (saved.rect(), saved.mode);
        }
    }
    let width = DEFAULT_WINDOW_WIDTH.min((primary.width - 48.0).max(640.0));
    let height = DEFAULT_WINDOW_HEIGHT.min((primary.height - 80.0).max(480.0));
    let x = primary.x + ((primary.width - width) / 2.0).max(0.0);
    let y = primary.y + ((primary.height - height) / 2.0).max(0.0);
    (Rect::new(x, y, width, height), WindowMode::Windowed)
}

/// Resolve an optional folder argument passed on the command line (e.g. `zeod .`).
/// Returns `(fallback_or_requested_dir, explicit_project_to_activate)`.
pub(crate) fn resolve_launch_directory<I>(args: I, cwd: &Path) -> (PathBuf, Option<PathBuf>)
where
    I: IntoIterator<Item = OsString>,
{
    for arg in args {
        let text = arg.to_string_lossy();
        if text.is_empty() || text.starts_with('-') {
            continue;
        }
        let path = PathBuf::from(&arg);
        let resolved = if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        };
        if let Ok(canonical) = fs::canonicalize(&resolved)
            && canonical.is_dir()
        {
            let clean = super::paths::without_windows_verbatim_prefix(&canonical);
            return (clean.clone(), Some(clean));
        }
        break;
    }
    (cwd.to_path_buf(), None)
}

#[derive(Deserialize, Serialize)]
struct StoredWindowState {
    version: u32,
    #[serde(flatten)]
    window: SavedWindow,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(crate) fn load(path: &Path) -> io::Result<Option<SavedWindow>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let stored: StoredWindowState = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if stored.version != WINDOW_STATE_VERSION {
        return Err(invalid("Unsupported window state version"));
    }
    if !stored.window.is_valid() {
        return Err(invalid("Window bounds are out of range"));
    }
    Ok(Some(stored.window))
}

pub(crate) fn save(path: &Path, window: SavedWindow) -> io::Result<()> {
    if !window.is_valid() {
        return Err(invalid("Window bounds are out of range"));
    }
    let stored = StoredWindowState {
        version: WINDOW_STATE_VERSION,
        window,
    };
    let mut bytes = serde_json::to_vec_pretty(&stored)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    bytes.push(b'\n');
    super::atomic_file::write(path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "zeod-window-state-{}-{}",
                std::process::id(),
                NEXT_TEST.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir_all(&path).unwrap();
            Self(fs::canonicalize(path).unwrap())
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn window_state_roundtrips_and_preserves_corrupt_files() {
        let root = TestDirectory::new();
        let path = root.0.join("window-state.json");
        assert_eq!(load(&path).unwrap(), None);

        let state = SavedWindow {
            x: 120.0,
            y: 80.0,
            width: 1280.0,
            height: 760.0,
            mode: WindowMode::Maximized,
        };
        save(&path, state).unwrap();
        assert_eq!(load(&path).unwrap(), Some(state));

        for bytes in [
            b"not json".as_slice(),
            br#"{"version":2,"x":0,"y":0,"width":1000,"height":700,"mode":"windowed"}"#.as_slice(),
            br#"{"version":1,"x":0,"y":0,"width":200,"height":200,"mode":"windowed"}"#.as_slice(),
        ] {
            fs::write(&path, bytes).unwrap();
            assert_eq!(load(&path).unwrap_err().kind(), io::ErrorKind::InvalidData);
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn offscreen_saved_bounds_fall_back_to_primary_display() {
        let primary = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        let secondary = Rect::new(1920.0, 0.0, 1920.0, 1080.0);
        let saved = SavedWindow {
            x: 2040.0,
            y: 100.0,
            width: 1200.0,
            height: 720.0,
            mode: WindowMode::Windowed,
        };
        // Both monitors attached -> restore exact saved bounds.
        assert_eq!(
            resolve_window_bounds(Some(saved), &[primary, secondary], primary),
            (saved.rect(), WindowMode::Windowed)
        );
        // Secondary unplugged -> fall back to centered on primary.
        let (fallback, mode) = resolve_window_bounds(Some(saved), &[primary], primary);
        assert_eq!(mode, WindowMode::Windowed);
        assert_eq!(fallback, Rect::new(240.0, 90.0, 1440.0, 900.0));
    }

    #[test]
    fn launch_directory_skips_flags_and_resolves_valid_folders() {
        let root = TestDirectory::new();
        let sub = root.0.join("project-a");
        fs::create_dir(&sub).unwrap();
        let clean_sub = crate::services::paths::without_windows_verbatim_prefix(&sub);

        let (initial, requested) = resolve_launch_directory(
            [OsString::from("--flag"), OsString::from("project-a")],
            &root.0,
        );
        assert_eq!(initial, clean_sub);
        assert_eq!(requested, Some(clean_sub));

        let (initial, requested) =
            resolve_launch_directory([OsString::from("nonexistent")], &root.0);
        assert_eq!(initial, root.0);
        assert_eq!(requested, None);
    }
}

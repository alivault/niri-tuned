//! Desktop-entry icons and app names for MRU previews. Filesystem lookup and
//! image decoding run on a worker; the compositor thread only uploads/caches
//! the resulting pixels and names. Each time the switcher opens the cache is
//! revalidated: the worker resolves every shown icon again (desktop entries
//! and icon themes may have changed) and only sends pixels back when the
//! resolved file did change, so the old icon stays up until then.
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::SystemTime;

use gio::glib::{KeyFile, KeyFileFlags};
use gio::prelude::*;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::utils::Transform;

use super::MruTexture;
use crate::render_helpers::texture::TextureBuffer;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    app_id: String,
    pixels: i32,
}

enum Request {
    /// An icon; `true` when the cache already shows one for this key, so an
    /// unchanged file needs no new pixels.
    Icon(Key, bool),
    Name(String),
    /// Forget the resolver (desktop entries, icon theme) before the next request.
    Refresh,
}

enum Reply {
    /// `None`: the icon resolves to the same file as before.
    Icon(Key, Option<Vec<u8>>),
    Name(String, String),
}

/// The file an icon came from, to tell whether it changed.
type Stamp = Option<(PathBuf, Option<SystemTime>, u64)>;

fn stamp(path: Option<PathBuf>) -> Stamp {
    path.map(|path| {
        let meta = std::fs::metadata(&path).ok();
        let modified = meta.as_ref().and_then(|m| m.modified().ok());
        let len = meta.map_or(0, |m| m.len());
        (path, modified, len)
    })
}

struct Entry {
    pending: bool,
    /// Resolve again at the next use (set when the switcher opens).
    stale: bool,
    pixels: Option<Vec<u8>>,
    texture: Option<MruTexture>,
}

pub struct IconCache {
    requests: Option<Sender<Request>>,
    replies: Option<Receiver<Reply>>,
    entries: HashMap<Key, Entry>,
    /// App names by app id; `None` while the worker looks one up.
    names: HashMap<String, Option<String>>,
    /// Names to look up again at their next use, keeping the old one meanwhile.
    stale_names: HashSet<String>,
}

impl IconCache {
    pub fn new() -> Self {
        Self {
            requests: None,
            replies: None,
            entries: HashMap::new(),
            names: HashMap::new(),
            stale_names: HashSet::new(),
        }
    }

    pub fn invalidate(&mut self) {
        self.requests = None;
        self.replies = None;
        self.entries.clear();
        self.names.clear();
        self.stale_names.clear();
    }

    /// Revalidate everything at its next use, showing the cached icons and
    /// names until the worker answers.
    pub fn refresh(&mut self) {
        let Some(requests) = &self.requests else {
            return;
        };
        if requests.send(Request::Refresh).is_err() {
            return;
        }
        for entry in self.entries.values_mut() {
            entry.stale = true;
        }
        self.stale_names = self
            .names
            .iter()
            .filter(|(_, name)| name.is_some())
            .map(|(id, _)| id.clone())
            .collect();
    }

    fn start(&mut self) {
        if self.requests.is_some() {
            return;
        }
        let (requests, rx) = mpsc::channel::<Request>();
        let (tx, replies) = mpsc::channel();
        if let Err(err) = std::thread::Builder::new()
            .name("niri-app-icons".into())
            .spawn(move || {
                // GIO objects never cross threads or connect to the compositor's display.
                let mut resolver = None;
                let mut stamps: HashMap<Key, Stamp> = HashMap::new();
                while let Ok(request) = rx.recv() {
                    if let Request::Refresh = request {
                        resolver = None;
                        continue;
                    }
                    let resolver = resolver.get_or_insert_with(Resolver::new);
                    let reply = match request {
                        Request::Refresh => unreachable!(),
                        Request::Icon(key, shown) => {
                            let stamp = stamp(resolver.resolve(&key.app_id, key.pixels));
                            if shown && stamps.get(&key) == Some(&stamp) {
                                Reply::Icon(key, None)
                            } else {
                                let pixels = stamp
                                    .as_ref()
                                    .and_then(|(path, ..)| decode(path, key.pixels))
                                    .unwrap_or_else(|| fallback(key.pixels));
                                if stamps.len() >= 512 {
                                    stamps.clear();
                                }
                                stamps.insert(key.clone(), stamp);
                                Reply::Icon(key, Some(pixels))
                            }
                        }
                        Request::Name(app_id) => {
                            let name = resolver
                                .app_name(&app_id)
                                .unwrap_or_else(|| fallback_name(&app_id));
                            Reply::Name(app_id, name)
                        }
                    };
                    if tx.send(reply).is_err() {
                        break;
                    }
                }
            })
        {
            warn!("could not start app-icon loader: {err:?}");
        }
        self.requests = Some(requests);
        self.replies = Some(replies);
    }

    pub fn pending(&self) -> bool {
        // Animation/redraw demand is checked BEFORE rendering starts requests.
        // Keep polling across the first frame after invalidation, even if every
        // client is static; otherwise freshly requested icons never get painted.
        self.requests.is_none()
            || self.entries.values().any(|entry| entry.pending)
            || self.names.values().any(Option::is_none)
    }

    fn poll(&mut self) {
        self.start();
        for reply in self.replies.as_ref().unwrap().try_iter() {
            match reply {
                Reply::Icon(key, pixels) => {
                    if let Some(entry) = self.entries.get_mut(&key) {
                        entry.pending = false;
                        if let Some(pixels) = pixels {
                            entry.pixels = Some(pixels);
                            entry.texture = None;
                        }
                    }
                }
                Reply::Name(app_id, name) => {
                    if let Some(entry) = self.names.get_mut(&app_id) {
                        *entry = Some(name);
                    }
                }
            }
        }
    }

    /// The app's display name: its desktop entry's (localized) Name, else one
    /// made from the app id. `None` while it is being looked up, or without an
    /// app id.
    pub fn name(&mut self, app_id: Option<&str>) -> Option<String> {
        let app_id = app_id.filter(|id| !id.is_empty())?;
        let app_id: String = app_id.chars().take(512).collect();
        self.poll();
        if self.stale_names.remove(&app_id) {
            // Keep showing the old name until the new one arrives.
            let _ = self
                .requests
                .as_ref()
                .unwrap()
                .send(Request::Name(app_id.clone()));
        } else if !self.names.contains_key(&app_id) {
            // Bound memory like the icons; pending lookups stay.
            if self.names.len() >= 256 {
                self.names.retain(|_, name| name.is_none());
            }
            let sent = self
                .requests
                .as_ref()
                .unwrap()
                .send(Request::Name(app_id.clone()))
                .is_ok();
            let name = (!sent).then(|| fallback_name(&app_id));
            self.names.insert(app_id.clone(), name);
        }
        self.names.get(&app_id).cloned().flatten()
    }

    pub fn get(
        &mut self,
        renderer: &mut GlesRenderer,
        app_id: Option<&str>,
        size: f64,
        scale: f64,
    ) -> Option<MruTexture> {
        self.poll();
        let key = Key {
            app_id: app_id.unwrap_or_default().chars().take(512).collect(),
            pixels: (size * scale).round().clamp(16., 1024.) as i32,
        };
        // Bound retained GPU/CPU memory; pending replies for evicted keys are ignored.
        if self.entries.len() >= 128 && !self.entries.contains_key(&key) {
            self.entries.retain(|_, entry| entry.pending);
            if self.entries.len() >= 128 {
                return None;
            }
        }
        let entry = self.entries.entry(key.clone()).or_insert_with(|| {
            let pending = self
                .requests
                .as_ref()
                .unwrap()
                .send(Request::Icon(key.clone(), false))
                .is_ok();
            Entry {
                pending,
                stale: false,
                pixels: (!pending).then(|| fallback(key.pixels)),
                texture: None,
            }
        });
        if entry.stale && !entry.pending {
            entry.stale = false;
            entry.pending = self
                .requests
                .as_ref()
                .unwrap()
                .send(Request::Icon(key.clone(), true))
                .is_ok();
        }
        if entry.texture.is_none() {
            if let Some(pixels) = entry.pixels.take() {
                entry.texture = TextureBuffer::from_memory(
                    renderer,
                    &pixels,
                    Fourcc::Abgr8888,
                    (key.pixels, key.pixels),
                    false,
                    1.,
                    Transform::Normal,
                    Vec::new(),
                )
                .ok();
            }
        }
        // The same pixels serve every logical size that rounds to them.
        let mut texture = entry.texture.clone()?;
        texture.set_texture_scale(f64::from(key.pixels) / size.max(1.));
        Some(texture)
    }
}

#[derive(Debug)]
struct IconDir {
    path: PathBuf,
    min: i32,
    max: i32,
}

#[derive(Default)]
struct Theme {
    dirs: Vec<IconDir>,
    inherits: Vec<String>,
}

struct Resolver {
    roots: Vec<PathBuf>,
    pixmaps: Vec<PathBuf>,
    theme: String,
    themes: HashMap<String, Theme>,
    apps: Vec<gio::DesktopAppInfo>,
}

fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

impl Resolver {
    fn new() -> Self {
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
        let mut data = data_home.into_iter().collect::<Vec<_>>();
        let dirs = std::env::var_os("XDG_DATA_DIRS")
            .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        data.extend(std::env::split_paths(&dirs).filter(|p| p.is_absolute()));
        let mut roots = data.iter().map(|p| p.join("icons")).collect::<Vec<_>>();
        if let Some(home) = std::env::var_os("HOME") {
            roots.insert(1.min(roots.len()), PathBuf::from(home).join(".icons"));
        }
        let theme = gio::SettingsSchemaSource::default()
            .and_then(|source| source.lookup("org.gnome.desktop.interface", true))
            .map(|schema| {
                gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None)
                    .string("icon-theme")
                    .to_string()
            })
            .unwrap_or_else(|| "Adwaita".into());
        let apps = gio::AppInfo::all()
            .into_iter()
            .filter_map(|app| app.downcast().ok())
            .collect();
        Self {
            roots,
            pixmaps: data.iter().map(|p| p.join("pixmaps")).collect(),
            theme,
            themes: HashMap::new(),
            apps,
        }
    }

    /// The desktop entry of an app id: `<id>.desktop`, else one whose id or
    /// StartupWMClass matches it in any case.
    fn app_info(&self, app_id: &str) -> Option<gio::DesktopAppInfo> {
        let id = app_id.strip_suffix(".desktop").unwrap_or(app_id);
        if safe_relative(id) && !id.contains('/') {
            gio::DesktopAppInfo::new(&format!("{id}.desktop")).or_else(|| {
                self.apps
                    .iter()
                    .find(|app| {
                        app.id().is_some_and(|name| {
                            name.trim_end_matches(".desktop").eq_ignore_ascii_case(id)
                        }) || app
                            .startup_wm_class()
                            .is_some_and(|name| name.eq_ignore_ascii_case(id))
                    })
                    .cloned()
            })
        } else {
            None
        }
    }

    fn app_name(&self, app_id: &str) -> Option<String> {
        self.app_info(app_id)
            .map(|app| app.name().trim().to_string())
            .filter(|name| !name.is_empty())
    }

    fn resolve(&mut self, app_id: &str, pixels: i32) -> Option<PathBuf> {
        let id = app_id.strip_suffix(".desktop").unwrap_or(app_id);
        let info = self.app_info(app_id);
        let name = info
            .and_then(|app| app.string("Icon"))
            .map(|s| s.to_string());
        for name in name
            .as_deref()
            .into_iter()
            .chain([id, "application-x-executable"])
        {
            if Path::new(name).is_absolute() {
                if Path::new(name).is_file() {
                    return Some(PathBuf::from(name));
                }
                continue;
            }
            if !safe_relative(name) || name.contains('/') {
                continue;
            }
            let mut seen = HashSet::new();
            let theme = self.theme.clone();
            if let Some(path) = self
                .find_in_theme(&theme, name, pixels, &mut seen)
                .or_else(|| self.find_in_theme("hicolor", name, pixels, &mut seen))
            {
                return Some(path);
            }
            for root in self.roots.iter().chain(&self.pixmaps) {
                if let Some(path) = icon_file(root, name) {
                    return Some(path);
                }
            }
        }
        None
    }

    fn find_in_theme(
        &mut self,
        theme: &str,
        name: &str,
        pixels: i32,
        seen: &mut HashSet<String>,
    ) -> Option<PathBuf> {
        if !safe_relative(theme)
            || theme.contains('/')
            || seen.len() >= 16
            || !seen.insert(theme.into())
        {
            return None;
        }
        let info = self
            .themes
            .entry(theme.into())
            .or_insert_with(|| load_theme(&self.roots, theme));
        let mut dirs = info.dirs.iter().collect::<Vec<_>>();
        dirs.sort_by_key(|dir| {
            if pixels < dir.min {
                dir.min - pixels
            } else {
                (pixels - dir.max).max(0)
            }
        });
        for dir in dirs {
            if let Some(path) = icon_file(&dir.path, name) {
                return Some(path);
            }
        }
        let parents = info.inherits.clone();
        for parent in parents {
            if let Some(path) = self.find_in_theme(&parent, name, pixels, seen) {
                return Some(path);
            }
        }
        None
    }
}

/// A name for an app without a desktop entry: the last part of a reverse-DNS
/// app id ("org.gnome.Nautilus" -> "Nautilus"), first letter capitalised.
fn fallback_name(app_id: &str) -> String {
    let id = app_id.strip_suffix(".desktop").unwrap_or(app_id);
    let last = id.rsplit('.').find(|part| !part.is_empty()).unwrap_or(id);
    let mut chars = last.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn icon_file(dir: &Path, name: &str) -> Option<PathBuf> {
    if name.ends_with(".png") || name.ends_with(".svg") || name.ends_with(".xpm") {
        let path = dir.join(name);
        if path.is_file() {
            return Some(path);
        }
    }
    ["png", "svg", "xpm"]
        .into_iter()
        .map(|ext| dir.join(format!("{name}.{ext}")))
        .find(|p| p.is_file())
}

fn load_theme(roots: &[PathBuf], theme: &str) -> Theme {
    let mut result = Theme::default();
    // One index describes the theme across all XDG roots (including user overrides).
    let index = roots.iter().find_map(|root| {
        let key = KeyFile::new();
        key.load_from_file(root.join(theme).join("index.theme"), KeyFileFlags::NONE)
            .ok()?;
        Some(key)
    });
    let Some(index) = index else {
        return result;
    };
    if let Ok(inherits) = index.string("Icon Theme", "Inherits") {
        result.inherits = inherits
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
    }
    for key in ["Directories", "ScaledDirectories"] {
        let Ok(dirs) = index.string("Icon Theme", key) else {
            continue;
        };
        for dir in dirs.split(',').map(str::trim).filter(|s| safe_relative(s)) {
            let size = index.integer(dir, "Size").unwrap_or(48).clamp(1, 4096);
            let scale = index.integer(dir, "Scale").unwrap_or(1).clamp(1, 16);
            let kind = index
                .string(dir, "Type")
                .unwrap_or_else(|_| "Threshold".into());
            let (min, max) = match kind.as_str() {
                "Scalable" => (
                    index.integer(dir, "MinSize").unwrap_or(size),
                    index.integer(dir, "MaxSize").unwrap_or(size),
                ),
                "Fixed" => (size, size),
                _ => {
                    let threshold = index.integer(dir, "Threshold").unwrap_or(2).clamp(0, 4096);
                    (size - threshold, size + threshold)
                }
            };
            for root in roots {
                result.dirs.push(IconDir {
                    path: root.join(theme).join(dir),
                    min: min.clamp(0, 8192) * scale,
                    max: max.clamp(0, 8192) * scale,
                });
            }
        }
    }
    result
}

fn decode(path: &Path, size: i32) -> Option<Vec<u8>> {
    let image = gdk_pixbuf::Pixbuf::from_file_at_scale(path, size, size, true).ok()?;
    let (w, h, stride, channels) = (
        image.width() as usize,
        image.height() as usize,
        image.rowstride() as usize,
        image.n_channels() as usize,
    );
    let size = size as usize;
    if w > size || h > size || ![3, 4].contains(&channels) || image.bits_per_sample() != 8 {
        return None;
    }
    let bytes = image.read_pixel_bytes();
    let bytes = bytes.as_ref();
    let mut output = vec![0; size * size * 4];
    for y in 0..h {
        for x in 0..w {
            let src = y * stride + x * channels;
            let dst = ((y + (size - h) / 2) * size + x + (size - w) / 2) * 4;
            let alpha = if channels == 4 { bytes[src + 3] } else { 255 };
            for c in 0..3 {
                output[dst + c] =
                    ((u16::from(bytes[src + c]) * u16::from(alpha) + 127) / 255) as u8;
            }
            output[dst + 3] = alpha;
        }
    }
    Some(output)
}

fn fallback(size: i32) -> Vec<u8> {
    // A neutral application-window glyph when no desktop-entry icon is available.
    let mut pixels = vec![0; (size * size * 4) as usize];
    for y in size / 8..size * 7 / 8 {
        for x in size / 8..size * 7 / 8 {
            let i = ((y * size + x) * 4) as usize;
            let color = if y < size / 3 {
                [70, 100, 145, 255]
            } else {
                [220, 225, 232, 255]
            };
            pixels[i..i + 4].copy_from_slice(&color);
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalidation_requests_a_frame_before_the_loader_is_started() {
        let mut cache = IconCache::new();
        assert!(cache.pending());
        let (sender, _receiver) = mpsc::channel();
        cache.requests = Some(sender);
        assert!(!cache.pending());
        cache.invalidate();
        assert!(cache.pending());
    }

    #[test]
    fn rejects_traversal_but_accepts_theme_subdirectories() {
        assert!(safe_relative("48x48@2x/apps"));
        assert!(!safe_relative("../outside"));
        assert!(!safe_relative("/absolute"));
        assert!(!safe_relative(""));
    }

    #[test]
    fn fallback_name_uses_the_last_app_id_part() {
        assert_eq!(fallback_name("org.gnome.Nautilus"), "Nautilus");
        assert_eq!(fallback_name("firefox"), "Firefox");
        assert_eq!(fallback_name("com.mitchellh.ghostty.desktop"), "Ghostty");
        assert_eq!(fallback_name("trailing."), "Trailing");
        assert_eq!(fallback_name(""), "");
    }

    #[test]
    fn fallback_is_bounded_transparent_rgba() {
        let pixels = fallback(48);
        assert_eq!(pixels.len(), 48 * 48 * 4);
        assert_eq!(&pixels[..4], &[0, 0, 0, 0]);
        assert_eq!(pixels[(24 * 48 + 24) * 4 + 3], 255);
    }

    #[test]
    fn stamp_changes_with_the_file_or_its_path() {
        let shell = xshell::Shell::new().unwrap();
        let tmp = shell.create_temp_dir().unwrap();
        let path = tmp.path().join("icon.png");
        std::fs::write(&path, "a").unwrap();
        let first = stamp(Some(path.clone()));
        assert_eq!(first, stamp(Some(path.clone())));
        std::fs::write(&path, "ab").unwrap();
        assert_ne!(first, stamp(Some(path.clone())));
        assert_ne!(first, stamp(Some(tmp.path().join("other.png"))));
        assert_eq!(stamp(None), None);
    }

    #[test]
    fn resolves_inherited_theme_and_rgba_svg_without_a_display() {
        let shell = xshell::Shell::new().unwrap();
        let tmp = shell.create_temp_dir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("child")).unwrap();
        std::fs::create_dir_all(root.join("parent/scalable/apps")).unwrap();
        std::fs::write(
            root.join("child/index.theme"),
            "[Icon Theme]\nInherits=parent\nDirectories=\n",
        )
        .unwrap();
        std::fs::write(root.join("parent/index.theme"), "[Icon Theme]\nInherits=child\nDirectories=scalable/apps\n[scalable/apps]\nSize=48\nType=Scalable\nMinSize=16\nMaxSize=256\n").unwrap();
        let path = root.join("parent/scalable/apps/test.svg");
        std::fs::write(&path, r#"<svg xmlns="http://www.w3.org/2000/svg" width="48" height="48"><rect width="48" height="48" fill="red" fill-opacity="0.5"/></svg>"#).unwrap();
        let mut resolver = Resolver {
            roots: vec![root.into()],
            pixmaps: vec![],
            theme: "child".into(),
            themes: HashMap::new(),
            apps: vec![],
        };
        assert_eq!(resolver.resolve("test", 96), Some(path.clone()));
        assert!(resolver.resolve("missing", 96).is_none());
        let bytes = decode(&path, 96).unwrap();
        assert_eq!(bytes.len(), 96 * 96 * 4);
        let center = (48 * 96 + 48) * 4;
        assert_eq!(&bytes[center..center + 4], &[128, 0, 0, 128]);
    }
}

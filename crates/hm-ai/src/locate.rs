//! Where everything lives on this machine: environment overrides, each tool's settings, and the
//! gate that keeps a scan from raising a permission prompt.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use hm_core::{fsutil, ScanContext};
use serde::Deserialize;

use crate::files::{link_target, read_capped, sqlite_query};

const SETTINGS_CAP: u64 = 4 * 1024 * 1024;

/// Folders macOS guards with a prompt. The first five are rule 6. Removable and network volumes
/// prompt just the same, and a model folder on an external SSD is common among exactly the people
/// this crate is for.
const PROMPTING_HOME_FOLDERS: &[&str] = &[
    "Documents",
    "Desktop",
    "Downloads",
    "Library/Containers",
    "Library/Group Containers",
    "Library/Mobile Documents",
    "Library/CloudStorage",
];

fn prompts(home: &Path, path: &Path) -> bool {
    path.starts_with("/Volumes")
        || PROMPTING_HOME_FOLDERS
            .iter()
            .any(|folder| path.starts_with(home.join(folder)))
}

/// Whether environment overrides apply. They describe the real user's machine, so they must not
/// leak into a fixture home: a developer with `OLLAMA_MODELS` exported would otherwise have tests
/// reading their real models. This compares against `$HOME` only to make that decision. No path is
/// ever derived from it (rule 5).
pub(crate) fn is_real_home(ctx: &ScanContext) -> bool {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return false;
    };
    home == ctx.home
        || match (home.canonicalize(), ctx.home.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
}

/// How much work resolving may do. The UI polls [`Detail::Light`], which never spawns a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Detail {
    Light,
    Full,
}

#[derive(Debug, Default)]
pub(crate) struct OmlxPaths {
    pub base: PathBuf,
    pub model_dirs: Vec<PathBuf>,
    pub ssd_cache: PathBuf,
    pub port: u16,
}

#[derive(Debug, Default)]
pub(crate) struct Roots {
    pub home: PathBuf,
    pub full_disk_access: bool,
    /// `~/.cache`, or wherever `XDG_CACHE_HOME` puts it.
    pub cache: PathBuf,
    /// All that could hold models: the app's own setting, `OLLAMA_MODELS` and the default.
    pub ollama: Vec<PathBuf>,
    pub ollama_logs: PathBuf,
    pub ollama_host: Option<String>,
    pub hub: PathBuf,
    pub datasets_cache: PathBuf,
    pub xet_cache: PathBuf,
    pub lm_home: PathBuf,
    pub lm_models: Vec<PathBuf>,
    pub omlx: OmlxPaths,
    pub llama_cache: PathBuf,
    pub torch_home: PathBuf,
    pub sentence_transformers: PathBuf,
    pub whisper: Vec<PathBuf>,
    pub gpt4all: PathBuf,
    pub jan: Vec<PathBuf>,
    pub comfy: Vec<PathBuf>,
    pub a1111: Vec<PathBuf>,
    pub draw_things: Option<PathBuf>,
    skipped: Mutex<BTreeSet<PathBuf>>,
}

impl Roots {
    pub fn resolve(ctx: &ScanContext, detail: Detail) -> Self {
        let real = is_real_home(ctx);
        let env = |name: &str| -> Option<String> {
            if !real {
                return None;
            }
            std::env::var(name).ok().filter(|value| !value.is_empty())
        };
        let full_disk_access = detail == Detail::Full && fsutil::has_full_disk_access(&ctx.home);
        Self::resolve_with(&ctx.home, full_disk_access, detail, &env)
    }

    pub fn resolve_with(
        home: &Path,
        full_disk_access: bool,
        detail: Detail,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Self {
        let path_env = |name: &str| env(name).map(|value| expand(&value, home));
        let cache = path_env("XDG_CACHE_HOME").unwrap_or_else(|| home.join(".cache"));

        let mut ollama = Vec::new();
        if detail == Detail::Full {
            // The desktop app keeps its "Model location" here and hands it to its server over any
            // OLLAMA_MODELS, so this comes first.
            let database = home.join("Library/Application Support/Ollama/db.sqlite");
            if database.is_file() {
                if let Some(models) =
                    sqlite_query(&database, "SELECT models FROM settings LIMIT 1;")
                {
                    let models = models.trim();
                    if models.starts_with('/') || models.starts_with('~') {
                        ollama.push(expand(models, home));
                    }
                }
            }
        }
        ollama.extend(path_env("OLLAMA_MODELS"));
        ollama.push(home.join(".ollama/models"));

        let hf_home = path_env("HF_HOME").unwrap_or_else(|| cache.join("huggingface"));
        let hub = path_env("HF_HUB_CACHE")
            .or_else(|| path_env("HUGGINGFACE_HUB_CACHE"))
            .unwrap_or_else(|| hf_home.join("hub"));

        let lm_home = read_capped(&home.join(".lmstudio-home-pointer"), 4096)
            .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_string())
            .filter(|text| text.starts_with('/'))
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".lmstudio"));
        // A tool's home can itself be somewhere that prompts, and its settings live inside. Those
        // aren't read: the defaults stand in, and the store gets gated like any other.
        let readable = |path: &Path| full_disk_access || !prompts(home, path);
        let mut lm_models = Vec::new();
        if readable(&lm_home) {
            lm_models.extend(lm_downloads_folder(&lm_home, home));
        }
        lm_models.push(lm_home.join("models"));
        lm_models.push(cache.join("lm-studio/models"));

        let omlx = resolve_omlx(home, env, path_env("OMLX_BASE_PATH"), &readable);

        let torch_home = path_env("TORCH_HOME").unwrap_or_else(|| cache.join("torch"));
        let sentence_transformers = path_env("SENTENCE_TRANSFORMERS_HOME")
            .unwrap_or_else(|| torch_home.join("sentence_transformers"));

        let app_support = home.join("Library/Application Support");
        let mut jan = Vec::new();
        jan.extend(
            json_string(&app_support.join("Jan/settings.json"), "data_folder")
                .map(|p| expand(&p, home)),
        );
        jan.push(app_support.join("Jan/data"));
        jan.push(home.join("jan"));

        let mut comfy = Vec::new();
        comfy.extend(
            json_string(&app_support.join("ComfyUI/config.json"), "basePath")
                .map(|p| expand(&p, home)),
        );
        comfy.push(home.join("ComfyUI"));
        comfy.push(home.join("comfy/ComfyUI"));
        comfy.push(home.join("Documents/ComfyUI"));

        let mut a1111 = vec![
            home.join("stable-diffusion-webui"),
            home.join("stable-diffusion-webui-forge"),
            home.join("Documents/stable-diffusion-webui"),
        ];
        // People clone these next to their other projects, such as ~/Projects/ComfyUI.
        for dir in hm_core::config::common_project_dirs(home) {
            comfy.push(dir.join("ComfyUI"));
            a1111.push(dir.join("stable-diffusion-webui"));
            a1111.push(dir.join("stable-diffusion-webui-forge"));
        }

        let mut roots = Self {
            home: home.to_path_buf(),
            full_disk_access,
            cache: cache.clone(),
            ollama,
            ollama_logs: home.join(".ollama/logs"),
            ollama_host: env("OLLAMA_HOST"),
            hub,
            datasets_cache: path_env("HF_DATASETS_CACHE")
                .unwrap_or_else(|| hf_home.join("datasets")),
            xet_cache: path_env("HF_XET_CACHE").unwrap_or_else(|| hf_home.join("xet")),
            lm_home,
            lm_models,
            omlx,
            llama_cache: path_env("LLAMA_CACHE")
                .unwrap_or_else(|| home.join("Library/Caches/llama.cpp")),
            torch_home,
            sentence_transformers,
            whisper: vec![
                cache.join("whisper"),
                cache.join("whisper.cpp"),
                home.join("whisper.cpp/models"),
            ],
            gpt4all: gpt4all_dir(home),
            jan,
            comfy,
            a1111,
            draw_things: Some(
                home.join("Library/Containers/com.liuliu.draw-things/Data/Documents/Models"),
            ),
            skipped: Mutex::new(BTreeSet::new()),
        };
        roots.ollama = dedupe(std::mem::take(&mut roots.ollama));
        roots.lm_models = dedupe(std::mem::take(&mut roots.lm_models));
        roots.omlx.model_dirs = dedupe(std::mem::take(&mut roots.omlx.model_dirs));
        roots.jan = dedupe(std::mem::take(&mut roots.jan));
        roots.comfy = dedupe(std::mem::take(&mut roots.comfy));
        roots
    }

    /// Whether touching `path` is allowed without risking a prompt. A refused path the user chose
    /// is remembered, so the report can say what was left out. Uses only the text of the path.
    pub fn allowed(&self, path: &Path) -> bool {
        if self.allowed_quietly(path) {
            return true;
        }
        if let Ok(mut skipped) = self.skipped.lock() {
            skipped.insert(path.to_path_buf());
        }
        false
    }

    /// Like [`Roots::allowed`], for places that are merely usual locations. If it's refused nothing
    /// says the user even has this folder, so it isn't worth reporting.
    pub fn allowed_quietly(&self, path: &Path) -> bool {
        self.full_disk_access || !prompts(&self.home, path)
    }

    /// Follows the root itself if it's a symlink, since moving a model folder to another disk and
    /// leaving a link behind is common. Each hop is checked before it's touched. `None` if the
    /// chain leads somewhere that would prompt, or goes on too long.
    pub fn follow_root(&self, path: &Path) -> Option<PathBuf> {
        if !self.allowed(path) {
            return None;
        }
        let mut current = path.to_path_buf();
        for _ in 0..8 {
            let meta = std::fs::symlink_metadata(&current).ok()?;
            if !meta.file_type().is_symlink() {
                return meta.is_dir().then_some(current);
            }
            current = link_target(&current)?;
            if !self.allowed(&current) {
                return None;
            }
        }
        None
    }

    /// Roots left out to avoid a prompt.
    pub fn skipped(&self) -> Vec<PathBuf> {
        self.skipped
            .lock()
            .map(|skipped| skipped.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn note_skipped(&self, path: &Path) {
        if let Ok(mut skipped) = self.skipped.lock() {
            skipped.insert(path.to_path_buf());
        }
    }
}

fn dedupe(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    paths
        .into_iter()
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

/// Expands a leading `~`, as these tools do for their settings and environment.
pub(crate) fn expand(value: &str, home: &Path) -> PathBuf {
    if value == "~" {
        home.to_path_buf()
    } else if let Some(rest) = value.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(value)
    }
}

fn json_string(path: &Path, key: &str) -> Option<String> {
    let bytes = read_capped(path, SETTINGS_CAP)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get(key)?
        .as_str()
        .map(str::to_string)
        .filter(|text| !text.is_empty())
}

fn lm_downloads_folder(lm_home: &Path, home: &Path) -> Option<PathBuf> {
    json_string(&lm_home.join("settings.json"), "downloadsFolder")
        .filter(|text| text.starts_with('/') || text.starts_with('~'))
        .map(|text| expand(&text, home))
}

fn gpt4all_dir(home: &Path) -> PathBuf {
    let configured = read_capped(&home.join(".config/gpt4all.io/GPT4All.ini"), 1024 * 1024)
        .and_then(|bytes| {
            String::from_utf8_lossy(&bytes).lines().find_map(|line| {
                line.strip_prefix("modelPath=")
                    .map(|value| value.trim().trim_matches('"').to_string())
                    .filter(|value| value.starts_with('/'))
            })
        })
        .map(PathBuf::from);
    configured.unwrap_or_else(|| home.join("Library/Application Support/nomic.ai/GPT4All"))
}

/// Only the fields needed. `settings.json` also holds the user's API keys, and typing this means
/// they are never even parsed into memory, let alone used.
#[derive(Default, Deserialize)]
#[serde(default)]
struct OmlxSettings {
    server: OmlxServer,
    model: OmlxModel,
    cache: OmlxCache,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct OmlxServer {
    port: Option<u16>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct OmlxModel {
    model_dirs: Vec<String>,
    model_dir: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct OmlxCache {
    ssd_cache_dir: Option<String>,
}

/// Mirrors `omlx/settings.py`: `OMLX_BASE_PATH`, then the app's bootstrap file, then `~/.omlx`.
fn resolve_omlx(
    home: &Path,
    env: &dyn Fn(&str) -> Option<String>,
    base_from_env: Option<PathBuf>,
    readable: &dyn Fn(&Path) -> bool,
) -> OmlxPaths {
    let base = base_from_env
        .or_else(|| {
            read_capped(
                &home.join("Library/Application Support/oMLX/base-path"),
                4096,
            )
            .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_string())
            .filter(|text| text.starts_with('/') || text.starts_with('~'))
            .map(|text| expand(&text, home))
        })
        .unwrap_or_else(|| home.join(".omlx"));

    let settings: OmlxSettings = Some(base.join("settings.json"))
        .filter(|path| readable(path))
        .and_then(|path| read_capped(&path, SETTINGS_CAP))
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();

    let mut model_dirs: Vec<PathBuf> = settings
        .model
        .model_dirs
        .iter()
        .map(|dir| expand(dir, home))
        .collect();
    if model_dirs.is_empty() {
        model_dirs.extend(
            settings
                .model
                .model_dir
                .as_deref()
                .map(|dir| expand(dir, home)),
        );
    }
    if model_dirs.is_empty() {
        model_dirs.push(base.join("models"));
    }

    let ssd_cache = env("OMLX_SSD_CACHE_DIR")
        .or(settings.cache.ssd_cache_dir)
        .map(|dir| expand(&dir, home))
        .unwrap_or_else(|| base.join("cache"));

    let port = env("OMLX_PORT")
        .and_then(|port| port.parse().ok())
        .or(settings.server.port)
        .unwrap_or(8000);

    OmlxPaths {
        base,
        model_dirs,
        ssd_cache,
        port,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;

    fn with_env(home: &Path, vars: &[(&str, &str)]) -> Roots {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        Roots::resolve_with(home, false, Detail::Light, &|name| vars.get(name).cloned())
    }

    #[test]
    fn defaults_come_from_the_home() {
        let roots = with_env(Path::new("/h"), &[]);
        assert_eq!(roots.ollama, vec![PathBuf::from("/h/.ollama/models")]);
        assert_eq!(roots.hub, Path::new("/h/.cache/huggingface/hub"));
        assert_eq!(roots.lm_models[0], Path::new("/h/.lmstudio/models"));
        assert!(roots
            .lm_models
            .contains(&PathBuf::from("/h/.cache/lm-studio/models")));
        assert_eq!(
            roots.omlx.model_dirs,
            vec![PathBuf::from("/h/.omlx/models")]
        );
        assert_eq!(roots.omlx.port, 8000);
    }

    #[test]
    fn hugging_face_overrides_follow_its_precedence() {
        let roots = with_env(Path::new("/h"), &[("HF_HOME", "/data/hf")]);
        assert_eq!(roots.hub, Path::new("/data/hf/hub"));
        assert_eq!(roots.datasets_cache, Path::new("/data/hf/datasets"));

        let roots = with_env(
            Path::new("/h"),
            &[
                ("HF_HOME", "/data/hf"),
                ("HUGGINGFACE_HUB_CACHE", "/old"),
                ("HF_HUB_CACHE", "~/new"),
            ],
        );
        assert_eq!(roots.hub, Path::new("/h/new"));

        let roots = with_env(Path::new("/h"), &[("HUGGINGFACE_HUB_CACHE", "/old")]);
        assert_eq!(roots.hub, Path::new("/old"));

        let roots = with_env(Path::new("/h"), &[("XDG_CACHE_HOME", "/xdg")]);
        assert_eq!(roots.hub, Path::new("/xdg/huggingface/hub"));
    }

    #[test]
    fn ollama_models_is_added_but_the_default_stays_a_candidate() {
        let roots = with_env(Path::new("/h"), &[("OLLAMA_MODELS", "/big/ollama")]);
        assert_eq!(
            roots.ollama,
            vec![
                PathBuf::from("/big/ollama"),
                PathBuf::from("/h/.ollama/models")
            ]
        );
    }

    #[test]
    fn lm_studio_downloads_folder_and_home_pointer_are_honoured() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let custom = home.join("elsewhere");
        fs::create_dir_all(&custom).unwrap();
        fs::write(
            home.join(".lmstudio-home-pointer"),
            format!("{}\n", custom.display()),
        )
        .unwrap();
        fs::write(
            custom.join("settings.json"),
            r#"{"language":"es","downloadsFolder":"~/Modelos"}"#,
        )
        .unwrap();
        let roots = with_env(home, &[]);
        assert_eq!(roots.lm_home, custom);
        assert_eq!(roots.lm_models[0], home.join("Modelos"));
        assert_eq!(roots.lm_models[1], custom.join("models"));
    }

    #[test]
    fn malformed_settings_fall_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        fs::create_dir_all(home.join(".lmstudio")).unwrap();
        fs::write(home.join(".lmstudio/settings.json"), "{nope").unwrap();
        fs::create_dir_all(home.join(".omlx")).unwrap();
        fs::write(home.join(".omlx/settings.json"), "[1,2,").unwrap();
        let roots = with_env(home, &[]);
        assert_eq!(roots.lm_models[0], home.join(".lmstudio/models"));
        assert_eq!(roots.omlx.model_dirs, vec![home.join(".omlx/models")]);
    }

    #[test]
    fn omlx_follows_its_bootstrap_file_and_settings() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let base = home.join("omlx-data");
        fs::create_dir_all(home.join("Library/Application Support/oMLX")).unwrap();
        fs::write(
            home.join("Library/Application Support/oMLX/base-path"),
            format!("{}\n", base.display()),
        )
        .unwrap();
        fs::create_dir_all(&base).unwrap();
        fs::write(
            base.join("settings.json"),
            r#"{"server":{"port":8123},"model":{"model_dirs":["/a","~/b"]},
                "cache":{"ssd_cache_dir":"/ssd"},"auth":{"api_key":"secreto"}}"#,
        )
        .unwrap();
        let roots = with_env(home, &[]);
        assert_eq!(roots.omlx.base, base);
        assert_eq!(
            roots.omlx.model_dirs,
            vec![PathBuf::from("/a"), home.join("b")]
        );
        assert_eq!(roots.omlx.ssd_cache, Path::new("/ssd"));
        assert_eq!(roots.omlx.port, 8123);

        let roots = with_env(home, &[("OMLX_BASE_PATH", "/x"), ("OMLX_PORT", "9000")]);
        assert_eq!(roots.omlx.base, Path::new("/x"));
        assert_eq!(roots.omlx.port, 9000);
    }

    #[test]
    fn the_ollama_app_setting_is_read_from_its_database() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let app = home.join("Library/Application Support/Ollama");
        fs::create_dir_all(&app).unwrap();
        let database = app.join("db.sqlite");
        let created = hm_core::exec::run(
            Path::new("/usr/bin/sqlite3"),
            &[
                database.to_str().unwrap(),
                "CREATE TABLE settings (models TEXT NOT NULL DEFAULT ''); \
                 INSERT INTO settings VALUES ('/big/ollama');",
            ],
            std::time::Duration::from_secs(5),
        )
        .unwrap();
        assert!(created.success(), "{}", created.stderr);
        let roots = Roots::resolve_with(home, false, Detail::Full, &|_| None);
        assert_eq!(
            roots.ollama,
            vec![PathBuf::from("/big/ollama"), home.join(".ollama/models")]
        );
        // The poll path must not spawn a process for this.
        let light = Roots::resolve_with(home, false, Detail::Light, &|_| None);
        assert_eq!(light.ollama, vec![home.join(".ollama/models")]);
    }

    #[test]
    fn settings_inside_a_prompting_folder_are_not_read_without_access() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let lm = home.join("Documents/lm");
        let omlx = home.join("Documents/omlx");
        fs::create_dir_all(&lm).unwrap();
        fs::create_dir_all(&omlx).unwrap();
        fs::write(
            home.join(".lmstudio-home-pointer"),
            lm.display().to_string(),
        )
        .unwrap();
        fs::write(lm.join("settings.json"), r#"{"downloadsFolder":"/leido"}"#).unwrap();
        fs::write(
            omlx.join("settings.json"),
            r#"{"server":{"port":9999},"model":{"model_dirs":["/leido"]}}"#,
        )
        .unwrap();
        let env = |name: &str| (name == "OMLX_BASE_PATH").then(|| omlx.display().to_string());

        let gated = Roots::resolve_with(home, false, Detail::Light, &env);
        assert!(!gated.lm_models.contains(&PathBuf::from("/leido")));
        assert_eq!(
            gated.lm_models[0],
            lm.join("models"),
            "still known, and gated when it's walked"
        );
        assert_eq!(gated.omlx.port, 8000);
        assert_eq!(gated.omlx.model_dirs, vec![omlx.join("models")]);

        let open = Roots::resolve_with(home, true, Detail::Light, &env);
        assert_eq!(open.lm_models[0], Path::new("/leido"));
        assert_eq!(open.omlx.port, 9999);
    }

    #[test]
    fn the_gate_refuses_prompting_places_unless_full_disk_access() {
        let gated = Roots::resolve_with(Path::new("/h"), false, Detail::Light, &|_| None);
        assert!(!gated.allowed(Path::new("/h/Documents/ComfyUI")));
        assert!(!gated.allowed(Path::new("/h/Library/Containers/x")));
        assert!(!gated.allowed(Path::new("/Volumes/SSD/models")));
        assert!(gated.allowed(Path::new("/h/.cache/huggingface/hub")));
        assert!(gated.allowed(Path::new("/h/Documentos")));
        assert_eq!(gated.skipped().len(), 3);

        let open = Roots::resolve_with(Path::new("/h"), true, Detail::Light, &|_| None);
        assert!(open.allowed(Path::new("/h/Documents/ComfyUI")));
    }

    #[test]
    fn a_root_that_links_into_a_prompting_place_is_refused_before_it_is_followed() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap();
        fs::create_dir_all(home.join("Documents/models")).unwrap();
        fs::create_dir_all(home.join("Modelos")).unwrap();
        std::os::unix::fs::symlink(home.join("Documents/models"), home.join("via-docs")).unwrap();
        std::os::unix::fs::symlink(home.join("Modelos"), home.join("via-home")).unwrap();
        let roots = Roots::resolve_with(&home, false, Detail::Light, &|_| None);
        assert_eq!(roots.follow_root(&home.join("via-docs")), None);
        assert_eq!(roots.skipped(), vec![home.join("Documents/models")]);
        assert_eq!(
            roots.follow_root(&home.join("via-home")),
            Some(home.join("Modelos"))
        );
        assert_eq!(roots.follow_root(&home.join("missing")), None);
    }
}

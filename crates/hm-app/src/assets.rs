//! Icons from Phosphor (regular weight, MIT: `assets/icons/LICENSE-phosphor.txt`), embedded in the
//! binary. One family, one weight, so every glyph has the same stroke. GPUI uses them as alpha masks,
//! tinted by `text_color`. To add one, drop its SVG from `@phosphor-icons/core/assets/regular` into
//! `assets/icons` and name it below.

use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};

macro_rules! icons {
    ($($variant:ident => $name:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Icon { $($variant),+ }

        impl Icon {
            pub fn path(self) -> &'static str {
                match self { $(Icon::$variant => concat!("icons/", $name, ".svg")),+ }
            }
        }

        fn lookup(path: &str) -> Option<&'static str> {
            match path {
                $(concat!("icons/", $name, ".svg") => Some(include_str!(concat!("../assets/icons/", $name, ".svg"))),)+
                _ => None,
            }
        }
    };
}

icons! {
    Gauge => "gauge",
    Sparkle => "broom",
    Code => "code",
    Nodes => "brain",
    File => "file",
    Cube => "cube",
    Folder => "folder",
    Plug => "plug",
    Grid => "squares-four",
    Power => "power",
    Treemap => "layout",
    Activity => "pulse",
    Sliders => "sliders-horizontal",
    Check => "check",
    Minus => "minus",
    ChevronRight => "caret-right",
    ChevronDown => "caret-down",
    ArrowUp => "arrow-up",
    X => "x",
    Plus => "plus",
    Trash => "trash",
    Refresh => "arrow-clockwise",
    Scan => "magnifying-glass",
    Shield => "shield-check",
    Alert => "warning",
    Info => "info",
    Lock => "lock-simple",
    External => "arrow-square-out",
    Branch => "git-branch",
    Disk => "hard-drives",
    Battery => "battery-high",
    Thermometer => "thermometer-simple",
    Cpu => "cpu",
    Memory => "memory",
    Network => "arrows-down-up",
    Container => "package",
    Stop => "stop",
    Bolt => "lightning",
    Copy => "copy",
    Flask => "flask",
    Spinner => "circle-notch",
    Layers => "stack",
}

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(lookup(path).map(|svg| Cow::Borrowed(svg.as_bytes())))
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}

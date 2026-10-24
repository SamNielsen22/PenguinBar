use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Context;
use evdev::KeyCode;
use serde::Deserialize;

use crate::layout::{Action, Groups, Item, Key, Layout, Slider};

// The example config is also the built-in default.
const BUILTIN: &str = include_str!("../examples/magicbar.toml");

// These structs are the shape of the TOML file.
#[derive(Deserialize)]
struct ItemFile {
    label: Option<String>,
    key: Option<String>,
    command: Option<String>,
    slider: Option<String>,
    widget: Option<String>,
}

#[derive(Deserialize)]
struct StripFile {
    collapsed: Vec<ItemFile>,
    expanded: Vec<Vec<ItemFile>>,
}

#[derive(Deserialize)]
struct LayoutFile {
    #[serde(default)]
    apps: Vec<String>,
    groups: Vec<Vec<ItemFile>>,
}

#[derive(Deserialize)]
struct ConfigFile {
    esc: Option<ItemFile>,
    strip: StripFile,
    #[serde(default)]
    layouts: HashMap<String, LayoutFile>,
}

// The config once checked and turned into items.
pub struct Config {
    esc: Key,
    strip: Strip,
    // Shown for every app.
    default: Groups,
    // Window class -> the groups that app adds after the default ones.
    apps: HashMap<String, Groups>,
}

struct Strip {
    collapsed: Vec<Item>,
    expanded: Groups,
}

impl Config {
    pub fn dir() -> PathBuf {
        let config_home = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(dir) => PathBuf::from(dir),
            None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"),
        };
        config_home.join("magicbar")
    }

    pub fn path() -> PathBuf {
        Self::dir().join("magicbar.toml")
    }

    // The user's config if there is one, otherwise the built-in default.
    pub fn load() -> anyhow::Result<Config> {
        let path = Self::path();
        if !path.exists() {
            tracing::info!("no {} yet, using the built-in layout", path.display());
            return Self::parse(BUILTIN);
        }
        let text = std::fs::read_to_string(&path)?;
        Self::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    // The bar for a window class: esc, the default groups, the app's own
    // groups, then the Control Strip. Expanded, the full strip takes the
    // place of everything but esc.
    // An open slider takes the strip's place, with nothing else but esc.
    pub fn layout_for(&self, class: &str, expanded: bool, has_media: bool, slider: Option<Item>) -> Layout {
        let mut groups = vec![vec![Item::Key(self.esc.clone())]];
        let mut strip = Vec::new();
        if let Some(slider) = slider {
            strip.push(slider);
        } else if expanded {
            groups.push(vec![toggle("\u{f0159}")]);
            groups.extend(self.strip.expanded.clone());
        } else {
            groups.extend(self.default.clone());
            if let Some(app) = self.apps.get(class) {
                groups.extend(app.clone());
            }
            strip.push(toggle("\u{f104}"));
            strip.extend(self.strip.collapsed.clone());
        }
        // The now-playing widget only shows while something is playing.
        if !has_media {
            for group in &mut groups {
                group.retain(|item| *item != Item::Media);
            }
            groups.retain(|group| !group.is_empty());
        }
        Layout { groups, strip }
    }

    fn parse(text: &str) -> anyhow::Result<Config> {
        let file: ConfigFile = toml::from_str(text)?;

        let esc = match file.esc {
            Some(item) => match parse_item(item)? {
                Item::Key(key) => key,
                _ => anyhow::bail!("esc must be a key"),
            },
            None => Key { label: "esc".to_string(), action: Action::Key(KeyCode::KEY_ESC) },
        };
        let mut collapsed = Vec::new();
        for item in file.strip.collapsed {
            collapsed.push(parse_item(item)?);
        }
        let expanded = parse_groups(file.strip.expanded)?;

        let mut apps = HashMap::new();
        let mut default = Vec::new();
        for layout in file.layouts.into_values() {
            let groups = parse_groups(layout.groups)?;
            if layout.apps.is_empty() {
                default = groups;
                continue;
            }
            for app in layout.apps {
                apps.insert(app, groups.clone());
            }
        }
        Ok(Config { esc, strip: Strip { collapsed, expanded }, default, apps })
    }
}

fn toggle(label: &str) -> Item {
    Item::Key(Key { label: label.to_string(), action: Action::ToggleStrip })
}

fn parse_groups(groups: Vec<Vec<ItemFile>>) -> anyhow::Result<Groups> {
    let mut out = Vec::new();
    for group in groups {
        let mut items = Vec::new();
        for item in group {
            items.push(parse_item(item)?);
        }
        out.push(items);
    }
    Ok(out)
}

fn parse_item(item: ItemFile) -> anyhow::Result<Item> {
    match item.widget.as_deref() {
        Some("media") => return Ok(Item::Media),
        Some(other) => anyhow::bail!("unknown widget {other}. The only widget is media"),
        None => {}
    }
    let label = item.label.context("a key needs a label")?;
    let action = match (item.key, item.command, item.slider) {
        (Some(name), None, None) => {
            let code = name
                .parse()
                .map_err(|_| anyhow::anyhow!("unknown key {name} on \"{label}\". Names look like KEY_ESC"))?;
            Action::Key(code)
        }
        (None, Some(command), None) => Action::Command(command),
        (None, None, Some(slider)) => Action::Slider(match slider.as_str() {
            "brightness" => Slider::Brightness,
            "volume" => Slider::Volume,
            "keyboard" => Slider::Keyboard,
            other => anyhow::bail!("unknown slider {other} on \"{label}\". Sliders are brightness, volume and keyboard"),
        }),
        _ => anyhow::bail!("\"{label}\" needs one of key, command or slider"),
    };
    Ok(Item::Key(Key { label, action }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip_only(items: &str) -> String {
        format!("[strip]\ncollapsed = [{items}]\nexpanded = []\n")
    }

    fn action(item: &Item) -> &Action {
        match item {
            Item::Key(key) => &key.action,
            other => panic!("expected a key, got {other:?}"),
        }
    }

    #[test]
    fn the_example_config_is_valid() {
        Config::parse(BUILTIN).unwrap();
    }

    #[test]
    fn every_app_gets_esc_the_default_groups_and_the_strip() {
        let config = Config::parse(BUILTIN).unwrap();
        let layout = config.layout_for("kitty", false, true, None);
        assert_eq!(action(&layout.groups[0][0]), &Action::Key(KeyCode::KEY_ESC));
        assert_eq!(layout.groups[1][0], Item::Media);
        assert_eq!(action(&layout.strip[0]), &Action::ToggleStrip);
        assert_eq!(layout.strip.len(), 4);
    }

    #[test]
    fn an_app_adds_its_own_keys_after_the_default_groups() {
        let text = format!(
            "{}[layouts.default]\ngroups = [[{{ widget = \"media\" }}]]\n[layouts.editor]\napps = [\"code\"]\ngroups = [[{{ label = \"run\", key = \"KEY_F5\" }}]]\n",
            strip_only("")
        );
        let config = Config::parse(&text).unwrap();
        let layout = config.layout_for("code", false, true, None);
        assert_eq!(layout.groups[1][0], Item::Media);
        assert_eq!(action(&layout.groups[2][0]), &Action::Key(KeyCode::KEY_F5));
        let other = config.layout_for("kitty", false, true, None);
        assert_eq!(other.groups.len(), 2);
    }

    #[test]
    fn the_expanded_strip_replaces_the_app_keys() {
        let config = Config::parse(BUILTIN).unwrap();
        let layout = config.layout_for("firefox", true, true, None);
        assert_eq!(action(&layout.groups[0][0]), &Action::Key(KeyCode::KEY_ESC));
        assert_eq!(action(&layout.groups[1][0]), &Action::ToggleStrip);
        assert!(layout.items().all(|item| *item != Item::Media));
        assert!(layout.strip.is_empty());
    }

    #[test]
    fn the_media_widget_hides_when_nothing_is_playing() {
        let config = Config::parse(BUILTIN).unwrap();
        let shown = config.layout_for("firefox", false, true, None);
        let hidden = config.layout_for("firefox", false, false, None);
        assert!(shown.items().any(|item| *item == Item::Media));
        assert!(hidden.items().all(|item| *item != Item::Media));
    }

    #[test]
    fn esc_defaults_to_the_escape_key() {
        let config = Config::parse(&strip_only("")).unwrap();
        assert_eq!(config.esc.action, Action::Key(KeyCode::KEY_ESC));
    }

    #[test]
    fn a_key_name_must_exist() {
        let result = Config::parse(&strip_only(r#"{ label = "x", key = "KEY_NOPE" }"#));
        let message = result.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(message.contains("KEY_NOPE"), "got: {message}");
    }

    #[test]
    fn a_key_needs_exactly_one_action() {
        assert!(Config::parse(&strip_only(r#"{ label = "x" }"#)).is_err());
        assert!(Config::parse(&strip_only(r#"{ label = "x", key = "KEY_ESC", command = "ls" }"#)).is_err());
        assert!(Config::parse(&strip_only(r#"{ label = "x", command = "ls" }"#)).is_ok());
        assert!(Config::parse(&strip_only(r#"{ label = "x", slider = "loudness" }"#)).is_err());
        let config = Config::parse(&strip_only(r#"{ label = "x", slider = "volume" }"#)).unwrap();
        assert_eq!(action(&config.strip.collapsed[0]), &Action::Slider(Slider::Volume));
    }

    #[test]
    fn an_open_slider_takes_the_strips_place() {
        let config = Config::parse(BUILTIN).unwrap();
        let open = Item::Slider { kind: Slider::Volume, value: 0.5, left: 1000.0 };
        let layout = config.layout_for("firefox", false, true, Some(open.clone()));
        assert_eq!(layout.groups.len(), 1);
        assert_eq!(layout.strip, vec![open]);
    }

    #[test]
    fn a_widget_must_be_known_and_esc_must_be_a_key() {
        assert!(Config::parse(&strip_only(r#"{ widget = "media" }"#)).is_ok());
        assert!(Config::parse(&strip_only(r#"{ widget = "clock" }"#)).is_err());
        assert!(Config::parse(&format!("esc = {{ widget = \"media\" }}\n{}", strip_only(""))).is_err());
    }

    #[test]
    fn the_strip_table_is_required() {
        assert!(Config::parse("").is_err());
    }
}

use anyhow::{Context, Result, bail};
use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

const BUILTINS: &[(&str, &str)] = &[
    ("default", include_str!("../themes/default.toml")),
    ("dark", include_str!("../themes/dark.toml")),
    ("light", include_str!("../themes/light.toml")),
    ("matrix", include_str!("../themes/matrix.toml")),
];

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum ColorValue {
    Index(u8),
    Text(String),
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct StyleSpec {
    fg: Option<ColorValue>,
    bg: Option<ColorValue>,
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
}

impl StyleSpec {
    fn overlay(&mut self, other: Self) {
        if other.fg.is_some() {
            self.fg = other.fg;
        }
        if other.bg.is_some() {
            self.bg = other.bg;
        }
        if other.bold.is_some() {
            self.bold = other.bold;
        }
        if other.italic.is_some() {
            self.italic = other.italic;
        }
        if other.underline.is_some() {
            self.underline = other.underline;
        }
    }
    fn resolve(&self, palette: &BTreeMap<String, ColorValue>) -> Result<Style> {
        let mut style = Style::default();
        if let Some(color) = &self.fg {
            style = style.fg(resolve_color(color, palette, &mut vec![])?);
        }
        if let Some(color) = &self.bg {
            style = style.bg(resolve_color(color, palette, &mut vec![])?);
        }
        for (value, modifier) in [
            (self.bold, Modifier::BOLD),
            (self.italic, Modifier::ITALIC),
            (self.underline, Modifier::UNDERLINED),
        ] {
            if let Some(value) = value {
                style = if value {
                    style.add_modifier(modifier)
                } else {
                    style.remove_modifier(modifier)
                };
            }
        }
        Ok(style)
    }
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ThemeFile {
    name: Option<String>,
    palette: BTreeMap<String, ColorValue>,
    styles: BTreeMap<String, StyleSpec>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub name: String,
    styles: BTreeMap<String, Style>,
}

impl Default for Theme {
    fn default() -> Self {
        Self::parse(BUILTINS[0].1, "default").expect("valid embedded theme")
    }
}

impl Theme {
    pub fn style(&self, role: &str) -> Style {
        self.styles[role]
    }

    pub fn parse(text: &str, id: &str) -> Result<Self> {
        let custom: ThemeFile = toml::from_str(text).context("Invalid theme TOML")?;
        let mut base: ThemeFile = toml::from_str(BUILTINS[0].1)?;
        base.palette.extend(custom.palette);
        // Validate unused palette entries too, so typos never go unnoticed.
        for (name, value) in &base.palette {
            resolve_color(value, &base.palette, &mut vec![])
                .with_context(|| format!("palette.{name}"))?;
        }
        for (role, spec) in custom.styles {
            base.styles
                .get_mut(&role)
                .with_context(|| format!("Unknown style role: styles.{role}"))?
                .overlay(spec);
        }
        let styles = base
            .styles
            .into_iter()
            .map(|(role, spec)| {
                let style = spec
                    .resolve(&base.palette)
                    .with_context(|| format!("styles.{role}"))?;
                Ok((role, style))
            })
            .collect::<Result<_>>()?;
        let name = custom.name.unwrap_or_else(|| id.into());
        if name.trim().is_empty() {
            bail!("name must not be empty");
        }
        Ok(Self { name, styles })
    }
}

fn resolve_color(
    value: &ColorValue,
    palette: &BTreeMap<String, ColorValue>,
    stack: &mut Vec<String>,
) -> Result<Color> {
    let text = match value {
        ColorValue::Index(index) => return Ok(Color::Indexed(*index)),
        ColorValue::Text(text) => text,
    };
    if let Some(name) = text.strip_prefix('$') {
        if stack.iter().any(|s| s == name) {
            bail!("Cyclic palette reference: ${name}");
        }
        let value = palette
            .get(name)
            .with_context(|| format!("Unknown palette reference: ${name}"))?;
        stack.push(name.into());
        let color = resolve_color(value, palette, stack);
        stack.pop();
        return color;
    }
    if text.starts_with('#') && text.len() == 7 && text[1..].bytes().all(|c| c.is_ascii_hexdigit())
    {
        let rgb = u32::from_str_radix(&text[1..], 16)?;
        return Ok(Color::Rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8));
    }
    let color = match text.as_str() {
        "reset" => Color::Reset,
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" => Color::Gray,
        "dark_gray" => Color::DarkGray,
        "light_red" => Color::LightRed,
        "light_green" => Color::LightGreen,
        "light_yellow" => Color::LightYellow,
        "light_blue" => Color::LightBlue,
        "light_magenta" => Color::LightMagenta,
        "light_cyan" => Color::LightCyan,
        "white" => Color::White,
        _ => bail!("Invalid color: {text:?}; expected #RRGGBB, an ANSI name, 0..255, or reset"),
    };
    Ok(color)
}

pub struct Entry {
    pub id: String,
    pub name: String,
    pub theme: Option<Theme>,
    pub error: Option<String>,
}

pub struct Catalog {
    pub entries: Vec<Entry>,
    pub diagnostics: Vec<String>,
}

pub fn directory(config: &Path) -> PathBuf {
    config
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("themes")
}

pub fn discover(config: Option<&Path>) -> Catalog {
    let mut entries: Vec<_> = BUILTINS
        .iter()
        .map(|(id, text)| {
            let theme = Theme::parse(text, id).expect("valid embedded theme");
            Entry {
                id: (*id).into(),
                name: theme.name.clone(),
                theme: Some(theme),
                error: None,
            }
        })
        .collect();
    let mut diagnostics = vec![];
    if let Some(config) = config {
        let dir = directory(config);
        let read = std::fs::read_dir(&dir);
        match read {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => diagnostics.push(format!(
                "Cannot read themes directory {}: {e}",
                dir.display()
            )),
            Ok(files) => {
                let mut paths = vec![];
                for file in files {
                    match file {
                        Ok(file) => paths.push(file.path()),
                        Err(e) => {
                            diagnostics.push(format!("Cannot read entry in {}: {e}", dir.display()))
                        }
                    }
                }
                paths.sort();
                for path in paths {
                    if path.extension().and_then(|s| s.to_str()) != Some("toml") {
                        continue;
                    }
                    let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                        diagnostics
                            .push(format!("Theme filename is not UTF-8: {}", path.display()));
                        continue;
                    };
                    match std::fs::read_to_string(&path)
                        .with_context(|| format!("Cannot read {}", path.display()))
                        .and_then(|text| Theme::parse(&text, id))
                    {
                        Ok(theme) => {
                            entries.retain(|entry| entry.id != id);
                            entries.push(Entry {
                                id: id.into(),
                                name: theme.name.clone(),
                                theme: Some(theme),
                                error: None,
                            });
                        }
                        Err(e) => {
                            let error = format!("{}: {e:#}", path.display());
                            diagnostics.push(error.clone());
                            entries.push(Entry {
                                id: id.into(),
                                name: format!("{id} (invalid)"),
                                theme: None,
                                error: Some(error),
                            });
                        }
                    }
                }
            }
        }
    }
    entries.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.id.cmp(&b.id))
    });
    Catalog {
        entries,
        diagnostics,
    }
}

pub fn check(path: &Path, selected: &str) -> Result<()> {
    let catalog = discover(Some(path));
    let mut errors = catalog.diagnostics;
    if !catalog
        .entries
        .iter()
        .any(|e| e.id == selected && e.theme.is_some())
    {
        errors.push(format!("display.theme: unknown theme {selected:?}"));
    }
    if !errors.is_empty() {
        bail!("{}", errors.join("\n"));
    }
    Ok(())
}

/// Re-read and edit the active file without reserializing the account or bindings.
pub fn save_selection(path: &Path, id: &str) -> Result<()> {
    let source =
        std::fs::read_to_string(path).with_context(|| format!("Cannot read {}", path.display()))?;
    let mut document = source.parse::<toml_edit::DocumentMut>()?;
    if document.get("display").is_none() {
        document["display"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let display = document["display"]
        .as_table_like_mut()
        .context("display must be a table")?;
    if let Some(item) = display.get_mut("theme") {
        let previous = item.as_value().context("display.theme must be a value")?;
        let mut value = toml_edit::Value::from(id);
        *value.decor_mut() = previous.decor().clone();
        *item = toml_edit::Item::Value(value);
    } else {
        display.insert("theme", toml_edit::value(id));
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.as_file()
        .set_permissions(std::fs::metadata(path)?.permissions())?;
    use std::io::Write;
    file.write_all(document.to_string().as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Character positions are shared by ranking and match highlighting.
pub fn fuzzy(query: &str, candidate: &str) -> Option<(i64, Vec<usize>)> {
    let chars: Vec<char> = candidate.chars().collect();
    let mut positions = vec![];
    let mut start = 0;
    let mut score = 0;
    for wanted in query.chars() {
        let index =
            (start..chars.len()).find(|&i| chars[i].to_lowercase().eq(wanted.to_lowercase()))?;
        if positions.last().is_some_and(|last| *last + 1 == index) {
            score += 10;
        }
        if index == 0 || !chars[index - 1].is_alphanumeric() {
            score += 15;
        }
        score -= (index - start) as i64;
        positions.push(index);
        start = index + 1;
    }
    Some((score, positions))
}

pub struct Picker {
    pub catalog: Catalog,
    pub query: String,
    pub matches: Vec<usize>,
    pub selected: usize,
    pub original: Theme,
    pub original_id: String,
    pub error: Option<String>,
}

impl Picker {
    pub fn new(catalog: Catalog, original: Theme, original_id: String) -> Self {
        let mut picker = Self {
            catalog,
            query: String::new(),
            matches: vec![],
            selected: 0,
            original,
            original_id,
            error: None,
        };
        picker.filter();
        picker.selected = picker
            .matches
            .iter()
            .position(|&i| {
                picker.catalog.entries[i].id == picker.original_id
                    && picker.catalog.entries[i].theme.is_some()
            })
            .unwrap_or(0);
        picker
    }
    pub fn filter(&mut self) {
        let mut matches: Vec<_> = self
            .catalog
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, entry)| {
                let name = fuzzy(&self.query, &entry.name).map(|m| m.0);
                let id = fuzzy(&self.query, &entry.id).map(|m| m.0);
                name.into_iter().chain(id).max().map(|score| (i, score))
            })
            .collect();
        // Catalog is already alphabetized; stable sorting keeps deterministic ties.
        matches.sort_by_key(|entry| std::cmp::Reverse(entry.1));
        self.matches = matches.into_iter().map(|(i, _)| i).collect();
        self.selected = 0;
        self.error = None;
    }
    pub fn entry(&self) -> Option<&Entry> {
        self.matches
            .get(self.selected)
            .map(|&i| &self.catalog.entries[i])
    }
    pub fn preview(&self) -> Theme {
        self.entry()
            .and_then(|e| e.theme.as_ref())
            .unwrap_or(&self.original)
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_themes_and_partial_styles_resolve_against_palette() {
        for (id, text) in BUILTINS {
            Theme::parse(text, id).unwrap();
        }
        let theme = Theme::parse(
            r##"
            [palette]
            accent = "#010203"
            custom = 42
            [styles.unread]
            bold = false
            italic = true
            underline = true
            bg = "$custom"
        "##,
            "custom",
        )
        .unwrap();
        assert_eq!(theme.name, "custom");
        assert_eq!(theme.style("border_active").fg, Some(Color::Rgb(1, 2, 3)));
        assert_eq!(
            theme.style("unread").fg,
            Theme::default().style("unread").fg
        );
        assert_eq!(theme.style("unread").bg, Some(Color::Indexed(42)));
        assert!(theme.style("unread").sub_modifier.contains(Modifier::BOLD));
        assert!(
            theme
                .style("unread")
                .add_modifier
                .contains(Modifier::ITALIC | Modifier::UNDERLINED)
        );
        let terminal = Theme::parse(
            "[palette]\nbackground = 'reset'\naccent = 'light_cyan'",
            "terminal",
        )
        .unwrap();
        assert_eq!(terminal.style("background").bg, Some(Color::Reset));
        assert_eq!(terminal.style("accent").fg, Some(Color::LightCyan));
    }

    #[test]
    fn invalid_fields_colors_and_references_are_rejected() {
        for text in [
            "unknown = 1",
            "name = ''",
            "[styles.typo]\nfg = 'red'",
            "[styles.text]\nblink = true",
            "[styles.text]\nfg = '$missing'",
            "[palette]\naccent = '#bad'",
            "[palette]\naccent = '#gggggg'",
            "[palette]\naccent = 256",
            "[palette]\naccent = -1",
            "[palette]\nunused = '$missing'",
            "[palette]\na = '$b'\nb = '$a'",
        ] {
            assert!(Theme::parse(text, "test").is_err(), "{text}");
        }
    }

    #[test]
    fn custom_location_override_errors_and_missing_directories() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("other.toml");
        assert_eq!(discover(Some(&config)).entries.len(), BUILTINS.len());
        std::fs::create_dir(directory(&config)).unwrap();
        std::fs::write(directory(&config).join("dark.toml"), "name = 'Custom dark'").unwrap();
        std::fs::write(directory(&config).join("broken.toml"), "[styles.typo]").unwrap();
        std::fs::write(directory(&config).join("ignored.txt"), "broken").unwrap();
        let catalog = discover(Some(&config));
        assert_eq!(catalog.entries.len(), BUILTINS.len() + 1);
        assert_eq!(catalog.entries.iter().filter(|e| e.id == "dark").count(), 1);
        assert_eq!(
            catalog
                .entries
                .iter()
                .find(|e| e.id == "dark")
                .unwrap()
                .name,
            "Custom dark"
        );
        assert!(catalog.diagnostics[0].contains("broken.toml"));
        assert!(
            catalog
                .entries
                .iter()
                .find(|e| e.id == "broken")
                .unwrap()
                .theme
                .is_none()
        );
        assert!(check(&config, "default").is_err());
        std::fs::write(directory(&config).join("default.toml"), "invalid!").unwrap();
        assert!(
            discover(Some(&config))
                .entries
                .iter()
                .any(|e| e.id == "default" && e.theme.is_some())
        );
    }

    #[test]
    fn check_validates_selected_theme_and_every_file() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        check(&config, "default").unwrap();
        assert!(
            check(&config, "missing")
                .unwrap_err()
                .to_string()
                .contains("display.theme")
        );
        std::fs::create_dir(directory(&config)).unwrap();
        std::fs::write(directory(&config).join("custom.toml"), "name = 'Custom'").unwrap();
        check(&config, "custom").unwrap();
    }

    #[test]
    fn persistence_preserves_comments_settings_and_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let text = "# keep header\n[account]\nusername = 'my-user' # keep account\n\n[display] # appearance\ntime_grouping = false\ntheme = 'default' # favorite\n\n[keybindings.global]\n'x' = 'quit'\n";
        std::fs::write(&path, text).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        }
        save_selection(&path, "light").unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert_eq!(saved, text.replace("'default'", "\"light\""));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o640
            );
        }
        // Re-read the current file, including changes made after a theme was loaded.
        std::fs::write(&path, saved.replace("my-user", "new-user")).unwrap();
        save_selection(&path, "dark").unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("new-user"));
    }

    #[test]
    fn persistence_adds_display_and_reports_errors_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        assert!(save_selection(&path, "dark").is_err());
        for source in [
            "# legacy\n[account]\nusername = 'user'\n",
            "display = { time_grouping = false }\n",
        ] {
            std::fs::write(&path, source).unwrap();
            save_selection(&path, "dark").unwrap();
            let saved: toml::Value =
                toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(saved["display"]["theme"].as_str(), Some("dark"));
        }
        std::fs::write(&path, "display = 42").unwrap();
        assert!(save_selection(&path, "dark").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "display = 42");
    }

    #[test]
    fn fuzzy_matching_ranks_boundaries_and_contiguous_matches() {
        assert!(fuzzy("dft", "Default").is_some());
        assert!(fuzzy("xyz", "Default").is_none());
        assert!(fuzzy("da", "Dark").unwrap().0 > fuzzy("da", "Distant").unwrap().0);
        assert!(fuzzy("日本", "日本語").is_some());
        assert_eq!(fuzzy("CAFÉ", "café").unwrap().1, vec![0, 1, 2, 3]);
        let mut picker = Picker::new(discover(None), Theme::default(), "default".into());
        picker.query = "dft".into();
        picker.filter();
        assert_eq!(picker.entry().unwrap().id, "default");
        picker.query = "does not exist".into();
        picker.filter();
        assert!(picker.matches.is_empty());
        assert_eq!(picker.preview(), picker.original);
    }
}

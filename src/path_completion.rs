use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct Completion {
    pub suggestions: Vec<String>,
    pub message: Option<String>,
}

pub fn expand_home(input: &str, home: Option<&Path>) -> Result<PathBuf> {
    if input == "~" || input.starts_with("~/") {
        let home = home.context("Cannot determine home directory")?;
        Ok(if input == "~" {
            home.to_path_buf()
        } else {
            home.join(&input[2..])
        })
    } else {
        Ok(PathBuf::from(input))
    }
}

pub fn home_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|base| base.home_dir().to_path_buf())
}

pub fn save_path(input: &str, filename: &str) -> Result<PathBuf> {
    let path = expand_home(input, home_dir().as_deref())?;
    Ok(if path.is_dir() {
        path.join(crate::compose::safe_filename(filename))
    } else {
        path
    })
}

impl Completion {
    pub fn complete(input: &mut String, home: Option<&Path>) -> Self {
        match complete(input, home) {
            Ok(suggestions) => Self {
                message: suggestions.is_empty().then(|| "No matching paths".into()),
                suggestions,
            },
            Err(error) => Self {
                message: Some(format!("Cannot complete path: {error}")),
                ..Self::default()
            },
        }
    }
}

fn complete(input: &mut String, home: Option<&Path>) -> Result<Vec<String>> {
    let typed = if input == "~" { "~/" } else { input.as_str() };
    let (parent, prefix) = typed
        .rsplit_once('/')
        .map_or(("", typed), |(parent, prefix)| {
            (&typed[..parent.len() + 1], prefix)
        });
    let directory = expand_home(if parent.is_empty() { "." } else { parent }, home)?;
    let mut suggestions = Vec::new();
    for entry in std::fs::read_dir(&directory)
        .with_context(|| format!("Cannot read {}", directory.display()))?
    {
        let entry = entry?;
        // The prompt stores UTF-8 text: do not offer a lossy, unsaveable path.
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !name.starts_with(prefix) || (name.starts_with('.') && !prefix.starts_with('.')) {
            continue;
        }
        let suffix = if entry.path().is_dir() { "/" } else { "" };
        suggestions.push(format!("{parent}{name}{suffix}"));
    }
    suggestions.sort();
    if let Some(first) = suggestions.first() {
        let mut common = first.len();
        for suggestion in &suggestions[1..] {
            common = first[..common]
                .chars()
                .zip(suggestion.chars())
                .take_while(|(a, b)| a == b)
                .map(|(ch, _)| ch.len_utf8())
                .sum();
        }
        *input = first[..common].to_owned();
    }
    Ok(suggestions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completes_common_prefix_and_then_unique_directory_and_file() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("Documents")).unwrap();
        std::fs::create_dir(root.path().join("Downloads")).unwrap();
        std::fs::write(root.path().join("Downloads/report.txt"), b"report").unwrap();
        let mut input = "~/D".to_string();
        let completion = Completion::complete(&mut input, Some(root.path()));
        assert_eq!(input, "~/Do");
        assert_eq!(completion.suggestions, ["~/Documents/", "~/Downloads/"]);
        // Repeated Tab lists the same matches rather than cycling.
        Completion::complete(&mut input, Some(root.path()));
        assert_eq!(input, "~/Do");
        input.push('w');
        Completion::complete(&mut input, Some(root.path()));
        assert_eq!(input, "~/Downloads/");
        Completion::complete(&mut input, Some(root.path()));
        assert_eq!(input, "~/Downloads/report.txt");
        assert_eq!(
            expand_home(&input, Some(root.path())).unwrap(),
            root.path().join("Downloads/report.txt")
        );
    }

    #[test]
    fn supports_spaces_unicode_hidden_names_and_case_sensitive_matching() {
        let root = tempfile::tempdir().unwrap();
        for name in ["résumé one.txt", "résumé two.txt", ".hidden", "Report.txt"] {
            std::fs::write(root.path().join(name), b"").unwrap();
        }
        let mut input = "~/ré".to_string();
        Completion::complete(&mut input, Some(root.path()));
        assert_eq!(input, "~/résumé ");
        input.push('o');
        Completion::complete(&mut input, Some(root.path()));
        assert_eq!(input, "~/résumé one.txt");
        input = "~/".into();
        let completion = Completion::complete(&mut input, Some(root.path()));
        assert_eq!(completion.suggestions.len(), 3);
        assert!(!completion.suggestions.iter().any(|s| s.contains("hidden")));
        input = "~/.".into();
        Completion::complete(&mut input, Some(root.path()));
        assert_eq!(input, "~/.hidden");
        input = "~/report".into();
        let completion = Completion::complete(&mut input, Some(root.path()));
        assert_eq!(input, "~/report");
        assert_eq!(completion.message.as_deref(), Some("No matching paths"));
    }

    #[test]
    fn completes_relative_absolute_and_bare_home_paths() {
        let root = tempfile::tempdir_in(".").unwrap();
        std::fs::write(root.path().join("target.txt"), b"").unwrap();
        let mut relative = format!("{}/tar", root.path().display());
        Completion::complete(&mut relative, None);
        assert_eq!(relative, format!("{}/target.txt", root.path().display()));
        let absolute_root = root.path().canonicalize().unwrap();
        let mut absolute = format!("{}/tar", absolute_root.display());
        Completion::complete(&mut absolute, None);
        assert_eq!(absolute, format!("{}/target.txt", absolute_root.display()));
        let mut home = "~".to_owned();
        Completion::complete(&mut home, Some(root.path()));
        assert_eq!(home, "~/target.txt");
        assert_eq!(expand_home("~", Some(root.path())).unwrap(), root.path());
        // Shell syntax other than ~/ remains literal.
        assert_eq!(
            expand_home("$HOME/file", None).unwrap(),
            Path::new("$HOME/file")
        );
        assert_eq!(
            expand_home("~someone/file", None).unwrap(),
            Path::new("~someone/file")
        );
    }

    #[test]
    fn completion_errors_preserve_input() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("file"), b"").unwrap();
        for typed in ["~/missing/abc", "~/file/abc"] {
            let mut input = typed.to_owned();
            let completion = Completion::complete(&mut input, Some(root.path()));
            assert_eq!(input, typed);
            assert!(completion.suggestions.is_empty());
            assert!(
                completion
                    .message
                    .unwrap()
                    .starts_with("Cannot complete path:")
            );
        }
        let mut input = "~/".to_owned();
        assert!(Completion::complete(&mut input, None).message.is_some());
        assert_eq!(input, "~/");
    }

    #[test]
    fn save_resolution_expands_home_and_uses_sanitized_filename_for_directories() {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().to_str().unwrap();
        assert_eq!(
            save_path(input, "../../report.txt").unwrap(),
            root.path().join("report.txt")
        );
        let file = root.path().join("renamed.txt");
        assert_eq!(
            save_path(file.to_str().unwrap(), "original.txt").unwrap(),
            file
        );
        assert_eq!(
            save_path("~/", "report.txt").unwrap(),
            home_dir().unwrap().join("report.txt")
        );
        assert_eq!(
            save_path("renamed.txt", "original.txt").unwrap(),
            Path::new("renamed.txt")
        );
    }
}

//! `mochic state --exe <name> --title <regex>`: the windows of the state
//! document a script asked for by name, so it never has to go by focus.

use regex::Regex;
use serde_json::{Value, json};

/// Which windows to list. Both parts must match when both are given.
#[derive(Debug)]
pub struct Filter {
    exe: Option<String>,
    title: Option<Regex>,
}

impl Filter {
    /// `None` when neither part is given, so plain `mochic state` keeps
    /// printing the whole document.
    pub fn new(exe: Option<&str>, title: Option<&str>) -> Result<Option<Self>, String> {
        if exe.is_none() && title.is_none() {
            return Ok(None);
        }
        let title = title
            .map(|pattern| {
                Regex::new(pattern)
                    .map_err(|err| format!("--title is not a regular expression: {err}"))
            })
            .transpose()?;
        Ok(Some(Self {
            exe: exe.map(str::to_owned),
            title,
        }))
    }

    fn matches(&self, window: &Value) -> bool {
        let text = |key: &str| window.get(key).and_then(Value::as_str).unwrap_or("");
        self.exe
            .as_deref()
            .is_none_or(|exe| text("exe").eq_ignore_ascii_case(exe))
            && self
                .title
                .as_ref()
                .is_none_or(|title| title.is_match(text("title")))
    }
}

/// The windows of a list in the document, or none when it is missing.
fn listed(value: Option<&Value>) -> impl Iterator<Item = &Value> {
    value.and_then(Value::as_array).into_iter().flatten()
}

/// Every managed window in `state` that `filter` matches, each with the
/// monitor and workspace it is on and how it is placed there.
pub fn windows(state: &Value, filter: &Filter) -> Vec<Value> {
    let mut found = Vec::new();
    for (monitor, screen) in listed(state.get("monitors")).enumerate() {
        for (workspace, space) in listed(screen.get("workspaces")).enumerate() {
            let tiled = listed(space.get("containers"))
                .flat_map(|container| listed(container.get("windows")))
                .map(|window| (window, "tiled"));
            let monocle = listed(
                space
                    .get("monocle_container")
                    .and_then(|c| c.get("windows")),
            )
            .map(|window| (window, "monocle"));
            let maximized = space
                .get("maximized_window")
                .filter(|window| window.is_object())
                .map(|window| (window, "maximized"));
            let floating = listed(space.get("floating_windows")).map(|window| (window, "floating"));
            for (window, placement) in tiled.chain(monocle).chain(maximized).chain(floating) {
                if !filter.matches(window) {
                    continue;
                }
                let mut entry = window.clone();
                if let Some(fields) = entry.as_object_mut() {
                    fields.insert("monitor".into(), json!(monitor));
                    fields.insert("workspace".into(), json!(workspace));
                    fields.insert(
                        "workspace_name".into(),
                        space.get("name").cloned().unwrap_or(Value::Null),
                    );
                    fields.insert("placement".into(), json!(placement));
                }
                found.push(entry);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(hwnd: i64, exe: &str, title: &str) -> Value {
        json!({ "hwnd": hwnd, "exe": exe, "title": title, "class": "C", "rect": null })
    }

    fn state() -> Value {
        json!({
            "monitors": [
                {
                    "index": 0,
                    "workspaces": [{
                        "index": 0,
                        "name": "1",
                        "containers": [
                            { "index": 0, "windows": [window(1, "WindowsTerminal.exe", "agents 1")] },
                            { "index": 1, "windows": [
                                window(2, "brave.exe", "Inbox"),
                                window(3, "WindowsTerminal.exe", "PowerShell"),
                            ] },
                        ],
                        "monocle_container": null,
                        "maximized_window": null,
                        "floating_windows": [window(4, "Spotify.exe", "Spotify Premium")],
                    }],
                },
                {
                    "index": 1,
                    "workspaces": [
                        {
                            "index": 0,
                            "name": "1",
                            "containers": [],
                            "monocle_container": { "index": 0, "windows": [window(5, "Code.exe", "main.rs")] },
                            "maximized_window": null,
                            "floating_windows": [],
                        },
                        {
                            "index": 1,
                            "name": "2",
                            "containers": [],
                            "monocle_container": null,
                            "maximized_window": window(6, "windowsterminal.exe", "agents 2"),
                            "floating_windows": [],
                        },
                    ],
                },
            ],
        })
    }

    fn pick(exe: Option<&str>, title: Option<&str>) -> Vec<Value> {
        windows(&state(), &Filter::new(exe, title).unwrap().unwrap())
    }

    fn hwnds(found: &[Value]) -> Vec<i64> {
        found.iter().map(|w| w["hwnd"].as_i64().unwrap()).collect()
    }

    #[test]
    fn no_filter_keeps_the_whole_document() {
        assert!(Filter::new(None, None).unwrap().is_none());
    }

    #[test]
    fn a_program_is_found_by_its_name_in_any_case_wherever_it_is_placed() {
        let found = pick(Some("WINDOWSTERMINAL.EXE"), None);
        assert_eq!(hwnds(&found), [1, 3, 6]);
        assert_eq!(found[0]["monitor"], 0);
        assert_eq!(found[0]["workspace"], 0);
        assert_eq!(found[0]["placement"], "tiled");
        assert_eq!(found[0]["title"], "agents 1");
        assert_eq!(found[2]["monitor"], 1);
        assert_eq!(found[2]["workspace"], 1);
        assert_eq!(found[2]["workspace_name"], "2");
        assert_eq!(found[2]["placement"], "maximized");
    }

    #[test]
    fn a_name_matches_whole_names_only() {
        assert!(pick(Some("Terminal.exe"), None).is_empty());
        assert!(pick(Some("WindowsTerminal"), None).is_empty());
    }

    #[test]
    fn a_title_is_matched_as_a_regular_expression() {
        let found = pick(None, Some("^agents \\d$"));
        assert_eq!(hwnds(&found), [1, 6]);
        assert_eq!(hwnds(&pick(None, Some("Spotify"))), [4]);
        assert_eq!(pick(None, Some("Spotify"))[0]["placement"], "floating");
        assert_eq!(pick(None, Some("main\\.rs"))[0]["placement"], "monocle");
    }

    #[test]
    fn both_parts_must_match() {
        assert_eq!(
            hwnds(&pick(Some("WindowsTerminal.exe"), Some("^agents"))),
            [1, 6]
        );
        assert!(pick(Some("brave.exe"), Some("^agents")).is_empty());
    }

    #[test]
    fn a_bad_pattern_is_refused_with_its_reason() {
        let err = Filter::new(None, Some("(")).unwrap_err();
        assert!(
            err.starts_with("--title is not a regular expression"),
            "{err}"
        );
    }

    #[test]
    fn a_document_without_monitors_has_no_windows() {
        let filter = Filter::new(Some("x.exe"), None).unwrap().unwrap();
        assert!(windows(&json!({}), &filter).is_empty());
    }
}

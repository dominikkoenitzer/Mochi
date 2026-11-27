// Failing-first test for Mochi: a Path rule should match the same folder whatever the case,
// as Windows does for non-ASCII letters too. Fails today: fold_case uses to_ascii_lowercase.
use mochi_core::rules::{ApplicationIdentifier, MatchingRule, MatchingStrategy, WindowInfo};

#[test]
fn a_path_rule_matches_a_folder_with_an_umlaut_in_any_case() {
    // Only the umlaut differs in case; ASCII letters already fold today.
    let window = WindowInfo::new(
        "Tool",
        "ToolWnd",
        "tool.exe",
        r"C:\Users\MÜLLER\Tools\tool.exe",
    );
    let rule = MatchingRule::simple(
        ApplicationIdentifier::Path,
        r"C:\Users\müller\Tools".to_owned(),
        MatchingStrategy::StartsWith,
    );
    assert!(rule.matches(&window));
}

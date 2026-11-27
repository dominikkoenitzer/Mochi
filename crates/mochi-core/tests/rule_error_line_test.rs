// A broken rule's error names the rule's own line and what is wrong with it.
use mochi_core::config::Config;

fn error(text: &str) -> String {
    Config::from_json(text).unwrap_err().to_string()
}

#[test]
fn a_rule_with_an_unknown_kind_is_reported_on_its_own_line_with_the_kind() {
    let e = error(
        "{\n  \"ignore_rules\": [\n    { \"kind\": \"Exee\", \"id\": \"a.exe\" },\n    { \"kind\": \"Exe\", \"id\": \"b.exe\" }\n  ]\n}",
    );
    assert!(e.contains("line 3"), "{e}");
    assert!(e.contains("Exee"), "{e}");
}

#[test]
fn a_rule_missing_its_id_says_so_and_names_a_line_of_that_rule() {
    let e = error(
        "{\n  \"ignore_rules\": [\n    {\n      \"kind\": \"Exe\",\n      \"idd\": \"a.exe\"\n    },\n    { \"kind\": \"Exe\", \"id\": \"b.exe\" }\n  ]\n}",
    );
    assert!(e.contains("`id`"), "{e}");
    assert!(
        ["line 3", "line 4", "line 5", "line 6"]
            .iter()
            .any(|l| e.contains(l)),
        "{e}"
    );
}

#[test]
fn a_bad_condition_inside_a_list_rule_is_reported_on_its_line() {
    let e = error(
        "{\n  \"manage_rules\": [\n    [ { \"kind\": \"Exe\", \"id\": \"a.exe\" }, { \"kind\": \"Titel\", \"id\": \"x\" } ],\n    { \"kind\": \"Exe\", \"id\": \"b.exe\" }\n  ]\n}",
    );
    assert!(e.contains("line 3") && e.contains("Titel"), "{e}");
}

#[test]
fn a_rule_that_is_neither_object_nor_list_keeps_the_plain_message() {
    let e = error("{\n  \"ignore_rules\": [ \"a.exe\" ]\n}");
    assert!(e.contains("rule object") && e.contains("line 2"), "{e}");
}

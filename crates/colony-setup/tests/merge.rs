//! The settings.json editor against awkward real-world files.

use colony_setup::merge::{inspect, merge, tagged_command, Action, EntryKind, Error, Wanted, EVENTS};
use serde_json::Value;

fn hook(event: &str, version: &str, timeout: u64) -> Wanted {
    Wanted {
        event: event.into(),
        kind: EntryKind::Hook,
        command: tagged_command("[ -x \"$HOME/.colony/bin/colony-hook\" ] && exec \"$HOME/.colony/bin/colony-hook\"; exit 0", EntryKind::Hook, version),
        timeout,
    }
}

fn approve(version: &str) -> Wanted {
    Wanted {
        event: "PermissionRequest".into(),
        kind: EntryKind::Approve,
        command: tagged_command("[ -f \"$HOME/.colony/bin/colony-approve.sh\" ] && exec sh \"$HOME/.colony/bin/colony-approve.sh\"; exit 0", EntryKind::Approve, version),
        timeout: 600,
    }
}

fn all(version: &str) -> Vec<Wanted> {
    EVENTS.iter().map(|e| hook(e, version, 5)).collect()
}

fn install(text: &str, wanted: &[Wanted]) -> String {
    merge(Some(text), wanted).expect("merge").text
}

fn valid(text: &str) -> Value {
    serde_json::from_str(text.trim_start_matches('\u{feff}')).unwrap_or_else(|e| panic!("not valid JSON ({e}):\n{text}"))
}

fn user_hooks(v: &Value, event: &str) -> Vec<String> {
    v["hooks"][event]
        .as_array()
        .map(|groups| {
            groups
                .iter()
                .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
                .filter_map(|h| h["command"].as_str().map(str::to_string))
                .filter(|c| !c.contains("colony-setup"))
                .collect()
        })
        .unwrap_or_default()
}

const TYPICAL: &str = r#"{
  "permissions": {
    "allow": [
      "Bash(git status:*)"
    ]
  },
  "model": "opus",
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          { "type": "command", "command": "my-linter --fast", "timeout": 10 }
        ]
      }
    ],
    "Stop": [
      { "hooks": [{ "type": "command", "command": "notify-send done" }] }
    ]
  },
  "theme": "dark"
}
"#;

#[test]
fn creates_the_file_when_missing() {
    let m = merge(None, &all("1.0.0")).unwrap();
    let v = valid(&m.text);
    for e in EVENTS {
        assert_eq!(v["hooks"][e].as_array().unwrap().len(), 1, "{e}");
    }
    assert!(m.text.ends_with("}\n"));
    assert_eq!(m.changes.len(), 14);
    assert_eq!(inspect(&m.text).unwrap().len(), 14);
}

#[test]
fn nothing_to_remove_from_a_missing_file() {
    let m = merge(None, &[]).unwrap();
    assert_eq!(m.text, "");
    assert!(m.changes.is_empty());
}

#[test]
fn an_empty_object_gets_hooks() {
    for start in ["{}", "{}\n", "{\n}\n", "  {  }  ", ""] {
        let out = install(start, &all("1.0.0"));
        assert_eq!(inspect(&out).unwrap().len(), 14, "from {start:?}");
        valid(&out);
    }
}

#[test]
fn merges_beside_the_users_hooks_without_touching_them() {
    let out = install(TYPICAL, &all("1.0.0"));
    let v = valid(&out);
    assert_eq!(v["model"], "opus");
    assert_eq!(v["theme"], "dark");
    assert_eq!(user_hooks(&v, "PreToolUse"), ["my-linter --fast"]);
    assert_eq!(user_hooks(&v, "Stop"), ["notify-send done"]);
    assert_eq!(v["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
    // The user's own text is still there verbatim.
    for piece in [
        "\"allow\": [\n      \"Bash(git status:*)\"\n    ]",
        "{ \"type\": \"command\", \"command\": \"my-linter --fast\", \"timeout\": 10 }",
        "{ \"hooks\": [{ \"type\": \"command\", \"command\": \"notify-send done\" }] }",
    ] {
        assert!(out.contains(piece), "lost {piece}");
    }
    assert_eq!(inspect(&out).unwrap().len(), 14);
}

#[test]
fn uninstall_gives_back_the_original_bytes() {
    let installed = install(TYPICAL, &all("1.0.0"));
    let back = merge(Some(&installed), &[]).unwrap();
    assert_eq!(back.text, TYPICAL);
    assert_eq!(back.changes.len(), 14);
    assert!(back.changes.iter().all(|c| c.action == Action::Remove));
}

#[test]
fn uninstall_round_trips_across_layouts() {
    let docs = [
        "{}",
        "{\n}\n",
        "{\"a\":1}",
        "{ \"a\": 1 }\n",
        "{\r\n  \"a\": 1\r\n}\r\n",
        "{\n    \"a\": 1,\n    \"b\": [1, 2]\n}",
        "{\n\t\"a\": 1\n}\n",
        "{\"hooks\":{\"Stop\":[{\"hooks\":[{\"type\":\"command\",\"command\":\"x\"}]}]},\"z\":0}",
        "{\n  \"hooks\": {\n    \"Stop\": [\n      {\n        \"hooks\": [\n          {\n            \"type\": \"command\",\n            \"command\": \"x\"\n          }\n        ]\n      }\n    ]\n  }\n}\n",
        "\u{feff}{\n  \"a\": 1\n}\n",
        "{\n  \"a\": 1   \n}   \n\n\n",
    ];
    for d in docs {
        let installed = install(d, &all("1.0.0"));
        valid(&installed);
        assert_eq!(inspect(&installed).unwrap().len(), 14, "{d:?}");
        let back = merge(Some(&installed), &[]).unwrap().text;
        assert_eq!(valid(&back), valid(d), "{d:?}");
        // Whitespace-only differences are only allowed where Colony created the hooks object.
        if d.contains("\"hooks\"") || d.contains('\n') {
            assert_eq!(back, d, "round trip changed bytes for {d:?}");
        }
    }
}

#[test]
fn is_idempotent() {
    let once = install(TYPICAL, &all("1.0.0"));
    let again = merge(Some(&once), &all("1.0.0")).unwrap();
    assert_eq!(again.text, once);
    assert!(again.changes.is_empty());
}

#[test]
fn repairs_a_partial_install() {
    let full = install(TYPICAL, &all("1.0.0"));
    // Lose two entries by hand-editing: drop Notification and the Stop entry of ours.
    let partial = merge(Some(&full), &all("1.0.0").into_iter().filter(|w| w.event != "Notification" && w.event != "Stop").collect::<Vec<_>>()).unwrap().text;
    assert_eq!(inspect(&partial).unwrap().len(), 12);
    let fixed = merge(Some(&partial), &all("1.0.0")).unwrap();
    let kinds: Vec<_> = fixed.changes.iter().map(|c| (c.event.as_str(), c.action)).collect();
    assert_eq!(kinds, [("Stop", Action::Add), ("Notification", Action::Add)]);
    assert_eq!(inspect(&fixed.text).unwrap().len(), 14);
    assert_eq!(user_hooks(&valid(&fixed.text), "Stop"), ["notify-send done"]);
}

#[test]
fn upgrades_in_place_and_keeps_extra_keys() {
    let old = install(TYPICAL, &all("0.9.0"));
    let extra = old.replacen("\"timeout\": 5", "\"async\": true, \"timeout\": 5", 1);
    let up = merge(Some(&extra), &all("0.10.0")).unwrap();
    assert!(up.changes.iter().all(|c| c.action == Action::Update), "{:?}", up.changes);
    assert_eq!(up.changes.len(), 14);
    assert!(up.text.contains("\"async\": true"));
    assert!(!up.text.contains("v=0.9.0"));
    let found = inspect(&up.text).unwrap();
    assert!(found.iter().all(|f| f.version.as_deref() == Some("0.10.0")));
    assert_eq!(user_hooks(&valid(&up.text), "PreToolUse"), ["my-linter --fast"]);
}

#[test]
fn a_timeout_change_is_an_update() {
    let a = install("{}", &all("1.0.0"));
    let mut w = all("1.0.0");
    w.iter_mut().find(|w| w.event == "PermissionRequest").unwrap().timeout = 600;
    let b = merge(Some(&a), &w).unwrap();
    assert_eq!(b.changes.len(), 1);
    assert_eq!(inspect(&b.text).unwrap().iter().find(|f| f.event == "PermissionRequest").unwrap().timeout, Some(600));
}

#[test]
fn adds_a_missing_timeout() {
    let doc = format!(
        "{{\"hooks\":{{\"Stop\":[{{\"hooks\":[{{\"type\":\"command\",\"command\":{}}}]}}]}}}}",
        serde_json::to_string(&hook("Stop", "1.0.0", 5).command).unwrap()
    );
    let m = merge(Some(&doc), &[hook("Stop", "1.0.0", 5)]).unwrap();
    assert_eq!(m.changes.len(), 1);
    assert_eq!(inspect(&m.text).unwrap()[0].timeout, Some(5));
}

#[test]
fn adopts_hand_registered_entries() {
    let doc = r#"{
  "hooks": {
    "SessionStart": [
      { "hooks": [{ "type": "command", "command": "C:/x/.colony/bin/colony-hook.exe", "timeout": 5 }] }
    ],
    "PermissionRequest": [
      { "hooks": [{ "type": "command", "command": "sh \"$HOME/.colony/bin/colony-approve.sh\"", "timeout": 600 }] }
    ]
  }
}
"#;
    let f = inspect(doc).unwrap();
    assert_eq!((f.len(), f[0].version.clone()), (2, None));
    let out = merge(Some(doc), &[hook("SessionStart", "1.0.0", 5), approve("1.0.0")]).unwrap();
    assert_eq!(out.changes.iter().filter(|c| c.action == Action::Update).count(), 2);
    assert_eq!(inspect(&out.text).unwrap().len(), 2);
    assert!(!out.text.contains("colony-hook.exe"));
}

#[test]
fn an_argument_that_mentions_colony_hook_is_not_ours() {
    let doc = r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo colony-hook was here"}]}]}}"#;
    assert!(inspect(doc).unwrap().is_empty());
    assert_eq!(merge(Some(doc), &[]).unwrap().text, doc);
}

#[test]
fn removes_only_ours_from_a_shared_group() {
    let ours = hook("Stop", "1.0.0", 5).command;
    let doc = format!(
        "{{\n  \"hooks\": {{\n    \"Stop\": [\n      {{\n        \"matcher\": \"*\",\n        \"hooks\": [\n          {{ \"type\": \"command\", \"command\": \"mine\" }},\n          {{ \"type\": \"command\", \"command\": {} }}\n        ]\n      }}\n    ]\n  }}\n}}\n",
        serde_json::to_string(&ours).unwrap()
    );
    let out = merge(Some(&doc), &[]).unwrap().text;
    let v = valid(&out);
    assert_eq!(user_hooks(&v, "Stop"), ["mine"]);
    assert_eq!(v["hooks"]["Stop"][0]["matcher"], "*");
    assert!(inspect(&out).unwrap().is_empty());
}

#[test]
fn drops_duplicate_entries() {
    let once = install("{}", &all("1.0.0"));
    // Paste the Stop group twice.
    let v = valid(&once);
    let group = v["hooks"]["Stop"][0].clone();
    let mut v2 = v.clone();
    v2["hooks"]["Stop"].as_array_mut().unwrap().push(group);
    let dup = serde_json::to_string_pretty(&v2).unwrap();
    assert_eq!(inspect(&dup).unwrap().len(), 15);
    let fixed = merge(Some(&dup), &all("1.0.0")).unwrap();
    assert_eq!(inspect(&fixed.text).unwrap().len(), 14);
    assert_eq!(fixed.changes.len(), 1);
    assert_eq!(fixed.changes[0].action, Action::Remove);
}

#[test]
fn wsl_style_has_two_permission_request_entries() {
    let mut w = all("1.0.0");
    w.push(approve("1.0.0"));
    let out = install(TYPICAL, &w);
    let perm = valid(&out)["hooks"]["PermissionRequest"].as_array().unwrap().len();
    assert_eq!(perm, 2);
    let found = inspect(&out).unwrap();
    assert_eq!(found.iter().filter(|f| f.kind == EntryKind::Approve).count(), 1);
    // Removing just the approval keeps the hook.
    let without = merge(Some(&out), &all("1.0.0")).unwrap();
    assert_eq!(without.changes.len(), 1);
    assert_eq!(inspect(&without.text).unwrap().len(), 14);
}

#[test]
fn crlf_files_stay_crlf() {
    let crlf = TYPICAL.replace('\n', "\r\n");
    let out = install(&crlf, &all("1.0.0"));
    assert!(!out.replace("\r\n", "").contains('\n'), "a bare LF was introduced");
    valid(&out);
    assert_eq!(merge(Some(&out), &[]).unwrap().text, crlf);
}

#[test]
fn mixed_line_endings_are_left_where_they_are() {
    let mixed = "{\r\n  \"a\": 1,\n  \"b\": 2\r\n}\n";
    let out = install(mixed, &all("1.0.0"));
    assert!(out.starts_with("{\r\n  \"a\": 1,\n  \"b\": 2,"));
    assert_eq!(merge(Some(&out), &[]).unwrap().text, mixed);
}

#[test]
fn follows_the_files_indentation() {
    for (doc, unit) in [("{\n    \"a\": 1\n}\n", "    "), ("{\n\t\"a\": 1\n}\n", "\t")] {
        let out = install(doc, &all("1.0.0"));
        assert!(out.contains(&format!("\n{unit}\"hooks\": {{\n{unit}{unit}\"SessionStart\"")), "{out}");
    }
}

#[test]
fn compact_files_stay_on_one_line() {
    let out = install("{\"a\":1}", &all("1.0.0"));
    assert!(!out.contains('\n'));
    assert!(out.starts_with("{\"a\":1,\"hooks\": {"));
    valid(&out);
}

#[test]
fn bom_is_kept() {
    let doc = "\u{feff}{\n  \"a\": 1\n}\n";
    let out = install(doc, &all("1.0.0"));
    assert!(out.starts_with('\u{feff}'));
    assert_eq!(merge(Some(&out), &[]).unwrap().text, doc);
}

#[test]
fn tricky_strings_do_not_confuse_the_scanner() {
    let doc = "{\n  \"note\": \"a \\\"quoted\\\" } ] , { [ string \\\\\",\n  \"\\u0068ooks\": 1\n}\n";
    // `\u0068ooks` is the key "hooks" spelled with an escape: not an object, so Colony refuses.
    assert!(matches!(merge(Some(doc), &all("1.0.0")), Err(Error::Shape(_))));
    let fine = "{\n  \"note\": \"a \\\"quoted\\\" } ] , { [ string \\\\\"\n}\n";
    let out = install(fine, &all("1.0.0"));
    assert_eq!(valid(&out)["note"], "a \"quoted\" } ] , { [ string \\");
    assert_eq!(merge(Some(&out), &[]).unwrap().text, fine);
}

#[test]
fn hooks_key_after_other_keys_and_nested_decoys() {
    let doc = r#"{"env":{"hooks":"decoy"},"nested":{"hooks":{"Stop":[]}},"hooks":{"Stop":[]}}"#;
    let out = install(doc, &[hook("Stop", "1.0.0", 5)]);
    let v = valid(&out);
    assert_eq!(v["env"]["hooks"], "decoy");
    assert_eq!(v["nested"]["hooks"]["Stop"].as_array().unwrap().len(), 0);
    assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 1);
}

#[test]
fn empty_event_arrays_get_filled() {
    let out = install("{\n  \"hooks\": {\n    \"Stop\": []\n  }\n}\n", &[hook("Stop", "1.0.0", 5)]);
    assert_eq!(inspect(&out).unwrap().len(), 1);
    valid(&out);
}

#[test]
fn refuses_invalid_json_and_leaves_it_alone() {
    for bad in ["{", "{\"a\":1,}", "// c\n{}", "{\"a\": 1} trailing", "not json", "{\"a\":'x'}"] {
        assert!(matches!(merge(Some(bad), &all("1.0.0")), Err(Error::Invalid(_))), "{bad:?}");
        assert!(matches!(merge(Some(bad), &[]), Err(Error::Invalid(_))), "{bad:?}");
        assert!(matches!(inspect(bad), Err(Error::Invalid(_))), "{bad:?}");
    }
}

#[test]
fn refuses_shapes_it_will_not_guess_about() {
    for (bad, why) in [
        ("[]", "array root"),
        ("\"x\"", "string root"),
        ("{\"hooks\": []}", "hooks array"),
        ("{\"hooks\": null}", "hooks null"),
        ("{\"hooks\": {}, \"hooks\": {}}", "duplicate hooks"),
        ("{\"hooks\": {\"Stop\": {}}}", "event not a list"),
    ] {
        assert!(matches!(merge(Some(bad), &[hook("Stop", "1.0.0", 5)]), Err(Error::Shape(_))), "{why}");
    }
}

#[test]
fn leaves_unrelated_malformed_events_alone() {
    let doc = r#"{"hooks": {"Stop": {}, "Notification": [1, "x", {"hooks": 3}]}}"#;
    let out = install(doc, &[hook("SessionStart", "1.0.0", 5)]);
    let v = valid(&out);
    assert_eq!(v["hooks"]["Stop"], serde_json::json!({}));
    assert_eq!(v["hooks"]["Notification"], serde_json::json!([1, "x", {"hooks": 3}]));
    assert_eq!(v["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
}

#[test]
fn uninstall_removes_an_empty_hooks_object_only_when_it_emptied_it() {
    let installed = install("{\n  \"a\": 1\n}\n", &all("1.0.0"));
    assert!(merge(Some(&installed), &[]).unwrap().text.find("hooks").is_none());
    let user_empty = "{\n  \"hooks\": {}\n}\n";
    assert_eq!(merge(Some(user_empty), &[]).unwrap().text, user_empty);
}

#[test]
fn no_trailing_newline_is_kept() {
    let doc = "{\n  \"a\": 1\n}";
    let out = install(doc, &all("1.0.0"));
    assert!(out.ends_with("}") && !out.ends_with("\n"));
    assert_eq!(merge(Some(&out), &[]).unwrap().text, doc);
}

#[test]
fn large_realistic_file_round_trips() {
    // Many user hooks across events, different shapes, odd spacing.
    let mut hooks = String::new();
    for (i, e) in EVENTS.iter().enumerate() {
        if i % 2 == 0 {
            hooks.push_str(&format!(
                "{}\"{e}\": [\n      {{ \"matcher\": \"Edit|Write\", \"hooks\": [ {{ \"type\": \"command\", \"command\": \"fmt-{i}\", \"timeout\": {i} }} ] }}\n    ]",
                if hooks.is_empty() { "" } else { ",\n    " }
            ));
        }
    }
    let doc = format!("{{\n  \"env\": {{ \"A\": \"1\" }},\n  \"hooks\": {{\n    {hooks}\n  }},\n  \"statusLine\": {{ \"type\": \"command\", \"command\": \"x\" }}\n}}\n");
    valid(&doc);
    let out = install(&doc, &all("1.0.0"));
    let v = valid(&out);
    assert_eq!(user_hooks(&v, "SessionStart"), ["fmt-0"]);
    assert_eq!(inspect(&out).unwrap().len(), 14);
    assert_eq!(merge(Some(&out), &[]).unwrap().text, doc);
}

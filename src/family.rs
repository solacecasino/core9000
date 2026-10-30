//! The family protocol: what every 9000 sister says to its adapters and
//! understands from them. Each app adds its own requests on top (`edit.*`
//! in splice9000, wave9000 and grace9000, `doc.*` in text9000, `book.*` in
//! leaf9000) and numbers those in its own `protocol`; this part is numbered
//! [`FAMILY`] and is the same everywhere, so an adapter that sticks to it
//! works in any sister that installs it (see `apps` in adapter.toml).
//!
//! Messages are JSON, one per line. A notification has `method` and
//! `params`; a request also has an `id`, and its answer has the same `id`
//! and `result` or `error`.
//!
//! App → adapter: `initialize` (below), `command` {command}, `event` {kind,
//! ...}, `shutdown`.
//!
//! Adapter → app, in every sister:
//!
//! | Method | Params | Result |
//! |---|---|---|
//! | `log` | message | written to the Log panel |
//! | `notify` | text | a short message in the window |
//! | `panel.set` | id, lines | fills one of its panels |
//! | `ui.confirm` | title, message | true or false |
//! | `ui.prompt` | title, message, default | the text, or null |
//! | `ui.save_dialog` | title, file_name, extension | a path, or null |
//! | `ui.open_dialog` | title, extensions | a path, or null |
//! | `sisters.list` | | [{name, kind, installed, opens}] |
//! | `sister.open` | app, paths | opens files in a sister app |
//!
//! [`call`] answers the ones that need no window (`log`, `sisters.*`); the
//! app answers the rest and its own.

use std::path::PathBuf;

use serde_json::{Map, Value, json};

use crate::sisters;

/// The family protocol's version. New versions only add things.
/// 1: the 9000 series' shared messages, `apps` in adapter.toml, and the
/// sisters (`sisters.list`, `sister.open`).
pub const FAMILY: u32 = 1;

/// Permissions every sister knows, with what each allows in words.
pub const PERMISSIONS: &[(&str, &str)] =
    &[("network", "connect to the internet or other computers")];

/// `initialize`'s params: who the app is and what it speaks, then `more`
/// from the app (such as its ffmpeg).
///
/// `{ "app": "splice9000", "version": "0.1.0", "<app>": "0.1.0",
/// "protocol": 3, "family": 1, "permissions": [...], "sisters": [...] }`;
/// `<app>` (the app's name as the key) is how sisters told adapters their
/// version before the family protocol, and stays.
pub fn initialize(permissions: &[String], more: &Map<String, Value>) -> Value {
    let app = crate::app();
    let mut params = json!({
        "app": app.name,
        "version": app.version,
        app.name: app.version,
        "protocol": app.protocol,
        "family": FAMILY,
        "permissions": permissions,
        "sisters": sisters::ALL.iter().map(|s| s.name).collect::<Vec<_>>(),
    });
    for (k, v) in more {
        params[k] = v.clone();
    }
    params
}

/// Answers a family request that needs no window. None: not one of these
/// (the app answers it).
pub fn call(adapter: &str, method: &str, params: &Value) -> Option<Result<Value, String>> {
    let text = |key: &str| {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    Some(match method {
        "log" => {
            crate::log::info(format!("{adapter}: {}", text("message")));
            Ok(Value::Null)
        }
        "sisters.list" => Ok(Value::Array(
            sisters::ALL
                .iter()
                .map(|s| {
                    let opens: Vec<&str> = s.owns.iter().chain(s.opens).copied().collect();
                    json!({ "name": s.name, "kind": s.kind, "installed": s.name == crate::app().name || s.installed(), "opens": opens })
                })
                .collect(),
        )),
        "sister.open" => open_in_sister(&text("app"), params),
        _ => return None,
    })
}

fn open_in_sister(app: &str, params: &Value) -> Result<Value, String> {
    let sister = sisters::get(app).ok_or_else(|| format!("no sister app called \"{app}\""))?;
    let paths: Vec<PathBuf> = params
        .get("paths")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(PathBuf::from)
        .collect();
    if let Some(missing) = paths.iter().find(|p| !p.exists()) {
        return Err(format!("{} doesn't exist", missing.display()));
    }
    let refs: Vec<&std::path::Path> = paths.iter().map(PathBuf::as_path).collect();
    sister.open(&refs, None)?;
    Ok(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_says_who_and_what() {
        let mut more = Map::new();
        more.insert("ffmpeg".into(), json!("/usr/bin/ffmpeg"));
        let v = initialize(&["network".into()], &more);
        assert_eq!(v["family"], FAMILY);
        assert_eq!(v["ffmpeg"], "/usr/bin/ffmpeg");
        assert_eq!(v["sisters"].as_array().unwrap().len(), 5);
        assert_eq!(v[crate::app().name], v["version"]);
    }

    #[test]
    fn answers_the_family_requests() {
        let list = call("a", "sisters.list", &Value::Null).unwrap().unwrap();
        assert!(
            list.as_array()
                .unwrap()
                .iter()
                .any(|s| s["name"] == "wave9000"
                    && s["opens"].as_array().unwrap().contains(&json!("flac")))
        );
        assert!(
            call("a", "sister.open", &json!({ "app": "nope" }))
                .unwrap()
                .is_err()
        );
        assert!(
            call(
                "a",
                "sister.open",
                &json!({ "app": "wave9000", "paths": ["/no/such/file.wav"] })
            )
            .unwrap()
            .is_err()
        );
        assert!(call("a", "edit.get", &Value::Null).is_none());
    }
}

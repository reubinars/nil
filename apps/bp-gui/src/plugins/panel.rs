//! Declarative plugin panels: the JSON a plugin sends with `panel.set`,
//! parsed into a typed tree that nil draws in its own theme.

use std::collections::HashSet;
use std::path::PathBuf;

use serde_json::Value;

use super::rpc::RpcError;

/// Most `row`s that may be nested inside one another.
const MAX_ROW_DEPTH: usize = 4;

#[derive(Debug, Clone, PartialEq)]
pub enum Element {
    Heading {
        text: String,
        muted: bool,
    },
    Text {
        text: String,
        muted: bool,
    },
    Input {
        id: String,
        placeholder: String,
        value: String,
        submit_label: String,
    },
    Button {
        id: String,
        label: String,
        enabled: bool,
    },
    Toggle {
        id: String,
        label: String,
        value: bool,
    },
    /// `value` is None while busy.
    Progress {
        value: Option<f32>,
        label: String,
    },
    Tracks {
        items: Vec<PanelTrack>,
        /// Drawn uppercased above the rows.
        label: Option<String>,
    },
    Row {
        children: Vec<Element>,
    },
    Unsupported {
        kind: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct PanelTrack {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub duration_s: Option<f64>,
    /// The file is gone from disk. False from [`parse`]; set by [`Panel::mark_missing`].
    pub missing: bool,
}

/// Where a panel is drawn: the plugin's tab, or its entry in Settings → Plugins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Place {
    Tab,
    Settings,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Panel {
    pub elements: Vec<Element>,
}

impl Panel {
    /// The paths of every listed track, rows included, in order.
    pub fn track_paths(&self) -> Vec<PathBuf> {
        fn walk(elements: &[Element], out: &mut Vec<PathBuf>) {
            for e in elements {
                match e {
                    Element::Tracks { items, .. } => {
                        out.extend(items.iter().map(|t| t.path.clone()));
                    }
                    Element::Row { children } => walk(children, out),
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.elements, &mut out);
        out
    }

    /// Flags the track rows whose file is gone from disk, as playlist rows are. Done
    /// once when a panel arrives, not every frame.
    pub fn mark_missing(&mut self) {
        fn walk(elements: &mut [Element]) {
            for e in elements {
                match e {
                    Element::Tracks { items, .. } => {
                        for t in items {
                            t.missing = !t.path.exists();
                        }
                    }
                    Element::Row { children } => walk(children),
                    _ => {}
                }
            }
        }
        walk(&mut self.elements);
    }
}

/// Parse a `{ "elements": [...] }` panel. Every rejection is -32602.
pub fn parse(v: &Value) -> Result<Panel, RpcError> {
    let elements = v
        .as_object()
        .ok_or_else(|| bad("panel must be an object".into()))?
        .get("elements")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("panel.elements must be an array".into()))?;
    let mut ids = HashSet::new();
    let elements = parse_list(elements, "elements", 0, &mut ids)?;
    Ok(Panel { elements })
}

/// Parse `panel.set` params: `panel` and an optional `place` (`"tab"` by default). A
/// settings panel may not hold `tracks` or `input`. Every rejection is -32602.
pub fn parse_set(params: &Value) -> Result<(Place, Panel), RpcError> {
    let place = match params.get("place") {
        None | Some(Value::Null) => Place::Tab,
        Some(Value::String(s)) if s == "tab" => Place::Tab,
        Some(Value::String(s)) if s == "settings" => Place::Settings,
        Some(_) => return Err(bad("place must be \"tab\" or \"settings\"".into())),
    };
    let panel = parse(
        params
            .get("panel")
            .ok_or_else(|| bad("missing field: panel".into()))?,
    )?;
    if place == Place::Settings {
        settings_only(&panel.elements, "elements")?;
    }
    Ok((place, panel))
}

/// Refuses the elements a settings panel cannot show, wherever they are.
fn settings_only(elements: &[Element], at: &str) -> Result<(), RpcError> {
    for (i, e) in elements.iter().enumerate() {
        let at = format!("{at}[{i}]");
        let kind = match e {
            Element::Tracks { .. } => "tracks",
            Element::Input { .. } => "input",
            Element::Row { children } => {
                settings_only(children, &format!("{at}.children"))?;
                continue;
            }
            _ => continue,
        };
        return Err(bad(format!(
            "{at}: `{kind}` is not allowed in a settings panel"
        )));
    }
    Ok(())
}

fn bad(msg: String) -> RpcError {
    RpcError::bad_params(msg)
}

fn parse_list(
    list: &[Value],
    at: &str,
    rows: usize,
    ids: &mut HashSet<String>,
) -> Result<Vec<Element>, RpcError> {
    list.iter()
        .enumerate()
        .map(|(i, e)| parse_element(e, &format!("{at}[{i}]"), rows, ids))
        .collect()
}

fn str_req(o: &Value, key: &str, at: &str) -> Result<String, RpcError> {
    o.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| bad(format!("{at}: missing or invalid string field `{key}`")))
}

fn str_opt(o: &Value, key: &str, at: &str) -> Result<String, RpcError> {
    match o.get(key) {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(bad(format!("{at}: `{key}` must be a string"))),
    }
}

fn bool_opt(o: &Value, key: &str, default: bool, at: &str) -> Result<bool, RpcError> {
    match o.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(bad(format!("{at}: `{key}` must be a boolean"))),
    }
}

/// An id, required and unique across the whole panel.
fn id_req(o: &Value, at: &str, ids: &mut HashSet<String>) -> Result<String, RpcError> {
    let id = str_req(o, "id", at)?;
    if !ids.insert(id.clone()) {
        return Err(bad(format!("{at}: duplicate id `{id}`")));
    }
    Ok(id)
}

fn parse_element(
    o: &Value,
    at: &str,
    rows: usize,
    ids: &mut HashSet<String>,
) -> Result<Element, RpcError> {
    if !o.is_object() {
        return Err(bad(format!("{at}: element must be an object")));
    }
    let kind = str_req(o, "type", at)?;
    Ok(match kind.as_str() {
        "heading" | "text" => {
            let text = str_req(o, "text", at)?;
            let muted = bool_opt(o, "muted", false, at)?;
            if kind == "heading" {
                Element::Heading { text, muted }
            } else {
                Element::Text { text, muted }
            }
        }
        "input" => Element::Input {
            id: id_req(o, at, ids)?,
            placeholder: str_req(o, "placeholder", at)?,
            value: str_opt(o, "value", at)?,
            submit_label: str_req(o, "submit_label", at)?,
        },
        "button" => Element::Button {
            id: id_req(o, at, ids)?,
            label: str_req(o, "label", at)?,
            enabled: bool_opt(o, "enabled", true, at)?,
        },
        "toggle" => Element::Toggle {
            id: id_req(o, at, ids)?,
            label: str_req(o, "label", at)?,
            value: o
                .get("value")
                .and_then(Value::as_bool)
                .ok_or_else(|| bad(format!("{at}: missing or invalid boolean field `value`")))?,
        },
        "progress" => {
            let value = match o.get("value") {
                Some(Value::Null) => None,
                Some(n) => Some(
                    n.as_f64()
                        .ok_or_else(|| bad(format!("{at}: `value` must be a number or null")))?
                        .clamp(0.0, 1.0) as f32,
                ),
                None => return Err(bad(format!("{at}: missing field `value`"))),
            };
            Element::Progress {
                value,
                label: str_opt(o, "label", at)?,
            }
        }
        "tracks" => {
            let items = o
                .get("items")
                .and_then(Value::as_array)
                .ok_or_else(|| bad(format!("{at}: missing or invalid array field `items`")))?;
            let items = items
                .iter()
                .enumerate()
                .map(|(i, t)| parse_track(t, &format!("{at}.items[{i}]")))
                .collect::<Result<_, _>>()?;
            let label = match o.get("label") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) => Some(s.clone()),
                Some(_) => return Err(bad(format!("{at}: `label` must be a string"))),
            };
            Element::Tracks { items, label }
        }
        "row" => {
            if rows + 1 > MAX_ROW_DEPTH {
                return Err(bad(format!(
                    "{at}: rows nested deeper than {MAX_ROW_DEPTH}"
                )));
            }
            let children = o
                .get("children")
                .and_then(Value::as_array)
                .ok_or_else(|| bad(format!("{at}: missing or invalid array field `children`")))?;
            Element::Row {
                children: parse_list(children, &format!("{at}.children"), rows + 1, ids)?,
            }
        }
        _ => Element::Unsupported { kind },
    })
}

fn parse_track(t: &Value, at: &str) -> Result<PanelTrack, RpcError> {
    if !t.is_object() {
        return Err(bad(format!("{at}: track must be an object")));
    }
    let path = PathBuf::from(str_req(t, "path", at)?);
    let title = match t.get("title").and_then(Value::as_str) {
        Some(s) => s.to_string(),
        None => path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    let duration_s = t
        .get("duration_s")
        .and_then(Value::as_f64)
        .filter(|d| d.is_finite() && *d > 0.0);
    Ok(PanelTrack {
        path,
        title,
        artist: str_opt(t, "artist", at)?,
        duration_s,
        missing: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn err(v: Value) -> RpcError {
        parse(&v).unwrap_err()
    }

    #[test]
    fn mark_missing_flags_tracks_that_are_gone_including_inside_rows() {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().join("here.flac");
        std::fs::write(&here, b"").unwrap();
        let gone = dir.path().join("gone.flac");
        let (here, gone) = (here.to_str().unwrap(), gone.to_str().unwrap());
        let mut p = parse(&json!({"elements": [
            {"type": "tracks", "items": [{"path": here}, {"path": gone}]},
            {"type": "row", "children": [
                {"type": "tracks", "items": [{"path": gone}]},
            ]},
        ]}))
        .unwrap();
        p.mark_missing();
        let Element::Tracks { items, .. } = &p.elements[0] else {
            panic!()
        };
        assert_eq!(
            items.iter().map(|t| t.missing).collect::<Vec<_>>(),
            [false, true]
        );
        let Element::Row { children } = &p.elements[1] else {
            panic!()
        };
        let Element::Tracks { items, .. } = &children[0] else {
            panic!()
        };
        assert!(items[0].missing);
    }

    fn nested(rows: usize) -> Value {
        let mut e = json!({"type": "text", "text": "x"});
        for _ in 0..rows {
            e = json!({"type": "row", "children": [e]});
        }
        json!({"elements": [e]})
    }

    #[test]
    fn parses_every_element_type() {
        let p = parse(&json!({"elements": [
            {"type": "heading", "text": "H", "muted": true},
            {"type": "text", "text": "T"},
            {"type": "input", "id": "q", "placeholder": "p", "submit_label": "Go", "value": "v"},
            {"type": "button", "id": "b", "label": "B", "enabled": false},
            {"type": "toggle", "id": "t", "label": "T", "value": true},
            {"type": "progress", "value": 0.5, "label": "half"},
            {"type": "tracks", "items": [{"path": "/m/a.flac", "title": "A", "artist": "X", "duration_s": 12.5}]},
            {"type": "row", "children": [{"type": "text", "text": "in"}]},
        ]}))
        .unwrap();
        assert_eq!(p.elements.len(), 8);
        assert_eq!(
            p.elements[0],
            Element::Heading {
                text: "H".into(),
                muted: true
            }
        );
        assert_eq!(
            p.elements[1],
            Element::Text {
                text: "T".into(),
                muted: false
            }
        );
        assert_eq!(
            p.elements[2],
            Element::Input {
                id: "q".into(),
                placeholder: "p".into(),
                value: "v".into(),
                submit_label: "Go".into()
            }
        );
        assert_eq!(
            p.elements[3],
            Element::Button {
                id: "b".into(),
                label: "B".into(),
                enabled: false
            }
        );
        assert_eq!(
            p.elements[4],
            Element::Toggle {
                id: "t".into(),
                label: "T".into(),
                value: true
            }
        );
        assert_eq!(
            p.elements[5],
            Element::Progress {
                value: Some(0.5),
                label: "half".into()
            }
        );
        assert_eq!(
            p.elements[6],
            Element::Tracks {
                items: vec![PanelTrack {
                    path: "/m/a.flac".into(),
                    title: "A".into(),
                    artist: "X".into(),
                    duration_s: Some(12.5),
                    missing: false,
                }],
                label: None,
            }
        );
        assert_eq!(
            p.elements[7],
            Element::Row {
                children: vec![Element::Text {
                    text: "in".into(),
                    muted: false
                }]
            }
        );
    }

    #[test]
    fn defaults_apply() {
        let p = parse(&json!({"elements": [
            {"type": "input", "id": "q", "placeholder": "p", "submit_label": "Go"},
            {"type": "button", "id": "b", "label": "B"},
            {"type": "progress", "value": 7},
            {"type": "tracks", "items": [
                {"path": "/m/song.flac", "duration_s": 0},
                {"path": "/m/b.flac", "duration_s": -3},
            ]},
        ]}))
        .unwrap();
        assert!(matches!(&p.elements[0], Element::Input { value, .. } if value.is_empty()));
        assert!(matches!(
            &p.elements[1],
            Element::Button { enabled: true, .. }
        ));
        assert_eq!(
            p.elements[2],
            Element::Progress {
                value: Some(1.0),
                label: String::new()
            }
        );
        let Element::Tracks { items, .. } = &p.elements[3] else {
            panic!()
        };
        assert_eq!(items[0].title, "song");
        assert_eq!(items[0].artist, "");
        assert_eq!(items[0].duration_s, None);
        assert_eq!(items[1].duration_s, None);
    }

    #[test]
    fn unknown_type_becomes_unsupported() {
        let p = parse(&json!({"elements": [{"type": "chart", "data": 1}]})).unwrap();
        assert_eq!(
            p.elements,
            vec![Element::Unsupported {
                kind: "chart".into()
            }]
        );
    }

    #[test]
    fn missing_field_is_32602_and_names_the_index() {
        let e = err(json!({"elements": [
            {"type": "text", "text": "ok"},
            {"type": "button", "id": "b"},
        ]}));
        assert_eq!(e.code, -32602);
        assert!(e.message.contains("elements[1]"), "{}", e.message);

        let e = err(json!({"elements": [
            {"type": "text", "text": "ok"},
            {"type": "text", "text": "ok"},
            {"type": "row", "children": [{"type": "text"}]},
        ]}));
        assert_eq!(e.code, -32602);
        assert!(
            e.message.contains("elements[2].children[0]"),
            "{}",
            e.message
        );

        let e = err(json!({"elements": [{"type": "tracks", "items": [{"title": "x"}]}]}));
        assert_eq!(e.code, -32602);
        assert!(e.message.contains("elements[0].items[0]"), "{}", e.message);
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let e = err(json!({"elements": [
            {"type": "button", "id": "x", "label": "a"},
            {"type": "row", "children": [{"type": "toggle", "id": "x", "label": "b", "value": false}]},
        ]}));
        assert_eq!(e.code, -32602);
        assert!(
            e.message.contains("elements[1].children[0]"),
            "{}",
            e.message
        );
    }

    #[test]
    fn progress_null_is_busy() {
        let p = parse(&json!({"elements": [{"type": "progress", "value": null, "label": "wait"}]}))
            .unwrap();
        assert_eq!(
            p.elements[0],
            Element::Progress {
                value: None,
                label: "wait".into()
            }
        );
    }

    #[test]
    fn row_depth_boundary() {
        assert!(parse(&nested(4)).is_ok());
        assert_eq!(err(nested(5)).code, -32602);
    }

    fn set(v: Value) -> Result<(Place, Panel), RpcError> {
        parse_set(&v)
    }

    #[test]
    fn place_defaults_to_tab() {
        let empty = json!({"elements": []});
        assert_eq!(set(json!({"panel": empty})).unwrap().0, Place::Tab);
        assert_eq!(
            set(json!({"panel": empty, "place": "tab"})).unwrap().0,
            Place::Tab
        );
        assert_eq!(
            set(json!({"panel": empty, "place": "settings"})).unwrap().0,
            Place::Settings
        );
        assert_eq!(set(json!({"place": "tab"})).unwrap_err().code, -32602);
    }

    #[test]
    fn bad_place_is_refused() {
        for place in [json!("side"), json!(3)] {
            let e = set(json!({"panel": {"elements": []}, "place": place})).unwrap_err();
            assert_eq!(e.code, -32602);
        }
    }

    #[test]
    fn tracks_label_is_read() {
        let label = |t: Value| match parse(&json!({"elements": [t]})).map(|p| p.elements) {
            Ok(e) => match &e[0] {
                Element::Tracks { label, .. } => Ok(label.clone()),
                _ => panic!(),
            },
            Err(e) => Err(e.code),
        };
        assert_eq!(
            label(json!({"type": "tracks", "items": [], "label": "Mix · 2 tracks"})),
            Ok(Some("Mix · 2 tracks".into()))
        );
        assert_eq!(label(json!({"type": "tracks", "items": []})), Ok(None));
        assert_eq!(
            label(json!({"type": "tracks", "items": [], "label": 5})),
            Err(-32602)
        );
    }

    #[test]
    fn settings_refuse_tracks_and_input() {
        let settings =
            |elements: Value| set(json!({"panel": {"elements": elements}, "place": "settings"}));
        let e = settings(json!([{"type": "tracks", "items": []}])).unwrap_err();
        assert_eq!(e.code, -32602);
        assert!(e.message.contains("tracks"), "{}", e.message);
        let e = settings(json!([{"type": "row", "children": [
            {"type": "input", "id": "q", "placeholder": "p", "submit_label": "Go"},
        ]}]))
        .unwrap_err();
        assert_eq!(e.code, -32602);
        assert!(
            e.message.contains("elements[0].children[0]"),
            "{}",
            e.message
        );
        assert!(
            settings(json!([
                {"type": "heading", "text": "H"},
                {"type": "text", "text": "T"},
                {"type": "progress", "value": 0.5},
                {"type": "button", "id": "b", "label": "B"},
                {"type": "toggle", "id": "t", "label": "T", "value": true},
                {"type": "row", "children": [{"type": "text", "text": "in"}]},
            ]))
            .is_ok()
        );
    }

    #[test]
    fn ids_are_unique_per_panel() {
        let index = json!({"elements": [{"type": "button", "id": "index", "label": "I"}]});
        assert!(set(json!({"panel": index})).is_ok());
        assert!(set(json!({"panel": index, "place": "settings"})).is_ok());
        let twice = json!({"elements": [
            {"type": "button", "id": "index", "label": "I"},
            {"type": "button", "id": "index", "label": "J"},
        ]});
        let e = set(json!({"panel": twice, "place": "settings"})).unwrap_err();
        assert_eq!(e.code, -32602);
    }

    #[test]
    fn non_panel_input_is_32602() {
        for v in [
            json!(null),
            json!([]),
            json!("x"),
            json!({}),
            json!({"elements": {}}),
        ] {
            assert_eq!(err(v).code, -32602);
        }
    }
}

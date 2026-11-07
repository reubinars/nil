//! JSON-RPC 2.0 line codec and typed calls for the stdio plugin host.
//!
//! One UTF-8 JSON message per line. Nothing here does I/O; the host's reader
//! and writer threads feed lines in and out.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Longest accepted message in bytes, not counting the line terminator; a longer
/// message is a parse error.
pub const MAX_LINE: usize = 1_048_576;
/// Protocol major version sent in `initialize`.
pub const API_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    Playback,
    Library,
    Playlists,
}

impl Permission {
    pub fn name(self) -> &'static str {
        match self {
            Permission::Playback => "playback",
            Permission::Library => "library",
            Permission::Playlists => "playlists",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>, data: Option<Value>) -> Self {
        Self {
            code,
            message: message.into(),
            data,
        }
    }
    /// The code of [`RpcError::denied`].
    pub const DENIED: i64 = -32001;

    pub fn parse() -> Self {
        Self::new(-32700, "parse error", None)
    }
    pub fn invalid_request(msg: impl Into<String>) -> Self {
        Self::new(-32600, msg, None)
    }
    pub fn unknown_method(m: &str) -> Self {
        Self::new(-32601, format!("unknown method: {m}"), None)
    }
    pub fn bad_params(msg: impl Into<String>) -> Self {
        Self::new(-32602, msg, None)
    }
    pub fn denied(perm: Permission) -> Self {
        Self::new(
            Self::DENIED,
            format!("permission denied: needs {}", perm.name()),
            None,
        )
    }
    pub fn not_found(msg: impl Into<String>, data: Option<Value>) -> Self {
        Self::new(-32002, msg, data)
    }
    pub fn busy(msg: impl Into<String>) -> Self {
        Self::new(-32003, msg, None)
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::new(-32603, msg, None)
    }
    /// A queue edit that no longer fits the queue: `reason` is `"changed"`, `"started"`
    /// or `"stopped"`.
    pub fn queue_changed(reason: &str) -> Self {
        Self::new(-32004, "queue changed", Some(json!({ "reason": reason })))
    }

    fn to_value(&self) -> Value {
        let mut o = Map::new();
        o.insert("code".into(), json!(self.code));
        o.insert("message".into(), json!(self.message));
        if let Some(d) = &self.data {
            o.insert("data".into(), d.clone());
        }
        Value::Object(o)
    }

    fn from_value(v: &Value) -> Option<Self> {
        let o = v.as_object()?;
        Some(Self {
            code: o.get("code")?.as_i64()?,
            message: o.get("message")?.as_str()?.to_string(),
            data: o.get("data").cloned(),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    /// A request with `"id": null`. It is answered with -32600 (and a null id)
    /// and its method is not run.
    InvalidRequest,
    Response {
        id: Value,
        result: Result<Value, RpcError>,
    },
}

/// Decode one line (without or with its trailing newline). One trailing `\n` or
/// `\r\n` is stripped before the size check, as the host's reader does.
///
/// Oversized, non-UTF-8, non-JSON, wrong-version and structurally invalid
/// messages (both or neither of result/error, no method, no id) all give
/// the parse error.
pub fn decode(line: &[u8]) -> Result<Incoming, RpcError> {
    // The `\r` belongs to the terminator only when a `\n` follows it.
    let line = match line.strip_suffix(b"\n") {
        Some(l) => l.strip_suffix(b"\r").unwrap_or(l),
        None => line,
    };
    if line.len() > MAX_LINE {
        return Err(RpcError::parse());
    }
    let text = std::str::from_utf8(line).map_err(|_| RpcError::parse())?;
    let v: Value = serde_json::from_str(text).map_err(|_| RpcError::parse())?;
    let Value::Object(mut o) = v else {
        return Err(RpcError::parse());
    };
    if o.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(RpcError::parse());
    }
    let params = o.remove("params").unwrap_or(Value::Null);
    let id = o.remove("id");
    if let Some(m) = o.remove("method") {
        let Value::String(method) = m else {
            return Err(RpcError::parse());
        };
        return Ok(match id {
            None => Incoming::Notification { method, params },
            Some(Value::Null) => Incoming::InvalidRequest,
            Some(id) => Incoming::Request { id, method, params },
        });
    }
    let Some(id) = id.filter(|i| !i.is_null()) else {
        return Err(RpcError::parse());
    };
    match (o.remove("result"), o.remove("error")) {
        (Some(r), None) => Ok(Incoming::Response { id, result: Ok(r) }),
        (None, Some(e)) => {
            let e = RpcError::from_value(&e).ok_or_else(RpcError::parse)?;
            Ok(Incoming::Response { id, result: Err(e) })
        }
        _ => Err(RpcError::parse()),
    }
}

fn line(v: Value) -> String {
    let mut s = v.to_string();
    s.push('\n');
    s
}

pub fn encode_request(id: u64, method: &str, params: Value) -> String {
    line(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
}

pub fn encode_notification(method: &str, params: Value) -> String {
    line(json!({ "jsonrpc": "2.0", "method": method, "params": params }))
}

pub fn encode_response(id: &Value, r: &Result<Value, RpcError>) -> String {
    match r {
        Ok(result) => line(json!({ "jsonrpc": "2.0", "id": id, "result": result })),
        Err(e) => line(json!({ "jsonrpc": "2.0", "id": id, "error": e.to_value() })),
    }
}

/// A request from a plugin that needs a permission.
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    PlayerState,
    PlayerPlay {
        paths: Vec<PathBuf>,
        start: usize,
    },
    PlayerAppend {
        paths: Vec<PathBuf>,
    },
    LibraryFolders,
    LibraryTracks,
    PlaylistsList,
    PlaylistsRead {
        name: String,
    },
    PlaylistsSave {
        name: String,
        paths: Vec<PathBuf>,
    },
    PlaylistsAdd {
        name: String,
        paths: Vec<PathBuf>,
    },
    /// Queue rows as (index, the path the plugin saw there), in the order given.
    PlayerRemove {
        tracks: Vec<(usize, PathBuf)>,
    },
    /// The rows go, in the order given, in front of the row now at index `to`.
    PlayerMove {
        tracks: Vec<(usize, PathBuf)>,
        to: usize,
    },
    PlayerClearUpcoming,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    Call(Call),
    /// `panel.set` carries its raw params (`panel`, `place`); it needs no permission.
    PanelSet(Value),
}

fn field<'a>(params: &'a Value, key: &str) -> Result<&'a Value, RpcError> {
    params
        .get(key)
        .ok_or_else(|| RpcError::bad_params(format!("missing field: {key}")))
}

fn string_field(params: &Value, key: &str) -> Result<String, RpcError> {
    field(params, key)?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| RpcError::bad_params(format!("{key} must be a string")))
}

fn paths_field(params: &Value, key: &str) -> Result<Vec<PathBuf>, RpcError> {
    let bad = || RpcError::bad_params(format!("{key} must be an array of strings"));
    field(params, key)?
        .as_array()
        .ok_or_else(bad)?
        .iter()
        .map(|p| p.as_str().map(PathBuf::from).ok_or_else(bad))
        .collect()
}

fn index(v: Option<&Value>, what: &str) -> Result<usize, RpcError> {
    v.and_then(Value::as_u64)
        .and_then(|i| usize::try_from(i).ok())
        .ok_or_else(|| RpcError::bad_params(format!("{what} must be a non-negative integer")))
}

/// `tracks`: a non-empty array of `{ "index", "path" }`, no index twice.
fn tracks_field(params: &Value) -> Result<Vec<(usize, PathBuf)>, RpcError> {
    let bad = || RpcError::bad_params("tracks must be an array of {index, path}");
    let rows = field(params, "tracks")?.as_array().ok_or_else(bad)?;
    if rows.is_empty() {
        return Err(RpcError::bad_params("tracks must not be empty"));
    }
    let mut tracks: Vec<(usize, PathBuf)> = Vec::with_capacity(rows.len());
    for row in rows {
        let i = index(row.get("index"), "index")?;
        let path = row.get("path").and_then(Value::as_str).ok_or_else(bad)?;
        if tracks.iter().any(|(t, _)| *t == i) {
            return Err(RpcError::bad_params(format!(
                "index {i} is in tracks twice"
            )));
        }
        tracks.push((i, PathBuf::from(path)));
    }
    Ok(tracks)
}

pub fn parse_call(method: &str, params: Value) -> Result<Parsed, RpcError> {
    // Methods that take parameters need an object. Methods without parameters accept
    // any, and an unknown method stays -32601 whatever its params.
    if matches!(
        method,
        "player.play"
            | "player.append"
            | "player.remove"
            | "player.move"
            | "playlists.read"
            | "playlists.save"
            | "panel.set"
    ) && !params.is_object()
    {
        return Err(RpcError::bad_params("params must be an object"));
    }
    let call = match method {
        "player.state" => Call::PlayerState,
        "player.play" => {
            let paths = paths_field(&params, "paths")?;
            if paths.is_empty() {
                return Err(RpcError::bad_params("paths must not be empty"));
            }
            let start = match params.get("start") {
                None | Some(Value::Null) => 0,
                Some(s) => s
                    .as_u64()
                    .and_then(|s| usize::try_from(s).ok())
                    .ok_or_else(|| RpcError::bad_params("start must be a non-negative integer"))?,
            };
            if start >= paths.len() {
                return Err(RpcError::bad_params("start is past the end of paths"));
            }
            Call::PlayerPlay { paths, start }
        }
        // Empty `paths` is allowed on purpose for append and save (a no-op and an
        // empty playlist).
        "player.append" => Call::PlayerAppend {
            paths: paths_field(&params, "paths")?,
        },
        "player.remove" => Call::PlayerRemove {
            tracks: tracks_field(&params)?,
        },
        // Whether `to` is past the end of the queue is for the app to say.
        "player.move" => Call::PlayerMove {
            tracks: tracks_field(&params)?,
            to: index(Some(field(&params, "to")?), "to")?,
        },
        "player.clear_upcoming" => Call::PlayerClearUpcoming,
        "library.folders" => Call::LibraryFolders,
        "library.tracks" => Call::LibraryTracks,
        "playlists.list" => Call::PlaylistsList,
        "playlists.read" => Call::PlaylistsRead {
            name: string_field(&params, "name")?,
        },
        "playlists.save" => Call::PlaylistsSave {
            name: string_field(&params, "name")?,
            paths: paths_field(&params, "paths")?,
        },
        "playlists.add" => Call::PlaylistsAdd {
            name: string_field(&params, "name")?,
            paths: paths_field(&params, "paths")?,
        },
        "panel.set" => {
            field(&params, "panel")?;
            return Ok(Parsed::PanelSet(params));
        }
        other => return Err(RpcError::unknown_method(other)),
    };
    Ok(Parsed::Call(call))
}

pub fn required(call: &Call) -> Permission {
    match call {
        Call::PlayerState
        | Call::PlayerPlay { .. }
        | Call::PlayerAppend { .. }
        | Call::PlayerRemove { .. }
        | Call::PlayerMove { .. }
        | Call::PlayerClearUpcoming => Permission::Playback,
        Call::LibraryFolders | Call::LibraryTracks => Permission::Library,
        Call::PlaylistsList
        | Call::PlaylistsRead { .. }
        | Call::PlaylistsSave { .. }
        | Call::PlaylistsAdd { .. } => Permission::Playlists,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A valid notification padded to exactly `len` bytes.
    fn padded(len: usize) -> Vec<u8> {
        let head = br#"{"jsonrpc":"2.0","method":"m","params":""#;
        let tail = br#""}"#;
        let mut v = head.to_vec();
        v.resize(len - tail.len(), b'x');
        v.extend_from_slice(tail);
        assert_eq!(v.len(), len);
        v
    }

    #[test]
    fn size_limit_counts_content_not_the_terminator() {
        let at = padded(MAX_LINE);
        assert!(decode(&at).is_ok());
        let mut nl = at.clone();
        nl.push(b'\n');
        assert!(decode(&nl).is_ok());
        let mut crlf = at;
        crlf.extend_from_slice(b"\r\n");
        assert!(decode(&crlf).is_ok());
        let over = padded(MAX_LINE + 1);
        assert_eq!(decode(&over).unwrap_err().code, -32700);
        let mut over_nl = over;
        over_nl.push(b'\n');
        assert_eq!(decode(&over_nl).unwrap_err().code, -32700);
    }

    #[test]
    fn encoded_messages_are_a_single_line() {
        let l = encode_request(1, "m", json!({"s": "a\nb"}));
        assert_eq!(l.matches('\n').count(), 1);
        assert!(l.ends_with('\n'));
        match decode(l.as_bytes()).unwrap() {
            Incoming::Request { params, .. } => assert_eq!(params["s"], "a\nb"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn positional_array_params_are_32602() {
        for (m, p) in [
            ("player.play", json!([["/a"], 0])),
            ("playlists.read", json!(["x"])),
            ("panel.set", json!([1])),
            ("player.append", json!(null)),
        ] {
            assert_eq!(parse_call(m, p).unwrap_err().code, -32602, "{m}");
        }
        assert!(parse_call("player.state", json!(null)).is_ok());
    }

    #[test]
    fn unknown_method_stays_32601_with_any_params() {
        for p in [json!(null), json!([1]), json!(5), json!({})] {
            assert_eq!(parse_call("nope", p).unwrap_err().code, -32601);
        }
    }

    #[test]
    fn a_bare_trailing_cr_is_not_a_terminator() {
        let mut at = padded(MAX_LINE);
        at.push(b'\r');
        assert_eq!(decode(&at).unwrap_err().code, -32700);
    }

    #[test]
    fn empty_play_paths_say_so() {
        let e = parse_call("player.play", json!({"paths": []})).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str()),
            (-32602, "paths must not be empty")
        );
        assert!(parse_call("player.append", json!({"paths": []})).is_ok());
        assert!(parse_call("playlists.save", json!({"name": "x", "paths": []})).is_ok());
    }

    #[test]
    fn decode_rejects_malformed_json_and_wrong_version() {
        for bad in [
            &b"{not json"[..],
            br#"{"jsonrpc":"1.0","id":1,"method":"x"}"#,
            br#"{"id":1,"method":"x"}"#,
            &[0xff, 0xfe][..],
            br#"{"jsonrpc":"2.0","id":1}"#,
            br#"{"jsonrpc":"2.0","id":1,"result":1,"error":{"code":1,"message":"m"}}"#,
        ] {
            assert_eq!(decode(bad).unwrap_err().code, -32700, "{bad:?}");
        }
    }

    #[test]
    fn a_null_id_is_an_invalid_request_not_a_notification() {
        assert_eq!(
            decode(br#"{"jsonrpc":"2.0","id":null,"method":"player.play","params":{}}"#).unwrap(),
            Incoming::InvalidRequest
        );
        // Without an id at all it is still a notification.
        assert!(matches!(
            decode(br#"{"jsonrpc":"2.0","method":"player.play","params":{}}"#).unwrap(),
            Incoming::Notification { .. }
        ));
        // A response with a null id is still malformed.
        assert_eq!(
            decode(br#"{"jsonrpc":"2.0","id":null,"result":{}}"#)
                .unwrap_err()
                .code,
            -32700
        );
    }

    #[test]
    fn encode_then_decode_round_trips_a_request() {
        let s = encode_request(7, "player.state", json!({}));
        assert!(s.ends_with('\n'));
        assert_eq!(
            decode(s.as_bytes()).unwrap(),
            Incoming::Request {
                id: json!(7),
                method: "player.state".into(),
                params: json!({})
            }
        );
        let n = encode_notification("initialized", json!(null));
        assert!(matches!(
            decode(n.as_bytes()).unwrap(),
            Incoming::Notification { method, .. } if method == "initialized"
        ));
    }

    #[test]
    fn responses_round_trip_ok_and_error() {
        let ok = encode_response(&json!(3), &Ok(json!({"a": 1})));
        assert_eq!(
            decode(ok.as_bytes()).unwrap(),
            Incoming::Response {
                id: json!(3),
                result: Ok(json!({"a": 1}))
            }
        );
        let e = RpcError::not_found("gone", Some(json!(["x"])));
        let err = encode_response(&json!(4), &Err(e.clone()));
        assert_eq!(
            decode(err.as_bytes()).unwrap(),
            Incoming::Response {
                id: json!(4),
                result: Err(e)
            }
        );
    }

    #[test]
    fn unknown_method_is_32601() {
        assert_eq!(parse_call("nope", json!({})).unwrap_err().code, -32601);
    }

    #[test]
    fn every_method_maps_to_its_permission() {
        let p =
            json!({"paths": ["/a"], "name": "n", "tracks": [{"index": 1, "path": "/a"}], "to": 1});
        let table = [
            ("player.state", Permission::Playback),
            ("player.play", Permission::Playback),
            ("player.append", Permission::Playback),
            ("library.folders", Permission::Library),
            ("library.tracks", Permission::Library),
            ("playlists.list", Permission::Playlists),
            ("playlists.read", Permission::Playlists),
            ("playlists.save", Permission::Playlists),
            ("playlists.add", Permission::Playlists),
            ("player.remove", Permission::Playback),
            ("player.move", Permission::Playback),
            ("player.clear_upcoming", Permission::Playback),
        ];
        for (m, perm) in table {
            let Parsed::Call(c) = parse_call(m, p.clone()).unwrap() else {
                panic!("{m}");
            };
            assert_eq!(required(&c), perm, "{m}");
        }
    }

    #[test]
    fn play_with_start_past_the_end_is_32602() {
        let e = parse_call("player.play", json!({"paths": ["/a"], "start": 1})).unwrap_err();
        assert_eq!(e.code, -32602);
        let e = parse_call("player.play", json!({"paths": []})).unwrap_err();
        assert_eq!(e.code, -32602);
        let ok = parse_call("player.play", json!({"paths": ["/a", "/b"]})).unwrap();
        assert_eq!(
            ok,
            Parsed::Call(Call::PlayerPlay {
                paths: vec!["/a".into(), "/b".into()],
                start: 0
            })
        );
    }

    #[test]
    fn parses_playlists_add() {
        let c = parse_call("playlists.add", json!({"name": "Mix", "paths": ["/a"]})).unwrap();
        assert_eq!(
            c,
            Parsed::Call(Call::PlaylistsAdd {
                name: "Mix".into(),
                paths: vec!["/a".into()]
            })
        );
        assert!(parse_call("playlists.add", json!({"name": "Mix", "paths": []})).is_ok());
    }

    #[test]
    fn playlists_add_needs_the_playlists_permission() {
        let Parsed::Call(c) =
            parse_call("playlists.add", json!({"name": "n", "paths": ["/a"]})).unwrap()
        else {
            panic!()
        };
        assert_eq!(required(&c), Permission::Playlists);
    }

    #[test]
    fn playlists_add_rejects_missing_paths() {
        assert_eq!(
            parse_call("playlists.add", json!({"name": "n"}))
                .unwrap_err()
                .code,
            -32602
        );
        assert_eq!(
            parse_call("playlists.add", json!({"paths": []}))
                .unwrap_err()
                .code,
            -32602
        );
    }

    #[test]
    fn missing_or_wrong_typed_fields_are_32602() {
        for (m, p) in [
            ("player.append", json!({})),
            ("player.append", json!({"paths": [1]})),
            ("playlists.read", json!({"name": 5})),
            ("playlists.save", json!({"name": "n"})),
            ("panel.set", json!({})),
        ] {
            assert_eq!(parse_call(m, p).unwrap_err().code, -32602, "{m}");
        }
    }

    #[test]
    fn parses_queue_edit_calls() {
        let one = json!([{"index": 2, "path": "/a"}]);
        assert_eq!(
            parse_call("player.remove", json!({"tracks": one})).unwrap(),
            Parsed::Call(Call::PlayerRemove {
                tracks: vec![(2, "/a".into())]
            })
        );
        let two = json!([{"index": 3, "path": "/b"}, {"index": 2, "path": "/a"}]);
        assert_eq!(
            parse_call("player.move", json!({"tracks": two, "to": 1})).unwrap(),
            Parsed::Call(Call::PlayerMove {
                tracks: vec![(3, "/b".into()), (2, "/a".into())],
                to: 1
            })
        );
        for p in [json!(null), json!({})] {
            assert_eq!(
                parse_call("player.clear_upcoming", p).unwrap(),
                Parsed::Call(Call::PlayerClearUpcoming)
            );
        }
    }

    #[test]
    fn queue_edit_params_are_checked() {
        let a = json!({"index": 1, "path": "/a"});
        for (m, p) in [
            ("player.remove", json!({})),
            ("player.remove", json!(null)),
            ("player.remove", json!({"tracks": "/a"})),
            ("player.remove", json!({"tracks": []})),
            ("player.remove", json!({"tracks": [{"index": 1}]})),
            ("player.remove", json!({"tracks": [{"path": "/a"}]})),
            (
                "player.remove",
                json!({"tracks": [{"index": -1, "path": "/a"}]}),
            ),
            (
                "player.remove",
                json!({"tracks": [{"index": 1.5, "path": "/a"}]}),
            ),
            (
                "player.remove",
                json!({"tracks": [a, {"index": 1, "path": "/b"}]}),
            ),
            ("player.move", json!({"tracks": [a]})),
            ("player.move", json!({"tracks": [a], "to": "x"})),
            ("player.move", json!({"tracks": [a], "to": -1})),
            ("player.move", json!({"tracks": [], "to": 1})),
        ] {
            assert_eq!(
                parse_call(m, p.clone()).unwrap_err().code,
                -32602,
                "{m} {p}"
            );
        }
    }

    #[test]
    fn queue_changed_error_has_its_reason() {
        let e = RpcError::queue_changed("started");
        assert_eq!(
            (e.code, e.message.as_str(), e.data),
            (-32004, "queue changed", Some(json!({"reason": "started"})))
        );
    }

    #[test]
    fn panel_set_is_parsed_without_a_permission() {
        let params = json!({"panel": [{"type": "label", "text": "hi"}], "place": "settings"});
        assert_eq!(
            parse_call("panel.set", params.clone()).unwrap(),
            Parsed::PanelSet(params)
        );
    }

    #[test]
    fn denied_names_the_permission() {
        assert!(
            RpcError::denied(Permission::Library)
                .message
                .contains("library")
        );
        assert_eq!(
            serde_json::to_string(&Permission::Playlists).unwrap(),
            "\"playlists\""
        );
    }
}

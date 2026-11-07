# nil plugin API (version 1)

A nil plugin is a separate program that nil starts and talks to. The plugin
can read your library, control playback, save playlists and show a panel
inside nil. It can be written in any language; this document is the whole
contract.

- [Installing a plugin](#installing-a-plugin)
- [The manifest](#the-manifest)
- [Permissions and consent](#permissions-and-consent)
- [How nil runs your program](#how-nil-runs-your-program)
- [The protocol](#the-protocol)
- [Handshake](#handshake)
- [Methods your plugin can call](#methods-your-plugin-can-call)
- [Notifications from nil](#notifications-from-nil)
- [Panels](#panels)
- [Limits](#limits)
- [Statuses and restarts](#statuses-and-restarts)
- [Walk-through: the hello example](#walk-through-the-hello-example)

## Installing a plugin

Plugins live in the `plugins` folder next to `nil.exe`, one sub-folder each:

```
plugins\
  hello\
    plugin.toml
    hello.py
```

nil looks for plugins when it starts. A new plugin is **off**. Open
Settings, then Plugins, read the permissions it asks for and switch it on.
A plugin may keep its own files in its folder; nil creates `data\` inside it
for that purpose.

## The manifest

`plugins\<name>\plugin.toml`:

```toml
name = "hello"                        # must equal the folder name
version = "0.1.0"
description = "Greets you and shows ten random tracks from your library"
api_version = 1
command = ["python", "hello.py"]
permissions = ["library", "playback"]
```

All six fields are required. `title` is optional.

| Field | Meaning |
|---|---|
| `name` | Letters, digits, `.`, `_` and `-` only. Must be the same as the folder name. |
| `version` | Your plugin's version; shown in Settings. |
| `description` | One line, shown in Settings. |
| `api_version` | The protocol version this plugin speaks. This document describes `1`. A plugin with any other value is listed as *Incompatible* and never started. |
| `command` | Program and arguments. See below. Must not be empty. |
| `permissions` | Any of `"playback"`, `"library"`, `"playlists"`. May be empty. |
| `title` | Optional. The name shown on the plugin's top-bar tab and in Settings, e.g. `"DJ"`; one line, 1–24 characters. Without it, `name` is shown. |

**Resolving `command[0]`.** If it contains `/` or `\`, or starts with `.`, it
is taken relative to the plugin folder (for example
`".venv\\Scripts\\python.exe"`). An absolute path is used as it is. Any other
name, such as `python`, is looked up on `PATH`. The arguments are passed
unchanged. The working directory is always the plugin folder, so
`"hello.py"` above finds the script next to the manifest.

An unreadable or invalid manifest is shown in Settings as *Invalid manifest*
with the reason.

## Permissions and consent

| Permission | Allows |
|---|---|
| `playback` | `player.state`, `player.play`, `player.append`, `player.remove`, `player.move`, `player.clear_upcoming`, and receiving `player.changed`. |
| `library` | `library.folders`, `library.tracks`. |
| `playlists` | `playlists.list`, `playlists.read`, `playlists.save`, `playlists.add`. |

`panel.set` needs no permission.

A plugin is an ordinary program running as you. Permissions limit what it can
do through nil, not what it can do on your PC. Nothing stops a plugin from
reading your files or using the network, so only switch on plugins you trust.
Settings shows this warning before a plugin is switched on.

The owner approves the permissions in the manifest when switching the plugin
on. nil only starts a plugin that is switched on and has every permission its
manifest declares approved. If a new version of a plugin declares an extra
permission, the plugin stays stopped until the owner approves again. A call
that needs a permission the plugin lacks is answered with error `-32001` and
is not carried out.

## How nil runs your program

- **Start.** nil starts the process with the plugin folder as its working
  directory, and sends `initialize` straight away.
- **stdin and stdout** carry the protocol, one message per line. Write
  nothing else to stdout, and flush after every line.
- **stderr** is free-form text for you. The last 20 lines show in Settings,
  and all of it is written to `data\logs\plugins\<name>.log` next to
  `nil.exe`.
- **Children.** On Windows every plugin runs inside a Job Object. When nil
  stops the plugin, or nil itself ends (even if it is killed), everything the
  plugin started is ended too. Elsewhere nil ends the plugin's process group.
- **Stopping.** When the plugin is switched off, restarted, or nil quits, nil
  sends the `shutdown` request, waits up to **3 seconds** for the process to
  exit, then ends it and everything it started. Reply to `shutdown` and exit.
  From the moment nil decides to stop the plugin, requests the plugin sends
  get error `-32603` (`"plugin is stopping"`) and are not carried out, and
  notifications from it are ignored.

## The protocol

[JSON-RPC 2.0](https://www.jsonrpc.org/specification) over stdio. Each
message is one JSON object on one line, UTF-8, ending in `\n` (`\r\n` is also
accepted). The JSON itself must not contain a raw newline (newlines inside
strings are written `\n`, as every JSON encoder does).

Lines are UTF-8 in both directions, whatever the system's code page. Many
runtimes default to the local encoding on Windows (Python uses cp1252), which
garbles non-ASCII titles and paths, so set UTF-8 explicitly on stdin and
stdout (in Python: `sys.stdin.reconfigure(encoding="utf-8")` and the same for
`sys.stdout`).

- Requests have an `id`, which is a number or a string. Always send one; do
  not use `"id": null`. nil answers a request with `"id": null` with error
  `-32600` (and `"id": null` in the reply) and does not run it.
- A message without an `id` is a notification and gets no reply.
- Parameters are always an object (`{}` when there are none).
- A line that is not valid JSON-RPC, or is too long, is ignored. nil writes
  one note about it to the plugin's log and sends no reply.
- Methods nil does not know give error `-32601`.

An error reply looks like:

```json
{"jsonrpc":"2.0","id":7,"error":{"code":-32002,"message":"not in nil's library: 1 path(s)","data":{"paths":["E:\\Music\\x.flac"]}}}
```

### Error codes

| Code | Meaning |
|---|---|
| `-32600` | Invalid request: the `id` is `null`. |
| `-32601` | Unknown method. |
| `-32602` | Invalid params (missing or wrong-typed field, bad panel). `message` says which. |
| `-32603` | Internal error, for example a playlist file could not be read or written. `message` has the reason. |
| `-32001` | Permission denied. `message` names the permission needed. |
| `-32002` | Not found: a path that is not in nil's library (`data.paths` lists every unknown path, in the order sent), or a playlist that does not exist. |
| `-32003` | Busy: `"no audio device"`, when `player.play` or `player.append` is called while no output device is selected. |
| `-32004` | `"queue changed"`: a queue edit was refused. `data.reason` is `"stopped"` (nothing is playing), `"changed"` (an index is past the end of the queue, or its path does not match the track there; compared ignoring case and slash direction) or `"started"` (an index, or `to`, is at or before the current track, including the next track during its last roughly 2 seconds, which the decoder has already taken). Read `player.state` again and decide. |

## Handshake

1. nil sends the request `initialize`:

   ```json
   {"jsonrpc":"2.0","id":1,"method":"initialize","params":{
     "api_version":1,"nil_version":"0.3.0",
     "granted":["library","playback"],
     "data_dir":"E:\\nil\\plugins\\hello\\data"}}
   ```

   `granted` lists the permissions approved. `data_dir` is a folder for the
   plugin's own files; it exists already.

2. The plugin replies with any result, for example `{}`. This must arrive
   within **10 seconds**.
3. nil sends the notification `initialized`. The plugin is now *Running*
   and may call methods. Calls made before `initialized` are not guaranteed
   to be answered.

An error reply to `initialize`, or no reply in time, counts as a failed start.
Exiting before replying counts as a crash. See
[Statuses and restarts](#statuses-and-restarts).

## Methods your plugin can call

Paths are absolute file paths, as returned by `library.tracks`. On Windows
they are compared ignoring case and the direction of slashes. A track must be
in nil's library to be played, queued or saved.

| Method | Permission | Params | Result | Errors |
|---|---|---|---|---|
| `player.state` | playback | none | `{ "state": "playing"\|"paused"\|"stopped", "queue": [path, ...], "index"?: n, "position_s"?: seconds, "path"?: path }`. `index`, `position_s` and `path` are present only when something is playing or paused. | |
| `player.play` | playback | `{ "paths": [path, ...], "start"?: n }`. Non-empty; `start` (default 0) is the index in `paths` to begin with. | `{}`. Replaces the queue and starts playing. | `-32602` (empty `paths`, `start` out of range), `-32002`, `-32003` |
| `player.append` | playback | `{ "paths": [path, ...] }`. May be empty (does nothing). | `{}`. Adds the tracks to the end of the queue. If nothing is playing, they start playing. | `-32602` (`paths` missing or not an array of strings), `-32002`, `-32003` |
| `player.remove` | playback | `{ "tracks": [ {"index": n, "path": path}, ... ] }`. Non-empty; each track is named by its queue index and the path seen there. | `{}`. Removes those tracks from the queue; the current and played tracks cannot be removed. | `-32602`, `-32004` |
| `player.move` | playback | `{ "tracks": [ {"index": n, "path": path}, ... ], "to": n }`. The tracks go, in the order given, in front of the track now at index `to`. `to` = current index + 1 means play next; `to` = `queue_len` means the end. | `{}`. Never restarts playback. | `-32602`, `-32004` |
| `player.clear_upcoming` | playback | none | `{ "removed": n }`. Removes every track after the current one (`0` when there are none). While the next track is starting it clears after it; when another edit changed the queue first, it clears what is upcoming now. nil retries once for either reason. | `-32004`: `"stopped"`, or `"started"` or `"changed"` when the queue moved again during that one retry |
| `library.folders` | library | none | `[path, ...]` the music folders. | |
| `library.tracks` | library | none | `[ { "path", "title", "artist", "album", "duration_s" }, ... ]` for every track, in library order (album, disc, number). `artist` is the album's artist. `duration_s` is `null` when the length is not known. | |
| `playlists.list` | playlists | none | `[name, ...]` | `-32603` |
| `playlists.read` | playlists | `{ "name": string }` | `[ { "path", "title", "artist", "duration_s", "missing" }, ... ]`. `missing` is true when the file is gone from disk. `duration_s` is `null` when unknown. | `-32602` (`name` missing or not a string), `-32002` (no such playlist), `-32603` |
| `playlists.save` | playlists | `{ "name": string, "paths": [path, ...] }`. `paths` may be empty. | `{ "name": stored_name }`. Never replaces a playlist: a taken name gets the first free ` (2)`, ` (3)`, … suffix. Characters Windows does not allow in file names become `_`, so `stored_name` can differ from `name`. | `-32602` (`name` or `paths` missing or wrong-typed), `-32002`, `-32603` |
| `playlists.add` | playlists | `{ "name": string, "paths": [path, ...] }`. `paths` may be empty. | `{ "added": n, "skipped": n }`. Appends the tracks to the existing playlist, skipping paths already in it. Never removes or reorders anything. | `-32602` (`name` or `paths` missing or wrong-typed), `-32002` (a path not in nil's library, or no such playlist; never creates one), `-32603` |
| `panel.set` | none | `{ "panel": { "elements": [...] } }`, see [Panels](#panels) | `{}` | `-32602` |

### Editing the queue

`player.remove`, `player.move` and `player.clear_upcoming` edit the tracks after
the current one. Name each track by its `index` in the queue (as in
`player.state`) and the `path` you saw there; if either no longer matches,
nothing is changed and the call fails with `-32004`.

- `-32602`: `tracks` missing, empty or not an array; an item without `index`
  or `path`; an index that is negative, not an integer, or given twice; `to`
  missing or negative, or greater than `queue_len`.
- The reply is deferred: it comes once the player applies or refuses the edit,
  so it can arrive a moment after the call. If playback stops first, the call
  gets `-32004` with `"stopped"`.
- `player.remove` and `player.clear_upcoming` change `queue_len` and so fire
  `player.changed`. `player.move` does not; call `player.state` to read the
  new order.

Needs nil 0.3.0; older nil answers `-32601`.

### Keeping playback gapless

`player.append` keeps playback gapless when the new tracks are formatted for
the same output as the current track and the call arrives before the last
roughly 2 seconds of the current track. Appending later may leave a gap. To
keep a queue topped up, append when the second-to-last track starts: that is
the `player.changed` whose `index` equals `queue_len - 2`.

## Notifications from nil

| Method | Params | When |
|---|---|---|
| `initialized` | `{}` | Once, after the handshake. |
| `player.changed` | `{ "state": "playing"\|"paused"\|"stopped", "queue_len": n, "index"?: n, "path"?: path }` | Only to plugins with the `playback` permission. Sent when the state, the current track or the length of the queue changes. Never sent for the playback position. |
| `panel.event` | `{ "id": string, "value"?: ... }` | The user used an element of your panel. See [Panels](#panels). |

nil does not send an initial `player.changed`. A plugin that wants the
current state calls `player.state` after `initialized`.

Notifications reach the plugin only while it is running. Each plugin has a
queue of 1024 pending notifications; if the plugin stops reading and the
queue fills up, further notifications are dropped (nil logs one line).
Replies to your requests and the `shutdown` request are never dropped.

## Panels

A plugin shows a panel by calling `panel.set`. The plugin gets its own tab in
nil's top bar while it has a panel. Each call replaces the whole panel; the
panel disappears when the plugin stops. nil draws the elements in its own
theme, under the tab's title: the manifest `title` (or `name`).

```json
{"panel": {"elements": [
  {"type": "heading", "text": "Hello"},
  {"type": "row", "children": [
    {"type": "input", "id": "name", "placeholder": "Your name", "submit_label": "Greet"},
    {"type": "button", "id": "shuffle", "label": "Shuffle"}
  ]}
]}}
```

Every element is an object with a `"type"`. `id` values must be unique within
the whole panel. Any breach of the rules below is answered with `-32602` and
the old panel stays.

| Type | Fields | Event |
|---|---|---|
| `heading` | `text` (required), `muted` (bool, default false) | none |
| `text` | `text` (required), `muted` (bool, default false) | none |
| `input` | `id`, `placeholder`, `submit_label` (all required), `value` (string, default empty) | `panel.event {id, value}` when the user submits, where `value` is the text typed |
| `button` | `id`, `label` (required), `enabled` (bool, default true) | `panel.event {id}` when clicked |
| `toggle` | `id`, `label`, `value` (bool; all required); drawn as a switch | `panel.event {id, value}` with the new state |
| `progress` | `value` (required: a number, clamped to 0 to 1, or `null` for busy), `label` (string, default empty) | none |
| `tracks` | `items` (required): `[ { "path", "title"?, "artist"?, "duration_s"? }, ... ]`; `label` (string, optional): drawn uppercased above the rows | none; see below |
| `row` | `children` (required): a list of elements laid out side by side. Rows may be nested up to 4 deep. | none |

An element with an unknown `type` is shown as "unsupported element" and does
no harm, so newer panels degrade gracefully.

In `tracks`, `path` is required. `title` falls back to the file name without its
extension, `artist` must be a string if present (anything else is `-32602`), and
`duration_s` is left out when it is missing, not a positive number, or
`null`. Rows look like the Now Playing queue: tracks in nil's library show their
album art, the playing track is highlighted, and double-click plays from a row.
The list has its own **Play all**, **Queue all** and **Save as
playlist** actions, which nil carries out itself: they do not send events and
need no permission.

Text the user has typed into an `input` survives new panels as long as the
input's `id` is still present and its `value` is unchanged. If the plugin
changes `value`, the input restarts from that value; an input whose `id` is no
longer in the panel is dropped. Send a new panel after a `panel.event` if you
want the display to change.

### A panel in Settings

A plugin can also show a second panel inside its entry in **Settings →
Plugins**, below its status and above Restart / Open log / Open folder: a good
place for housekeeping such as an index status or an update button. Send it
with `"place": "settings"`:

```json
{"panel": {"elements": [
  {"type": "text", "text": "2,890 tracks indexed"},
  {"type": "button", "id": "index", "label": "Update index"}
]}, "place": "settings"}
```

`place` is `"tab"` (the default) or `"settings"`; anything else is `-32602`.

- A settings panel may hold `heading`, `text`, `progress`, `button`, `toggle`
  and `row`. A `tracks` or `input` anywhere in it, rows included, is
  `-32602` and the old settings panel stays.
- The tab panel and the settings panel are separate: setting one never
  changes the other, and `id` values need only be unique within each panel.
- A settings panel alone does not give the plugin a tab.
- Both panels are cleared when the plugin stops, crashes or is switched off.
- Events from the settings panel carry `"place": "settings"`, for example
  `panel.event {"id": "index", "place": "settings"}`. Events from the tab carry
  no `place`.

Needs nil 0.2.0, as do `label` on `tracks` and toggles drawn as switches. Older
nil ignores `place`, so a settings panel sent to it replaces the tab panel:
check `nil_version` from `initialize` first.

## Limits

- A message is at most 1 MB (1,048,576 bytes), counting the content of the
  line but not its `\n` or `\r\n`. A longer line is ignored.
- At most 1024 pending notifications per plugin (see above).
- Rows nest at most 4 deep.

## Statuses and restarts

Settings shows each plugin's status:

| Status | Meaning |
|---|---|
| Off | Switched off, or waiting for the owner to approve new permissions. |
| Starting | Process launched, handshake in progress, or waiting to restart after a crash. |
| Running | Handshake done. |
| Crashed | The plugin stopped and nil will not restart it by itself; the reason is shown, with the last stderr lines. |
| Incompatible | The manifest's `api_version` is not 1. |
| Invalid manifest | `plugin.toml` could not be used; the reason is shown. |

**Crashes.** If a running plugin exits (or its stdout closes) without being
asked to, nil restarts it after 1 second. Exiting before the reply to
`initialize` is also a crash, and so is a `command` that cannot be launched
(for example no `python` on `PATH`); the error is written to the plugin's log
and shows in the stderr tail in Settings. After a second crash it restarts
after 5 seconds. A third crash within 5 minutes stops the restarts: the
status becomes *Crashed* and stays so until the owner presses **Restart** or
switches the plugin off and on. Restart and switching on start again with a
clean count.

**Failed starts.** A plugin that cannot finish the handshake (an error reply
to `initialize`, or no reply within 10 seconds) is *Crashed* with
"didn't start" and is not restarted automatically.

## Walk-through: the hello example

`examples/plugins/hello` is a complete plugin in about 80 lines of Python
using only the standard library. Copy the folder into `plugins\`, switch it on
in Settings (it asks for `library` and `playback`), and open its tab.

- `plugin.toml` names the plugin, says it speaks API version 1 and starts it
  with `python hello.py`.
- `send()` writes one JSON line and flushes. Everything the plugin sends goes
  through it.
- The main loop reads one line at a time and passes it to `handle()`.
- On `initialize` the plugin answers `{}`. On `initialized` it calls
  `panel.set` with a heading, a row holding the `name` input and the
  `shuffle` button, and an empty `tracks` list.
- On `panel.event` with `id` `name`, it changes the heading to
  `Hello, <value>` and sends the panel again. With `id` `shuffle`, it calls
  `library.tracks`, picks 10 tracks at random for the list and sends the
  panel again.
- `call()` sends a request and reads lines until the matching reply arrives,
  handling anything else that comes in meanwhile.
- On `shutdown` it replies `{}` and exits, well inside the 3-second grace.
- Diagnostics go to stderr, which nil logs.

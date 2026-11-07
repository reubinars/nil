"""A tiny nil plugin: a greeting and ten random tracks. Standard library only.

nil starts this program with the plugin folder as its working directory and
talks to it over stdin/stdout, one JSON message per line (JSON-RPC 2.0).
Anything written to stderr goes to the plugin's log.
"""
import json
import random
import sys

heading = "Hello from nil"
tracks = []  # what the list on the panel shows
next_id = 100  # ids for the requests we send to nil

# nil speaks UTF-8 both ways; Windows would otherwise use the local code page.
sys.stdin.reconfigure(encoding="utf-8")
sys.stdout.reconfigure(encoding="utf-8", newline="\n")


def send(message):
    """Write one message as one line, and flush so nil sees it now."""
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def reply(request, result):
    send({"jsonrpc": "2.0", "id": request["id"], "result": result})


def call(method, params):
    """Send a request to nil and wait for its answer, handling messages that
    arrive in the meantime. Returns the result, or None if nil refused."""
    global next_id
    next_id += 1
    send({"jsonrpc": "2.0", "id": next_id, "method": method, "params": params})
    for line in sys.stdin:
        message = json.loads(line)
        if message.get("id") == next_id and "method" not in message:
            if "error" in message:
                print(f"{method} failed: {message['error']}", file=sys.stderr)
                return None
            return message["result"]
        handle(message)
    sys.exit(0)  # nil closed our stdin


def show_panel():
    call("panel.set", {"panel": {"elements": [
        {"type": "heading", "text": heading},
        {"type": "row", "children": [
            {"type": "input", "id": "name", "placeholder": "Your name",
             "submit_label": "Greet"},
            {"type": "button", "id": "shuffle", "label": "Ten random tracks"},
        ]},
        {"type": "tracks", "items": tracks},
    ]}})


def shuffle():
    global tracks
    library = call("library.tracks", {}) or []
    tracks = [
        {"path": t["path"], "title": t["title"], "artist": t["artist"],
         "duration_s": t["duration_s"]}
        for t in random.sample(library, min(10, len(library)))
    ]


def handle(message):
    global heading
    method = message.get("method")
    params = message.get("params") or {}
    if method == "initialize":
        reply(message, {})
    elif method == "initialized":
        show_panel()  # nil is ready for requests now
    elif method == "panel.event":
        if params.get("id") == "name":
            heading = f"Hello, {params.get('value', '')}"
        elif params.get("id") == "shuffle":
            shuffle()
        show_panel()
    elif method == "shutdown":
        reply(message, {})
        sys.exit(0)  # nil waits three seconds, then ends the process


for line in sys.stdin:
    handle(json.loads(line))

# dpc-note-taker

A quick and convenient editing buffer for capturing automated notes. The
primary use case is receiving text from external sources — e.g. transcriptions
from [handy.computer](https://handy.computer) — and collecting them in one
place for review and editing.

It provides a GUI editor backed by an in-memory buffer and a Unix socket RPC
interface, so other programs and scripts can programmatically append or prepend
text to the buffer. The `--focus` flag can be
used to automatically raise the window when new text arrives via RPC.

Notes are not persisted to disk — the buffer lives only as long as the GUI
window is open.

## Usage

Open a session (defaults to `--session default`):

```sh
dpc-note-taker
```

Open a named session:

```sh
dpc-note-taker --session my-notes
```

Append text from stdin to a running session (or start a new one):

```sh
echo "hello" | dpc-note-taker --session my-notes append
```

Prepend text:

```sh
echo "header" | dpc-note-taker --session my-notes prepend
```

### RPC options

These flags control behavior when text is appended/prepended via RPC:

- `--focus` — raise the window and grab focus
- `--scroll` — scroll the editor to the inserted text
- `--select` — select the inserted text

```sh
echo "hello" | dpc-note-taker --focus --scroll --select append
```

## How it works

Each session is identified by name and gets a Unix socket at
`$XDG_RUNTIME_DIR/dpc-note-taker/<session>.sock`. When `append` or `prepend`
is invoked:

- If a GUI instance for the session is already running, the text is sent via
  a simple JSON RPC over the socket.
- If no instance is running, a new GUI window opens with the piped text as
  initial content.

The editor is a full multiline text widget — you can freely edit the captured
notes.

## AI usage disclosure

[I use LLMs when working on my projects.](https://dpc.pw/posts/personal-ai-usage-disclosure/)

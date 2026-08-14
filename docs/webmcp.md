# Driving the web client with an agent

The web client publishes its own actions as [WebMCP](https://webmachinelearning.github.io/webmcp/)
tools, so an agent can open a share, walk it, read files, seed them and publish
new ones without installing anything.

That is the point of doing it in a browser. The native path needs the Rust
binary; `npx agent-share` needs the `node-datachannel` addon, because Node has
no `RTCPeerConnection`. A browser already has WebRTC, a sandbox and storage. The
tab becomes the runtime.

Registration lives in `web/src/lib/agentTools/`. It is feature-detected, so on a
browser without WebMCP — which today is every browser by default — it reads one
property and does nothing.

## What you need

- **Chrome 150 or newer**, started with `--enable-features=WebMCP`.
- **`chrome-devtools-mcp`**, which is what actually carries the tools to an
  agent. It launches Chrome itself, so it can turn the feature on for you.

WebMCP is a W3C Community Group draft, not a standard. Chrome is the only
implementation; Edge, Firefox and Safari have nothing. And the spec only covers
*publishing* tools — `getTools()` and `executeTool()` are still marked
`TODO: Spec and describe`, and the explainer assumes an agent built into the
browser. So the page half below is stable, and the connection half is not.

## Setup

```bash
claude mcp add chrome-devtools-webmcp --scope user -- \
  npx -y chrome-devtools-mcp@latest \
  --categoryExperimentalWebmcp \
  --chromeArg=--enable-features=WebMCP
```

Both flags are load-bearing and neither implies the other: `--categoryExperimentalWebmcp`
exposes the two MCP tools, and `--chromeArg=--enable-features=WebMCP` turns on
the browser feature they read. With only the first, the tools exist and always
return an empty list.

If you already have the `chrome-devtools-mcp` plugin installed, add this as a
*separate* entry rather than editing the plugin: its arguments are fixed and a
plugin update would overwrite them.

## Using it

```
navigate_page       https://share.agent-habilis.com/files/<ticket>
list_webmcp_tools   -> the eight tools below
execute_webmcp_tool { toolName: "shareRead", input: "{\"path\":\"src/lib.rs\"}" }
```

The ticket in the page URL is the default for every tool, so once you have
navigated you can call `shareConnect` with no arguments at all.

## The tools

There are two families. The **share tools** work on any route and need nobody
present. The **interface tools** move what a person is looking at, so they need
a share page open — and two of them need that person to click something.

### Share tools

| Tool | Does | Changes state |
|---|---|---|
| `shareConnect` | Open a share; report file count, bytes, transport, peers | no |
| `shareList` | List files and directories, `depth` levels deep | no |
| `shareStat` | Size, mtime, manifest index, and how much is held locally | no |
| `shareRead` | Read a byte window of one file | no |
| `shareSearch` | Find a string across the share's text files | no |
| `shareSync` | Pull files into browser storage; this tab then seeds them | yes |
| `shareStatus` | Transport, peers, how many files are held locally | no |
| `sharePublish` | Publish files as a new share and return a ticket | yes |

### Interface tools

| Tool | Does | Needs a person |
|---|---|---|
| `shareUiState` | What the person is looking at: view, selection, status | no |
| `shareNavigate` | Move the file browser to a folder or file | no |
| `shareOpenView` | Switch between files, info and preview | no |
| `shareSeedSelection` | Seed the selection, as the Seed button does | no |
| `shareDownload` | Download the selection, as the Download button does | **yes** |
| `shareMount` | Mirror into a local folder, as the Mount button does | **yes** |

They fail with `no_session` when no share page is mounted — on `/`, for
instance. That is deliberate: opening one invisibly would move a page nobody
asked about.

`shareDownload` and `shareMount` need `showSaveFilePicker` and
`showDirectoryPicker`, which only a real click can open, and no directory handle
is persisted anywhere so there is nothing to reuse from last time. Driven
unattended they return `needs_user_gesture` with the browser's own words:

```json
{ "ok": false, "code": "needs_user_gesture",
  "error": "The browser refused: Failed to execute 'showSaveFilePicker' … Must be
            handling a user gesture to show a file picker. … ask the person at
            this page to press the button." }
```

That is the honest answer, not a bug. For bulk transfer to a filesystem, use
`agent-share mirror`.

Every result is flat and carries `ok`. A failure is `{ ok: false, code, error }`
with a stable `code` — `not_found`, `unauthorized`, `bad_argument`,
`not_a_file`, `unsupported`, `failed`.

### Reading a file

`shareRead` is windowed. It returns UTF-8 `text`, or `data` as base64 when the
bytes are binary, plus `size`, `eof` and `nextOffset`. Follow `nextOffset` until
`eof` is true:

```json
{ "ok": true, "path": "src/lib.rs", "encoding": "utf8", "text": "fn main() {…",
  "offset": 0, "length": 65536, "size": 210433, "eof": false, "nextOffset": 65536 }
```

The default window is 64 KiB and the maximum is 256 KiB, which is the protocol's
own cap on a READ. Bulk transfer is deliberately not a tool: the browser cannot
write to your filesystem without a person clicking a folder picker, so moving a
large tree is still `agent-share mirror`'s job.

### Publishing

`sharePublish` writes into the origin private file system and serves it with the
same producer the **Add folder** button uses, so no user gesture is involved and
the result is an ordinary share a native peer can read:

```
sharePublish { files: [{ path: "notes.md", content: "# hi\n" }] }
  -> { ticket: "SvL6pH…", url: "https://…/files/SvL6pH…", files: 1 }

agent-share mirror SvL6pH… ./out     # works from the CLI
```

It cannot share a folder that already exists on your disk — that needs
`showDirectoryPicker()` and a real click. And the share lives only as long as
the tab: the bytes sit in per-origin browser storage, under that origin's quota
and subject to eviction.

## Telling the user an agent is here

The name of the app in the top bar carries it. **agent-share** turns green the
first time any tool is called and stays green; while a call is running, or for
ten seconds after one, its characters shimmer between full and dimmed green.
Hovering gives the sentence — which tool, how many times, how long ago — and
before anything has happened it says how many tools are published, or that this
browser has no WebMCP at all. See `web/src/components/Brand`.

The wording is careful, because **the thing you would want to show cannot be
observed**. WebMCP lets a page publish tools; it never tells the page that
something has connected to them, and the spec has no notion of an agent session
at all. `getTools()` is a *caller's* API — a page calling it learns about its own
tools, not about who else is looking. A tab whose tools nobody has ever called
is indistinguishable from a tab no agent has found.

So the colour is driven by the only real evidence, a tool actually being
invoked, and it never claims more than that. The name is untouched until the
first call, and once the active window lapses the tooltip drops the present
tense and says outright that the agent may no longer be attached. Do not
"improve" it into a connected/disconnected indicator; there is nothing to drive
one with.

Two states rather than three, and green never decays back to grey: the page
cannot un-know that an agent was here, so only the movement is allowed to stop.

## Browser behaviour worth knowing

Measured against Chrome 151, and the reason parts of the implementation look the
way they do. Some of it contradicts the published examples.

- **`document.modelContext`, not `navigator.modelContext`.** The `navigator`
  alias still exists but is deprecated as of Chrome 150 — the same version that
  the WebMCP tooling requires.
- **`executeTool` takes a `RegisteredTool`, not a name.** Get the object from
  `getTools()` first. Passing a string throws
  `The provided value is not of type 'RegisteredTool'`.
- **Its `input` must be a JSON string.** An object throws
  `Failed to parse input arguments`.
- **Input is never validated against `inputSchema`.** A call omitting a
  `required` field runs anyway, with that field `undefined`. Every tool
  therefore checks its own arguments.
- **A throw is flattened.** Whatever a tool throws reaches the agent as
  `UnknownError: Tool was executed but the invocation failed`, with the message
  stripped. So failures are returned as data instead — hence `ok: false`.
- **Return values are serialized to a string.** Returning a plain object is
  fine; it arrives JSON-encoded.
- **Registration is per-call and asynchronous.** The tools are registered
  concurrently, because until the last one lands `getTools()` returns a partial
  list and says nothing about being incomplete.
- **Unregistering is only possible through an `AbortSignal`** given at
  registration time.

## Checking it works

Without any agent, open the page in Chrome 150+ with the feature flag and look
at **DevTools → Application → WebMCP**. *Available Tools* lists what the page
published; *Invoked Tools* logs each call with its input and output.

From the console:

```js
const tools = await document.modelContext.getTools()
await document.modelContext.executeTool(
  tools.find(t => t.name === 'shareList'), '{}')
```

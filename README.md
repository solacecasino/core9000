# core9000

The shared core of the 9000 series. The series is five sister apps:

| App | What it is |
|---|---|
| [text9000](https://github.com/solacecasino/text9000) | Writing app |
| [splice9000](https://github.com/solacecasino/splice9000) | Video editor |
| [wave9000](https://github.com/solacecasino/wave9000) | Audio workstation |
| [grace9000](https://github.com/solacecasino/grace9000) | Photo editor |
| [leaf9000](https://github.com/solacecasino/leaf9000) | Comic reader |

What's the same in all five is written once, here, so a fix lands in every
sister.

| Module | What it is | Used by |
|---|---|---|
| `shell` | The window, drawn by the CPU (winit + egui): no graphics driver loaded. Only the parts that changed are redrawn. | text9000, splice9000, wave9000, grace9000 |
| `adapters` | The adapter host: `adapter.toml`, programs speaking JSON lines, approvals, installing a dropped zip, removing and adding back built-in adapters. Generic over each app's own `adapter.toml` sections. | splice9000, wave9000, grace9000 |
| `family` | The family protocol: the messages every sister speaks to adapters. | all five |
| `sisters` | The five apps, which files each opens, and handing a file to the sister that opens it. | all five |
| `watch`, `incoming`, `log` | Config folders watched for changes, files macOS hands over, and the Log panel's lines. | the egui sisters; every sister's log takes the core's lines |

## The family protocol

Every sister talks to its adapters the same way:

- **Messages:** JSON, one per line, over stdin and stdout.
- **From the app:** `initialize`, `command`, `event` and `shutdown`.
- **From the adapter:**
  - `log`, `notify`
  - `panel.set`
  - `ui.confirm`, `ui.prompt`, `ui.save_dialog`, `ui.open_dialog`
  - `sisters.list` and `sister.open`

Each app adds its own requests on top: `doc.*` in text9000, `edit.*` in
splice9000, wave9000 and grace9000, and `book.*` in leaf9000.

`initialize` says who is talking:

```json
{"app": "wave9000", "version": "0.0.5", "wave9000": "0.0.5", "protocol": 1, "family": 1,
 "permissions": ["edit.read"], "sisters": ["text9000", "splice9000", "wave9000", "grace9000", "leaf9000"]}
```

- `protocol` numbers the app's own requests.
- `family` numbers the shared ones (see `src/family.rs`).
- An adapter that sticks to the family messages works in any sister.
- An adapter lists the sisters it's made for in `adapter.toml`:

  ```toml
  apps = ["splice9000", "wave9000"]
  ```

  Any other sister refuses it, and says which apps it's for.

| Request | Params | Result |
|---|---|---|
| `sisters.list` | | `[{name, kind, installed, opens}]` |
| `sister.open` | `app`, `paths` | Opens the files in that sister app |

## Sisters working together

Each app keeps the files it handles itself and hands the rest to the sister
that opens them:

- A photo dropped on splice9000 opens in grace9000.
- A video dropped on text9000 opens in splice9000.
- A comic dropped on wave9000 opens in leaf9000.

Each app also has its own hand-offs in its File menu, such as splice9000's
**Open Frame in grace9000** and grace9000's **Page Through Folder in
leaf9000**.

## Using it

```toml
[dependencies]
core9000 = { git = "https://github.com/solacecasino/core9000", tag = "v0.1.1" }
# leaf9000, which draws its own window:
# core9000 = { git = "...", tag = "v0.1.1", default-features = false }
```

Call `core9000::init(&APP)` first, with the app's `core9000::App`: its name,
version, environment-variable prefix, protocol and permissions.

MIT licensed.

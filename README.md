# `stp` — Stimma Tools Protocol CLI

`stp` is a command-line client for the
[Stimma Tools Protocol](https://github.com/stimma-ai/stimma-tools-protocol) (STP). It connects
to an STP provider — a cloud service, a remote machine, or a local subprocess — and lets you
list the tools that provider exposes, inspect their input and output schemas, run them, and
retrieve the resulting images, video, or other assets as files.

It implements the host (client) side of STP only; provider implementations live in the SDKs
([Python](https://github.com/stimma-ai/stimma-tools-protocol-python),
[TypeScript](https://github.com/stimma-ai/stimma-tools-protocol-ts)).

See the [Stimma Tools Protocol spec](https://github.com/stimma-ai/stimma-tools-protocol) for the full design.

## Install

### macOS / Linux

```sh
curl -fsSL https://raw.githubusercontent.com/stimma-ai/stimma-tools-protocol-cli/main/install.sh | sh
```

### Windows (PowerShell)

```powershell
irm https://raw.githubusercontent.com/stimma-ai/stimma-tools-protocol-cli/main/install.ps1 | iex
```

The installer drops `stp` into `~/.local/bin` (macOS/Linux) or `%LOCALAPPDATA%\Programs\stp`
(Windows) and tells you if that directory isn't on your `PATH`. Override the location with
`STP_INSTALL_DIR=/usr/local/bin`; pin a version with `STP_VERSION=v0.1.0`.

You can also download a binary directly from the
[Releases page](https://github.com/stimma-ai/stimma-tools-protocol-cli/releases), or build
from source with [Rust](https://rustup.rs):

```sh
cargo install --git https://github.com/stimma-ai/stimma-tools-protocol-cli stp
```

Verify with `stp version`.

## Quickstart

A local ComfyUI with the Stimma extension serves STP on port 8188. Point `stp` at it and list
the tools it exposes:

```sh
stp --url ws://localhost:8188/stp-v1 tools
```

Pick one, look at its parameters, and run it — the prompt is positional, everything else is a
flag:

```sh
stp --url ws://localhost:8188/stp-v1 show flux
stp --url ws://localhost:8188/stp-v1 show flux sampler   # one parameter's allowed values
stp --url ws://localhost:8188/stp-v1 run flux "a low-poly fox at dusk" --steps 30
```

`run` writes each output to disk and prints its path.

## Server aliases

Typing the URL every time gets old. Save it under a short name and select it with `-s`:

```sh
stp config add comfy --url ws://localhost:8188/stp-v1
stp -s comfy tools
stp config set-default comfy      # now a bare `stp tools` uses it
```

Point an alias at any host — a GPU box on your network, say — the same way. Aliases live in a
YAML config file (`stp config path`, `stp config edit`). If a provider needs a token, store it
by reference with `--token-env` or `--token-command` so the secret stays out of the file.

## Commands

| Command | Purpose |
|---------|---------|
| `tools` (`ls`) | List the provider's tools (`--json`, `--format=tsv`, `--task <t>`). |
| `show [tool] [param]` | No args: provider info + queue. `<tool>`: parameter table (`--values` expands enums; `--schema-only` / `--output-schema` / `--json` dump). `<tool> <param>`: one parameter's allowed values & constraints (`--json` dumps its schema). |
| `run <tool> [prompt]` | Execute: `--<param>` flags, `--params @file`, `-o`, `--stdout`, `--dry-run`, `--json`. |
| `raw <method> [json\|@file\|-]` | Send one arbitrary JSON-RPC request. |
| `watch` | Stream provider notifications until interrupted. |
| `config …` | `list`/`add`/`remove`/`set-default`/`get`/`init`/`path`/`edit`. |
| `agent-guide` | Machine-readable guide for driving `stp` programmatically. |
| `completions <shell>` · `version` | Shell completions; version info. |

Provider selection (first wins): `--url`/`--exec` → `-s <alias>` → `STP_SERVER`/`STP_URL`/
`STP_EXEC` → config `default_server`.

## Parameters

Each tool parameter is its own flag, named after the schema property. The conventional
`prompt` is positional and may appear before, after, or among the flags:

```sh
stp run flux "a cat" --guidance 1.0 --steps 30 --seed -1
stp run flux --steps 30 "a cat"                             # prompt last is fine
echo "a surreal landscape" | stp run flux -                 # prompt from stdin
stp run flux "a cat" --loras='[{"path":"x","weight":1}]'    # JSON for arrays/objects
stp run flux "a cat" --params @base.json --seed 7           # a JSON blob, plus overrides
```

Values are coerced and validated against the tool's `parameter_schema` before executing, so a
typo'd name or out-of-range value fails fast. `--dry-run` prints the assembled request without
running it. A local file passed to a media parameter (e.g. `--input_images ./cat.png`) is
uploaded automatically.

## Exit codes

- `0` — success
- `1` — runtime error
- `2` — usage error
- `3` — connection or protocol error
- `4` — job failed (provider returned an error)
- `5` — timeout
- `6` — validation failed
- `130` — interrupted

## Contributing

Bug reports and feature requests are welcome — please
[open an issue](https://github.com/stimma-ai/stimma-tools-protocol-cli/issues). For the
protocol itself, file issues on the
[Stimma Tools Protocol repo](https://github.com/stimma-ai/stimma-tools-protocol). We are not
accepting pull requests at this time.

## License

Apache-2.0. See [`LICENSE`](LICENSE).

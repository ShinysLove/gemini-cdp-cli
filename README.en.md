<div align="center">

# EVIL GEMINI

**A lightweight desktop workspace for Google AI Mode**

Chat, share images, and work on code with parallel agents.

[![Release](https://img.shields.io/github/v/release/ShinysLove/gemini-cdp-cli?color=8ab4f8)](https://github.com/ShinysLove/gemini-cdp-cli/releases/latest)
![Windows x64](https://img.shields.io/badge/Windows-x64-0078D4)
![Rust](https://img.shields.io/badge/built_with-Rust-dea584)
![WebView2](https://img.shields.io/badge/UI-WebView2-5c8ee6)

[Русский](README.md) · **English**

[Download](https://github.com/ShinysLove/gemini-cdp-cli/releases/latest) · [Report an issue](https://github.com/ShinysLove/gemini-cdp-cli/issues)

</div>

---

## Overview

Chat and project work in one application. Chrome handles requests to **Google Search AI Mode**, while the Rust client controls tabs through the local Chrome DevTools Protocol. The interface uses the system **WebView2** runtime, HTML/CSS/JavaScript, and Mica/Acrylic. Running a packaged build requires no Electron, Node.js, or external CDNs.

This is web interface automation, not an official Google API or a local model. The application interface is primarily in Russian; the documentation and About description are available in both languages.

**Navigation:** [Quick start](#quick-start) · [Features](#features) · [Project workflow](#project-workflow) · [Access modes](#access-modes) · [Development](#development) · [Maintainers](#maintainers)

> [!IMPORTANT]
> This project was created **solely for research, educational purposes, and as a proof of concept**. It explores browser automation through CDP, distributed system resilience, and local isolation of AI agent workspaces.
>
> The authors do not endorse, encourage, or promote commercial use of this tool, abuse of third-party platform resources, or actions that violate their Terms of Service. Users assume all risks associated with using the tool.

## Quick start

1. Install **Google Chrome** and **Microsoft WebView2 Runtime** if needed.
2. Download the Windows x64 ZIP from the [latest release](https://github.com/ShinysLove/gemini-cdp-cli/releases/latest).
3. Extract the entire archive to a writable folder. Keep `evil-gemini.exe`, `WebView2Loader.dll`, and `config.json` together.
4. Run `evil-gemini.exe`. If Chrome is installed elsewhere, edit `chrome_binary` in the configuration.
5. Choose **«Чат» (Chat)** for conversations or **«Работа» (Work)** for coding tasks.

Windows 11 uses Mica; supported Windows 10 systems use Acrylic. You can disable the effect in settings. AI Mode availability depends on Google, your region, and your account; signing in is not always required. Keep `headless: false` on first launch to complete sign-in, Google consent, or CAPTCHA manually.

## Features

| | Available features |
| --- | --- |
| **Chat** | Streaming responses, Markdown, search, local history, copying replies, and retrying requests |
| **Attachments** | File picker, paths, drag and drop, and image paste with Ctrl+V; previews beside drafts and sent messages |
| **Long requests** | Text over 12,000 characters is sent as `message.txt`; text attachments in ordinary chat are read in full |
| **Agents** | 1–50 workers, DAG dependencies, and default, manual, or AI-generated plans |
| **Change review** | Unified diffs with line numbers and highlighting, a dedicated apply confirmation, and a saved application status |
| **Inspector** | Changed files, command output, model explanations, and actual tool calls |
| **Interface** | Resizable panels, separate mode drafts, and smooth transitions that respect reduced motion |

### Images and documents

Clipboard paste supports **PNG, JPEG, WebP, GIF, and BMP up to 20 MB**. Previews appear inside the composer. Sent images remain attached to their messages and open in a larger preview when clicked. Older messages without stored attachment information remain text-only.

Files are stored as local copies with size and SHA-256 checks. The default limit is 20 MiB per attachment, up to 10 files. For coding tasks, agents see a text preview and can read the full content in chunks with `read_attachment`. PDFs, images, and DOCX files use the page's native file input when the current Google session supports it; otherwise, the application reports an error. Selected content is sent to Google.

Upload confirmation waits up to **120 seconds**, checking filenames or newly loaded thumbnails in Google's composer. Retrying the same batch before submission does not upload it again. When a CAPTCHA appears, the application shows Chrome, waits for manual verification, then returns focus to its own window. Verification time does not consume the response timeout.

## Project workflow

1. Select a project folder and describe the task.
2. In **«Агенты» (Agents)**, set the worker count and optionally edit the JSON DAG plan or request a plan from the model.
3. Start the task. Independent stages run in parallel; dependent stages wait for their prerequisites.
4. Open **«Файлы и терминал» (Files and terminal)** to review diffs, commands, and the **«Действия» (Activity)** tab.
5. Click **«Проверить и применить изменения…» (Review and apply changes)**, check the project path and diff in the dedicated dialog, then confirm the write. The applied status is saved.

Each stage runs in a private project snapshot with its own Git directory. Snapshots include uncommitted changes and permitted untracked files. Dependencies pass reviewed changes to subsequent stages; conflicting parallel changes are rejected. Source hashes are checked again before application to avoid overwriting newer user edits.

Defaults are **3 workers**, at most **2 independent anonymous conversations in one Chrome instance**, and a shared **5-second interval** between requests. The conversation pool has a separate limit; Chrome may display contexts as separate windows. Changing the worker count affects new assignments while preserving running stages.

After a failure, unfinished stages in the current task can be retried without losing successful results or the original snapshot. Execution retries are available until the application closes. Saved completed tasks can be viewed and applied after a restart; automatic resumption of unfinished stages is not supported.

HTTP 429 triggers a shared cooldown respecting Retry-After; retries reuse the same session. Persistent limits stop remaining work without rotating sessions to bypass quotas. Google refusals are detected as `SERVICE_REFUSAL`. A stage must successfully use a tool and return valid `summary/done/actions` JSON before it can be considered complete.

### Terminal and activity

The terminal is a command/output log, without an interactive PTY or input to running processes. A manual command takes a program name and a JSON argument array, runs in a fresh project copy, and follows the selected access mode. Cancellation terminates the application's command process and its children. Its file changes are not automatically included in the agent diff.

Activity shows available responses, explanations, tools, and errors. Hidden model reasoning is not available; the protocol is not executed as HTML/JavaScript.

## Access modes

| Mode | Rules |
| --- | --- |
| **Ask for approval** | Default: confirmation for commands, patch/deletion operations, and applying source changes |
| **Approve for me** | Automatically allows validated patch/deletion operations in the copy and standard `cargo check/build/test` commands with restricted flags; other commands, Git, and source application require approval |
| **Full access** | Tool requests run without confirmation; applying the final diff still starts from a separate button |

Mode changes affect subsequent requests and do not approve pending actions. Neither the model nor repository content can grant permission. Direct file tools stay within the workspace: traversal, symlinks/junctions, secret directories, and ambiguous Windows paths are blocked.

> [!WARNING]
> **A project copy is not an operating system sandbox.** Approved commands and build scripts execute on your computer and may access locations outside the copy. Review arguments and the working directory before approval.

## Keyboard shortcuts

| Action | Desktop application |
| --- | --- |
| Send / insert a newline | `Enter` / `Shift+Enter` |
| Start a task | `Ctrl+Enter` |
| New chat or task | `Ctrl+N` |
| Search | `Ctrl+K` |
| Chat / Work | `Ctrl+1` / `Ctrl+2` |
| Stop waiting | `Esc` or the stop button; Google may continue generating |

Panel dividers support dragging and keyboard arrows. Positions are saved locally. Token estimates use one token per four Unicode characters, not Google's usage, quota, or billing data. Tasks count sent contexts and responses; chat counts visible history and the draft.

## Terminal client

```powershell
cargo run --locked --bin gemini-cdp-cli
cargo run --locked --bin gemini-cdp-cli -- --agents
```

Ordinary CLI mode supports text and keeps history in memory. Each launch starts a fresh conversation. `Enter` sends; `Ctrl+J` / `Shift+Enter` / `Ctrl+Enter` insert a newline; `Ctrl+V` pastes text; arrows scroll; `Ctrl+N` starts a new conversation; `Esc` stops waiting; `Ctrl+C` exits. Chrome stays open. Model selection through the CLI is not implemented.

In `--agents` mode, `/workspace PATH`, `/attach PATH`, `/plan FILE`, and `/plan-ai REQUEST` set the project, attachments, and plan. `Tab` focuses the slider; arrows, Home/End, and the mouse change the worker count. `F4` shows attachments, `F5` shows the diff, `/apply` requests application, `Ctrl+A` / `Ctrl+D` approve / deny an action, and `Esc` cancels the task. Pasting dragged paths depends on terminal support for bracketed paste.

## Configuration and local data

Example `config.json`:

```json
{
  "chrome_binary": "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
  "profile_dir": "profile_ai_mode",
  "cdp_port": 9223,
  "headless": false,
  "response_timeout_secs": 180
}
```

Run the CLI from the configuration folder. The desktop app looks for `config.json` in the working directory or near its EXE. The optional `ai_mode` object overrides `input_selector`, `response_selector`, `stop_selector`, and `complete_selector`; defaults are in `src/ai_mode.rs`. Changes to Google's markup may require adapter updates.

| Path | Contents |
| --- | --- |
| `data/history.json` | Chats, message attachment metadata, and interface settings |
| `data/workbench.json` | Selected and recent projects |
| `data/agents.json` | Access mode, workers, conversation pool (1–8), intervals, steps, retries, and attachment limits |
| `data/agent_runs/` | Reports, logs, and private working copies |
| `data/attachments/` | Copies of selected files |
| `webview_data/` | WebView2 data |
| `profile_ai_mode/` | Dedicated Chrome profile |

These user folders are excluded from Git. The old `profile_gemini` is not migrated automatically. Desktop and terminal clients should not control the same profile/port simultaneously. If no suitable tab is found, close Chrome for that profile and restart the client. Reopening a chat restores its saved Google URL; start a new conversation if the session is no longer available.

CDP accepts only local WebSocket addresses; do not expose its port to the network. The application does not modify DNS, VPN, or routing: Chrome uses your computer's current network. Opening the workspace does not create conversations; connections start when a task launches or chat opens.

## Development

### Build for Windows x64

```powershell
cargo build --release --locked --bin evil-gemini
.\target\release\evil-gemini.exe

# ZIP with the EXE, WebView2 loader, and documentation
.\scripts\package-release.ps1 -Version 0.1.2
```

For GNU builds, `build.rs` copies the official `WebView2Loader.dll` from the SDK next to the EXE. HTML/CSS/JavaScript are embedded in the application. Build the separate CLI with `cargo build --release --locked --bin gemini-cdp-cli`.

### Automated checks

```powershell
cargo test --locked --all-targets
node --test tests/adapter.test.cjs tests/workbench.test.cjs tests/desktop-ui.test.cjs
```

Ordinary tests do not contact Google. Submission errors preserve the draft; Markdown rendering escapes model HTML and blocks executable links. The CLI restores the terminal on exit and errors.

<details>
<summary><strong>Checks that contact Google</strong></summary>

```powershell
cargo run --locked --bin gemini-cdp-cli -- --diagnose
cargo run --locked --bin gemini-cdp-cli -- --smoke-test
cargo run --locked --bin gemini-cdp-cli -- --live-agent-smoke
```

`--diagnose` prints input/response structure without conversation text or cookies. `--smoke-test` opens a fresh chat, sends two fixed messages, and checks memory of the BLUEORBIT token. `--live-agent-smoke` checks independent conversations and verifies that source files are not written without approval. These modes use Google's normal service limits.

</details>

<details>
<summary><strong>Source code map</strong></summary>

| Module | Purpose |
| --- | --- |
| `src/browser.rs` | Chrome lifecycle and CDP transport |
| `src/ai_mode.rs`, `src/ai_mode.js` | Readiness, submission, response completion, Google DOM, and Markdown |
| `src/agents/` | DAG planning, session pool, workers, context, result integration, and events |
| `src/tools.rs` | File tools, snapshots, commands, approvals, and application |
| `src/attachments.rs` | Preparing, verifying, and storing attachments |
| `src/desktop.rs`, `src/history.rs` | Native window, IPC, background worker, and history |
| `src/main.rs`, `src/markdown.rs`, `src/agent_tui.rs` | Terminal chat, Markdown, and agent panel |
| `ui/` | Embedded application interface |

Tokio tasks, bounded queues, session semaphores, cancellation tokens, and the DAG separate the engine from its interfaces.

</details>

## Maintainers

- [@ShinysLove](https://github.com/ShinysLove)
- [@p1ckqq](https://github.com/p1ckqq)

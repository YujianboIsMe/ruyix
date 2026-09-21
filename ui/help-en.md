# Help

Type commands in the bottom **command bar** and press Enter. Unknown input is handed to
the AI translator, which rewrites it into a standard command.

## Commands

| Command | Description |
| --- | --- |
| `open project <path>` | Open a project folder |
| `open file <path>` | Open a file |
| `close project` | Close current project |
| `close all` | Close all tabs |
| `close <index>` | Close tab by index (negative = from right) |
| `close other` | Close all but current tab |
| `close left` | Close tabs left of current |
| `close right` | Close tabs right of current |
| `new py\|rs\|md\|c <name>` | Create file (auto extension) |
| `new file <relative_path>` | Create a file |
| `new folder\|dir <path>` | Create a directory |
| `rename\|mv <old-path> <new-name>` | Rename file/directory |
| `del\|delete\|remove\|rm <path>` | Delete file/folder (with confirmation) |
| `refresh [path]` | Refresh file tree / folder from disk |
| `run <name>=<cmd>` | Quick-add a run target |
| `config add\|get\|update\|remove [-g\|-p\|-r] <key>[=<value>]` | Manage configuration (default `-r` runtime) |
| `agent [message]` | Open a new Agent session; sends the message when given |
| `service` | Open the service panel (long-running processes started by the agent) |
| `service log <pid>` | Follow a service's terminal output live |
| `git <args...>` | Run a native git command (e.g. `git status`) |
| `project lang <lang> <project path>` | Set project language |
| `project edit "<path>" "<name>" <lang>` | Edit project name and icon |
| `project delete <project path>` | Remove from project list (folder untouched) |
| `project migrate` | Migrate legacy project config |
| `help` | Open this help |

## Shortcuts

| Shortcut | Description |
| --- | --- |
| `Ctrl+S` | Save current file |
| `Ctrl+Enter` | Send message in the Agent session input |
| `Tab` | Insert indentation in the editor |

Edits auto-save 1 second after you stop typing, and immediately on blur or tab switch.

## Syntax Highlighting

| Language | Extension |
| --- | --- |
| Python | `.py` |
| Rust | `.rs` |
| HTML (incl. embedded CSS/JS) | `.html`, `.htm` |
| CSS | `.css` |
| JavaScript | `.js`, `.mjs`, `.cjs` |
| Markdown | `.md`, `.markdown` |
| SQL | `.sql` |
| Java | `.java` |

Markdown headings are parsed into the **outline panel** for quick navigation.

## Contact

Visit <https://newest-ai.com> or email yujianboisme@outlook.com.

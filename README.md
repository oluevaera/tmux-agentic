<h1 align="center">tmux-agentic</h1>

<p align="center"><strong>Monitor and manage all your coding agents from a single board.</strong></p>

<p align="center">Claude Code, Codex and OpenCode side by side in tmux, with live status and live previews.<br>
Nothing to install into your agents. No hooks, no wrappers.</p>

https://github.com/user-attachments/assets/77ec8aa6-fcbe-4d86-a84e-a6ddb574afc6

- **Never hunt for the stuck one.** Every agent is marked working, needs you, done or idle, across every session, and
  your status bar counts them.
- **Jump straight in.** One key takes you to the agent that's waiting on you. Another brings you right back.
- **Organise without thinking about tmux.** Grab agents from anywhere and regroup them into a window or a fresh session.
- **Switch CLIs mid-task.** Hand a Claude Code conversation to Codex, or any way round, and the new agent picks up where
  the old one left off.
- **Every conversation, one history.** Past sessions from every CLI, ready to resume one at a time or side by side.

## Install

With [TPM](https://github.com/tmux-plugins/tpm), add this to your `tmux.conf` and press `prefix` + `I`:

```tmux
set -g @plugin 'oluevaera/tmux-agentic#release'
```

Without TPM, clone the same branch and run it from `tmux.conf`, after your own `status-right` line:

```sh
git clone -b release --single-branch https://github.com/oluevaera/tmux-agentic ~/.tmux/plugins/tmux-agentic
```

```tmux
run-shell ~/.tmux/plugins/tmux-agentic/agentic.tmux
```

**Requirements:** tmux 3.3 or newer, on macOS or Linux (ARM or Intel), and at least one of Claude Code, Codex or
OpenCode. `sqlite3` adds Codex and OpenCode to History, and `zoxide` adds folder suggestions.

## Quick start

| Key | |
|---|---|
| `prefix` `a` | open the board (again to close it) |
| `enter` | go to the agent under the cursor |
| `prefix` `b` | back to where you were before the board took you |
| `?` | every key, right in the board |

## What it can do

### Live status, everywhere

Your status bar shows `◆ 1 needs you  ✓ 2 done  ● 3 working` and updates within about a second of an agent changing.
An agent that finishes while you're looking elsewhere stays **done** until you've seen it.

### One board for every agent

`prefix` `a` opens a popup with every agent on your tmux server: its name, CLI, folder, status, how full its context
window is, and what it's doing right now. The list sorts itself, so whatever needs you is always on top. A live preview
streams the selected agent's screen as it works. Tabs narrow it to what needs you or to one CLI, `/` searches, and `f`
switches to a tree of every session, window and agent.

### Jump in, and back

`enter` takes you to the agent under the cursor. Answer its question, then `prefix` `b` returns you to where you were.
Press it again to hop back to the agent.

### Organise your sessions

Mark agents with `space`, wherever they are. `⇧ tab` sends them together into one new window, a second press makes it a
fresh session, and `enter` carries it out. Whole windows move intact, panes keep running, and the board stays open in
the tree so you can keep going. `r` renames an agent, a window or a session right in its row.

### Hand off between CLIs

`tab` on an agent stages a switch to another CLI. Apply it and the new agent takes the old one's place: it reads the
saved transcript and picks up the task where it stopped. The old agent closes once the new one is running, and its
conversation stays in History. Run out of usage in Claude Code? Carry on in Codex.

### Spin up agents anywhere

`n` starts a new agent and `tab` picks which CLI it runs. In the tree it goes where you press it: a new pane in that
window, or a new window in that session, and `shift+tab` takes it out a level, up to a new session. `o` picks its folder
from recent agent folders and everything `zoxide` knows. `n` on a new agent adds another like it, so `n n n` stages
three, each with its own CLI and folder; `enter` starts them all, those headed for the same new window sharing it. On an
idle agent, `o` moves it to another folder with its own `/cd`, keeping the conversation.

### Stage now, run together

Handoffs, folder moves and new windows are staged. Each row shows what will happen, and you can keep staging. `enter`
runs everything in order while the footer counts the steps. Every handoff is checked first, so if one can't go ahead,
nothing changes and it all stays staged.

### Every conversation, one history

`0` opens History: past conversations from every CLI, newest first, with your last message and the answer in the
preview. `enter` resumes one in its folder. Mark several and they open side by side. `x` removes one the way its CLI
keeps it recoverable.

### Make it yours

It works out of the box. To change something, create `~/.config/tmux-agentic/config.toml` and copy in the lines you
want from the full file below. Every setting is there at its default, with a note on what it does. Changes apply the
moment you save, key bindings included. The plugin can also write the file for you:
`~/.tmux/plugins/tmux-agentic/bin/agentic config`.

<details>
<summary><b>config.toml</b>: every setting, ready to copy</summary>

```toml
# tmux-agentic settings. Every line is optional: uncomment one and change it to use it.
# The plugin picks up changes as you save, tmux keys included; no reload needed.

[keys]
# tmux keys, in tmux's names ("M-a" is Alt+a, "C-g" Ctrl+g); "none" leaves one unbound.
# open = "a"                  # the board, after the prefix
# open_root = "none"          # the board without the prefix, e.g. "M-a"; in the board, it closes it
# back = "b"                  # back to the agent you were on, after the prefix

[board]
# layout = "auto"             # "auto" reopens on the list or tree it was closed on; "list" or "tree" always opens there
#                             # (f still switches until it closes). The tab is always All.
# split = "horizontal"        # agents opened or moved together: "horizontal" side by side, "vertical" stacked
# tab_numbers = false         # each tab's number key in front of its name: "1. All·3", "0. History"
# width = "90%"               # of the window
# height = "80%"

[board.keys]
# The board's own keys, in tmux's names: one or a list; [] leaves an action without one. A key another action has by
# default moves to the one you give it. Escape and Ctrl+C stay as they are.
# down = ["j", "Down"]
# up = ["k", "Up"]
# first = ["g", "Home"]
# last = ["G", "End"]
# prev_tab = ["h", "Left"]
# next_tab = ["l", "Right"]
# go = "Enter"                # go to an agent, open a conversation, apply what's staged
# mark = "Space"
# switch_agent = "Tab"        # continue a conversation in another CLI
# detach = "BTab"             # Shift+Tab: tree, to a new window or session
# rename = "r"
# kill = "x"                  # kill an agent, remove a conversation (pressed twice)
# compact = "c"
# new = "n"
# folder = "o"
# search = "/"
# layout = "f"                # the list or the tree
# help = "?"
# close = "q"
# tab_1 = "1"                 # a tab by its place in the bar, 1 the first (tab_1 … tab_9); history, History
# tab_2 = "2"
# tab_3 = "3"
# tab_4 = "4"
# tab_5 = "5"
# tab_6 = "6"
# tab_7 = "7"
# tab_8 = "8"
# tab_9 = "9"
# history = "0"

[agents]
# enabled = ["claude", "codex", "opencode"]   # left out: every one that's installed

[history]
# hide = "^(/private)?/tmp/"  # folders History leaves out (a regular expression); "" leaves none out
# limit = 300                 # how many conversations it loads

[handoff]
# What a conversation's new agent is told when ⇥ hands it to another CLI. {agent}, {id} and {transcript} stand for the
# agent it comes from, the conversation's id and where its transcript is; {transcript} has to be in it. For example:
# prompt = "Continue the {agent} conversation {id}: its transcript is at {transcript}. Summarize it, then carry on."

[theme]
# Text, symbol and border colors. "terminal" takes them from your terminal's own, so the board follows its theme; the
# others bring their own, all made for a dark background. A theme never changes a background: those are below.
# name = "terminal"           # "terminal", "catppuccin-mocha", "tokyo-night", "gruvbox-dark", "nord", "dracula",
#                             # "rose-pine", "solarized-dark", "one-dark", "kanagawa", "everforest"
# Any of the theme's colors, as "#rrggbb", one of the terminal's ("yellow", "brightblack") or "colour173":
# needs_you = "brightyellow"
# working = "green"
# done = "brightblue"
# idle = "brightblack"
# failed = "brightred"
# success = "brightgreen"
# accent = "brightblue"       # what's yours: marks and staged changes
# muted = "brightblack"       # secondary text and the rules between parts
# border = "brightblack"
# claude = "colour173"
# codex = "cyan"
# opencode = "colour141"
# Backgrounds, whatever the theme:
# background = "terminal"     # behind the popup: "terminal" (yours; transparent stays so) or a color like "#1e1e2e"
# cursor = "#262c38"          # the bands behind the row you're on, marked rows, and what you're typing into
# marked = "#1f2c40"
# field = "#1a1f29"

[status]
# enabled = true                               # false keeps the counts out of your status bar; Done still works
# show = ["needs-you", "done", "working"]     # what the status bar counts; "idle" can go in too
```

</details>

To place the status counts yourself, put `#{agentic_status}` in your `status-right`.

## License

[MIT](LICENSE)

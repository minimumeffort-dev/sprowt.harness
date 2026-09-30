# Terminal and companion

The interface keeps the project and worker status at the top, the conversation left aligned and the composer at the bottom.

```text
[sprowt companion]  project + worker status

<code mod/>  ◇ selected mod

> your message
▤ planner message or saved plan
◆ executor reply

queue / waiting steering, when present

[message composer]
keyboard hints
```

Code mod and queue dialogs share selection, spacing and keyboard hints. Hints adapt to the current view and terminal width.

## Commands

| Command | Action |
| --- | --- |
| `sprowt-harness` | Open the current project |
| `sprowt-harness setup` | Install the local Laya runtime and model |
| `sprowt-harness --no-motion` | Open with animations disabled |
| `sprowt-harness --help` | Show available options |
| `sprowt-harness --version` | Show the installed version |

## Controls

| Key | Action |
| --- | --- |
| Enter | Create a described mod, queue a message or save an edit |
| Ctrl+J | Add a newline |
| Ctrl+P | Switch, create or delete a code mod |
| Ctrl+Q | Manage queued instructions |
| Ctrl+R | Start, stop or retry the current worker |
| Fn + ↑ / ↓ on Mac | Scroll the conversation |
| Page Up / Page Down | Scroll on keyboards with those keys |
| Esc | Back from a dialog; quit from the conversation |
| Ctrl+C | Quit |

Pasted text keeps its line breaks. Dialog actions are covered in [Code mods and messages](code-mods.md).

## Sprowt companion

The companion uses terminal cells and glyphs, so it needs no image support. It sits beside the project heading in an 8-column, 4-row area.

It blinks, nods, hops and changes expressions. Creating a mod, queueing a message or saving a queue edit triggers a brief celebration. Worker progress appears in the status text beside it.

Run `sprowt-harness --no-motion` for a still companion and no entrance animation. On exit, the harness restores normal terminal input, paste handling and the previous screen.

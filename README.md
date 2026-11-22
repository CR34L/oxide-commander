# Oxide Commander

A minimalistic file manager in the spirit of Norton Commander / Midnight Commander,
built in Rust, with only the essential features for coders.

Dual panels, Total Commander function keys and a clean, Path Finder style look,
built with eframe/egui: one standalone binary for macOS, Windows and Linux.

- **Sidebar**: the standard folders (Home, Desktop, Documents, Downloads, …). On macOS,
  if you give the app Full Disk Access (System Settings → Privacy & Security), it shows
  your Finder sidebar Favorites instead, in Finder's order. Below them, every mounted drive (internal
  disks, USB, disk images, network shares) with a capacity bar, refreshed every few seconds.
- **Mouse like Finder / Explorer**: click selects, double-click opens (on `..`: goes up),
  ⌘/Ctrl-click and ⇧-click extend the selection, right-click for actions, clicking empty
  space clears the selection.
- **Two panels**: a `..` row on top of every list,
  back/forward/up buttons, clickable path bar, filter box, sortable columns
  (Name, Size, Kind, Date Modified), free space in the status line.
- **Remembered per panel**: sort column and direction plus the "Hidden files"
  switch. They're stored in `~/Library/Application Support/Oxide Commander/settings.txt`
  (Windows: `%APPDATA%\Oxide Commander`, Linux: `~/.config/oxide-commander`).
- **Drag and drop** between panels, onto folders, onto sidebar drives, or from
  Finder/Explorer. You're asked whether to copy or move.
- Folder sizes are calculated in the background (shown as "…" until ready),
  so sorting by Size works for folders too.
- Copy, move and delete run in the background with a progress bar, MB/s, time left
  and Cancel. Moves within one drive are instant; across drives they copy, then delete.
  New operations started while one is running are **queued** and run one after
  another, never in parallel.
- When a target already exists you choose: **Replace** (the old item is removed),
  **Merge** (folders: contents are combined, same names inside are replaced) or
  **Rename** (keep both: the new item gets a number, e.g. `photo (1).jpg`).
- **Open in… (F1)**: a popup opens the current folder in the terminal, an installed
  editor (VS Code, Cursor, Zed, Sublime Text, JetBrains IDEs, …) or Finder. Pick with
  ↑↓ and Enter or the number; the last choice is preselected. F1 twice opens the terminal.
- **Built-in terminals (F1 F1 / ⌘T)**: each panel can have its own. The terminal splits the current
  panel: the file list stays on top, and a shell opens below it in that panel's folder.
  Open one on the left and one on the right to work in two shells side by side. Drag the divider to resize. F1 F1 or
  ⌘T gives the terminal the keyboard and hides it again; the shell keeps running while
  hidden. Click a panel to type into the file list again. "cd to panel folder" sends
  the panel's folder to the shell. Mouse-select and ⌘C copy, ⌘V pastes, and the wheel
  scrolls back. Full-screen programs such as less get their own screen.
- **Settings (F9 / ⌘,)**: Light, Dark or Norton (retro blue/cyan/yellow, monospace)
  look; the terminal is built in or a separate terminal app window; F8 moves to Trash or deletes immediately;
  reopen the last folders at startup.

## Download

Apple Silicon Macs (M1 and later): get the zip from the
[latest release](https://github.com/CR34L/oxide-commander/releases/latest), unzip it and drag
**Oxide Commander.app** to Applications. The app is not notarized: on first launch right-click
it and choose **Open**.

Releases are built by GitHub Actions when a version tag is pushed
(`git tag v0.1.0 && git push origin v0.1.0`; the tag must match `version` in `Cargo.toml`).

## Run

```sh
cargo run --release -- [LEFT_DIR] [RIGHT_DIR]   # defaults: home, Downloads
./bundle-macos.sh                               # builds dist/Oxide Commander.app
./make-dmg.sh                                   # builds the installer dist/Oxide-Commander-<version>-macos-<arch>.dmg
```

## Keys

| Key | Action |
|-----|--------|
| Tab / ⇧Tab | switch between left and right panel |
| ↑ ↓ | move (Shift = select) |
| PgUp / PgDn (fn↑ / fn↓) | to the first / last visible row, then page by page |
| ⌘PgUp / ⌘PgDn, Home / End (fn← / fn→) | jump to top / bottom of the list |
| ⌥↑ / ⌥↓ | move one screen |
| Enter / double-click | open folder or file (on `..`: go up) |
| Backspace, ⌘↑ / ⌘[ ⌘] | enclosing folder / back, forward |
| Space / Insert | select and move down |
| ⌘-click / ⇧-click | add to selection / select range |
| type letters | jump to name |
| ⌘F | filter |
| F1 | open the folder in… terminal, VS Code / other editor, Finder (F1 twice: terminal) |
| ⌘T | terminal below the current panel; again: hide it (or a separate window, see Settings) |
| ⇧F1 / ⌘F1 / ⌘? | help |
| F2 / ⇧F6 / ⌘F6 | rename |
| F3 | view: built-in text viewer (default), Quick Look or an editor, see Settings |
| ⇧F3 / ⌘F3 | Quick Look |
| F4 | edit: VS Code if installed, else the system text editor; choose another in Settings |
| ⇧F4 / ⌘F4 | edit in the system text editor |
| F5 | copy to other panel |
| ⇧F5 | duplicate in the same folder (“name (1)”) |
| F6 | move to other panel |
| F7 / ⇧⌘N | new folder (Tab in the dialog switches to a new file) |
| ⇧F7 / ⌘F7 / ⌥⌘N | new file |
| F8 / ⌘⌫ | move to Trash (or delete, see Settings) |
| ⇧F8 / ⌘F8 | delete permanently |
| ⇧⌘. / ⇧⌘H | show hidden files in this panel |
| ⌃U | swap panels |
| F9 / ⌘, | settings |
| F10 / ⌘Q | quit |

Hold ⇧ or ⌘ and the function-key bar shows the second layer (⌘F5 is taken by macOS for VoiceOver, so Duplicate is ⇧F5).

On a Mac the F-keys are media keys by default. Hold `fn`, or enable *System
Settings → Keyboard → Use F1, F2, etc. keys as standard function keys*. The
F-key buttons at the bottom are always clickable. On Windows and Linux, ⌘ means Ctrl.

## License

MIT, see [LICENSE](LICENSE).

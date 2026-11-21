# Oxide Commander

A dual-panel file manager with Total Commander function keys and a clean,
Path Finder style look. Written in Rust (eframe/egui): one standalone binary
for macOS, Windows and Linux.

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
- **Built-in terminals (F1)**: each panel can have its own. F1 splits the current
  panel: the file list stays on top, and a shell opens below it in that panel's folder.
  Open one on the left and one on the right to work in two shells side by side. Drag the divider to resize. F1 or
  ⌘T gives the terminal the keyboard and hides it again; the shell keeps running while
  hidden. Click a panel to type into the file list again. "cd to panel folder" sends
  the panel's folder to the shell. Mouse-select and ⌘C copy, ⌘V pastes, and the wheel
  scrolls back. Full-screen programs such as less get their own screen.
- **Settings (F9 / ⌘,)**: Light, Dark or Norton (retro blue/cyan/yellow, monospace)
  look; F1 opens the built-in terminal or a separate terminal app window; F8 moves to Trash or deletes immediately;
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
| F1 / ⌘T | terminal below the current panel; again: hide it (or a separate window, see Settings) |
| ⇧F1 / ⌘? | help |
| F2 / ⇧F6 | rename |
| F3 | view text / Quick Look |
| F4 | edit in text editor |
| F5 | copy to other panel |
| F6 | move to other panel |
| F7 / ⇧⌘N | new folder |
| F8 / ⌘⌫ | move to Trash (or delete, see Settings) |
| ⇧F8 | delete permanently |
| ⇧⌘. / ⇧⌘H | show hidden files in this panel |
| ⌃U | swap panels |
| F9 / ⌘, | settings |
| F10 / ⌘Q | quit |

On a Mac the F-keys are media keys by default. Hold `fn`, or enable *System
Settings → Keyboard → Use F1, F2, etc. keys as standard function keys*. The
F-key buttons at the bottom are always clickable. On Windows and Linux, ⌘ means Ctrl.

## License

MIT, see [LICENSE](LICENSE).

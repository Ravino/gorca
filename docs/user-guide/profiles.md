# Profiles

A profile is a small set of settings that applies to one application, window, site or situation.
It lets you change the speech rate in a terminal without changing it in Firefox. The design is in
[ADR-0016](../adr/0016-profiles.md); the settings themselves are in the
[configuration reference](../config-reference.md).

## What profiles are

- **Global settings** are your `~/.config/gorca/gorca.toml`.
- **User profiles** are overrides stored one per file in `~/.config/gorca/profiles/`. A profile
  holds only the keys it changes, with the same sections and key names as `gorca.toml`. gorca
  reloads them when they change.
- **Built-in rows** ship with gorca and are on automatically. Today there is one: Visual Studio
  Code and its relatives (Code, VSCodium, Cursor) get no virtual buffer, because they are editors
  with their own key bindings.
- gorca picks the profile for the focused application automatically. You can pin a different
  choice for one application with the keys below.

## Where your changes go

When you change a setting by command while an application is focused, the change is saved in
that application's profile. The profile is created if it does not exist, and gorca announces
"for <application>". A change made in Firefox therefore never changes Signal.

The global settings change only when you ask for it (save to global) or point the edit target at
global. Person-level settings (speech backend, keyboard, interface language and privacy) are
always global.

## The layering rule

An application profile can change only keys that exist in the global settings: core keys, or
keys of an installed extension, with the same type and range. Unknown keys are rejected with a
diagnostic. Keys of an extension you uninstalled stay in the file, inactive, and become active
again when you reinstall it.

## Keys

The same chords are bound in all four presets.

| Chord | Command | What it does |
|---|---|---|
| gorca+Ctrl+C | `settings.save` | Saves every changed setting to its own file. |
| gorca+Ctrl+Shift+C | `profiles.save_global` | Copies the settings you changed by command in this session from the application's profile to the global settings. Press twice to apply them everywhere. It never deletes or resets other global keys, and refuses if the global file changed meanwhile. |
| gorca+Ctrl+P | `profiles.cycle` | Cycles the profile pinned for this application: Automatic, each matching profile, then "global only". Speaks the new choice. |
| gorca+Ctrl+Shift+P | `profiles.toggle_enabled` | Switches the profile in effect for this application off or on. |
| gorca+Ctrl+Alt+P | `profiles.edit_target_cycle` | Chooses where edits go: the application's profile or the global settings. |
| gorca+Shift+P | `profiles.announce` | Says the active profile and where edits go. Press twice for the full stack. |

`profiles.suspend` is a command without a key. `browse.save_mode_default` has no key either:
gorca does not yet apply `browse.mode_default` when a document first gets focus, so its planned
chord is withheld.

## Web applications and Electron

The browse and focus mode toggle (`browse.toggle`: gorca+Space in the NVDA presets, gorca+A in
the Orca presets) works as a sticky focus mode in Chrome, Chromium and Electron applications.
Arrow keys go to the application and gorca follows the focus, which suits message lists with
their own arrow navigation. Per-application defaults are data rows, not special cases in the
core. The first row is the editor family above; a row for Signal is planned.

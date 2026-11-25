# Changelog

## 0.1.0 — 2026-11-24 (first public release)

Linen is a calm writing app for notes, lists and passwords on Omarchy: spelling as you type, tabs, autosave, password-locked notes (GPG), optional offline AI helpers, and it wears your running Omarchy theme.

Before release, Gemini 4 Neon High read the whole code without changing anything and reported 18 findings. Claude checked each one against the code and fixed them in two batches, with tests (20 pass).

**Safety and saving**
- The built-in local server answers only requests addressed to itself, and its secret is placed directly into Linen's window instead of being served in the page.
- Closing the window waits for every tab to save (with a 3-second fallback); closing a tab with unsaved changes waits for its save, and stays open if it fails.
- Saves use private, unique temporary files; Linen's own folders are private (0700).
- Locked notes are never written to the session file, locking a link locks the real file, and an existing locked note is never silently replaced.
- Long notes can't deadlock text cleanups; the random source never falls back to zeros.

**Speed and polish**
- Typing no longer redraws the tab bar or recounts the note on every key; locked notes save less often.
- Save as, Save to project, AI rewrites and cleanups only change the note they started on.
- Opening several files opens them all, each in its own tab.
- No personal paths built in: the Obsidian vault is chosen once and remembered; with no project folders, Save opens a normal Save box.
- The window never navigates away from Linen; damaged requests are refused.

**New: follows your Omarchy theme**
- Linen maps the running Omarchy palette onto its colors and recolors live when the theme changes, using a file watcher (no polling). Linen's own Dusk and Dawn themes are still in the menu.

# v2026.10.02.0002

## AppImage clean-system check: 0 failures, was 4

### Fixes

- Start the AppImage on Linux systems that lack some window libraries.
- Show file modified times in local time, not UTC.
- Keep every output line when Undo reverses a merge Take after a later edit.
- Restore the merge conflict when Undo reverses a Take.
- Keep an Ignored or Conflict mark set after a Take when Undo reverses it.
- Count merge differences under the current rules after Undo.
- Format XML that uses declared entities in attribute values.
- Scroll to the changed line after Undo and Redo.
- Show control and format characters in file diagnostics as escaped text.
- Update the Explorer menu text after an upgrade.
- Keep notice text inside narrow windows.

### Changes

- Rename the AppImage file to `compare-all-<version>-x86_64.AppImage`.
- Add update information to the AppImage and publish a `.zsync` file.
- Bundle the window libraries and their license files in the AppImage.
- Show the product name as "Compare All" in the window, the menus and the installer.

Full changelog: CHANGELOG.md

# v2026.10.02.0001

Initial release.

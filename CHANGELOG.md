# v2026.10.15.0006

## Text Merge

- Keep edits recoverable after a failed save and ask before closing the view.
- Check generated output for decoding replacements before writing it.

## Folder Merge

- Show expected and current modified times when a source changes after planning.

## Remote comparisons

- Detect incomplete FTP and HTTP downloads instead of treating them as complete.
- Keep the previous file when an SFTP replacement fails.
- Parse escaped S3 and WebDAV listing entries correctly and reject keys outside the requested folder.
- Reject revoked SSH host keys even when another rule accepts the same key.

## Commands and packaging

- Show accurate reasons when commands are unavailable.
- Check AppImage catalog compatibility under Firejail before publishing.

# v2026.10.14.0005

## Merge takes restore selected input consistently after joined-line edits

### Fixes

- Restore explicitly taken merge sections consistently in either take order.
- Keep joined text once after reloads and line takes.
- Preserve unresolved conflicts when a line take makes no text change.
- Keep merge output and saved bytes in sync after removing a joined separator.
- Preserve a replaced file's access list, named streams, creation time, and Hidden and System flags on Windows.
- Preserve file permissions during copies, replacements and backups on Linux and macOS.
- Replace a Windows file link with the copied file while leaving the link's target intact.
- Open folders and select other items in the system file manager; show macOS packages from the inside and report missing items.
- Handle file names beginning with a hyphen in Open With.
- Sweep only temporary copy folders created by the application.

### Security

- Set backup access before writing its content, and refuse a backup when the required access cannot be preserved.
- Keep operation journals and their folder private to the current user on Linux and macOS.
- Hide URL passwords in script logs and errors.
- Validate RAR archive signatures before opening entries.
- Limit 7z decoding to 256 MiB per coder, 272 MiB per block and 32 nested coders.
- Copy archive entries with Windows device names out under valid file names.
- Use the trusted Subversion executable for remote sessions.

### Changes

- Ask before permanent deletion on shares and removable drives; report when a location has no recycle bin.
- Refuse ambiguous Windows write names and renames that name another folder.
- Refuse Windows backups when the destination cannot keep the file's access list or the account cannot finish the backup.
- Refuse a Windows backup when a conditional access entry cannot be reproduced safely.
- Give the owner the write bit when clearing read-only on Linux and macOS.
- Name the item, operation, cause and available remedies when a local write or backup is refused.

# v2026.10.04.0004

## Text Compare copies after an edit: 0 text errors in 1600 random test sequences

### Fixes

- Stop a copy made right after an edit from duplicating or losing a line.
- Hold the copy and section commands until the comparison of the last edit finishes.
- Let Next Section pass a section that has no line in the active pane.
- Make a section at the top of the file current when a move reaches it.
- Keep the current section after a rule or option change.
- Move only to differences that the display filter shows.
- Stop Next Section from reading every section on each press.
- Show the reason on disabled menu lines, toolbar buttons and dialog controls.
- Add the reason of a disabled control to its accessibility description.
- Give the true reason when a view refuses a copy.
- Enable commands as soon as a background job delivers its result.
- Say that Registry Compare reads export files only on Linux and macOS.

Full changelog: CHANGELOG.md

# v2026.10.03.0003

## AppImage starts on 9 of 9 tested Linux distributions, was 6 of 9

### Fixes

- Start the AppImage on systems with glibc 2.31 and newer.
- Remove the keyboard compose errors at start on Fedora, Arch and openSUSE.
- Give programs that the app starts the environment of the system, not of the AppImage.
- Keep settings out of shared temporary folders when no home folder is set.
- Ignore an empty or relative `XDG_CONFIG_HOME`.
- Show the real cause when the settings folder cannot be written.
- Print the version for `compare-all --version` on Linux and macOS.

### Security

- Restrict temporary copies of archive entries to the current user.
- Harden the check of Subversion host names.

### Changes

- Bundle the C library, the loader and software OpenGL in the AppImage.
- Grow the AppImage to 109 MB.
- Add AppStream metadata to the AppImage.
- Ship license texts and a package list for the bundled libraries.

Full changelog: CHANGELOG.md

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

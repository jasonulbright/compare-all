//! The usage summary.

use std::fmt::Write as _;

use crate::exit;

/// The part of the summary that does not change.
const BODY: &str = "\
ca: compare files and folders from a console.

Usage:
  ca [switches] <left> [<right> [<center> [<output>]]]
  ca @<script file> [switches] [arguments]
  ca /qc[=<type>] <left> <right>
  ca text <left> <right>
  ca folder <left> <right>

A switch may be written /name, -name or --name on every platform. A switch
that takes a value is written name=value. Quote any value that holds a space.

Paths:
  Two paths open a comparison. Three or four open a merge, in the order left,
  right, center, output.

Script:
  @<file>              run the script file. The arguments after it, switches
                       left out, become %1 through %9.

Switches:
  /?, /h, /help        write this summary.
  /version             write the version.
  /automerge           merge without asking, stopping only at a conflict.
  /center=<file>       name the common ancestor file of a merge.
  /closescript         close the script status window when the script ends.
                       The desktop program acts on this.
  /dry-run             build every plan and write nothing.
  /edit                open the file for editing. The desktop program acts
                       on this.
  /expandall           open every subfolder during the first comparison.
  /favorleft           take a non-conflicting change from the left side.
  /favorright          take a non-conflicting change from the right side.
  /filters=<masks>     name masks for the first folder comparison, separated
                       by semicolons.
  /force               write a conflict into the merge output with markers.
  /fv=<type>           open the named view type.
  /fileviewer=<type>   the same switch under its long name.
  /iu                  treat an unimportant difference as no conflict.
  /ignoreunimportant   the same switch under its long name.
  /mergeoutput=<file>  the file a merge result is written to.
  /nobackups           write no backup files for this run.
  /qc[=<type>]         compare two files and return the answer as an exit
                       code. The type is size, crc, binary or rules-based.
                       Rules-based is used when no type is given.
  /quickcompare[=...]  the same switch under its long name.
  /reviewconflicts     open the merge view only when conflicts are left.
                       The desktop program acts on this.
  /ro, /readonly       lock both sides against editing.
  /ro1, /lro, /leftreadonly    lock the left side.
  /ro2, /rro, /rightreadonly   lock the right side.
  /savetarget=<file>   the file the save command writes instead of the
                       original.
  /silent              show no window and ask nothing.
  /solo                start a process of its own. The desktop program acts
                       on this.
  /sync                open the paths in the folder sync view. The desktop
                       program acts on this.
  /title1=<text> .. /title4=<text>    replacement path text, per pane.
  /lefttitle=, /righttitle=, /centertitle=, /outputtitle=    the same four
                       switches under their named forms.
  /vcs1=<path> .. /vcs4=<path>        version control path, per pane.
  /vcsleft=, /vcsright=, /vcscenter=, /vcsoutput=    the same four switches
                       under their named forms.

Subcommands:
  text <left> <right>      write the differing line ranges of two text files.
  folder <left> <right>    write the differing entries of two folders.

This program opens no window. A command line that asks for a view is read in
full and reported, and the desktop program opens it.

Exit codes:
";

/// The whole usage summary, exit code table included.
#[must_use]
pub fn text() -> String {
    let mut out = String::from(BODY);
    for entry in exit::TABLE {
        let _ = writeln!(out, "  {:<4}{}", entry.code, entry.meaning);
    }
    out
}

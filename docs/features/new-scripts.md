# Creating scripts in place

New scripts are made where they belong, from the files panel:

- The **"+" on the panel's title row** creates a script in the working folder
  itself, which has no row of its own.
- The **"+" that appears on a folder row** when you hover it creates one
  inside that folder.
- **New Script…** in a row's [right-click menu](files.md#right-click-menu) —
  inside a folder row, beside a file row.

Either way a dialog asks for the name, a `.sql` extension is forced onto
whatever you type, and the file opens in a tab as soon as it is made — so a
new migration or object script is being edited, not hunted for.

Creation never clobbers: a name that already exists is refused rather than
overwritten.

# Editor and code insight

Use Search Everywhere for mixed destinations, Navigate for declarations,
implementations, symbols and hierarchies, and Find Usages for references. Search
scope and project/module exclusions affect project searches. Include Non-Project
Items expands applicable navigation to interpreter/library indexes. Persistent
and shared indexes are caches; source and the selected SDK remain authoritative.

Completion, parameter information and documentation use the current document and
analysis revision. Auto-import options control popup proposals, preferred import
style, exclusions and unambiguous insertion. Excluded modules are filtered before
insertion. Optimize Imports and Reformat Code have independent actions and preview
paths; preview acceptance still checks the source revision. Formatting respects
configured scopes, markers and existing file policy. Refactor previews must be
reviewed before applying multi-file changes. Read-only or stale destinations must
be resolved instead of being overwritten by an old result.

Settings › Editor contains controls for smart keys, caret/selection behavior,
soft wraps and file masks, line numbers, gutter lanes, sticky lines, breadcrumbs,
code vision and inlay hints. Soft wrapping changes display geometry, not document
newlines. Reader Mode can independently hide line numbers or inlay hints. Parameter
hint blacklist patterns and language toggles are applied to actual hints. Font
fallbacks depend on installed font coverage; a requested family is not a bundled
font. Scheme font overrides and console font choices are separate.

File Types supports pattern and shebang associations, user keywords, ignored
patterns and external application choices. Ignored patterns affect the project
view/index/search scope. File Encodings provides a global default and project or
directory rules; a file's explicit encoding override takes precedence. New-file
BOM and line separator choices apply to new template writes. Existing documents
retain their encoding and separator until explicitly converted. Language Injections
can be disabled globally or per language and extended by file/prefix rules.

Color Scheme has shared syntax attributes and context colors for editor, console,
debugger, diff and VCS views. Import/export supports supported `.icls`, JSON and
bounded scheme JAR content; importing a scheme does not execute the archive.
Duplicate analysis uses the current Python analysis engine with configured minimum
size and anonymization policy. Shared language color categories do not imply a
separate override map for every language grammar.

Markdown has source/preview controls, navigation, fenced code and export workflows.
reStructuredText uses its own source actions. HTML/CSS/JS tools and Emmet are separate
from Python analysis. Data editors and archive views choose their available editor
by file type. Notebook cells use the notebook's document/undo authority; editing
JSON and cells is not two independent documents. See [scientific mode](../scientific-mode.md).

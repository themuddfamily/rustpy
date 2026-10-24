//! Action system and keymaps (roadmap §0.3, §5.1 Keymap).
//!
//! Every user-invokable command is an [`Action`] with a stable id, a menu
//! text and default shortcuts in the *Default* (Windows/Linux) keymap.

#[path = "actions/reference.rs"]
mod reference;
#[path = "actions/shortcuts.rs"]
pub(crate) mod shortcuts;

use std::fmt;

use egui::{Key, Modifiers};
use rustc_hash::FxHashMap;

macro_rules! actions {
    ($( $variant:ident => ($id:literal, $text:literal, [$($chord:literal),*]) ),* $(,)?) => {
        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Action { $( $variant ),* }

        impl Action {
            pub const ALL: &'static [Action] = &[ $( Action::$variant ),* ];

            /// Stable identifier (used in keymap files), e.g. `EditorUndo`.
            pub fn id(self) -> &'static str {
                match self { $( Action::$variant => $id ),* }
            }
            /// Human readable text for menus and *Find Action*, in the UI language
            /// (the English text when it has no translation).
            pub fn text(self) -> &'static str {
                rustpy_i18n::tr_static(self.english_text())
            }
            /// The English text: the translation key, also matched by *Find Action*.
            pub fn english_text(self) -> &'static str {
                match self { $( Action::$variant => $text ),* }
            }
            /// Default-keymap shortcuts as strings like `"Ctrl+Shift+Z"`.
            pub fn default_shortcuts(self) -> &'static [&'static str] {
                match self { $( Action::$variant => &[ $( $chord ),* ] ),* }
            }
        }
    };
}

actions! {
    // ----- File ------------------------------------------------------------
    NewFile => ("NewFile", "New File…", ["Ctrl+Alt+Insert"]),
    NewScratchFile => ("NewScratchFile", "New Scratch File", ["Ctrl+Alt+Shift+Insert"]),
    NewScratchBuffer => ("ScratchNewBuffer", "New Scratch Buffer", []),
    SaveFileAsTemplate => ("SaveFileAsTemplate", "Save File as Template…", []),
    ShowProjectWindows => ("ShowProjectWindows", "Project Windows…", []),
    NextProjectWindow => ("NextProjectWindow", "Next Project Window", []),
    PreviousProjectWindow => ("PreviousProjectWindow", "Previous Project Window", []),
    MergeAllProjectWindows => ("MergeAllProjectWindows", "Merge All Project Windows", []),
    OpenProject => ("OpenFile", "Open…", ["Ctrl+O"]),
    NewProject => ("NewProject", "New Project…", []),
    CloseProject => ("CloseProject", "Close Project", []),
    GetFromVcs => ("Vcs.Checkout", "Get from Version Control…", []),
    Save => ("SaveDocument", "Save", ["Ctrl+S"]),
    SaveAll => ("SaveAll", "Save All", ["Ctrl+Shift+S"]),
    ToggleReadOnly => ("ToggleReadOnlyAttribute", "Toggle Read-Only Attribute", []),
    MakeWritable => ("MakeFileWritable", "Make Writable…", []),
    CloseEditor => ("CloseContent", "Close Tab", ["Ctrl+F4"]),
    CloseAllEditors => ("CloseAllEditors", "Close All Tabs", []),
    CloseOtherEditors => ("CloseAllEditorsButActive", "Close Other Tabs", []),
    CloseAllButPinned => ("CloseAllUnpinnedEditors", "Close All but Pinned", []),
    CloseTabsToTheLeft => ("CloseAllToTheLeft", "Close Tabs to the Left", []),
    CloseTabsToTheRight => ("CloseAllToTheRight", "Close Tabs to the Right", []),
    SortTabsByFilename => ("SortTabsByFilename", "Sort Tabs by File Name", []),
    PinActiveTab => ("PinActiveEditorTab", "Pin / Unpin Tab", []),
    ReopenClosedTab => ("ReopenClosedTab", "Reopen Closed Tab", []),
    Settings => ("ShowSettings", "Settings…", ["Ctrl+Alt+S"]),
    Plugins => ("WelcomeScreen.Plugins", "Plugins…", []),
    Exit => ("Exit", "Exit", []),
    // ----- Edit ------------------------------------------------------------
    Undo => ("$Undo", "Undo", ["Ctrl+Z"]),
    Redo => ("$Redo", "Redo", ["Ctrl+Shift+Z"]),
    UndoHistory => ("UndoHistory", "Undo History…", []),
    Cut => ("$Cut", "Cut", ["Ctrl+X", "Shift+Delete"]),
    Copy => ("$Copy", "Copy", ["Ctrl+C", "Ctrl+Insert"]),
    Paste => ("$Paste", "Paste", ["Ctrl+V", "Shift+Insert"]),
    PasteFromHistory => ("PasteMultiple", "Paste from History…", ["Ctrl+Shift+V"]),
    CopyPath => ("CopyPaths", "Copy Path…", ["Ctrl+Shift+C"]),
    CopyReference => ("CopyReference", "Copy Reference", ["Ctrl+Alt+Shift+C"]),
    LocalHistory => ("LocalHistory.ShowHistory", "Show Local History", []),
    ReloadFromDisk => ("ReloadFromDisk", "Reload from Disk", []),
    Synchronize => ("Synchronize", "Reload All from Disk", ["Ctrl+Alt+Y"]),
    InvalidateCaches => ("InvalidateCaches", "Invalidate Caches…", []),
    TogglePowerSaveMode => ("TogglePowerSave", "Power Save Mode", []),
    ShowBackgroundTasks => ("ShowBackgroundTasks", "Background Tasks", []),
    StartStopMacroRecording => ("StartStopMacroRecording", "Start/Stop Macro Recording", []),
    PlaybackLastMacro => ("PlaybackLastMacro", "Play Back Last Macro", []),
    EditMacros => ("EditMacros", "Edit Macros…", []),
    PasteSimple => ("PasteSimple", "Paste as Plain Text", ["Ctrl+Alt+Shift+V"]),
    Annotate => ("Annotate", "Annotate with Git Blame", []),
    ShowFileHistory => ("Vcs.ShowTabbedFileHistory", "Show History", []),
    ToggleOtherVcsView => ("Vcs.OtherProviders", "Mercurial / SVN / Perforce", []),
    StashChanges => ("Git.Stash", "Stash Changes…", []),
    AddToGitignore => ("Git.Ignore", "Add to .gitignore", []),
    GitHubPullRequests => ("Github.PullRequests.ToolWindow", "Pull Requests", []),
    GitHubAccounts => ("Github.Accounts", "GitHub Accounts…", []),
    GitHubShareProject => ("Github.ShareProject", "Share Project on GitHub…", []),
    GitHubCreateGist => ("Github.CreateGist", "Create Gist…", []),
    GitHubCreatePullRequest => ("Github.CreatePullRequest", "Create Pull Request…", []),
    GitHubCloneRepo => ("Github.Clone", "Clone from GitHub…", []),
    GitHubOpenOnWeb => ("Github.OpenOnWeb", "Open on GitHub", []),
    GitHubOpenIssue => ("Github.OpenIssue", "Open GitHub Issue", []),
    GitBranches => ("Git.Branches", "Branches…", ["Ctrl+Shift+Backquote"]),
    TypeHierarchy => ("TypeHierarchy", "Type Hierarchy", ["Ctrl+H"]),
    MethodHierarchy => ("MethodHierarchy", "Method Hierarchy", ["Ctrl+Shift+H"]),
    CallHierarchy => ("CallHierarchy", "Call Hierarchy", ["Ctrl+Alt+H"]),
    GotoImplementation => ("GotoImplementation", "Go to Implementation(s)", ["Ctrl+Alt+B"]),
    GotoSuperMethod => ("GotoSuperMethod", "Go to Super Method", ["Ctrl+U"]),
    ToggleHierarchyView => ("ActivateHierarchyToolWindow", "Hierarchy", []),
    CompareWithClipboard => ("CompareClipboardWithSelection", "Compare with Clipboard", []),
    CompareFiles => ("CompareTwoFiles", "Compare File with…", []),
    ToggleMarkdownPreview => ("Markdown.Layout.Toggle", "Markdown: Cycle Preview Layout", []),
    ToggleRstPreview => ("Rst.Layout.Toggle", "reStructuredText: Cycle Preview Layout", []),
    SelectAll => ("$SelectAll", "Select All", ["Ctrl+A"]),
    ExtendSelection => ("EditorSelectWord", "Extend Selection", ["Ctrl+W"]),
    ShrinkSelection => ("EditorUnSelectWord", "Shrink Selection", ["Ctrl+Shift+W"]),
    AddSelectionForNextOccurrence => ("SelectNextOccurrence", "Add Selection for Next Occurrence", ["Alt+J"]),
    UnselectOccurrence => ("UnselectPreviousOccurrence", "Unselect Occurrence", ["Alt+Shift+J"]),
    SelectAllOccurrences => ("SelectAllOccurrences", "Select All Occurrences", ["Ctrl+Alt+Shift+J"]),
    // Press Ctrl twice and hold it, then Up/Down (see `EditorView::clones_caret`).
    AddCaretAbove => ("EditorCloneCaretAbove", "Clone Caret Above", []),
    AddCaretBelow => ("EditorCloneCaretBelow", "Clone Caret Below", []),
    ToggleStickySelection => ("EditorToggleStickySelection", "Sticky Selection", []),
    ToggleColumnSelectionMode => ("EditorToggleColumnMode", "Column Selection Mode", ["Alt+Shift+Insert"]),
    DuplicateLine => ("EditorDuplicate", "Duplicate Line or Selection", ["Ctrl+D"]),
    DeleteLine => ("EditorDeleteLine", "Delete Line", ["Ctrl+Y"]),
    JoinLines => ("EditorJoinLines", "Join Lines", ["Ctrl+Shift+J"]),
    MoveLineUp => ("MoveLineUp", "Move Line Up", ["Alt+Shift+ArrowUp"]),
    MoveLineDown => ("MoveLineDown", "Move Line Down", ["Alt+Shift+ArrowDown"]),
    MoveStatementUp => ("MoveStatementUp", "Move Statement Up", ["Ctrl+Shift+ArrowUp"]),
    MoveStatementDown => ("MoveStatementDown", "Move Statement Down", ["Ctrl+Shift+ArrowDown"]),
    CommentLine => ("CommentByLineComment", "Comment with Line Comment", ["Ctrl+Slash"]),
    CommentBlock => ("CommentByBlockComment", "Comment with Block Comment", ["Ctrl+Shift+Slash"]),
    MoveElementLeft => ("MoveElementLeft", "Move Element Left", ["Ctrl+Alt+Shift+ArrowLeft"]),
    MoveElementRight => ("MoveElementRight", "Move Element Right", ["Ctrl+Alt+Shift+ArrowRight"]),
    SortLines => ("SortLines", "Sort Lines", []),
    ReverseLines => ("ReverseLines", "Reverse Lines", []),
    RemoveDuplicateLines => ("RemoveDuplicateLines", "Remove Duplicate Lines", []),
    TrimTrailingWhitespace => ("StripTrailingSpaces", "Strip Trailing Spaces", []),
    ConvertIndentsToSpaces => ("ConvertIndentsToSpaces", "Convert Indents to Spaces", []),
    ConvertIndentsToTabs => ("ConvertIndentsToTabs", "Convert Indents to Tabs", []),
    ToggleCase => ("EditorToggleCase", "Toggle Case", ["Ctrl+Shift+U"]),
    IndentSelection => ("EditorIndentSelection", "Indent Selection", ["Tab"]),
    UnindentSelection => ("EditorUnindentSelection", "Unindent Line or Selection", ["Shift+Tab"]),
    CompleteStatement => ("EditorCompleteStatement", "Complete Current Statement", ["Ctrl+Shift+Enter"]),
    Unwrap => ("Unwrap", "Unwrap/Remove…", ["Ctrl+Shift+Delete"]),
    ContextInfo => ("EditorContextInfo", "Context Info", ["Alt+Q"]),
    QuickDefinition => ("QuickImplementations", "Quick Definition", ["Ctrl+Shift+I"]),
    HippieCompletion => ("HippieCompletion", "Cyclic Expand Word", ["Alt+Slash"]),
    StartNewLine => ("EditorStartNewLine", "Start New Line", ["Shift+Enter"]),
    StartNewLineBefore => ("EditorStartNewLineBefore", "Start New Line Before Current", ["Ctrl+Alt+Enter"]),
    DeleteToWordStart => ("EditorDeleteToWordStart", "Delete to Word Start", ["Ctrl+Backspace"]),
    DeleteToWordEnd => ("EditorDeleteToWordEnd", "Delete to Word End", ["Ctrl+Delete"]),
    DeleteToLineStart => ("EditorDeleteToLineStart", "Delete to Line Start", []),
    DeleteToLineEnd => ("EditorDeleteToLineEnd", "Delete to Line End", []),
    ReformatCode => ("ReformatCode", "Reformat Code", ["Ctrl+Alt+L"]),
    ReformatFile => ("ReformatFile", "Reformat File…", ["Ctrl+Alt+Shift+L"]),
    RearrangeCode => ("RearrangeCode", "Rearrange Code", []),
    InspectCode => ("InspectCode", "Inspect Code…", []),
    RunInspectionByName => ("RunInspectionByName", "Run Inspection by Name…", []),
    SilentCodeCleanup => ("SilentCodeCleanup", "Silent Code Cleanup", []),
    CodeCleanup => ("CodeCleanup", "Code Cleanup…", []),
    InjectLanguage => ("InjectLanguage", "Inject language or reference", []),
    EditInjectedFragment => ("EditInjectedFragment", "Edit injected fragment", []),
    GoToInjectedReference => ("GoToInjectedReference", "Go to injected file reference", []),
    CheckRegExp => ("CheckRegExp", "Check RegExp…", []),
    SmartTypeCompletion => ("SmartTypeCompletion", "Smart Type Completion", ["Ctrl+Shift+Space"]),
    OpenWebPreview => ("WebPreview.Open", "Open in Built-in Web Preview", []),
    ToggleWebPreview => ("WebPreview.Toggle", "Web Preview Tool Window", []),
    OpenUrl => ("WebOpenUrl", "Open URL…", []),
    OpenInBrowser => ("WebOpenInAction", "Open in Browser", ["Alt+F2"]),
    BuildSphinxDocs => ("Sphinx.Build", "Build Sphinx Docs", []),
    RunDocutilsTask => ("Docutils.Run", "Run Docutils Task", []),
    OpenDocumentationPreview => ("Documentation.OpenPreview", "Open Generated Documentation", []),
    Frameworks => ("Frameworks.Workspace", "Frameworks / Django Structure…", []),
    RunManageTask => ("Django.RunManageTask", "Run manage.py Task…", ["Ctrl+Alt+R"]),
    ToggleSemanticHighlighting => ("ToggleSemanticHighlighting", "Semantic Highlighting (rainbow variables)", []),
    TrustProject => ("TrustProject", "Trust Project", []),
    ShowInotifyLimitInstructions => ("ShowInotifyLimitInstructions", "Inotify Watches Limit Instructions…", []),
    HideInotifyLimitWarning => ("HideInotifyLimitWarning", "Don't Show File Watcher Limit Warning", []),
    AddRemoteInterpreter => ("AddRemoteInterpreter", "Add Remote Interpreter…", []),
    QuickList => ("QuickList", "Quick List", ["Ctrl+Alt+Shift+Q"]),
    CreateLauncherScript => ("CreateLauncherScript", "Create Command-line Launcher…", []),
    ShowLogInFiles => ("ShowLogInFiles", "Show Log in Files", []),
    CollectDiagnostics => ("CollectDiagnostics", "Collect Diagnostic Data", []),
    CustomizeToolbar => ("CustomizeToolbar", "Customize Toolbar and Window…", []),
    ZoomIdeIn => ("ZoomIDEIn", "Zoom IDE In", ["Ctrl+Equals"]),
    ZoomIdeOut => ("ZoomIDEOut", "Zoom IDE Out", ["Ctrl+Minus"]),
    ZoomIdeReset => ("ZoomIDEReset", "Reset IDE Scale", []),
    IdeActivityMonitor => ("IdeActivityMonitor", "IDE Activity Monitor", []),
    DumpIdeThreads => ("DumpIdeThreads", "Dump IDE Threads", []),
    ProfileIdeCpu => ("ProfileIdeCpu", "Profile IDE CPU", []),
    ReportIdeIssue => ("ReportIdeIssue", "Report a RustPy Issue", []),
    AnalyzeStackTrace => ("AnalyzeStackTrace", "Analyze Stack Trace or Thread Dump…", []),
    DebugLogSettings => ("DebugLogSettings", "Debug Log Settings…", []),
    LearnIdeFeatures => ("ShowLearnPanel", "Learn IDE Features", []),
    DeploymentConfiguration => ("Deployment.Configuration", "Configuration…", []),
    DeploymentUpload => ("PublishAction", "Upload to Default Server", ["Ctrl+Alt+Shift+X"]),
    DeploymentDownload => ("Deployment.Download", "Download from Default Server", []),
    DeploymentSync => ("Deployment.Sync", "Sync with Deployed…", []),
    DeploymentBrowse => ("Deployment.BrowseRemoteHost", "Browse Remote Host", []),
    CopyAsRichText => ("CopyAsRichText", "Copy as Rich Text", []),
    Collaborate => ("Collaboration.Session", "Collaborate…", []),
    AiInlineCompletion => ("AIAssistant.Editor.InlineCompletion", "Request AI Inline Completion", []),
    ToggleAiAssistant => ("AIAssistant.ToolWindow", "AI Assistant", []),
    AiExplainCode => ("AIAssistant.Editor.ExplainCode", "Explain Code", []),
    AiSuggestRefactoring => ("AIAssistant.Editor.SuggestRefactoring", "Suggest Refactoring", []),
    AiWriteTests => ("AIAssistant.Editor.GenerateTests", "Generate Unit Tests", []),
    AiGenerateDocstring => ("AIAssistant.Editor.GenerateDocumentation", "Generate Documentation", []),
    AiFixError => ("AIAssistant.Editor.FixError", "Fix with AI", []),
    CopyAsPlainText => ("CopyAsPlainText", "Copy as Plain Text", []),
    OpenTask => ("tasks.goto", "Open Task…", ["Alt+Shift+N"]),
    ExportSettings => ("ExportSettings", "Export Settings…", []),
    SettingsSync => ("SettingsSync", "Settings Sync…", []),
    SettingsRepository => ("SettingsRepository", "Settings Repository…", []),
    ImportSettings => ("ImportSettings", "Import Settings…", []),
    RestoreDefaultSettings => ("RestoreDefaultSettings", "Restore Default Settings…", []),
    NewProjectSettings => ("Settings.NewProjects", "Settings for New Projects…", []),
    DialogPreferences => ("Settings.Dialogs", "Popup and Dialog Options…", []),
    RepairIde => ("Maintenance.RepairIde", "Repair IDE…", []),
    AdvancedRegistry => ("Maintenance.Registry", "Registry…", []),
    UiInspector => ("Internal.UiInspector", "UI Inspector…", []),
    RestartIde => ("Maintenance.Restart", "Restart IDE…", []),
    SwitchTask => ("tasks.switch", "Switch Task…", ["Alt+Shift+T"]),
    CloseActiveTask => ("tasks.close", "Close Active Task…", []),
    SaveContext => ("context.save", "Save Context…", ["Alt+Shift+S"]),
    LoadContext => ("context.load", "Load Context…", ["Alt+Shift+L"]),
    ClearContext => ("context.clear", "Clear Context", ["Alt+Shift+X"]),
    GenerateNativeStub => ("GenerateNativeStub", "Generate stub for native module", []),
    GenerateDocstring => ("GenerateDocstring", "Generate Docstring", []),
    UpdateCopyright => ("UpdateCopyright", "Update Copyright", []),
    CopyFile => ("CopyElement", "Copy File…", ["F5"]),
    ShowDjangoDiagram => ("Diagrams.DjangoModels", "Show Django Model Diagram", []),
    ShowErDiagram => ("Diagrams.DatabaseER", "Show Database ER Diagram", []),
    ShowUmlDiagram => ("ShowUmlDiagram", "Show Diagram (classes)", ["Ctrl+Alt+Shift+U"]),
    ToggleDiagramView => ("ActivateDiagramToolWindow", "Diagram", []),
    ToggleEndpointsView => ("ActivateEndpointsToolWindow", "Endpoints", []),
    FillParagraph => ("FillParagraph", "Fill Paragraph", []),
    OptimizeImports => ("OptimizeImports", "Optimize Imports", ["Ctrl+Alt+O"]),
    ShowIntentionActions => ("ShowIntentionActions", "Show Context Actions", ["Alt+Enter"]),
    CodeCompletion => ("CodeCompletion", "Basic Completion", ["Ctrl+Space"]),
    ParameterInfo => ("ParameterInfo", "Parameter Info", ["Ctrl+P"]),
    WebDevelopment => ("Web.Development", "Web Development / Language Services…", []),
    DataFileTools => ("DataFile.Tools", "Data File Tools…", []),
    CopyJsonPath => ("Json.CopyPath", "Copy JSON Path", []),
    QuickDocumentation => ("QuickJavaDoc", "Quick Documentation", ["Ctrl+Q"]),
    ExternalDocumentation => ("ExternalJavaDoc", "External Documentation", ["Shift+F1"]),
    EmmetPreview => ("EmmetPreview", "Emmet Preview…", []),
    SurroundWithEmmet => ("SurroundWithEmmet", "Surround with Emmet…", []),
    SurroundWith => ("SurroundWith", "Surround With…", ["Ctrl+Alt+T"]),
    RunAllTests => ("RunAllTests", "Run All Tests", []),
    RunDoctests => ("RunDoctests", "Run Doctests in Current File", []),
    DetachTab => ("DetachEditor", "Detach Tab to Window", []),
    ReattachTab => ("ReattachEditor", "Reattach Detached Tab", []),
    RunWithCoverage => ("Coverage", "Run with Coverage", []),
    RunWithConcurrencyDiagram => ("RunWithConcurrencyDiagram", "Run with Concurrency Diagram", []),
    ToggleConcurrencyView => ("ActivateConcurrencyToolWindow", "Concurrency Diagram", []),
    ProfileRun => ("Profile", "Profile", []),
    ToggleCoverageView => ("ActivateCoverageToolWindow", "Coverage", []),
    ToggleProfilerView => ("ActivateProfilerToolWindow", "Profiler", []),
    InstallRequirements => ("InstallRequirements", "Install Requirements", []),
    RunTestsInFile => ("RunTestsInFile", "Run Tests in Current File", []),
    RerunFailedTests => ("RerunFailedTests", "Rerun Failed Tests", []),
    ToggleTestsView => ("ActivateTestsToolWindow", "Tests", []),
    SaveSelectionAsLiveTemplate => ("SaveSelectionAsLiveTemplate", "Save Selection as Live Template…", []),
    InsertLiveTemplate => ("InsertLiveTemplate", "Insert Live Template…", ["Ctrl+J"]),
    Generate => ("Generate", "Generate…", ["Alt+Insert"]),
    Rename => ("RenameElement", "Rename…", ["Shift+F6"]),
    RefactorThis => ("Refactorings.QuickListPopupAction", "Refactor This…", ["Ctrl+Alt+Shift+T"]),
    ExtractVariable => ("IntroduceVariable", "Extract Variable…", ["Ctrl+Alt+V"]),
    ExtractConstant => ("IntroduceConstant", "Extract Constant…", ["Ctrl+Alt+C"]),
    ExtractField => ("IntroduceField", "Extract Field…", ["Ctrl+Alt+F"]),
    ExtractParameter => ("IntroduceParameter", "Extract Parameter…", ["Ctrl+Alt+P"]),
    ExtractMethod => ("ExtractMethod", "Extract Method…", ["Ctrl+Alt+M"]),
    Inline => ("Inline", "Inline…", ["Ctrl+Alt+N"]),
    SafeDelete => ("SafeDelete", "Safe Delete…", ["Alt+Delete"]),
    ChangeSignature => ("ChangeSignature", "Change Signature…", ["Ctrl+F6"]),
    MoveRefactoring => ("Move", "Move…", ["F6"]),
    MoveSymbol => ("MoveSymbolToModule", "Move Symbol to Module…", []),
    RenameModule => ("RenameModule", "Rename Module…", []),
    PullMembersUp => ("MembersPullUp", "Pull Members Up…", []),
    PushMembersDown => ("MemberPushDown", "Push Members Down…", []),
    ExtractSuperclass => ("ExtractSuperclass", "Extract Superclass…", []),
    MakeAbstract => ("MakeAbstract", "Make Method Abstract", []),
    ImplementAbstractMethods => ("ImplementMethods", "Implement Abstract Methods", ["Ctrl+I"]),
    ConvertToDataclass => ("ConvertToDataclass", "Convert to Dataclass", []),
    AddTypeHints => ("AddTypeHints", "Add Type Hints", []),
    IntroduceTypeAlias => ("IntroduceTypeAlias", "Introduce Type Alias", []),
    InlineTypeAlias => ("InlineTypeAlias", "Inline Type Alias", []),
    ConvertRefactoring => ("ConvertRefactoring", "Convert…", []),
    ConvertToPackage => ("ConvertToPackage", "Convert to Package", []),
    ConvertToModule => ("ConvertToModule", "Convert to Module", []),
    // ----- Find ------------------------------------------------------------
    Find => ("Find", "Find…", ["Ctrl+F"]),
    Replace => ("Replace", "Replace…", ["Ctrl+R"]),
    FindNext => ("FindNext", "Find Next / Move to Next Occurrence", ["F3"]),
    FindPrevious => ("FindPrevious", "Find Previous / Move to Previous Occurrence", ["Shift+F3"]),
    FindWordAtCaret => ("FindWordAtCaret", "Find Word at Caret", ["Ctrl+F3"]),
    FindInFiles => ("FindInPath", "Find in Files…", ["Ctrl+Shift+F"]),
    SearchStructurally => ("StructuralSearchPlugin.StructuralSearchAction", "Search Structurally…", []),
    ReplaceStructurally => ("StructuralSearchPlugin.StructuralReplaceAction", "Replace Structurally…", []),
    ReplaceInFiles => ("ReplaceInPath", "Replace in Files…", ["Ctrl+Shift+R"]),
    FindUsages => ("FindUsages", "Find Usages", ["Alt+F7"]),
    FindUsagesInFile => ("FindUsagesInFile", "Find Usages in File", ["Alt+Shift+F7"]),
    FindUsagesSettings => ("FindUsagesSettings", "Find Usages Settings", []),
    ShowUsages => ("ShowUsages", "Show Usages", ["Ctrl+Alt+F7"]),
    HighlightUsagesInFile => ("HighlightUsagesInFile", "Highlight Usages in File", ["Ctrl+Shift+F7"]),
    // ----- Navigate --------------------------------------------------------
    SearchEverywhere => ("SearchEverywhere", "Search Everywhere", ["Shift+Shift"]),
    FindAction => ("GotoAction", "Find Action…", ["Ctrl+Shift+A"]),
    GoToClass => ("GotoClass", "Go to Class…", ["Ctrl+N"]),
    GoToFile => ("GotoFile", "Go to File…", ["Ctrl+Shift+N"]),
    GoToSymbol => ("GotoSymbol", "Go to Symbol…", ["Ctrl+Alt+Shift+N"]),
    GoToLine => ("GotoLine", "Go to Line:Column…", ["Ctrl+G"]),
    GoToDeclaration => ("GotoDeclaration", "Go to Declaration or Usages", ["Ctrl+B"]),
    GoToSourceOrStub => ("GotoSourceOrStub", "Go to Source / Stub", []),
    GoToTypeDeclaration => ("GotoTypeDeclaration", "Go to Type Declaration", ["Ctrl+Shift+B"]),
    GoToTest => ("GotoTest", "Go to Test", ["Ctrl+Shift+T"]),
    FileStructure => ("FileStructurePopup", "File Structure", ["Ctrl+F12"]),
    RecentFiles => ("RecentFiles", "Recent Files", ["Ctrl+E"]),
    RecentlyChangedFiles => ("RecentlyChangedFiles", "Recently Changed Files", []),
    RecentlyEditedFiles => ("RecentlyEditedFiles", "Recently Edited Files", []),
    RecentLocations => ("RecentLocations", "Recent Locations", ["Ctrl+Shift+E"]),
    Switcher => ("Switcher", "Switcher", ["Ctrl+Tab"]),
    NavigateBack => ("Back", "Back", ["Ctrl+Alt+ArrowLeft"]),
    NavigateForward => ("Forward", "Forward", ["Ctrl+Alt+ArrowRight"]),
    LastEditLocation => ("JumpToLastChange", "Last Edit Location", ["Ctrl+Shift+Backspace"]),
    ShowErrorDescription => ("ShowErrorDescription", "Show Error Description", ["Ctrl+F1"]),
    NextError => ("GotoNextError", "Next Highlighted Error", ["F2"]),
    PreviousError => ("GotoPreviousError", "Previous Highlighted Error", ["Shift+F2"]),
    NextMethod => ("MethodDown", "Next Method", ["Alt+ArrowDown"]),
    PreviousMethod => ("MethodUp", "Previous Method", ["Alt+ArrowUp"]),
    NextTab => ("NextTab", "Select Next Tab", ["Alt+ArrowRight"]),
    PreviousTab => ("PreviousTab", "Select Previous Tab", ["Alt+ArrowLeft"]),
    MatchingBrace => ("EditorMatchBrace", "Move Caret to Matching Brace", ["Ctrl+Shift+M"]),
    ScrollToCenter => ("EditorScrollToCenter", "Scroll to Center", ["Ctrl+M"]),
    CodeBlockStart => ("EditorCodeBlockStart", "Move Caret to Code Block Start", ["Ctrl+OpenBracket"]),
    CodeBlockEnd => ("EditorCodeBlockEnd", "Move Caret to Code Block End", ["Ctrl+CloseBracket"]),
    ToggleBookmark => ("ToggleBookmark", "Toggle Bookmark", ["F11"]),
    ToggleBookmarkWithMnemonic => ("ToggleBookmarkWithMnemonic", "Toggle Bookmark with Mnemonic", ["Ctrl+F11"]),
    GoToBookmark0 => ("GotoBookmark0", "Go to Bookmark 0", ["Ctrl+Num0"]),
    GoToBookmark1 => ("GotoBookmark1", "Go to Bookmark 1", ["Ctrl+Num1"]),
    GoToBookmark2 => ("GotoBookmark2", "Go to Bookmark 2", ["Ctrl+Num2"]),
    GoToBookmark3 => ("GotoBookmark3", "Go to Bookmark 3", ["Ctrl+Num3"]),
    GoToBookmark4 => ("GotoBookmark4", "Go to Bookmark 4", ["Ctrl+Num4"]),
    GoToBookmark5 => ("GotoBookmark5", "Go to Bookmark 5", ["Ctrl+Num5"]),
    GoToBookmark6 => ("GotoBookmark6", "Go to Bookmark 6", ["Ctrl+Num6"]),
    GoToBookmark7 => ("GotoBookmark7", "Go to Bookmark 7", ["Ctrl+Num7"]),
    GoToBookmark8 => ("GotoBookmark8", "Go to Bookmark 8", ["Ctrl+Num8"]),
    GoToBookmark9 => ("GotoBookmark9", "Go to Bookmark 9", ["Ctrl+Num9"]),
    NextBookmark => ("GotoNextBookmark", "Next Bookmark", []),
    PreviousBookmark => ("GotoPreviousBookmark", "Previous Bookmark", []),
    ShowBookmarks => ("ShowBookmarks", "Show Bookmarks", ["Shift+F11"]),
    SelectInProjectView => ("SelectIn", "Select In Project View", []),
    SelectIn => ("SelectInPopup", "Select In…", ["Alt+F1"]),
    GoToRelatedSymbol => ("GotoRelated", "Go to Related Symbol", ["Ctrl+Alt+Home"]),
    JumpToSource => ("EditSource", "Jump to Source", ["F4"]),
    OpenInRightSplit => ("OpenInRightSplit", "Open in Right Split", []),
    // ----- Caret movement (handled by the editor) ----------------------------
    CaretLeft => ("EditorLeft", "Left", ["ArrowLeft"]),
    CaretRight => ("EditorRight", "Right", ["ArrowRight"]),
    CaretUp => ("EditorUp", "Up", ["ArrowUp"]),
    CaretDown => ("EditorDown", "Down", ["ArrowDown"]),
    CaretWordLeft => ("EditorPreviousWord", "Move Caret to Previous Word", ["Ctrl+ArrowLeft"]),
    CaretWordRight => ("EditorNextWord", "Move Caret to Next Word", ["Ctrl+ArrowRight"]),
    CaretLineStart => ("EditorLineStart", "Move Caret to Line Start", ["Home"]),
    CaretLineEnd => ("EditorLineEnd", "Move Caret to Line End", ["End"]),
    CaretPageUp => ("EditorPageUp", "Page Up", ["PageUp"]),
    CaretPageDown => ("EditorPageDown", "Page Down", ["PageDown"]),
    CaretTextStart => ("EditorTextStart", "Move Caret to Text Start", ["Ctrl+Home"]),
    CaretTextEnd => ("EditorTextEnd", "Move Caret to Text End", ["Ctrl+End"]),
    SelectLeft => ("EditorLeftWithSelection", "Left with Selection", ["Shift+ArrowLeft"]),
    SelectRight => ("EditorRightWithSelection", "Right with Selection", ["Shift+ArrowRight"]),
    SelectUp => ("EditorUpWithSelection", "Up with Selection", ["Shift+ArrowUp"]),
    SelectDown => ("EditorDownWithSelection", "Down with Selection", ["Shift+ArrowDown"]),
    SelectWordLeft => ("EditorPreviousWordWithSelection", "Move Caret to Previous Word with Selection", ["Ctrl+Shift+ArrowLeft"]),
    SelectWordRight => ("EditorNextWordWithSelection", "Move Caret to Next Word with Selection", ["Ctrl+Shift+ArrowRight"]),
    SelectLineStart => ("EditorLineStartWithSelection", "Move Caret to Line Start with Selection", ["Shift+Home"]),
    SelectLineEnd => ("EditorLineEndWithSelection", "Move Caret to Line End with Selection", ["Shift+End"]),
    SelectPageUp => ("EditorPageUpWithSelection", "Page Up with Selection", ["Shift+PageUp"]),
    SelectPageDown => ("EditorPageDownWithSelection", "Page Down with Selection", ["Shift+PageDown"]),
    SelectTextStart => ("EditorTextStartWithSelection", "Move Caret to Text Start with Selection", ["Ctrl+Shift+Home"]),
    SelectTextEnd => ("EditorTextEndWithSelection", "Move Caret to Text End with Selection", ["Ctrl+Shift+End"]),
    ScrollUp => ("EditorScrollUp", "Scroll Up", ["Ctrl+ArrowUp"]),
    ScrollDown => ("EditorScrollDown", "Scroll Down", ["Ctrl+ArrowDown"]),
    Backspace => ("EditorBackSpace", "Backspace", ["Backspace", "Shift+Backspace"]),
    Delete => ("EditorDelete", "Delete", ["Delete"]),
    Enter => ("EditorEnter", "Enter", ["Enter"]),
    Escape => ("EditorEscape", "Escape", ["Escape"]),
    EmacsKillLine => ("EditorEmacsKillLine", "Emacs: Kill Line", []),
    EmacsKillWordForward => ("EditorEmacsKillWordForward", "Emacs: Kill Word Forward", []),
    EmacsKillWordBackward => ("EditorEmacsKillWordBackward", "Emacs: Kill Word Backward", []),
    EmacsKillRegion => ("EditorEmacsKillRegion", "Emacs: Kill Region", []),
    EmacsCopyRegion => ("EditorEmacsCopyRegion", "Emacs: Copy Region to Kill Ring", []),
    EmacsYank => ("EditorEmacsYank", "Emacs: Yank", []),
    EmacsYankPop => ("EditorEmacsYankPop", "Emacs: Yank Pop", []),
    EmacsTranspose => ("EditorEmacsTranspose", "Emacs: Transpose Characters", []),
    ToggleInsertState => ("EditorToggleInsertState", "Toggle Insert/Overwrite", ["Insert"]),
    EditorLineActions => ("EditorLineActions", "Editor Line Actions", ["Ctrl+Alt+G"]),
    ToggleFold => ("CollapseRegion", "Collapse/Expand Region", ["Ctrl+Period"]),
    ExpandRegion => ("ExpandRegion", "Expand", ["Ctrl+Alt+Equals"]),
    CollapseRegion => ("CollapseRegionAlt", "Collapse", ["Ctrl+Alt+Minus"]),
    ExpandAll => ("ExpandAllRegions", "Expand All", ["Ctrl+Shift+Equals"]),
    CollapseAll => ("CollapseAllRegions", "Collapse All", ["Ctrl+Shift+Minus"]),
    NextDocstringSection => ("NextDocstringSection", "Next Docstring Section", []),
    PreviousDocstringSection => ("PreviousDocstringSection", "Previous Docstring Section", []),
    ToggleRenderedDoc => ("ToggleRenderedDocPresentation", "Toggle Rendered View", ["Ctrl+Alt+Q"]),
    ToggleRenderedDocAll => ("ToggleRenderedDocPresentationForAll", "Render All Doc Comments", []),
    ToggleReaderMode => ("ToggleReaderMode", "Reader Mode", []),
    // ----- View --------------------------------------------------------------
    ToggleProjectView => ("ActivateProjectToolWindow", "Project", ["Alt+Num1"]),
    ToggleCommitView => ("ActivateCommitToolWindow", "Commit", ["Alt+Num0"]),
    ToggleFindView => ("ActivateFindToolWindow", "Find", ["Alt+Num3"]),
    ToggleRunView => ("ActivateRunToolWindow", "Run", ["Alt+Num4"]),
    ToggleDebugView => ("ActivateDebugToolWindow", "Debug", ["Alt+Num5"]),
    ToggleProblemsView => ("ActivateProblemsViewToolWindow", "Problems", ["Alt+Num6"]),
    ToggleStructureView => ("ActivateStructureToolWindow", "Structure", ["Alt+Num7"]),
    ToggleHttpClientView => ("ActivateHTTPClientToolWindow", "HTTP Client", []),
    ToggleDatabaseView => ("ActivateDatabaseToolWindow", "Database", []),
    RunDatabaseSql => ("Database.RunSqlFromEditor", "Run SQL from Selection / Python String", []),
    DatabaseResolutionScope => ("Database.ResolutionScope", "SQL Dialect and Resolution Scope…", []),
    RunHttpRequest => ("HttpClient.RunRequest", "Run HTTP Request at Caret", []),
    ToggleServicesView => ("ActivateServicesToolWindow", "Services", ["Alt+Num8"]),
    ToggleBranchesView => ("ActivateBranchesToolWindow", "Branches", []),
    ToggleGitView => ("ActivateVersionControlToolWindow", "Git", ["Alt+Num9"]),
    ToggleTerminal => ("ActivateTerminalToolWindow", "Terminal", ["Alt+F12"]),
    OpenEditorTerminal => ("Terminal.OpenAtEditor", "Open in Terminal", []),
    SendSelectionToTerminal => ("Terminal.SendSelection", "Send Selection to Terminal", []),
    TogglePythonConsole => ("ActivatePythonConsoleToolWindow", "Python Console", []),
    TogglePythonPackages => ("ActivatePythonPackagesToolWindow", "Python Packages", []),
    ManagePythonPackages => ("Python.ManagePackages", "Manage Python Packages", []),
    RunSetupPyTask => ("Python.RunSetupTask", "Run setup.py Task…", []),
    BuildPythonDistribution => ("Python.BuildDistribution", "Build Python Distribution", []),
    ShowPythonDependencies => ("Python.Dependencies", "Dependencies", []),
    ToggleTodoView => ("ActivateTODOToolWindow", "TODO", []),
    ToggleNotificationsView => ("ActivateNotificationsToolWindow", "Notifications", []),
    ToggleDocumentationView => ("ActivateDocumentationToolWindow", "Documentation", []),
    ToggleBookmarksView => ("ActivateBookmarksToolWindow", "Bookmarks", ["Alt+Num2"]),
    HideAllToolWindows => ("HideAllWindows", "Hide All Tool Windows", ["Ctrl+Shift+F12"]),
    FocusToolWindowControls => ("FocusToolWindowControls", "Focus Tool Window Controls", ["Ctrl+Alt+F6"]),
    IncreaseLeftToolWidth => ("IncreaseLeftToolWidth", "Increase Left Tool Window Width", []),
    DecreaseLeftToolWidth => ("DecreaseLeftToolWidth", "Decrease Left Tool Window Width", []),
    IncreaseBottomToolHeight => ("IncreaseBottomToolHeight", "Increase Bottom Tool Window Height", []),
    DecreaseBottomToolHeight => ("DecreaseBottomToolHeight", "Decrease Bottom Tool Window Height", []),
    ToggleToolWindowFloating => ("ToggleToolWindowFloating", "Float or Dock Tool Window", []),
    ToggleToolWindowMaximized => ("ToggleToolWindowMaximized", "Maximize or Restore Tool Window", ["Ctrl+Shift+Quote"]),
    StretchToolWindowLeft => ("StretchToolWindowLeft", "Stretch Tool Window to Left", []),
    StretchToolWindowRight => ("StretchToolWindowRight", "Stretch Tool Window to Right", []),
    StretchToolWindowUp => ("StretchToolWindowUp", "Stretch Tool Window to Top", []),
    StretchToolWindowDown => ("StretchToolWindowDown", "Stretch Tool Window to Bottom", []),
    WidenToolWindow => ("WidenToolWindow", "Widen Tool Window", ["Ctrl+Alt+Shift+Quote"]),
    HideActiveToolWindow => ("HideActiveToolWindow", "Hide Active Tool Window", ["Shift+Escape"]),
    JumpToLastToolWindow => ("JumpToLastWindow", "Jump to Last Tool Window", ["F12"]),
    StoreNewLayout => ("StoreNewLayout", "Save Current Layout As…", []),
    StoreDefaultLayout => ("StoreDefaultLayout", "Store Current Layout as Default", []),
    RestoreDefaultLayout => ("RestoreDefaultLayout", "Restore Default Layout", []),
    RestoreFactoryDefaultLayout => ("RestoreFactoryDefaultLayout", "Restore Factory Default Layout", []),
    RestoreNamedLayout => ("RestoreNamedLayout", "Restore Layout…", []),
    DeleteNamedLayout => ("DeleteNamedLayout", "Delete Layout…", []),
    JumpToNavigationBar => ("JumpToNavigationBar", "Jump to Navigation Bar", ["Alt+Home"]),
    SplitVertically => ("SplitVertically", "Split Right", []),
    SplitHorizontally => ("SplitHorizontally", "Split Down", []),
    SplitAndMoveRight => ("SplitAndMoveRight", "Split and Move Right", []),
    SplitAndMoveDown => ("SplitAndMoveDown", "Split and Move Down", []),
    OpenInOppositeGroup => ("OpenEditorInOppositeTabGroup", "Open in Opposite Group", []),
    Unsplit => ("Unsplit", "Unsplit", []),
    UnsplitAll => ("UnsplitAll", "Unsplit All", []),
    EnlargeFirstSplit => ("EnlargeFirstSplit", "Enlarge First Editor Split", []),
    ShrinkFirstSplit => ("ShrinkFirstSplit", "Shrink First Editor Split", []),
    EqualizeSplits => ("EqualizeSplits", "Equalize Editor Splits", []),
    ChangeSplitOrientation => ("ChangeSplitOrientation", "Change Splitter Orientation", []),
    MoveTabToOppositeGroup => ("MoveEditorToOppositeTabGroup", "Move Tab to Opposite Group", []),
    NextSplitter => ("NextSplitter", "Goto Next Splitter", ["Alt+Tab"]),
    PrevSplitter => ("PrevSplitter", "Goto Previous Splitter", ["Alt+Shift+Tab"]),
    ZoomIn => ("EditorIncreaseFontSize", "Increase Font Size", ["Ctrl+Plus", "Ctrl+Shift+Period"]),
    ZoomOut => ("EditorDecreaseFontSize", "Decrease Font Size", ["Ctrl+Shift+Comma"]),
    ZoomReset => ("EditorResetFontSize", "Reset Font Size", ["Ctrl+Shift+Num0"]),
    ToggleLineNumbers => ("EditorToggleShowLineNumbers", "Show Line Numbers", []),
    ToggleWhitespaces => ("EditorToggleShowWhitespaces", "Show Whitespaces", []),
    ToggleSoftWrap => ("EditorToggleUseSoftWraps", "Soft-Wrap", []),
    ToggleInlayParameterHints => ("ToggleInlayParameterHints", "Inlay Hints: Parameter Names", []),
    ToggleInlayTypeHints => ("ToggleInlayTypeHints", "Inlay Hints: Variable Types", []),
    ToggleIndentGuides => ("EditorToggleShowIndentLines", "Show Indent Guides", []),
    ToggleTreeDetails => ("ShowDetailsInTreeViews", "Details in Tree Views", []),
    ToggleZenMode => ("ToggleZenMode", "Enter Zen Mode", []),
    ExitZenMode => ("ExitZenMode", "Exit Zen Mode", []),
    ToggleDistractionFree => ("ToggleDistractionFreeMode", "Enter Distraction Free Mode", []),
    ToggleFullScreen => ("ToggleFullScreen", "Enter Full Screen", []),
    TogglePresentationMode => ("TogglePresentationMode", "Enter Presentation Mode", []),
    QuickSwitchScheme => ("QuickChangeScheme", "Quick Switch Scheme…", ["Ctrl+Backtick"]),
    // ----- Run ---------------------------------------------------------------
    Run => ("Run", "Run", ["Shift+F10"]),
    Debug => ("Debug", "Debug", ["Shift+F9"]),
    Stop => ("Stop", "Stop", ["Ctrl+F2"]),
    RunContext => ("RunClass", "Run context configuration", ["Ctrl+Shift+F10"]),
    DebugContext => ("DebugClass", "Debug context configuration", ["Ctrl+Shift+F9"]),
    CoverageContext => ("CoverageContext", "Run context with Coverage", []),
    ProfileContext => ("ProfileContext", "Profile context configuration", []),
    ModifyContextConfiguration => ("ModifyContextConfiguration", "Modify Run Configuration…", []),
    CreateContextConfiguration => ("CreateContextConfiguration", "Create Run Configuration…", []),
    ChooseRunConfiguration => ("ChooseRunConfiguration", "Run…", ["Alt+Shift+F10"]),
    ChooseDebugConfiguration => ("ChooseDebugConfiguration", "Debug…", ["Alt+Shift+F9"]),
    Rerun => ("Rerun", "Rerun", ["Ctrl+F5"]),
    RunAnything => ("RunAnything", "Run Anything…", ["Ctrl+Ctrl"]),
    EditRunConfigurations => ("editRunConfigurations", "Edit Configurations…", []),
    ExecuteInConsole => ("ExecuteInPyConsoleAction", "Execute Selection in Python Console", ["Alt+Shift+E"]),
    RunFileInConsole => ("RunFileInPythonConsole", "Run File in Python Console", []),
    RunCell => ("PyRunCellAction", "Run Cell", []),
    RunCellsAbove => ("PyRunCellsAboveAction", "Run Cells Above", []),
    DebugCell => ("PyDebugCellAction", "Debug Cell", []),
    ToggleSciView => ("ActivateSciViewToolWindow", "SciView", []),
    ToggleScientificMode => ("ScientificMode", "Scientific Mode", []),
    RunCellAndAdvance => ("PyRunCellAndSelectBelowAction", "Run Cell and Select Below", []),
    ToggleLineBreakpoint => ("ToggleLineBreakpoint", "Toggle Line Breakpoint", ["Ctrl+F8"]),
    ViewBreakpoints => ("ViewBreakpoints", "View Breakpoints…", ["Ctrl+Shift+F8"]),
    StepOver => ("StepOver", "Step Over", ["F8"]),
    StepInto => ("StepInto", "Step Into", ["F7"]),
    StepOut => ("StepOut", "Step Out", ["Shift+F8"]),
    Resume => ("Resume", "Resume Program", ["F9"]),
    EvaluateExpression => ("EvaluateExpression", "Evaluate Expression…", ["Alt+F8"]),
    RunToCursor => ("RunToCursor", "Run to Cursor", ["Alt+F9"]),
    SmartStepInto => ("SmartStepInto", "Smart Step Into", ["Shift+F7"]),
    JumpToCursor => ("JumpToCursor", "Jump to Cursor", []),
    ShowExecutionPoint => ("ShowExecutionPoint", "Show Execution Point", ["Alt+F10"]),
    QuickEvaluateExpression => ("QuickEvaluateExpression", "Quick Evaluate Expression", ["Ctrl+Alt+F8"]),
    ToggleTemporaryLineBreakpoint => ("ToggleTemporaryLineBreakpoint", "Toggle Temporary Line Breakpoint", ["Ctrl+Alt+Shift+F8"]),
    MuteBreakpoints => ("XDebugger.MuteBreakpoints", "Mute Breakpoints", []),
    RestartDebug => ("RestartDebugger", "Restart Debugger", []),
    // ----- Git ---------------------------------------------------------------
    Commit => ("CheckinProject", "Commit…", ["Ctrl+K"]),
    Push => ("Vcs.Push", "Push…", ["Ctrl+Shift+K"]),
    UpdateProject => ("Vcs.UpdateProject", "Update Project…", ["Ctrl+T"]),
    GitFetch => ("Git.Fetch", "Fetch", []),
    ManageRemotes => ("Git.Configure.Remotes", "Manage Remotes…", []),
    VcsPopup => ("Vcs.QuickListPopupAction", "VCS Operations", ["Alt+Backtick"]),
    ShowDiff => ("Compare.SameVersion", "Show Diff", []),
    VcsShowNextChangeMarker => ("VcsShowNextChangeMarker", "Next Change", ["Ctrl+Alt+Shift+ArrowDown"]),
    VcsShowPrevChangeMarker => ("VcsShowPrevChangeMarker", "Previous Change", ["Ctrl+Alt+Shift+ArrowUp"]),
    // ----- Help --------------------------------------------------------------
    About => ("About", "About", []),
    HelpTopics => ("HelpTopics", "Help", ["F1"]),
    WelcomeGuide => ("Help.WelcomeGuide", "Welcome Guide", []),
    GettingStarted => ("Help.GettingStarted", "Getting Started", []),
    RemoteDevelopment => ("RemoteDevelopment.Open", "Remote Development…", []),
    StartGuidedTour => ("Help.GuidedTour", "Start Guided Tour", []),
    ProductivityGuide => ("Help.ProductivityGuide", "Productivity Guide", []),
    AccessibilityGuide => ("Help.AccessibilityGuide", "Accessibility Guide", []),
    ContactSupport => ("Help.ContactSupport", "Contact Support…", []),
    SubmitFeedback => ("Help.SubmitFeedback", "Submit Feedback…", []),
    EditCustomProperties => ("Help.EditCustomProperties", "Edit Custom Properties…", []),
    OnlineHelp => ("Help.Online", "Online Help", []),
    TipOfTheDay => ("ShowTips", "Tip of the Day", []),
    WhatsNew => ("WhatsNewAction", "What's New in RustPy", []),
    CheckForUpdates => ("CheckForUpdate", "Check for Updates…", []),
    KeymapReference => ("Help.KeymapReference", "Keyboard Shortcuts PDF", []),
}

impl Action {
    pub fn bookmark_digit(self) -> Option<char> {
        match self {
            Self::GoToBookmark0 => Some('0'),
            Self::GoToBookmark1 => Some('1'),
            Self::GoToBookmark2 => Some('2'),
            Self::GoToBookmark3 => Some('3'),
            Self::GoToBookmark4 => Some('4'),
            Self::GoToBookmark5 => Some('5'),
            Self::GoToBookmark6 => Some('6'),
            Self::GoToBookmark7 => Some('7'),
            Self::GoToBookmark8 => Some('8'),
            Self::GoToBookmark9 => Some('9'),
            _ => None,
        }
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

/// Whether `key` is a modifier on its own (egui reports Shift, Ctrl, Alt and
/// Super presses as keys too): a shortcut being recorded waits for the real key.
pub fn is_modifier_key(key: Key) -> bool {
    matches!(
        key,
        Key::ShiftLeft
            | Key::ShiftRight
            | Key::ControlLeft
            | Key::ControlRight
            | Key::AltLeft
            | Key::AltRight
            | Key::SuperLeft
            | Key::SuperRight
    )
}

/// A key with modifiers, e.g. `Ctrl+Shift+Z`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub key: Key,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl KeyChord {
    pub fn new(key: Key, mods: Modifiers) -> Self {
        Self {
            key,
            ctrl: mods.ctrl || mods.mac_cmd || mods.command,
            alt: mods.alt,
            shift: mods.shift,
        }
    }

    /// Parses `"Ctrl+Shift+Z"`, `"Alt+Num1"`, `"F3"`, `"Ctrl+Slash"`.
    pub fn parse(s: &str) -> Option<Self> {
        let mut chord = Self {
            key: Key::Escape,
            ctrl: false,
            alt: false,
            shift: false,
        };
        let mut key = None;
        for part in s.split('+') {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "cmd" | "meta" => chord.ctrl = true,
                "alt" | "option" => chord.alt = true,
                "shift" => chord.shift = true,
                _ => {
                    let name = part
                        .strip_prefix("Num")
                        .filter(|r| r.len() == 1 && r.chars().all(|c| c.is_ascii_digit()))
                        .unwrap_or(part);
                    if key.is_some() {
                        return None;
                    }
                    key = Some(
                        Key::from_name(name)
                            .or_else(|| Key::from_name(&name.to_ascii_uppercase()))?,
                    );
                }
            }
        }
        chord.key = key?;
        Some(chord)
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("Ctrl+")?;
        }
        if self.alt {
            f.write_str("Alt+")?;
        }
        if self.shift {
            f.write_str("Shift+")?;
        }
        // Display glyphs change between egui versions and punctuation can collide
        // with the shortcut grammar's '+' and ',' separators. Persist key names.
        f.write_str(self.key.name())
    }
}

/// Maps key chords to actions. Later entries override earlier ones.
#[derive(Clone, Debug, Default)]
pub struct Keymap {
    pub name: String,
    bindings: FxHashMap<KeyChord, Action>,
    by_action: FxHashMap<Action, Vec<KeyChord>>,
    /// The preset's own shortcuts per action: what the user's changes are
    /// measured against (custom shortcuts, Reset).
    base: FxHashMap<Action, Vec<KeyChord>>,
    extended: Vec<(Action, shortcuts::Shortcut)>,
    base_extended: Vec<(Action, shortcuts::Shortcut)>,
}

/// `format` of an exported keymap file.
const KEYMAP_FORMAT: &str = "rustpy-keymap";

/// Keymap presets layered over the Default keymap: `(name, json)` where the
/// JSON's `bindings` map action ids to their shortcuts in that preset.
const PRESETS: &[(&str, &str)] = &[
    ("Emacs", include_str!("../assets/keymaps/emacs.json")),
    (
        "Visual Studio",
        include_str!("../assets/keymaps/visualstudio.json"),
    ),
    ("NetBeans", include_str!("../assets/keymaps/netbeans.json")),
    ("Xcode", include_str!("../assets/keymaps/xcode.json")),
    ("macOS", include_str!("../assets/keymaps/macos.json")),
    ("VS Code", include_str!("../assets/keymaps/vscode.json")),
    ("Eclipse", include_str!("../assets/keymaps/eclipse.json")),
    (
        "Sublime Text",
        include_str!("../assets/keymaps/sublime.json"),
    ),
];

impl Keymap {
    /// Names offered in Settings › Keymap.
    pub fn preset_names() -> Vec<&'static str> {
        std::iter::once("Default")
            .chain(PRESETS.iter().map(|(n, _)| *n))
            .collect()
    }

    /// `name`'s preset (Default when unknown or empty).
    pub fn preset(name: &str) -> Keymap {
        let Some((pname, json)) = PRESETS.iter().find(|(n, _)| *n == name) else {
            return Self::default_keymap();
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
            log::warn!("keymap preset {pname} is not valid JSON");
            return Self::default_keymap();
        };
        let bindings: Vec<(&str, Vec<&str>)> = v["bindings"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(id, shortcuts)| {
                (
                    id.as_str(),
                    shortcuts
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|s| s.as_str())
                        .collect(),
                )
            })
            .collect();
        let (km, problems) =
            Self::from_bindings(pname, bindings.iter().map(|(id, s)| (*id, s.as_slice())));
        for p in problems {
            log::warn!("keymap preset {pname}: {p}");
        }
        km
    }

    /// A keymap called `name` layered over Default: each listed action id gets
    /// exactly the given shortcuts (an empty list unbinds it). Returns the
    /// keymap and what could not be applied (unknown action ids, shortcuts
    /// that do not parse). Used for the bundled presets and plugin keymaps.
    pub fn from_bindings<'a, S: AsRef<str> + 'a>(
        name: &str,
        bindings: impl IntoIterator<Item = (&'a str, &'a [S])>,
    ) -> (Keymap, Vec<String>) {
        let mut km = Self::default_keymap();
        let mut problems = Vec::new();
        for (id, shortcuts) in bindings {
            let Some(action) = Self::action_by_id(id) else {
                problems.push(format!("unknown action id {id}"));
                continue;
            };
            let inherited_mouse: Vec<_> = km
                .extended
                .iter()
                .filter(|(a, s)| *a == action && matches!(s, shortcuts::Shortcut::Mouse { .. }))
                .cloned()
                .collect();
            km.clear_action(action);
            if !shortcuts.iter().any(|s| s.as_ref().starts_with("Mouse:")) {
                km.extended.extend(inherited_mouse);
            }
            for s in shortcuts {
                if km.bind_definition(action, s.as_ref(), false).is_err() {
                    problems.push(format!(
                        "cannot parse the shortcut {:?} of {id}",
                        s.as_ref()
                    ));
                }
            }
        }
        km.name = name.to_string();
        // A preset or plugin keymap is its own default: Reset and "modified" compare with it.
        km.base = km.by_action.clone();
        km.base_extended = km.extended.clone();
        (km, problems)
    }

    /// The *Default* keymap for Windows/Linux.
    pub fn default_keymap() -> Keymap {
        let mut km = Keymap {
            name: "Default".into(),
            ..Default::default()
        };
        for &action in Action::ALL {
            for s in action.default_shortcuts() {
                if let Some(chord) = KeyChord::parse(s) {
                    km.bind(chord, action);
                } else if !matches!(*s, "Shift+Shift" | "Ctrl+Ctrl") {
                    log::warn!("unparsable default shortcut {s:?} for {:?}", action);
                }
            }
        }
        for definition in ["Mouse:Ctrl+Primary", "Mouse:Middle"] {
            let _ = km.bind_definition(Action::GoToDeclaration, definition, false);
        }
        km.base = km.by_action.clone();
        km.base_extended = km.extended.clone();
        km
    }

    pub fn bind(&mut self, chord: KeyChord, action: Action) {
        self.extended.retain(|(_, s)| match s {
            shortcuts::Shortcut::Sequence(keys) => keys.first() != Some(&chord),
            shortcuts::Shortcut::ControlSequence(keys) if !shortcuts::mac_keymap() => {
                keys.first() != Some(&chord)
            }
            shortcuts::Shortcut::PhysicalControl(c) if !shortcuts::mac_keymap() => *c != chord,
            shortcuts::Shortcut::Shared(c) => *c != chord,
            _ => true,
        });
        if let Some(old) = self.bindings.insert(chord, action) {
            if let Some(list) = self.by_action.get_mut(&old) {
                list.retain(|c| *c != chord);
            }
        }
        self.by_action.entry(action).or_default().push(chord);
    }

    /// Removes every shortcut of `action`.
    pub fn clear_action(&mut self, action: Action) {
        self.extended.retain(|(a, _)| *a != action);
        if let Some(list) = self.by_action.remove(&action) {
            for c in list {
                self.bindings.remove(&c);
            }
        }
    }

    /// Looks an action up by its stable id.
    pub fn action_by_id(id: &str) -> Option<Action> {
        Action::ALL.iter().copied().find(|a| a.id() == id)
    }

    /// True when the action has its preset's shortcuts (the user did not change them).
    pub fn is_default(&self, action: Action) -> bool {
        let sorted = |chords: &[KeyChord]| {
            let mut v = chords.to_vec();
            v.sort_by_key(|c| c.to_string());
            v
        };
        sorted(self.shortcuts(action)) == sorted(self.preset_shortcuts(action))
            && self
                .extended
                .iter()
                .filter(|(a, _)| *a == action)
                .collect::<Vec<_>>()
                == self
                    .base_extended
                    .iter()
                    .filter(|(a, _)| *a == action)
                    .collect::<Vec<_>>()
    }

    /// The shortcuts `action` has in this keymap's preset.
    pub fn preset_shortcuts(&self, action: Action) -> &[KeyChord] {
        self.base.get(&action).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Reset: gives `action` its preset's shortcuts back (taking them from
    /// any action they were moved to).
    pub fn reset_action(&mut self, action: Action) {
        self.clear_action(action);
        let definitions: Vec<_> = self
            .base_extended
            .iter()
            .filter(|(a, _)| *a == action)
            .map(|(_, s)| s.text())
            .collect();
        for definition in definitions {
            let _ = self.bind_definition(action, &definition, false);
        }
        for chord in self.preset_shortcuts(action).to_vec() {
            self.bind(chord, action);
        }
    }

    /// Whether `action` has the same shortcuts here and in `base` (order ignored).
    pub fn same_shortcuts(&self, base: &Keymap, action: Action) -> bool {
        let sorted = |km: &Keymap| {
            let mut v = km.shortcut_definitions(action);
            v.sort();
            v
        };
        sorted(self) == sorted(base)
    }

    /// The user's changes relative to `base` (the selected preset), as stored in
    /// `Settings::custom_shortcuts`: `(action id, shortcuts)` for every action
    /// whose shortcuts differ; an empty list unbinds the action.
    pub fn overrides_from(&self, base: &Keymap) -> Vec<(String, Vec<String>)> {
        let mut overrides: Vec<_> = Action::ALL
            .iter()
            .copied()
            .filter(|a| !self.same_shortcuts(base, *a))
            .map(|a| (a.id().to_string(), self.shortcut_definitions(a)))
            .collect();
        // JSON object persistence imports action IDs in lexical order. Keep
        // exported overrides in that same order, including displaced owners.
        overrides.sort_by(|left, right| left.0.cmp(&right.0));
        overrides
    }

    /// Applies stored overrides (see [`Keymap::overrides_from`]) on top of this keymap.
    pub fn apply_overrides(&mut self, overrides: &[(String, Vec<String>)]) {
        let entries: Vec<_> = overrides
            .iter()
            .filter_map(|(id, shortcuts)| Self::action_by_id(id).map(|action| (action, shortcuts)))
            .collect();
        for (action, _) in &entries {
            self.clear_action(*action);
        }
        // Shared bindings are applied last: restoring another action's ordinary
        // binding must not erase an explicitly retained conflict.
        for shared in [false, true] {
            for (action, shortcuts) in &entries {
                for definition in *shortcuts {
                    if definition.starts_with("Shared:") == shared {
                        let _ = self.bind_definition(*action, definition, false);
                    }
                }
            }
        }
    }

    /// The other action `chord` would be taken from if it were given to `action`.
    pub fn conflict(&self, chord: KeyChord, action: Action) -> Option<Action> {
        self.chord_owner(chord).filter(|owner| *owner != action)
    }

    /// Makes `chord` the only shortcut of `action` (Settings › Keymap › Change…),
    /// removing it from the action that had it; returns that action.
    pub fn set_shortcut(&mut self, action: Action, chord: KeyChord) -> Option<Action> {
        let previous = self.conflict(chord, action);
        self.clear_action(action);
        self.bind(chord, action);
        previous
    }

    /// Settings › Keymap › Export: the preset and the user's changes on top of it.
    pub fn export_json(preset: &str, overrides: &[(String, Vec<String>)]) -> String {
        let bindings: serde_json::Map<String, serde_json::Value> = overrides
            .iter()
            .map(|(id, chords)| (id.clone(), serde_json::json!(chords)))
            .collect();
        let doc = serde_json::json!({ "format": KEYMAP_FORMAT, "version": 1, "parent": if preset.is_empty() { "Default" } else { preset }, "bindings": bindings });
        serde_json::to_string_pretty(&doc).unwrap_or_default()
    }

    /// Settings › Keymap › Import: `(preset, overrides, skipped entries)` from an
    /// exported keymap (a preset file's `bindings` map is accepted too).
    pub fn import_json(
        text: &str,
    ) -> Result<(String, rustpy_settings::ShortcutOverrides, Vec<String>), String> {
        if text.len() > 4 * 1024 * 1024 {
            return Err("Keymap exceeds 4 MiB".into());
        }
        let v: serde_json::Value =
            serde_json::from_str(text).map_err(|e| format!("not a JSON keymap: {e}"))?;
        let bindings = v["bindings"]
            .as_object()
            .ok_or("the file has no \"bindings\" object")?;
        let parent = v["parent"].as_str().unwrap_or("Default");
        let preset = if Self::preset_names().contains(&parent) {
            parent.to_string()
        } else {
            "Default".to_string()
        };
        let mut overrides = Vec::new();
        let mut skipped = Vec::new();
        for (id, chords) in bindings {
            if Self::action_by_id(id).is_none() {
                skipped.push(format!("unknown action {id}"));
                continue;
            }
            let Some(chords) = chords.as_array() else {
                skipped.push(format!("{id}: shortcuts must be an array"));
                continue;
            };
            let mut list = Vec::new();
            for c in chords {
                match c
                    .as_str()
                    .filter(|s| shortcuts::Shortcut::parse(s).is_none())
                    .and_then(KeyChord::parse)
                {
                    Some(chord) => list.push(chord.to_string()),
                    None => match c.as_str().and_then(shortcuts::Shortcut::parse) {
                        Some(s) => list.push(s.text()),
                        None => skipped.push(format!("{id}: cannot read shortcut {c}")),
                    },
                }
            }
            overrides.push((id.clone(), list));
        }
        Ok((preset, overrides, skipped))
    }

    pub fn lookup(&self, chord: KeyChord) -> Option<Action> {
        self.bindings.get(&chord).copied()
    }

    /// Looks up a key event by its logical key, then by its physical key, so
    /// a binding like `Ctrl+Shift+Slash` still matches when the keyboard
    /// layout reports the shifted symbol (`?`).
    pub fn lookup_key(
        &self,
        key: Key,
        physical_key: Option<Key>,
        mods: Modifiers,
    ) -> Option<Action> {
        if shortcuts::mac_keymap() && mods.ctrl && !mods.mac_cmd {
            return None;
        }
        self.lookup(KeyChord::new(key, mods)).or_else(|| {
            physical_key
                .filter(|p| *p != key)
                .and_then(|p| self.lookup(KeyChord::new(p, mods)))
        })
    }

    pub fn shortcuts(&self, action: Action) -> &[KeyChord] {
        self.by_action
            .get(&action)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// First shortcut rendered for menus, e.g. `"Ctrl+S"`.
    pub fn shortcut_text(&self, action: Action) -> String {
        let text = self
            .shortcut_definitions(action)
            .first()
            .cloned()
            .unwrap_or_default();
        if shortcuts::mac_keymap() || matches!(self.name.as_str(), "macOS" | "Xcode") {
            text.replace("Ctrl+", "⌘")
                .replace("Alt+", "⌥")
                .replace("Shift+", "⇧")
                .replace("Control+", "⌃")
        } else {
            text
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_displays_chords() {
        let c = KeyChord::parse("Ctrl+Shift+Z").unwrap();
        assert_eq!(
            c,
            KeyChord {
                key: Key::Z,
                ctrl: true,
                alt: false,
                shift: true
            }
        );
        assert_eq!(c.to_string(), "Ctrl+Shift+Z");
        assert_eq!(KeyChord::parse("Alt+Num1").unwrap().key, Key::Num1);
        assert_eq!(KeyChord::parse("Ctrl+Slash").unwrap().key, Key::Slash);
        assert!(KeyChord::parse("Ctrl+Bogus").is_none());
    }

    #[test]
    fn named_keys_roundtrip_without_shortcut_separator_or_glyph_ambiguity() {
        for &key in egui::Key::ALL {
            for (ctrl, alt, shift) in [(false, false, false), (true, true, true)] {
                let chord = KeyChord {
                    key,
                    ctrl,
                    alt,
                    shift,
                };
                let text = chord.to_string();
                assert_eq!(KeyChord::parse(&text), Some(chord), "{text}");
                assert!(shortcuts::Shortcut::parse(&text).is_none(), "{text}");
            }
        }
        for (alias, canonical) in [
            ("ArrowDown", "Down"),
            ("Ctrl+Num1", "Ctrl+1"),
            ("Ctrl+Comma", "Ctrl+Comma"),
            ("Ctrl+Plus", "Ctrl+Plus"),
        ] {
            assert_eq!(KeyChord::parse(alias).unwrap().to_string(), canonical);
            assert_eq!(KeyChord::parse(alias), KeyChord::parse(canonical));
        }
        let sequence = shortcuts::Shortcut::parse("Ctrl+Plus, Ctrl+Comma").unwrap();
        assert_eq!(sequence.text(), "Ctrl+Plus, Ctrl+Comma");
        assert_eq!(shortcuts::Shortcut::parse(&sequence.text()), Some(sequence));
    }

    #[test]
    fn presets_parse_and_have_no_internal_conflicts() {
        for name in Keymap::preset_names().into_iter().skip(1) {
            let (_, json) = PRESETS.iter().find(|(n, _)| *n == name).unwrap();
            let v: serde_json::Value = serde_json::from_str(json).expect("valid JSON");
            let mut seen = std::collections::HashMap::new();
            for (id, shortcuts) in v["bindings"].as_object().expect("bindings") {
                assert!(
                    Keymap::action_by_id(id).is_some(),
                    "{name}: unknown action id {id}"
                );
                for s in shortcuts.as_array().unwrap() {
                    let raw = s.as_str().unwrap();
                    assert!(
                        KeyChord::parse(raw).is_some() || shortcuts::Shortcut::parse(raw).is_some(),
                        "{name}: cannot parse {s}"
                    );
                    let chord = shortcuts::Shortcut::parse(raw)
                        .map(|shortcut| shortcut.text())
                        .unwrap_or_else(|| KeyChord::parse(raw).unwrap().to_string());
                    if let Some(other) = seen.insert(chord, id.clone()) {
                        panic!("{name}: {s} is assigned to both {other} and {id}");
                    }
                }
            }
            let km = Keymap::preset(name);
            assert_eq!(km.name, name);
            for (id, shortcuts) in v["bindings"].as_object().unwrap() {
                let a = Keymap::action_by_id(id).unwrap();
                for s in shortcuts.as_array().unwrap() {
                    let raw = s.as_str().unwrap();
                    let extended = shortcuts::Shortcut::parse(raw);
                    let canonical = extended
                        .as_ref()
                        .map(|shortcut| shortcut.text())
                        .unwrap_or_else(|| KeyChord::parse(raw).unwrap().to_string());
                    assert!(
                        km.shortcut_definitions(a).contains(&canonical),
                        "{name}: {s} should run {id}"
                    );
                    // Compare semantic identities, then exercise dispatch: raw preset
                    // aliases (ArrowDown, Num1) need not equal canonical names (Down, 1).
                    let dispatched = match extended {
                        None => km.lookup(KeyChord::parse(raw).unwrap()),
                        Some(shortcuts::Shortcut::Sequence(keys)) => {
                            km.sequence_action(keys[0], keys[1])
                        }
                        Some(shortcuts::Shortcut::ControlSequence(keys)) => {
                            km.control_sequence_action(keys[0], keys[1])
                        }
                        Some(shortcuts::Shortcut::PhysicalControl(chord)) => km
                            .physical_control_action(
                                chord.key,
                                egui::Modifiers {
                                    ctrl: true,
                                    alt: chord.alt,
                                    shift: chord.shift,
                                    ..Default::default()
                                },
                            ),
                        other => panic!("{name}: add a dispatch assertion for {other:?}"),
                    };
                    assert_eq!(dispatched, Some(a), "{name}: {s} should dispatch {id}");
                }
            }
        }
        let vs = Keymap::preset("VS Code");
        assert_eq!(
            vs.lookup(KeyChord::parse("Ctrl+P").unwrap()),
            Some(Action::GoToFile)
        );
        assert_eq!(
            vs.lookup(KeyChord::parse("F2").unwrap()),
            Some(Action::Rename)
        );
        assert_eq!(Keymap::preset("").name, "Default");
    }

    #[test]
    fn a_preset_is_its_own_default_and_reset_goes_back_to_it() {
        let mut vs = Keymap::preset("VS Code");
        // Picking a preset is not a user change.
        let custom: Vec<Action> = Action::ALL
            .iter()
            .copied()
            .filter(|a| !vs.is_default(*a))
            .collect();
        assert!(custom.is_empty(), "{custom:?}");
        assert_eq!(
            vs.preset_shortcuts(Action::GoToFile),
            &[KeyChord::parse("Ctrl+P").unwrap()]
        );
        // A change is measured against the preset ...
        vs.clear_action(Action::CloseProject);
        vs.bind(
            KeyChord::parse("Ctrl+Alt+Shift+K").unwrap(),
            Action::CloseProject,
        );
        let custom: Vec<Action> = Action::ALL
            .iter()
            .copied()
            .filter(|a| !vs.is_default(*a))
            .collect();
        assert_eq!(custom, [Action::CloseProject]);
        // ... and Reset restores the preset's binding, not the built-in default.
        vs.bind(KeyChord::parse("Ctrl+P").unwrap(), Action::CloseProject);
        assert!(!vs.is_default(Action::GoToFile), "Ctrl+P was moved away");
        vs.reset_action(Action::GoToFile);
        assert_eq!(
            vs.lookup(KeyChord::parse("Ctrl+P").unwrap()),
            Some(Action::GoToFile)
        );
        assert!(vs.is_default(Action::GoToFile));
        vs.reset_action(Action::CloseProject);
        assert!(vs.is_default(Action::CloseProject));
        let def = Keymap::default_keymap();
        assert!(Action::ALL.iter().all(|a| def.is_default(*a)));
        assert_eq!(
            def.preset_shortcuts(Action::ParameterInfo),
            &[KeyChord::parse("Ctrl+P").unwrap()]
        );
    }

    #[test]
    fn keymaps_from_bindings_layer_over_default() {
        let bindings: Vec<(&str, Vec<String>)> = vec![
            ("$Copy", vec!["Alt+W".into()]),
            ("GotoLine", Vec::new()),
            ("Nope", vec!["F1".into()]),
            ("Run", vec!["Ctrl+Nope".into(), "Ctrl+F5".into()]),
        ];
        let (km, problems) =
            Keymap::from_bindings("Mine", bindings.iter().map(|(id, s)| (*id, s.as_slice())));
        assert_eq!(km.name, "Mine");
        assert_eq!(
            km.shortcuts(Action::Copy),
            [KeyChord::parse("Alt+W").unwrap()]
        );
        assert!(
            km.shortcuts(Action::GoToLine).is_empty(),
            "an empty list unbinds"
        );
        assert_eq!(
            km.lookup(KeyChord::parse("Ctrl+F5").unwrap()),
            Some(Action::Run)
        );
        assert_eq!(
            km.lookup(KeyChord::parse("Ctrl+S").unwrap()),
            Some(Action::Save),
            "unlisted actions keep Default"
        );
        assert_eq!(
            problems,
            [
                "unknown action id Nope",
                "cannot parse the shortcut \"Ctrl+Nope\" of Run"
            ]
        );
    }

    #[test]
    fn custom_shortcuts_are_stored_relative_to_the_preset() {
        let base = Keymap::preset("VS Code");
        let mut km = base.clone();
        // Untouched preset: nothing to store (the preset's own bindings are not "custom";
        // they used to be stored as overrides of the Default keymap).
        assert!(km.overrides_from(&base).is_empty());
        assert!(!km.overrides_from(&Keymap::default_keymap()).is_empty());
        // Ctrl+P is Go to File in VS Code: giving it to Save takes it from there.
        km.set_shortcut(Action::Save, KeyChord::parse("Ctrl+P").unwrap());
        let overrides = km.overrides_from(&base);
        let mut sorted = overrides.clone();
        sorted.sort();
        assert_eq!(
            sorted,
            vec![
                ("GotoFile".to_string(), vec![]),
                ("SaveDocument".to_string(), vec!["Ctrl+P".to_string()])
            ]
        );
        let mut again = Keymap::preset("VS Code");
        again.apply_overrides(&overrides);
        for &a in Action::ALL {
            assert!(again.same_shortcuts(&km, a), "{a:?}");
        }
    }

    #[test]
    fn modifier_presses_are_not_shortcuts() {
        for k in [
            Key::ControlLeft,
            Key::ShiftRight,
            Key::AltLeft,
            Key::SuperLeft,
        ] {
            assert!(is_modifier_key(k), "{k:?}");
        }
        for k in [Key::F, Key::Escape, Key::F12, Key::Backspace] {
            assert!(!is_modifier_key(k), "{k:?}");
        }
    }

    #[test]
    fn assigning_a_used_shortcut_reports_and_removes_the_old_owner() {
        let mut km = Keymap::default_keymap();
        let chord = KeyChord::parse("Ctrl+Shift+F").unwrap();
        let owner = km.lookup(chord).expect("Ctrl+Shift+F is bound by default");
        assert_eq!(km.conflict(chord, Action::CloseProject), Some(owner));
        assert_eq!(
            km.conflict(chord, owner),
            None,
            "re-assigning to the owner is no conflict"
        );
        assert_eq!(
            km.conflict(
                KeyChord::parse("Ctrl+Alt+Shift+F12").unwrap(),
                Action::CloseProject
            ),
            None
        );
        assert_eq!(km.set_shortcut(Action::CloseProject, chord), Some(owner));
        assert_eq!(km.lookup(chord), Some(Action::CloseProject));
        assert!(!km.shortcuts(owner).contains(&chord));
        assert_eq!(km.shortcuts(Action::CloseProject), &[chord]);
        // Change… replaces the action's shortcuts.
        km.set_shortcut(Action::Copy, KeyChord::parse("Ctrl+Alt+C").unwrap());
        assert_eq!(km.shortcuts(Action::Copy).len(), 1);
        assert_eq!(km.lookup(KeyChord::parse("Ctrl+C").unwrap()), None);
    }

    #[test]
    fn keymap_export_and_import_round_trip() {
        let base = Keymap::preset("Eclipse");
        let mut km = base.clone();
        km.set_shortcut(Action::Save, KeyChord::parse("Alt+Shift+S").unwrap());
        km.clear_action(Action::Exit);
        let overrides = km.overrides_from(&base);
        let json = Keymap::export_json("Eclipse", &overrides);
        let (preset, imported, skipped) = Keymap::import_json(&json).unwrap();
        assert_eq!(preset, "Eclipse");
        assert!(skipped.is_empty(), "{skipped:?}");
        let mut again = Keymap::preset(&preset);
        again.apply_overrides(&imported);
        for &a in Action::ALL {
            assert!(again.same_shortcuts(&km, a), "{a:?}");
        }
        // Unknown actions and shortcuts are reported, a missing parent means Default.
        let (preset, imported, skipped) = Keymap::import_json(
            r#"{"bindings": {"Bogus": ["Ctrl+B"], "SaveAll": ["Ctrl+Nope", "Ctrl+Alt+A"]}}"#,
        )
        .unwrap();
        assert_eq!(preset, "Default");
        assert_eq!(
            imported,
            vec![("SaveAll".to_string(), vec!["Ctrl+Alt+A".to_string()])]
        );
        assert_eq!(skipped.len(), 2, "{skipped:?}");
        assert!(Keymap::import_json("[1, 2]").is_err());
    }

    #[test]
    fn default_keymap_resolves_core_actions() {
        let km = Keymap::default_keymap();
        assert_eq!(
            km.lookup(KeyChord::parse("Ctrl+S").unwrap()),
            Some(Action::Save)
        );
        assert_eq!(
            km.lookup(KeyChord::parse("Ctrl+Shift+Z").unwrap()),
            Some(Action::Redo)
        );
        assert_eq!(
            km.lookup(KeyChord::parse("Shift+F10").unwrap()),
            Some(Action::Run)
        );
        assert_eq!(
            km.lookup(KeyChord::parse("Ctrl+F5").unwrap()),
            Some(Action::Rerun)
        );
        assert_eq!(
            km.lookup(KeyChord::parse("Alt+Shift+F9").unwrap()),
            Some(Action::ChooseDebugConfiguration)
        );
        assert!(
            km.shortcuts(Action::RunAnything).is_empty(),
            "double Ctrl is detected by the app, not bound as a chord"
        );
        // The debugger chords.
        for (chord, action) in [
            ("Shift+F7", Action::SmartStepInto),
            ("Alt+F10", Action::ShowExecutionPoint),
            ("Ctrl+Alt+F8", Action::QuickEvaluateExpression),
            ("Ctrl+Alt+Shift+F8", Action::ToggleTemporaryLineBreakpoint),
            ("Alt+F9", Action::RunToCursor),
        ] {
            assert_eq!(
                km.lookup(KeyChord::parse(chord).unwrap()),
                Some(action),
                "{chord}"
            );
        }
        assert!(km.shortcuts(Action::Copy).len() >= 2);
        assert_eq!(
            km.lookup(KeyChord::parse("Alt+Num1").unwrap()),
            Some(Action::ToggleProjectView)
        );
        assert_eq!(
            km.lookup(KeyChord::parse("Ctrl+Alt+F7").unwrap()),
            Some(Action::ShowUsages)
        );
        assert_eq!(
            km.lookup(KeyChord::parse("Ctrl+Shift+T").unwrap()),
            Some(Action::GoToTest)
        );
        assert_eq!(
            km.lookup(KeyChord::parse("Ctrl+Shift+B").unwrap()),
            Some(Action::GoToTypeDeclaration)
        );
        assert_eq!(
            km.lookup(KeyChord::parse("Alt+Shift+E").unwrap()),
            Some(Action::ExecuteInConsole)
        );
        // Next/Previous Change (Clone Caret uses the double-Ctrl gesture).
        assert_eq!(
            km.lookup(KeyChord::parse("Ctrl+Alt+Shift+ArrowDown").unwrap()),
            Some(Action::VcsShowNextChangeMarker)
        );
        assert_eq!(
            km.lookup(KeyChord::parse("Ctrl+Alt+Shift+ArrowUp").unwrap()),
            Some(Action::VcsShowPrevChangeMarker)
        );
        assert!(
            km.is_default(Action::AddCaretAbove) && km.is_default(Action::VcsShowPrevChangeMarker)
        );
        // Every action id must be unique.
        let mut ids: Vec<_> = Action::ALL.iter().map(|a| a.id()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), Action::ALL.len());
    }

    #[test]
    fn shifted_symbols_fall_back_to_the_physical_key() {
        let km = Keymap::default_keymap();
        let ctrl_shift = Modifiers {
            ctrl: true,
            shift: true,
            ..Modifiers::NONE
        };
        // US layout: Ctrl+Shift+/ arrives as `?` with the physical key `/`.
        assert_eq!(
            km.lookup_key(Key::Questionmark, Some(Key::Slash), ctrl_shift),
            Some(Action::CommentBlock)
        );
        assert_eq!(
            km.lookup_key(Key::Plus, Some(Key::Equals), ctrl_shift),
            Some(Action::ExpandAll)
        );
        // A bound logical chord wins over the physical key.
        assert_eq!(
            km.lookup_key(
                Key::Slash,
                Some(Key::Num7),
                Modifiers {
                    ctrl: true,
                    ..Modifiers::NONE
                }
            ),
            Some(Action::CommentLine)
        );
        assert_eq!(km.lookup_key(Key::Questionmark, None, ctrl_shift), None);
    }
}

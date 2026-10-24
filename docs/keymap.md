# Keymap reference

This table is generated from `crates/rustpy-ui/src/actions.rs`. It lists built-in
base bindings, including unbound actions. Settings › Keymap, the selected parent
map, plugins and user overrides determine the effective bindings. Find Action
shows the current binding; macOS and alternative parent maps can differ. Search
by action name or stable action ID when importing a keymap. Chords are dispatched
in their active context, and editor input/IME must not be treated as a global chord.

To change a binding, select an action in Settings › Keymap, add keyboard/mouse
shortcuts, and resolve displayed conflicts. Duplicate or export a map before
large changes. The bundled map assets live in `crates/rustpy-ui/assets/keymaps/`.
Regenerate this reference with `python3 scripts/update-keymap-reference.py` after
changing base action metadata.

| Action | Stable ID | Base shortcuts |
|---|---|---|
| New File… | `NewFile` | Ctrl+Alt+Insert |
| New Scratch File | `NewScratchFile` | Ctrl+Alt+Shift+Insert |
| New Scratch Buffer | `ScratchNewBuffer` | Unbound |
| Save File as Template… | `SaveFileAsTemplate` | Unbound |
| Project Windows… | `ShowProjectWindows` | Unbound |
| Next Project Window | `NextProjectWindow` | Unbound |
| Previous Project Window | `PreviousProjectWindow` | Unbound |
| Merge All Project Windows | `MergeAllProjectWindows` | Unbound |
| Open… | `OpenFile` | Ctrl+O |
| New Project… | `NewProject` | Unbound |
| Close Project | `CloseProject` | Unbound |
| Get from Version Control… | `Vcs.Checkout` | Unbound |
| Save | `SaveDocument` | Ctrl+S |
| Save All | `SaveAll` | Ctrl+Shift+S |
| Toggle Read-Only Attribute | `ToggleReadOnlyAttribute` | Unbound |
| Make Writable… | `MakeFileWritable` | Unbound |
| Close Tab | `CloseContent` | Ctrl+F4 |
| Close All Tabs | `CloseAllEditors` | Unbound |
| Close Other Tabs | `CloseAllEditorsButActive` | Unbound |
| Close All but Pinned | `CloseAllUnpinnedEditors` | Unbound |
| Close Tabs to the Left | `CloseAllToTheLeft` | Unbound |
| Close Tabs to the Right | `CloseAllToTheRight` | Unbound |
| Sort Tabs by File Name | `SortTabsByFilename` | Unbound |
| Pin / Unpin Tab | `PinActiveEditorTab` | Unbound |
| Reopen Closed Tab | `ReopenClosedTab` | Unbound |
| Settings… | `ShowSettings` | Ctrl+Alt+S |
| Plugins… | `WelcomeScreen.Plugins` | Unbound |
| Exit | `Exit` | Unbound |
| Undo | `$Undo` | Ctrl+Z |
| Redo | `$Redo` | Ctrl+Shift+Z |
| Undo History… | `UndoHistory` | Unbound |
| Cut | `$Cut` | Ctrl+X; Shift+Delete |
| Copy | `$Copy` | Ctrl+C; Ctrl+Insert |
| Paste | `$Paste` | Ctrl+V; Shift+Insert |
| Paste from History… | `PasteMultiple` | Ctrl+Shift+V |
| Copy Path… | `CopyPaths` | Ctrl+Shift+C |
| Copy Reference | `CopyReference` | Ctrl+Alt+Shift+C |
| Show Local History | `LocalHistory.ShowHistory` | Unbound |
| Reload from Disk | `ReloadFromDisk` | Unbound |
| Reload All from Disk | `Synchronize` | Ctrl+Alt+Y |
| Invalidate Caches… | `InvalidateCaches` | Unbound |
| Power Save Mode | `TogglePowerSave` | Unbound |
| Background Tasks | `ShowBackgroundTasks` | Unbound |
| Start/Stop Macro Recording | `StartStopMacroRecording` | Unbound |
| Play Back Last Macro | `PlaybackLastMacro` | Unbound |
| Edit Macros… | `EditMacros` | Unbound |
| Paste as Plain Text | `PasteSimple` | Ctrl+Alt+Shift+V |
| Annotate with Git Blame | `Annotate` | Unbound |
| Show History | `Vcs.ShowTabbedFileHistory` | Unbound |
| Mercurial / SVN / Perforce | `Vcs.OtherProviders` | Unbound |
| Stash Changes… | `Git.Stash` | Unbound |
| Add to .gitignore | `Git.Ignore` | Unbound |
| Pull Requests | `Github.PullRequests.ToolWindow` | Unbound |
| GitHub Accounts… | `Github.Accounts` | Unbound |
| Share Project on GitHub… | `Github.ShareProject` | Unbound |
| Create Gist… | `Github.CreateGist` | Unbound |
| Create Pull Request… | `Github.CreatePullRequest` | Unbound |
| Clone from GitHub… | `Github.Clone` | Unbound |
| Open on GitHub | `Github.OpenOnWeb` | Unbound |
| Open GitHub Issue | `Github.OpenIssue` | Unbound |
| Branches… | `Git.Branches` | Ctrl+Shift+Backquote |
| Type Hierarchy | `TypeHierarchy` | Ctrl+H |
| Method Hierarchy | `MethodHierarchy` | Ctrl+Shift+H |
| Call Hierarchy | `CallHierarchy` | Ctrl+Alt+H |
| Go to Implementation(s) | `GotoImplementation` | Ctrl+Alt+B |
| Go to Super Method | `GotoSuperMethod` | Ctrl+U |
| Hierarchy | `ActivateHierarchyToolWindow` | Unbound |
| Compare with Clipboard | `CompareClipboardWithSelection` | Unbound |
| Compare File with… | `CompareTwoFiles` | Unbound |
| Markdown: Cycle Preview Layout | `Markdown.Layout.Toggle` | Unbound |
| reStructuredText: Cycle Preview Layout | `Rst.Layout.Toggle` | Unbound |
| Select All | `$SelectAll` | Ctrl+A |
| Extend Selection | `EditorSelectWord` | Ctrl+W |
| Shrink Selection | `EditorUnSelectWord` | Ctrl+Shift+W |
| Add Selection for Next Occurrence | `SelectNextOccurrence` | Alt+J |
| Unselect Occurrence | `UnselectPreviousOccurrence` | Alt+Shift+J |
| Select All Occurrences | `SelectAllOccurrences` | Ctrl+Alt+Shift+J |
| Clone Caret Above | `EditorCloneCaretAbove` | Unbound |
| Clone Caret Below | `EditorCloneCaretBelow` | Unbound |
| Sticky Selection | `EditorToggleStickySelection` | Unbound |
| Column Selection Mode | `EditorToggleColumnMode` | Alt+Shift+Insert |
| Duplicate Line or Selection | `EditorDuplicate` | Ctrl+D |
| Delete Line | `EditorDeleteLine` | Ctrl+Y |
| Join Lines | `EditorJoinLines` | Ctrl+Shift+J |
| Move Line Up | `MoveLineUp` | Alt+Shift+ArrowUp |
| Move Line Down | `MoveLineDown` | Alt+Shift+ArrowDown |
| Move Statement Up | `MoveStatementUp` | Ctrl+Shift+ArrowUp |
| Move Statement Down | `MoveStatementDown` | Ctrl+Shift+ArrowDown |
| Comment with Line Comment | `CommentByLineComment` | Ctrl+Slash |
| Comment with Block Comment | `CommentByBlockComment` | Ctrl+Shift+Slash |
| Move Element Left | `MoveElementLeft` | Ctrl+Alt+Shift+ArrowLeft |
| Move Element Right | `MoveElementRight` | Ctrl+Alt+Shift+ArrowRight |
| Sort Lines | `SortLines` | Unbound |
| Reverse Lines | `ReverseLines` | Unbound |
| Remove Duplicate Lines | `RemoveDuplicateLines` | Unbound |
| Strip Trailing Spaces | `StripTrailingSpaces` | Unbound |
| Convert Indents to Spaces | `ConvertIndentsToSpaces` | Unbound |
| Convert Indents to Tabs | `ConvertIndentsToTabs` | Unbound |
| Toggle Case | `EditorToggleCase` | Ctrl+Shift+U |
| Indent Selection | `EditorIndentSelection` | Tab |
| Unindent Line or Selection | `EditorUnindentSelection` | Shift+Tab |
| Complete Current Statement | `EditorCompleteStatement` | Ctrl+Shift+Enter |
| Unwrap/Remove… | `Unwrap` | Ctrl+Shift+Delete |
| Context Info | `EditorContextInfo` | Alt+Q |
| Quick Definition | `QuickImplementations` | Ctrl+Shift+I |
| Cyclic Expand Word | `HippieCompletion` | Alt+Slash |
| Start New Line | `EditorStartNewLine` | Shift+Enter |
| Start New Line Before Current | `EditorStartNewLineBefore` | Ctrl+Alt+Enter |
| Delete to Word Start | `EditorDeleteToWordStart` | Ctrl+Backspace |
| Delete to Word End | `EditorDeleteToWordEnd` | Ctrl+Delete |
| Delete to Line Start | `EditorDeleteToLineStart` | Unbound |
| Delete to Line End | `EditorDeleteToLineEnd` | Unbound |
| Reformat Code | `ReformatCode` | Ctrl+Alt+L |
| Reformat File… | `ReformatFile` | Ctrl+Alt+Shift+L |
| Rearrange Code | `RearrangeCode` | Unbound |
| Inspect Code… | `InspectCode` | Unbound |
| Run Inspection by Name… | `RunInspectionByName` | Unbound |
| Silent Code Cleanup | `SilentCodeCleanup` | Unbound |
| Code Cleanup… | `CodeCleanup` | Unbound |
| Inject language or reference | `InjectLanguage` | Unbound |
| Edit injected fragment | `EditInjectedFragment` | Unbound |
| Go to injected file reference | `GoToInjectedReference` | Unbound |
| Check RegExp… | `CheckRegExp` | Unbound |
| Smart Type Completion | `SmartTypeCompletion` | Ctrl+Shift+Space |
| Open in Built-in Web Preview | `WebPreview.Open` | Unbound |
| Web Preview Tool Window | `WebPreview.Toggle` | Unbound |
| Open URL… | `WebOpenUrl` | Unbound |
| Open in Browser | `WebOpenInAction` | Alt+F2 |
| Build Sphinx Docs | `Sphinx.Build` | Unbound |
| Run Docutils Task | `Docutils.Run` | Unbound |
| Open Generated Documentation | `Documentation.OpenPreview` | Unbound |
| Frameworks / Django Structure… | `Frameworks.Workspace` | Unbound |
| Run manage.py Task… | `Django.RunManageTask` | Ctrl+Alt+R |
| Semantic Highlighting (rainbow variables) | `ToggleSemanticHighlighting` | Unbound |
| Trust Project | `TrustProject` | Unbound |
| Inotify Watches Limit Instructions… | `ShowInotifyLimitInstructions` | Unbound |
| Don't Show File Watcher Limit Warning | `HideInotifyLimitWarning` | Unbound |
| Add Remote Interpreter… | `AddRemoteInterpreter` | Unbound |
| Quick List | `QuickList` | Ctrl+Alt+Shift+Q |
| Create Command-line Launcher… | `CreateLauncherScript` | Unbound |
| Show Log in Files | `ShowLogInFiles` | Unbound |
| Collect Diagnostic Data | `CollectDiagnostics` | Unbound |
| Customize Toolbar and Window… | `CustomizeToolbar` | Unbound |
| Zoom IDE In | `ZoomIDEIn` | Ctrl+Equals |
| Zoom IDE Out | `ZoomIDEOut` | Ctrl+Minus |
| Reset IDE Scale | `ZoomIDEReset` | Unbound |
| IDE Activity Monitor | `IdeActivityMonitor` | Unbound |
| Dump IDE Threads | `DumpIdeThreads` | Unbound |
| Profile IDE CPU | `ProfileIdeCpu` | Unbound |
| Report a RustPy Issue | `ReportIdeIssue` | Unbound |
| Analyze Stack Trace or Thread Dump… | `AnalyzeStackTrace` | Unbound |
| Debug Log Settings… | `DebugLogSettings` | Unbound |
| Learn IDE Features | `ShowLearnPanel` | Unbound |
| Configuration… | `Deployment.Configuration` | Unbound |
| Upload to Default Server | `PublishAction` | Ctrl+Alt+Shift+X |
| Download from Default Server | `Deployment.Download` | Unbound |
| Sync with Deployed… | `Deployment.Sync` | Unbound |
| Browse Remote Host | `Deployment.BrowseRemoteHost` | Unbound |
| Copy as Rich Text | `CopyAsRichText` | Unbound |
| Collaborate… | `Collaboration.Session` | Unbound |
| Request AI Inline Completion | `AIAssistant.Editor.InlineCompletion` | Unbound |
| AI Assistant | `AIAssistant.ToolWindow` | Unbound |
| Explain Code | `AIAssistant.Editor.ExplainCode` | Unbound |
| Suggest Refactoring | `AIAssistant.Editor.SuggestRefactoring` | Unbound |
| Generate Unit Tests | `AIAssistant.Editor.GenerateTests` | Unbound |
| Generate Documentation | `AIAssistant.Editor.GenerateDocumentation` | Unbound |
| Fix with AI | `AIAssistant.Editor.FixError` | Unbound |
| Copy as Plain Text | `CopyAsPlainText` | Unbound |
| Open Task… | `tasks.goto` | Alt+Shift+N |
| Export Settings… | `ExportSettings` | Unbound |
| Settings Sync… | `SettingsSync` | Unbound |
| Settings Repository… | `SettingsRepository` | Unbound |
| Import Settings… | `ImportSettings` | Unbound |
| Restore Default Settings… | `RestoreDefaultSettings` | Unbound |
| Settings for New Projects… | `Settings.NewProjects` | Unbound |
| Popup and Dialog Options… | `Settings.Dialogs` | Unbound |
| Repair IDE… | `Maintenance.RepairIde` | Unbound |
| Registry… | `Maintenance.Registry` | Unbound |
| UI Inspector… | `Internal.UiInspector` | Unbound |
| Restart IDE… | `Maintenance.Restart` | Unbound |
| Switch Task… | `tasks.switch` | Alt+Shift+T |
| Close Active Task… | `tasks.close` | Unbound |
| Save Context… | `context.save` | Alt+Shift+S |
| Load Context… | `context.load` | Alt+Shift+L |
| Clear Context | `context.clear` | Alt+Shift+X |
| Generate stub for native module | `GenerateNativeStub` | Unbound |
| Generate Docstring | `GenerateDocstring` | Unbound |
| Update Copyright | `UpdateCopyright` | Unbound |
| Copy File… | `CopyElement` | F5 |
| Show Django Model Diagram | `Diagrams.DjangoModels` | Unbound |
| Show Database ER Diagram | `Diagrams.DatabaseER` | Unbound |
| Show Diagram (classes) | `ShowUmlDiagram` | Ctrl+Alt+Shift+U |
| Diagram | `ActivateDiagramToolWindow` | Unbound |
| Endpoints | `ActivateEndpointsToolWindow` | Unbound |
| Fill Paragraph | `FillParagraph` | Unbound |
| Optimize Imports | `OptimizeImports` | Ctrl+Alt+O |
| Show Context Actions | `ShowIntentionActions` | Alt+Enter |
| Basic Completion | `CodeCompletion` | Ctrl+Space |
| Parameter Info | `ParameterInfo` | Ctrl+P |
| Web Development / Language Services… | `Web.Development` | Unbound |
| Data File Tools… | `DataFile.Tools` | Unbound |
| Copy JSON Path | `Json.CopyPath` | Unbound |
| Quick Documentation | `QuickJavaDoc` | Ctrl+Q |
| External Documentation | `ExternalJavaDoc` | Shift+F1 |
| Emmet Preview… | `EmmetPreview` | Unbound |
| Surround with Emmet… | `SurroundWithEmmet` | Unbound |
| Surround With… | `SurroundWith` | Ctrl+Alt+T |
| Run All Tests | `RunAllTests` | Unbound |
| Run Doctests in Current File | `RunDoctests` | Unbound |
| Detach Tab to Window | `DetachEditor` | Unbound |
| Reattach Detached Tab | `ReattachEditor` | Unbound |
| Run with Coverage | `Coverage` | Unbound |
| Run with Concurrency Diagram | `RunWithConcurrencyDiagram` | Unbound |
| Concurrency Diagram | `ActivateConcurrencyToolWindow` | Unbound |
| Profile | `Profile` | Unbound |
| Coverage | `ActivateCoverageToolWindow` | Unbound |
| Profiler | `ActivateProfilerToolWindow` | Unbound |
| Install Requirements | `InstallRequirements` | Unbound |
| Run Tests in Current File | `RunTestsInFile` | Unbound |
| Rerun Failed Tests | `RerunFailedTests` | Unbound |
| Tests | `ActivateTestsToolWindow` | Unbound |
| Save Selection as Live Template… | `SaveSelectionAsLiveTemplate` | Unbound |
| Insert Live Template… | `InsertLiveTemplate` | Ctrl+J |
| Generate… | `Generate` | Alt+Insert |
| Rename… | `RenameElement` | Shift+F6 |
| Refactor This… | `Refactorings.QuickListPopupAction` | Ctrl+Alt+Shift+T |
| Extract Variable… | `IntroduceVariable` | Ctrl+Alt+V |
| Extract Constant… | `IntroduceConstant` | Ctrl+Alt+C |
| Extract Field… | `IntroduceField` | Ctrl+Alt+F |
| Extract Parameter… | `IntroduceParameter` | Ctrl+Alt+P |
| Extract Method… | `ExtractMethod` | Ctrl+Alt+M |
| Inline… | `Inline` | Ctrl+Alt+N |
| Safe Delete… | `SafeDelete` | Alt+Delete |
| Change Signature… | `ChangeSignature` | Ctrl+F6 |
| Move… | `Move` | F6 |
| Move Symbol to Module… | `MoveSymbolToModule` | Unbound |
| Rename Module… | `RenameModule` | Unbound |
| Pull Members Up… | `MembersPullUp` | Unbound |
| Push Members Down… | `MemberPushDown` | Unbound |
| Extract Superclass… | `ExtractSuperclass` | Unbound |
| Make Method Abstract | `MakeAbstract` | Unbound |
| Implement Abstract Methods | `ImplementMethods` | Ctrl+I |
| Convert to Dataclass | `ConvertToDataclass` | Unbound |
| Add Type Hints | `AddTypeHints` | Unbound |
| Introduce Type Alias | `IntroduceTypeAlias` | Unbound |
| Inline Type Alias | `InlineTypeAlias` | Unbound |
| Convert… | `ConvertRefactoring` | Unbound |
| Convert to Package | `ConvertToPackage` | Unbound |
| Convert to Module | `ConvertToModule` | Unbound |
| Find… | `Find` | Ctrl+F |
| Replace… | `Replace` | Ctrl+R |
| Find Next / Move to Next Occurrence | `FindNext` | F3 |
| Find Previous / Move to Previous Occurrence | `FindPrevious` | Shift+F3 |
| Find Word at Caret | `FindWordAtCaret` | Ctrl+F3 |
| Find in Files… | `FindInPath` | Ctrl+Shift+F |
| Search Structurally… | `StructuralSearchPlugin.StructuralSearchAction` | Unbound |
| Replace Structurally… | `StructuralSearchPlugin.StructuralReplaceAction` | Unbound |
| Replace in Files… | `ReplaceInPath` | Ctrl+Shift+R |
| Find Usages | `FindUsages` | Alt+F7 |
| Find Usages in File | `FindUsagesInFile` | Alt+Shift+F7 |
| Find Usages Settings | `FindUsagesSettings` | Unbound |
| Show Usages | `ShowUsages` | Ctrl+Alt+F7 |
| Highlight Usages in File | `HighlightUsagesInFile` | Ctrl+Shift+F7 |
| Search Everywhere | `SearchEverywhere` | Shift+Shift |
| Find Action… | `GotoAction` | Ctrl+Shift+A |
| Go to Class… | `GotoClass` | Ctrl+N |
| Go to File… | `GotoFile` | Ctrl+Shift+N |
| Go to Symbol… | `GotoSymbol` | Ctrl+Alt+Shift+N |
| Go to Line:Column… | `GotoLine` | Ctrl+G |
| Go to Declaration or Usages | `GotoDeclaration` | Ctrl+B |
| Go to Source / Stub | `GotoSourceOrStub` | Unbound |
| Go to Type Declaration | `GotoTypeDeclaration` | Ctrl+Shift+B |
| Go to Test | `GotoTest` | Ctrl+Shift+T |
| File Structure | `FileStructurePopup` | Ctrl+F12 |
| Recent Files | `RecentFiles` | Ctrl+E |
| Recently Changed Files | `RecentlyChangedFiles` | Unbound |
| Recently Edited Files | `RecentlyEditedFiles` | Unbound |
| Recent Locations | `RecentLocations` | Ctrl+Shift+E |
| Switcher | `Switcher` | Ctrl+Tab |
| Back | `Back` | Ctrl+Alt+ArrowLeft |
| Forward | `Forward` | Ctrl+Alt+ArrowRight |
| Last Edit Location | `JumpToLastChange` | Ctrl+Shift+Backspace |
| Show Error Description | `ShowErrorDescription` | Ctrl+F1 |
| Next Highlighted Error | `GotoNextError` | F2 |
| Previous Highlighted Error | `GotoPreviousError` | Shift+F2 |
| Next Method | `MethodDown` | Alt+ArrowDown |
| Previous Method | `MethodUp` | Alt+ArrowUp |
| Select Next Tab | `NextTab` | Alt+ArrowRight |
| Select Previous Tab | `PreviousTab` | Alt+ArrowLeft |
| Move Caret to Matching Brace | `EditorMatchBrace` | Ctrl+Shift+M |
| Scroll to Center | `EditorScrollToCenter` | Ctrl+M |
| Move Caret to Code Block Start | `EditorCodeBlockStart` | Ctrl+OpenBracket |
| Move Caret to Code Block End | `EditorCodeBlockEnd` | Ctrl+CloseBracket |
| Toggle Bookmark | `ToggleBookmark` | F11 |
| Toggle Bookmark with Mnemonic | `ToggleBookmarkWithMnemonic` | Ctrl+F11 |
| Go to Bookmark 0 | `GotoBookmark0` | Ctrl+Num0 |
| Go to Bookmark 1 | `GotoBookmark1` | Ctrl+Num1 |
| Go to Bookmark 2 | `GotoBookmark2` | Ctrl+Num2 |
| Go to Bookmark 3 | `GotoBookmark3` | Ctrl+Num3 |
| Go to Bookmark 4 | `GotoBookmark4` | Ctrl+Num4 |
| Go to Bookmark 5 | `GotoBookmark5` | Ctrl+Num5 |
| Go to Bookmark 6 | `GotoBookmark6` | Ctrl+Num6 |
| Go to Bookmark 7 | `GotoBookmark7` | Ctrl+Num7 |
| Go to Bookmark 8 | `GotoBookmark8` | Ctrl+Num8 |
| Go to Bookmark 9 | `GotoBookmark9` | Ctrl+Num9 |
| Next Bookmark | `GotoNextBookmark` | Unbound |
| Previous Bookmark | `GotoPreviousBookmark` | Unbound |
| Show Bookmarks | `ShowBookmarks` | Shift+F11 |
| Select In Project View | `SelectIn` | Unbound |
| Select In… | `SelectInPopup` | Alt+F1 |
| Go to Related Symbol | `GotoRelated` | Ctrl+Alt+Home |
| Jump to Source | `EditSource` | F4 |
| Open in Right Split | `OpenInRightSplit` | Unbound |
| Left | `EditorLeft` | ArrowLeft |
| Right | `EditorRight` | ArrowRight |
| Up | `EditorUp` | ArrowUp |
| Down | `EditorDown` | ArrowDown |
| Move Caret to Previous Word | `EditorPreviousWord` | Ctrl+ArrowLeft |
| Move Caret to Next Word | `EditorNextWord` | Ctrl+ArrowRight |
| Move Caret to Line Start | `EditorLineStart` | Home |
| Move Caret to Line End | `EditorLineEnd` | End |
| Page Up | `EditorPageUp` | PageUp |
| Page Down | `EditorPageDown` | PageDown |
| Move Caret to Text Start | `EditorTextStart` | Ctrl+Home |
| Move Caret to Text End | `EditorTextEnd` | Ctrl+End |
| Left with Selection | `EditorLeftWithSelection` | Shift+ArrowLeft |
| Right with Selection | `EditorRightWithSelection` | Shift+ArrowRight |
| Up with Selection | `EditorUpWithSelection` | Shift+ArrowUp |
| Down with Selection | `EditorDownWithSelection` | Shift+ArrowDown |
| Move Caret to Previous Word with Selection | `EditorPreviousWordWithSelection` | Ctrl+Shift+ArrowLeft |
| Move Caret to Next Word with Selection | `EditorNextWordWithSelection` | Ctrl+Shift+ArrowRight |
| Move Caret to Line Start with Selection | `EditorLineStartWithSelection` | Shift+Home |
| Move Caret to Line End with Selection | `EditorLineEndWithSelection` | Shift+End |
| Page Up with Selection | `EditorPageUpWithSelection` | Shift+PageUp |
| Page Down with Selection | `EditorPageDownWithSelection` | Shift+PageDown |
| Move Caret to Text Start with Selection | `EditorTextStartWithSelection` | Ctrl+Shift+Home |
| Move Caret to Text End with Selection | `EditorTextEndWithSelection` | Ctrl+Shift+End |
| Scroll Up | `EditorScrollUp` | Ctrl+ArrowUp |
| Scroll Down | `EditorScrollDown` | Ctrl+ArrowDown |
| Backspace | `EditorBackSpace` | Backspace; Shift+Backspace |
| Delete | `EditorDelete` | Delete |
| Enter | `EditorEnter` | Enter |
| Escape | `EditorEscape` | Escape |
| Emacs: Kill Line | `EditorEmacsKillLine` | Unbound |
| Emacs: Kill Word Forward | `EditorEmacsKillWordForward` | Unbound |
| Emacs: Kill Word Backward | `EditorEmacsKillWordBackward` | Unbound |
| Emacs: Kill Region | `EditorEmacsKillRegion` | Unbound |
| Emacs: Copy Region to Kill Ring | `EditorEmacsCopyRegion` | Unbound |
| Emacs: Yank | `EditorEmacsYank` | Unbound |
| Emacs: Yank Pop | `EditorEmacsYankPop` | Unbound |
| Emacs: Transpose Characters | `EditorEmacsTranspose` | Unbound |
| Toggle Insert/Overwrite | `EditorToggleInsertState` | Insert |
| Editor Line Actions | `EditorLineActions` | Ctrl+Alt+G |
| Collapse/Expand Region | `CollapseRegion` | Ctrl+Period |
| Expand | `ExpandRegion` | Ctrl+Alt+Equals |
| Collapse | `CollapseRegionAlt` | Ctrl+Alt+Minus |
| Expand All | `ExpandAllRegions` | Ctrl+Shift+Equals |
| Collapse All | `CollapseAllRegions` | Ctrl+Shift+Minus |
| Next Docstring Section | `NextDocstringSection` | Unbound |
| Previous Docstring Section | `PreviousDocstringSection` | Unbound |
| Toggle Rendered View | `ToggleRenderedDocPresentation` | Ctrl+Alt+Q |
| Render All Doc Comments | `ToggleRenderedDocPresentationForAll` | Unbound |
| Reader Mode | `ToggleReaderMode` | Unbound |
| Project | `ActivateProjectToolWindow` | Alt+Num1 |
| Commit | `ActivateCommitToolWindow` | Alt+Num0 |
| Find | `ActivateFindToolWindow` | Alt+Num3 |
| Run | `ActivateRunToolWindow` | Alt+Num4 |
| Debug | `ActivateDebugToolWindow` | Alt+Num5 |
| Problems | `ActivateProblemsViewToolWindow` | Alt+Num6 |
| Structure | `ActivateStructureToolWindow` | Alt+Num7 |
| HTTP Client | `ActivateHTTPClientToolWindow` | Unbound |
| Database | `ActivateDatabaseToolWindow` | Unbound |
| Run SQL from Selection / Python String | `Database.RunSqlFromEditor` | Unbound |
| SQL Dialect and Resolution Scope… | `Database.ResolutionScope` | Unbound |
| Run HTTP Request at Caret | `HttpClient.RunRequest` | Unbound |
| Services | `ActivateServicesToolWindow` | Alt+Num8 |
| Branches | `ActivateBranchesToolWindow` | Unbound |
| Git | `ActivateVersionControlToolWindow` | Alt+Num9 |
| Terminal | `ActivateTerminalToolWindow` | Alt+F12 |
| Open in Terminal | `Terminal.OpenAtEditor` | Unbound |
| Send Selection to Terminal | `Terminal.SendSelection` | Unbound |
| Python Console | `ActivatePythonConsoleToolWindow` | Unbound |
| Python Packages | `ActivatePythonPackagesToolWindow` | Unbound |
| Manage Python Packages | `Python.ManagePackages` | Unbound |
| Run setup.py Task… | `Python.RunSetupTask` | Unbound |
| Build Python Distribution | `Python.BuildDistribution` | Unbound |
| Dependencies | `Python.Dependencies` | Unbound |
| TODO | `ActivateTODOToolWindow` | Unbound |
| Notifications | `ActivateNotificationsToolWindow` | Unbound |
| Documentation | `ActivateDocumentationToolWindow` | Unbound |
| Bookmarks | `ActivateBookmarksToolWindow` | Alt+Num2 |
| Hide All Tool Windows | `HideAllWindows` | Ctrl+Shift+F12 |
| Focus Tool Window Controls | `FocusToolWindowControls` | Ctrl+Alt+F6 |
| Increase Left Tool Window Width | `IncreaseLeftToolWidth` | Unbound |
| Decrease Left Tool Window Width | `DecreaseLeftToolWidth` | Unbound |
| Increase Bottom Tool Window Height | `IncreaseBottomToolHeight` | Unbound |
| Decrease Bottom Tool Window Height | `DecreaseBottomToolHeight` | Unbound |
| Float or Dock Tool Window | `ToggleToolWindowFloating` | Unbound |
| Maximize or Restore Tool Window | `ToggleToolWindowMaximized` | Ctrl+Shift+Quote |
| Stretch Tool Window to Left | `StretchToolWindowLeft` | Unbound |
| Stretch Tool Window to Right | `StretchToolWindowRight` | Unbound |
| Stretch Tool Window to Top | `StretchToolWindowUp` | Unbound |
| Stretch Tool Window to Bottom | `StretchToolWindowDown` | Unbound |
| Widen Tool Window | `WidenToolWindow` | Ctrl+Alt+Shift+Quote |
| Hide Active Tool Window | `HideActiveToolWindow` | Shift+Escape |
| Jump to Last Tool Window | `JumpToLastWindow` | F12 |
| Save Current Layout As… | `StoreNewLayout` | Unbound |
| Store Current Layout as Default | `StoreDefaultLayout` | Unbound |
| Restore Default Layout | `RestoreDefaultLayout` | Unbound |
| Restore Factory Default Layout | `RestoreFactoryDefaultLayout` | Unbound |
| Restore Layout… | `RestoreNamedLayout` | Unbound |
| Delete Layout… | `DeleteNamedLayout` | Unbound |
| Jump to Navigation Bar | `JumpToNavigationBar` | Alt+Home |
| Split Right | `SplitVertically` | Unbound |
| Split Down | `SplitHorizontally` | Unbound |
| Split and Move Right | `SplitAndMoveRight` | Unbound |
| Split and Move Down | `SplitAndMoveDown` | Unbound |
| Open in Opposite Group | `OpenEditorInOppositeTabGroup` | Unbound |
| Unsplit | `Unsplit` | Unbound |
| Unsplit All | `UnsplitAll` | Unbound |
| Enlarge First Editor Split | `EnlargeFirstSplit` | Unbound |
| Shrink First Editor Split | `ShrinkFirstSplit` | Unbound |
| Equalize Editor Splits | `EqualizeSplits` | Unbound |
| Change Splitter Orientation | `ChangeSplitOrientation` | Unbound |
| Move Tab to Opposite Group | `MoveEditorToOppositeTabGroup` | Unbound |
| Goto Next Splitter | `NextSplitter` | Alt+Tab |
| Goto Previous Splitter | `PrevSplitter` | Alt+Shift+Tab |
| Increase Font Size | `EditorIncreaseFontSize` | Ctrl+Plus; Ctrl+Shift+Period |
| Decrease Font Size | `EditorDecreaseFontSize` | Ctrl+Shift+Comma |
| Reset Font Size | `EditorResetFontSize` | Ctrl+Shift+Num0 |
| Show Line Numbers | `EditorToggleShowLineNumbers` | Unbound |
| Show Whitespaces | `EditorToggleShowWhitespaces` | Unbound |
| Soft-Wrap | `EditorToggleUseSoftWraps` | Unbound |
| Inlay Hints: Parameter Names | `ToggleInlayParameterHints` | Unbound |
| Inlay Hints: Variable Types | `ToggleInlayTypeHints` | Unbound |
| Show Indent Guides | `EditorToggleShowIndentLines` | Unbound |
| Details in Tree Views | `ShowDetailsInTreeViews` | Unbound |
| Enter Zen Mode | `ToggleZenMode` | Unbound |
| Exit Zen Mode | `ExitZenMode` | Unbound |
| Enter Distraction Free Mode | `ToggleDistractionFreeMode` | Unbound |
| Enter Full Screen | `ToggleFullScreen` | Unbound |
| Enter Presentation Mode | `TogglePresentationMode` | Unbound |
| Quick Switch Scheme… | `QuickChangeScheme` | Ctrl+Backtick |
| Run | `Run` | Shift+F10 |
| Debug | `Debug` | Shift+F9 |
| Stop | `Stop` | Ctrl+F2 |
| Run context configuration | `RunClass` | Ctrl+Shift+F10 |
| Debug context configuration | `DebugClass` | Ctrl+Shift+F9 |
| Run context with Coverage | `CoverageContext` | Unbound |
| Profile context configuration | `ProfileContext` | Unbound |
| Modify Run Configuration… | `ModifyContextConfiguration` | Unbound |
| Create Run Configuration… | `CreateContextConfiguration` | Unbound |
| Run… | `ChooseRunConfiguration` | Alt+Shift+F10 |
| Debug… | `ChooseDebugConfiguration` | Alt+Shift+F9 |
| Rerun | `Rerun` | Ctrl+F5 |
| Run Anything… | `RunAnything` | Ctrl+Ctrl |
| Edit Configurations… | `editRunConfigurations` | Unbound |
| Execute Selection in Python Console | `ExecuteInPyConsoleAction` | Alt+Shift+E |
| Run File in Python Console | `RunFileInPythonConsole` | Unbound |
| Run Cell | `PyRunCellAction` | Unbound |
| Run Cells Above | `PyRunCellsAboveAction` | Unbound |
| Debug Cell | `PyDebugCellAction` | Unbound |
| SciView | `ActivateSciViewToolWindow` | Unbound |
| Scientific Mode | `ScientificMode` | Unbound |
| Run Cell and Select Below | `PyRunCellAndSelectBelowAction` | Unbound |
| Toggle Line Breakpoint | `ToggleLineBreakpoint` | Ctrl+F8 |
| View Breakpoints… | `ViewBreakpoints` | Ctrl+Shift+F8 |
| Step Over | `StepOver` | F8 |
| Step Into | `StepInto` | F7 |
| Step Out | `StepOut` | Shift+F8 |
| Resume Program | `Resume` | F9 |
| Evaluate Expression… | `EvaluateExpression` | Alt+F8 |
| Run to Cursor | `RunToCursor` | Alt+F9 |
| Smart Step Into | `SmartStepInto` | Shift+F7 |
| Jump to Cursor | `JumpToCursor` | Unbound |
| Show Execution Point | `ShowExecutionPoint` | Alt+F10 |
| Quick Evaluate Expression | `QuickEvaluateExpression` | Ctrl+Alt+F8 |
| Toggle Temporary Line Breakpoint | `ToggleTemporaryLineBreakpoint` | Ctrl+Alt+Shift+F8 |
| Mute Breakpoints | `XDebugger.MuteBreakpoints` | Unbound |
| Restart Debugger | `RestartDebugger` | Unbound |
| Commit… | `CheckinProject` | Ctrl+K |
| Push… | `Vcs.Push` | Ctrl+Shift+K |
| Update Project… | `Vcs.UpdateProject` | Ctrl+T |
| Fetch | `Git.Fetch` | Unbound |
| Manage Remotes… | `Git.Configure.Remotes` | Unbound |
| VCS Operations | `Vcs.QuickListPopupAction` | Alt+Backtick |
| Show Diff | `Compare.SameVersion` | Unbound |
| Next Change | `VcsShowNextChangeMarker` | Ctrl+Alt+Shift+ArrowDown |
| Previous Change | `VcsShowPrevChangeMarker` | Ctrl+Alt+Shift+ArrowUp |
| About | `About` | Unbound |
| Help | `HelpTopics` | F1 |
| Welcome Guide | `Help.WelcomeGuide` | Unbound |
| Getting Started | `Help.GettingStarted` | Unbound |
| Remote Development… | `RemoteDevelopment.Open` | Unbound |
| Start Guided Tour | `Help.GuidedTour` | Unbound |
| Productivity Guide | `Help.ProductivityGuide` | Unbound |
| Accessibility Guide | `Help.AccessibilityGuide` | Unbound |
| Contact Support… | `Help.ContactSupport` | Unbound |
| Submit Feedback… | `Help.SubmitFeedback` | Unbound |
| Edit Custom Properties… | `Help.EditCustomProperties` | Unbound |
| Online Help | `Help.Online` | Unbound |
| Tip of the Day | `ShowTips` | Unbound |
| What's New in RustPy | `WhatsNewAction` | Unbound |
| Check for Updates… | `CheckForUpdate` | Unbound |
| Keyboard Shortcuts PDF | `Help.KeymapReference` | Unbound |

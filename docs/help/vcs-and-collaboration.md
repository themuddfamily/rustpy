# Version control and collaboration

Directory mappings identify the provider and root for each path; nested mappings
can change provider routing. Git roots can be selected individually or as an
explicit multiple-root operation. Local Changes, Commit, Log, Branches and shelf
views share the captured repository context. Read the confirmation's root and
commit range before destructive reset, revert, rebase, branch or tag operations.
Log filters, details, comparison and interactive rebase are distinct workflows;
stale source/ref checks can require reopening a plan.

Resolve conflicts through the diff/merge viewer and explicitly apply the result.
A preview does not implicitly stage or commit it. Automatic preservation uses
owned, indexed worktree snapshots; a failure must retain the recovery reference.
User-created patch shelves and binary/index-preserving automatic shelves have
different guarantees. Local History is separate from Git history.

Git network operations use the configured credential/proxy/SSH policy. Native
SSH requires its executable; built-in SSH has explicit transport and hook
boundaries. In particular a required pre-push hook must not be silently skipped;
use the supported native transport or an explicit permitted no-verify choice.
Git LFS requires its installed executable. Other-provider mappings require their
Hg/SVN/Perforce tools. GitHub/Enterprise links and operations require the correct
server, repository and account; issue links must use that captured context.

Collaborative editing sessions use an explicit host invitation and authenticated
session transport. The implemented transport binds loopback by default; access
across machines uses an operator-managed SSH/tunnel, not an advertised public relay.
Protect invitation credentials. Host document versions arbitrate edits; stale
changes are conflicts and must not overwrite current text. Participants can follow,
be read-only, receive permissions, be revoked or disconnect. A revoked participant
must not regain write permission by submitting an old edit. Voice is unsupported.

AI Assistant supports the existing Anthropic provider and a configured compatible
Chat Completions provider. Choose the actual provider endpoint/model available to
your account; there is no guaranteed model availability or automatic provider
migration. Keys remain in the password safe. Sending editor context transmits that
text to the configured service. Cancelling delivery prevents stale insertion;
the physical HTTP worker may remain until the underlying timeout/reader exits and
stays in the running-work inventory meanwhile. Automatic and explicit AI actions
still respect the project's execution/trust boundary.

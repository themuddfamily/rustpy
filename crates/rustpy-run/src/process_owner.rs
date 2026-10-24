//! Shared supervised command ownership for transfers and external tools.
pub struct ProcessOwner(crate::process_tree::ProcessTree);
impl ProcessOwner {
    pub fn prepare(command: &mut std::process::Command) {
        crate::process_tree::ProcessTree::prepare(command);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000 | 0x00000004 | 0x00000200);
        }
    }
    pub fn attach(child: &mut std::process::Child) -> std::io::Result<Self> {
        crate::process_tree::ProcessTree::new(child).map(Self)
    }
    pub fn poll(
        &self,
        child: &mut std::process::Child,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.0.poll(child)
    }
    pub fn stop(&self, child: &mut std::process::Child) {
        self.0.force(child);
    }
    pub fn stamp(&self) -> String {
        self.0.stamp()
    }
}

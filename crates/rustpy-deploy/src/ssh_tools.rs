//! SSH terminal, external tools and remote interpreter plans using deployment mappings.
use crate::{sh_quote, CommandSpec, Server, ServerKind};
use std::path::{Path, PathBuf};
fn target(server: &Server) -> anyhow::Result<crate::SshTarget> {
    let (host, user, port, key) = match &server.kind {
        ServerKind::Sftp {
            host,
            user,
            port,
            key_file,
        } => (host, user, *port, key_file.clone()),
        ServerKind::Rsync { host, user, port } => (host, user, *port, None),
        _ => anyhow::bail!("Choose an SSH deployment server"),
    };
    let host = if server.options.ssh_alias.is_empty() {
        host
    } else {
        &server.options.ssh_alias
    };
    let target = crate::SshTarget::new(host, port, user, key);
    target.validate()?;
    Ok(target)
}
pub fn terminal(server: &Server) -> anyhow::Result<CommandSpec> {
    let t = target(server)?;
    let mut command = CommandSpec::new(if server.options.ssh_executable.is_empty() {
        "ssh"
    } else {
        &server.options.ssh_executable
    })
    .args([
        "-o".into(),
        "StrictHostKeyChecking=ask".into(),
        "-tt".into(),
        "-p".into(),
        t.port.to_string(),
    ]);
    if let Some(ref key) = t.key_file {
        command = command.args(["-i".into(), key.to_string_lossy().into_owned()]);
    }
    if !server.options.ssh_config_file.is_empty() {
        command = command.args(["-F".into(), server.options.ssh_config_file.clone()]);
    }
    if !server.options.jump_host.is_empty() {
        command = command.args(["-J".into(), server.options.jump_host.clone()]);
    }
    command = command.args(["--".into(), t.destination()]);
    Ok(command)
}
pub fn external(server: &Server, command: &str) -> anyhow::Result<CommandSpec> {
    anyhow::ensure!(!command.contains('\0'), "SSH command contains NUL");
    let t = target(server)?;
    let mut context = crate::transfer::current();
    context.options = server.options.clone();
    Ok(crate::transfer::with_context(context, || {
        crate::ssh::ssh_command(
            &t,
            &format!("cd -- {} && {command}", sh_quote(&server.root_path)),
        )
    }))
}
pub fn python(server: &Server, args: &[String]) -> anyhow::Result<CommandSpec> {
    anyhow::ensure!(
        !server.options.remote_python.is_empty() && !server.options.remote_python.starts_with('-'),
        "Choose a remote Python executable"
    );
    let mut command = if server.options.sudo {
        "exec sudo -n -- ".to_string()
    } else {
        "exec ".into()
    };
    let helpers = crate::join_remote(&server.root_path, &server.options.helper_directory);
    command.push_str(&format!(
        "env {} ",
        sh_quote(&format!("PYTHONPATH={helpers}"))
    ));
    command.push_str(&sh_quote(&server.options.remote_python));
    for arg in args {
        command.push(' ');
        command.push_str(&sh_quote(arg));
    }
    external(server, &command)
}
/// Write the genuine SSH interpreter adapter; `$@` is quoted for the remote shell.
pub fn install_interpreter(server: &Server, project: &Path) -> anyhow::Result<PathBuf> {
    let mut command = python(server, &[])?;
    let remote = command
        .args
        .pop()
        .ok_or_else(|| anyhow::anyhow!("Missing remote command"))?;
    let directory = project.join(".rustpy").join("interpreters");
    std::fs::create_dir_all(&directory)?;
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    server.name.hash(&mut hash);
    #[cfg(not(windows))]
    {
        let file = directory.join(format!("ssh-deployment-{:x}", hash.finish()));
        let prefix = std::iter::once(command.program.as_str())
            .chain(command.args.iter().map(String::as_str))
            .map(sh_quote)
            .collect::<Vec<_>>()
            .join(" ");
        let script=format!("#!/bin/sh\nquote() {{ printf \"'%s'\" \"$(printf '%s' \"$1\" | sed \"s/'/'\\\\''/g\")\"; }}\nremote={}\nfor arg do remote=\"$remote $(quote \"$arg\")\"; done\nexec {prefix} \"$remote\"\n",sh_quote(&remote));
        std::fs::write(&file, script)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700))?;
        Ok(file)
    }
    #[cfg(windows)]
    {
        let file = directory.join(format!("ssh-deployment-{:x}.cmd", hash.finish()));
        let mut spec = command.args;
        spec.push(remote);
        let encoded = serde_json::to_string(&spec)?;
        let ps = directory.join(format!("ssh-deployment-{:x}.ps1", hash.finish()));
        let script=format!("$prefix=ConvertFrom-Json '{}'\n$remote=$prefix[-1]\nforeach($a in $args){{$remote += \" '\" + $a.Replace(\"'\",\"'\\''\") + \"'\"}}\n& '{}' @($prefix[0..($prefix.Count-2)]) $remote\nexit $LASTEXITCODE\n",encoded.replace('\'',"''"),command.program.replace('\'',"''"));
        std::fs::write(&ps, script)?;
        std::fs::write(
            &file,
            format!(
                "@powershell.exe -NoProfile -ExecutionPolicy Bypass -File \"{}\" %*\r\n",
                ps.display()
            ),
        )?;
        Ok(file)
    }
}

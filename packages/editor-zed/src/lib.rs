use zed_extension_api::{self as zed, settings::LspSettings};

struct PyreExtension;

impl zed::Extension for PyreExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<zed::Command> {
        if let Some(binary) =
            LspSettings::for_worktree(language_server_id.as_ref(), worktree)?.binary
        {
            if let Some(path) = binary.path {
                return Ok(zed::Command {
                    command: path,
                    args: binary.arguments.unwrap_or_else(|| vec!["lsp".into()]),
                    env: binary.env.unwrap_or_default().into_iter().collect(),
                });
            }
        }

        let command = worktree.which("pyre").unwrap_or_else(|| {
            let executable = if zed::current_platform().0 == zed::Os::Windows {
                "target/debug/pyre.exe"
            } else {
                "target/debug/pyre"
            };
            format!("{}/{}", worktree.root_path(), executable)
        });

        Ok(zed::Command {
            command,
            args: vec!["lsp".into()],
            env: worktree.shell_env(),
        })
    }
}

zed::register_extension!(PyreExtension);

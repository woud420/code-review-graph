use anyhow::{Context, Result, ensure};
use std::{
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};

pub(super) struct Change {
    pub path: PathBuf,
    pub shown: String,
    pub before: Option<String>,
    pub after: Option<String>,
    prior_permissions: Option<fs::Permissions>,
    pub mode: Option<u32>,
}

pub(super) fn read(base: &Path, relative: &Path) -> Result<Option<String>> {
    ensure!(
        relative
            .components()
            .all(|c| matches!(c, Component::Normal(_))),
        "invalid install path"
    );
    let mut check = base.to_path_buf();
    for component in relative.components() {
        check.push(component);
        ensure!(
            !check.is_symlink(),
            "refusing to edit symlink {}",
            check.display()
        );
    }
    match fs::read_to_string(&check) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", check.display())),
    }
}

#[derive(Default)]
pub(super) struct Plan(pub Vec<Change>);

impl Plan {
    pub fn current(&self, base: &Path, file: &str) -> Result<Option<String>> {
        let path = base.join(file);
        if let Some(change) = self.0.iter().find(|change| change.path == path) {
            return Ok(change.after.clone());
        }
        read(base, Path::new(file))
    }
    pub fn backup(&mut self, base: &Path, file: &str, text: &str) -> Result<()> {
        let backup = format!("{file}.bak");
        if self.current(base, &backup)?.is_some() {
            return Ok(());
        }
        self.edit(base, &backup, backup.clone(), false, |_| Ok(text.into()))?;
        #[cfg(unix)]
        if let Some(change) = self.0.iter_mut().find(|c| c.path == base.join(&backup))
            && let Ok(meta) = base.join(file).symlink_metadata()
        {
            use std::os::unix::fs::PermissionsExt;
            change.mode = Some(meta.permissions().mode() & 0o777);
        }
        Ok(())
    }

    pub fn edit(
        &mut self,
        base: &Path,
        relative: &str,
        shown: String,
        executable: bool,
        render: impl FnOnce(&str) -> Result<String>,
    ) -> Result<()> {
        let path = base.join(relative);
        if let Some(index) = self.0.iter().position(|c| c.path == path) {
            let change = &mut self.0[index];
            let after = render(change.after.as_deref().unwrap_or(""))?;
            if change.before.as_deref().unwrap_or("") == after {
                self.0.remove(index);
            } else {
                change.after = Some(after);
            }
            return Ok(());
        }
        let before = read(base, Path::new(relative))?;
        let after = render(before.as_deref().unwrap_or(""))?;
        let mut needs_mode = false;
        #[cfg(unix)]
        if executable && let Ok(meta) = fs::metadata(&path) {
            use std::os::unix::fs::PermissionsExt;
            needs_mode = meta.permissions().mode() & 0o777 != 0o755;
        }
        if before.as_deref().unwrap_or("") != after || needs_mode {
            self.0.push(Change {
                prior_permissions: fs::metadata(&path).ok().map(|m| m.permissions()),
                path,
                shown,
                before,
                after: Some(after),
                mode: executable.then_some(0o755),
            });
        }
        Ok(())
    }

    pub fn remove_known(
        &mut self,
        base: &Path,
        path: &str,
        shown: String,
        matches: impl FnOnce(&str) -> bool,
    ) -> Result<()> {
        let before = read(base, Path::new(path))?;
        if before.as_deref().is_some_and(matches) {
            let path = base.join(path);
            self.0.push(Change {
                prior_permissions: fs::metadata(&path).ok().map(|m| m.permissions()),
                path,
                shown,
                before,
                after: None,
                mode: None,
            });
        }
        Ok(())
    }

    pub fn print(&self) {
        if self.0.is_empty() {
            println!(
                "\n[dry-run] Every file is already up to date.\n\n[dry-run] No files were modified."
            );
            return;
        }
        println!(
            "\n[dry-run] Install would change {} file(s):\n",
            self.0.len()
        );
        for c in &self.0 {
            let mut diff = crate::cli::wire::diff::unified(
                &c.shown,
                c.before.as_deref().unwrap_or(""),
                c.after.as_deref().unwrap_or(""),
            );
            if c.before.is_none() {
                diff = diff.replacen(&format!("--- a/{}", c.shown), "--- /dev/null", 1);
                if c.mode.is_some() {
                    println!("new file, executable: {}", c.shown);
                }
            }
            if c.after.is_none() {
                diff = diff.replacen(&format!("+++ b/{}", c.shown), "+++ /dev/null", 1);
            }
            if c.shown.starts_with("~/") {
                diff = diff.replacen(
                    &format!("+++ b/{}", c.shown),
                    &format!("+++ {}", c.shown),
                    1,
                );
            }
            println!("{diff}");
        }
        println!("[dry-run] No files were modified.");
    }

    pub fn apply(&self) -> Result<()> {
        let mut written: Vec<&Change> = vec![];
        for c in &self.0 {
            let attempt = (|| -> Result<()> {
                ensure!(
                    !c.path.ancestors().any(Path::is_symlink),
                    "symlink in {}",
                    c.path.display()
                );
                let current = match fs::read_to_string(&c.path) {
                    Ok(s) => Some(s),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                    Err(e) => return Err(e.into()),
                };
                ensure!(
                    current == c.before,
                    "{} changed during install",
                    c.path.display()
                );
                match &c.after {
                    Some(text) => replace(&c.path, text.as_bytes(), c.mode),
                    None => Ok(fs::remove_file(&c.path)?),
                }
            })();
            if let Err(err) = attempt {
                for w in written.into_iter().rev() {
                    if fs::read(&w.path).ok().as_deref() == w.after.as_deref().map(str::as_bytes) {
                        match &w.before {
                            Some(s) => {
                                replace(&w.path, s.as_bytes(), None)?;
                                if let Some(permissions) = &w.prior_permissions {
                                    fs::set_permissions(&w.path, permissions.clone())?;
                                }
                            }
                            None => fs::remove_file(&w.path)?,
                        };
                    }
                }
                return Err(err);
            }
            written.push(c);
        }
        Ok(())
    }
}

fn replace(path: &Path, bytes: &[u8], mode: Option<u32>) -> Result<()> {
    let parent = path.parent().context("missing install directory")?;
    fs::create_dir_all(parent)?;
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    if !path.exists() && mode.is_none() {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o666));
    }
    let mut tmp = builder.tempfile_in(parent)?;
    tmp.write_all(bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Some(mode) = mode.or_else(|| fs::metadata(path).ok().map(|m| m.permissions().mode()))
        {
            tmp.as_file()
                .set_permissions(fs::Permissions::from_mode(mode))?;
        }
    }
    tmp.as_file().sync_all()?;
    tmp.persist(path)?;
    Ok(())
}

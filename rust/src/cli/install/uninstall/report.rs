use anyhow::{Result, ensure};
use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};

pub(super) fn clean_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty() && path.components().all(|c| matches!(c, Component::Normal(_)))
}
#[derive(Default)]
pub(super) struct Report {
    dry: bool,
    deleted: Vec<String>,
    edited: Vec<String>,
    skipped: Vec<String>,
    errors: Vec<String>,
    touched: BTreeSet<PathBuf>,
    manifest_only: BTreeSet<PathBuf>,
}
impl Report {
    pub fn new(dry: bool) -> Self {
        Self {
            dry,
            ..Default::default()
        }
    }
    pub fn was_touched(&self, path: &Path) -> bool {
        self.touched.contains(path)
    }
    pub fn with_install_manifest(
        &mut self,
        paths: BTreeSet<PathBuf>,
        action: impl FnOnce(&mut Self),
    ) {
        self.manifest_only = paths;
        action(self);
        self.manifest_only.clear();
    }
    pub fn dry(&self) -> bool {
        self.dry
    }
    pub fn counts(&self) -> (usize, usize) {
        (self.deleted.len(), self.edited.len())
    }
    pub fn failed(&self) -> bool {
        !self.errors.is_empty()
    }
    pub fn print(&self) {
        for (label, rows) in [
            ("delete  ", &self.deleted),
            ("edit    ", &self.edited),
            ("skip    ", &self.skipped),
            ("error   ", &self.errors),
        ] {
            for row in rows {
                println!("  {label}{row}")
            }
        }
    }
    pub fn skip(&mut self, path: &Path, reason: &str) {
        self.skipped.push(format!("{} ({reason})", path.display()))
    }
    fn exists(&mut self, path: &Path, root: &Path) -> bool {
        if path.symlink_metadata().is_err() {
            return false;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            self.skip(
                path,
                &format!("outside allowed boundary {}", root.display()),
            );
            return false;
        };
        if !clean_relative(rel) || root.is_symlink() {
            self.skip(path, "symlink path is not removed");
            return false;
        }
        let mut cursor = root.to_owned();
        for c in rel.components() {
            cursor.push(c);
            if cursor.is_symlink() {
                if path
                    .canonicalize()
                    .ok()
                    .zip(root.canonicalize().ok())
                    .is_some_and(|(p, b)| !p.starts_with(b))
                {
                    self.skip(
                        path,
                        &format!("resolved outside allowed boundary {}", root.display()),
                    )
                } else {
                    self.skip(path, "symlink path is not removed")
                }
                return false;
            }
        }
        true
    }
    pub fn read(&mut self, path: &Path, base: &Path) -> Option<String> {
        if !self.exists(path, base) {
            return None;
        }
        fs::read_to_string(path)
            .map_err(|e| {
                self.errors
                    .push(format!("{}: read failed ({e})", path.display()))
            })
            .ok()
    }
    fn record(&mut self, path: &Path, msg: &str, deleted: bool) {
        let action = if self.dry {
            format!("would {msg}")
        } else {
            msg.replacen("remove", "removed", 1)
        };
        let list = if deleted {
            &mut self.deleted
        } else {
            &mut self.edited
        };
        list.push(format!("{} ({action})", path.display()));
        self.touched.insert(path.to_owned());
    }
    pub fn delete(&mut self, path: &Path, base: &Path, desc: &str, tree: bool) {
        if !self.exists(path, base) {
            return;
        }
        if tree && !path.is_dir() {
            self.skip(path, "expected an owned directory; left unchanged");
            return;
        }
        if !self.dry {
            let res = if tree {
                fs::remove_dir_all(path)
            } else {
                fs::remove_file(path)
            };
            if let Err(e) = res {
                self.errors
                    .push(format!("{}: remove failed ({e})", path.display()));
                return;
            }
        }
        self.record(path, desc, true)
    }
    pub fn write(&mut self, path: &Path, base: &Path, before: &str, after: &str, desc: &str) {
        if before == after {
            return;
        }
        if !self.exists(path, base) {
            return;
        }
        if self.manifest_only.contains(path) && serde_json::from_str(after).is_ok_and(|v| empty(&v))
        {
            self.delete(
                path,
                base,
                "remove file install wrote, as nothing else was in it",
                false,
            );
            return;
        }
        if !self.dry
            && let Err(e) = atomic(path, before, after, true)
        {
            self.errors
                .push(format!("{}: write failed ({e})", path.display()));
            return;
        }
        self.record(path, desc, false)
    }
    pub fn skill(&mut self, path: &Path, base: &Path) {
        self.delete(path, base, "remove generated skill", false);
        if !self.dry
            && self.touched.contains(path)
            && let Some(p) = path.parent()
        {
            prune(p, base)
        }
    }
}
pub(super) fn empty(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Object(m) => m.values().all(empty),
        serde_json::Value::Array(a) => a.iter().all(empty),
        _ => false,
    }
}
pub(super) fn prune(path: &Path, base: &Path) {
    if path != base
        && path.starts_with(base)
        && !path.is_symlink()
        && path
            .ancestors()
            .take_while(|p| *p != base)
            .all(|p| !p.is_symlink())
    {
        let _ = fs::remove_dir(path);
    }
}
pub(super) fn atomic(path: &Path, before: &str, after: &str, keep_mode: bool) -> Result<()> {
    let meta = path.symlink_metadata()?;
    ensure!(meta.is_file(), "file replaced");
    let mut tmp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    tmp.write_all(after.as_bytes())?;
    tmp.as_file().sync_all()?;
    if keep_mode {
        tmp.as_file().set_permissions(meta.permissions())?;
    }
    ensure!(
        !path.is_symlink() && fs::read_to_string(path)? == before,
        "file changed during uninstall"
    );
    tmp.persist(path)?;
    Ok(())
}

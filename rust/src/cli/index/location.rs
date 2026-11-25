use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub struct Location {
    #[arg(long)]
    pub repo: Option<PathBuf>,
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
}

#[derive(Args)]
pub struct FollowSymlinks {
    #[arg(long, overrides_with = "no_follow_symlinks")]
    follow_symlinks: bool,
    #[arg(long, overrides_with = "follow_symlinks")]
    no_follow_symlinks: bool,
}
impl FollowSymlinks {
    pub(in crate::cli) fn explicit(&self) -> Option<bool> {
        if self.follow_symlinks {
            Some(true)
        } else if self.no_follow_symlinks {
            Some(false)
        } else {
            None
        }
    }
}
